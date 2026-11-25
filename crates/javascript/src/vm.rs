use crate::{
    ErrorKind, JsError, JsString, ObjectId, PropertyKey, SymbolId, Value,
    bytecode::{self, Chunk, ClassName, Code, Op, ScopePlan},
    for_in::ForInKeys,
    heap::{
        Binding, Cell, EnvId, EnvKind, Environment, Heap, Object, ObjectKind, Property,
        PropertyKind, array_index,
    },
    parser::{FunctionKind, LabelId, VarKind},
    patterns::{Invocation, PatternMode},
};
use std::{
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::dynamic_import::SourceReferrer;

mod inspection;
pub(crate) mod realm;
mod task_deadline;

use task_deadline::TaskDeadline;

#[cfg(test)]
mod global_environment_tests;
#[cfg(test)]
mod global_reference_tests;

const DEFAULT_EXECUTION_DEPTH: usize = 16;
const DEFAULT_CALL_DEPTH: usize = 24;
const MAX_NATIVE_DEPTH: usize = 64;
const MAX_BYTECODE_DEPTH: usize = 96;
const MAX_CALL_DEPTH: usize = if cfg!(any(windows, target_os = "linux")) {
    MAX_NATIVE_DEPTH
} else {
    DEFAULT_CALL_DEPTH
};

#[inline]
fn native_stack_headroom() -> bool {
    #[cfg(windows)]
    {
        // The next interpreter/host frame may run author code through a
        // native builtin before reaching another depth check. Keep a margin
        // for those frames and honor the current thread's actual reservation.
        let mut low = 0;
        let mut high = 0;
        // SAFETY: both parameters are valid writable pointer-sized locals.
        unsafe {
            windows_sys::Win32::System::Threading::GetCurrentThreadStackLimits(&mut low, &mut high);
        }
        let current = std::ptr::addr_of!(low).addr();
        (low..high).contains(&current) && current - low >= 256 * 1024
    }
    #[cfg(target_os = "linux")]
    {
        // Thread stacks have different reservations, including test and worker threads.
        // Cache only the bounds. Depth checks sample the current frame on every call.
        thread_local! {
            static STACK: Option<(usize, usize)> = {
                let mut attr = std::mem::MaybeUninit::uninit();
                let mut base = std::ptr::null_mut();
                let mut size = 0;
                // SAFETY: pthread initializes attr on success. The two output pointers
                // are valid and the attributes are destroyed once, on the same thread.
                unsafe {
                    if libc::pthread_getattr_np(libc::pthread_self(), attr.as_mut_ptr()) != 0 {
                        None
                    } else {
                        let result = libc::pthread_attr_getstack(attr.as_ptr(), &mut base, &mut size);
                        libc::pthread_attr_destroy(attr.as_mut_ptr());
                        let low = base.addr();
                        low.checked_add(size).filter(|_| result == 0).map(|high| (low, high))
                    }
                }
            };
        }
        let marker = 0u8;
        let current = std::ptr::addr_of!(marker).addr();
        STACK.with(|bounds| {
            bounds.is_some_and(|(low, high)| {
                (low..high).contains(&current) && current - low >= 256 * 1024
            })
        })
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        false
    }
}

#[derive(Clone, Debug)]
pub struct VmLimits {
    /// Expose shared memory only to hosts that permit it.
    pub shared_array_buffer: bool,
    pub max_instructions: u64,
    /// Linear, bounded native buffer work per instruction. Bytecode, callbacks
    /// and backtracking still pay their normal instruction costs.
    pub buffer_units_per_instruction: usize,
    pub max_call_depth: usize,
    /// Synchronous nested bytecode chunks, including blocks within a call.
    pub max_bytecode_depth: usize,
    pub max_heap_objects: usize,
    /// Conservative aggregate allocation allowance, reset to traced live usage by GC.
    pub max_heap_bytes: usize,
    pub max_source_bytes: usize,
    pub max_string_units: usize,
    /// Bounds materialized elements and indexed work in native collection operations.
    pub max_array_length: u32,
    pub max_pending_jobs: usize,
}
impl Default for VmLimits {
    fn default() -> Self {
        Self {
            shared_array_buffer: true,
            max_instructions: 1_000_000,
            buffer_units_per_instruction: 1,
            max_call_depth: MAX_CALL_DEPTH,
            max_bytecode_depth: MAX_NATIVE_DEPTH,
            max_heap_objects: 100_000,
            max_heap_bytes: 64 * 1024 * 1024,
            max_source_bytes: 2 * 1024 * 1024,
            max_string_units: 8 * 1024 * 1024,
            max_array_length: 1_000_000,
            max_pending_jobs: 4096,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RootId(u64);
#[derive(Clone, Copy, Debug, Default)]
pub struct GcStats {
    pub before: usize,
    pub after: usize,
    pub collected: usize,
    pub skipped_active_execution: bool,
}

/// A single-thread-owned VM. Native callbacks must explicitly root any JavaScript
/// values retained outside the callback. Captured Rust/DOM data needs no JS tracing.
pub struct Vm {
    pub(crate) heap: Heap,
    pub(crate) realm: realm::RealmState,
    inactive_realms: BTreeMap<EnvId, realm::RealmState>,
    realm_depth: usize,
    realm_bootstrap_bytes: usize,
    realm_bootstrap_cells: usize,
    pub(crate) bigint_storage: crate::bigint::Storage,
    roots: BTreeMap<RootId, Value>,
    next_root: u64,
    pub(crate) next_private_id: u64,
    pub(crate) limits: VmLimits,
    remaining: u64,
    task_deadline: Option<TaskDeadline>,
    interrupt: Option<Arc<AtomicBool>>,
    active: usize,
    pub(crate) call_depth: usize,
    active_calls: Vec<Option<JsString>>,
    legacy_calls: Vec<LegacyCall>,
    inspection_enabled: bool,
    pub(crate) execution_depth: usize,
    string_storage: BTreeMap<usize, usize>,
    string_bytes: usize,
    pub(crate) code_bytes: usize,
    initializing: bool,
    pub(crate) jobs: VecDeque<crate::promise::Job>,
    pub(crate) atomic_waiters: crate::binary::waiters::Waiters,
    pub(crate) unhandled_promises: Vec<ObjectId>,
    pub(crate) json_depth: usize,
    pub(crate) well_known_symbols: BTreeMap<&'static str, SymbolId>,
    pub(crate) symbol_registry: BTreeMap<JsString, SymbolId>,
    pub(crate) symbol_storage: BTreeMap<usize, std::rc::Weak<crate::value::SymbolData>>,
    pub(crate) template_objects: BTreeMap<usize, (std::rc::Weak<bytecode::TemplateSite>, Value)>,
    pub(crate) pattern_depth: usize,
    pub(crate) module_evaluation_active: bool,
    pub(crate) module_async_order: u64,
}
struct LegacyCall {
    function: ObjectId,
    realm: EnvId,
    depth: usize,
    visible: bool,
}

#[derive(Clone)]
pub(crate) enum Reference {
    Binding(EnvId, JsString),
    Global(ObjectId, JsString),
    With(ObjectId, JsString),
    Property(Value, Value),
    Private(Value, crate::private::PrivateId),
    Super(Value, Value, Value),
    Import(EnvId, JsString),
    Unresolvable(JsString),
}
#[derive(Clone)]
pub(crate) enum Item {
    Value(Value),
    Reference(Reference),
    CallTarget {
        function: Value,
        this: Value,
        direct_eval: bool,
    },
    Arguments(Vec<Value>),
}
pub(crate) enum Completion {
    Normal(Option<Value>),
    Return(Value),
    Break(Option<LabelId>, Option<Value>),
    Continue(Option<LabelId>, Option<Value>),
}
impl Completion {
    pub(crate) fn update_empty(self, fallback: Option<Value>) -> Self {
        match self {
            Self::Normal(None) => Self::Normal(fallback),
            Self::Break(target, None) => Self::Break(target, fallback),
            Self::Continue(target, None) => Self::Continue(target, fallback),
            completion => completion,
        }
    }

    pub(crate) fn leave_label(self, label: LabelId) -> Self {
        match self {
            Self::Break(Some(target), value) if target == label => Self::Normal(value),
            completion => completion,
        }
    }

    /// None means the current iteration should continue. A completion must
    /// leave this loop, closing its iterator if it uses the iterator protocol.
    pub(crate) fn after_loop_body(
        self,
        label: Option<LabelId>,
        result: &mut Option<Value>,
    ) -> Option<Self> {
        let value = match self {
            Self::Normal(value) => value,
            Self::Continue(target, value) if target.is_none() || target == label => value,
            Self::Break(target, value) if target.is_none() || target == label => {
                return Some(Self::Normal(value.or_else(|| result.take())));
            }
            abrupt => return Some(abrupt.update_empty(result.take())),
        };
        if value.is_some() {
            *result = value;
        }
        None
    }
}
impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}
impl Vm {
    pub fn new() -> Self {
        Self::with_limits(VmLimits::default())
    }
    pub fn with_limits(mut limits: VmLimits) -> Self {
        limits.buffer_units_per_instruction = limits.buffer_units_per_instruction.clamp(1, 1024);
        // This first interpreter still uses bounded Rust calls for nested chunks.
        // Keep the host stack safe even when an embedder requests a larger limit.
        limits.max_call_depth = limits.max_call_depth.min(MAX_CALL_DEPTH);
        limits.max_bytecode_depth = limits.max_bytecode_depth.min(MAX_BYTECODE_DEPTH);
        let mut heap = Heap::default();
        let global_env = realm::allocate_primary_core(&mut heap);
        let mut vm = Self {
            realm: realm::RealmState::new(&heap, global_env, true),
            heap,
            inactive_realms: BTreeMap::new(),
            realm_depth: 0,
            realm_bootstrap_bytes: 0,
            realm_bootstrap_cells: 0,
            bigint_storage: crate::bigint::Storage::default(),
            roots: BTreeMap::new(),
            next_root: 1,
            next_private_id: 1,
            remaining: limits.max_instructions,
            limits,
            interrupt: None,
            task_deadline: None,
            active: 0,
            call_depth: 0,
            active_calls: Vec::new(),
            legacy_calls: Vec::new(),
            inspection_enabled: false,
            execution_depth: 0,
            string_storage: BTreeMap::new(),
            string_bytes: 0,
            code_bytes: 0,
            initializing: true,
            jobs: VecDeque::new(),
            atomic_waiters: Default::default(),
            unhandled_promises: Vec::new(),
            json_depth: 0,
            well_known_symbols: BTreeMap::new(),
            symbol_registry: BTreeMap::new(),
            symbol_storage: BTreeMap::new(),
            template_objects: BTreeMap::new(),
            pattern_depth: 0,
            module_evaluation_active: false,
            module_async_order: 0,
        };
        vm.install_realm_builtins();
        vm.realm_bootstrap_cells = vm.heap.live;
        vm.realm_bootstrap_bytes = vm.estimated_heap_bytes();
        vm
    }
    #[cfg(feature = "twitch-bootstrap-profile")]
    pub fn qa_factory(&self, entering: bool, id: u64) {
        QA_STACK.with_borrow_mut(|stack| {
            if entering {
                stack.push(id);
            } else {
                stack.pop();
            }
        });
    }

    #[cfg(feature = "twitch-bootstrap-profile")]
    pub fn qa_fuel(&self) -> u64 {
        self.remaining
    }

    #[cfg(feature = "twitch-bootstrap-profile")]
    pub fn qa_begin(&self) {
        QA_STACK.with_borrow_mut(Vec::clear);
        QA_DATA.with_borrow_mut(|histogram| *histogram = Some(Default::default()));
    }

    #[cfg(feature = "twitch-bootstrap-profile")]
    pub fn qa_finish(&self) {
        if let Some(histogram) = QA_DATA.with_borrow_mut(Option::take) {
            let mut rows: Vec<_> = histogram.into_iter().collect();
            rows.sort_by_key(|(_, (total, _))| std::cmp::Reverse(*total));
            for ((module, kind, path, line), (total, count)) in rows {
                println!("COST {kind}@{module} {path}:{line} {total} {count}");
            }
        }
    }

    pub fn limits(&self) -> &VmLimits {
        &self.limits
    }
    /// Controls whether this VM can compile new WebAssembly modules from bytes.
    ///
    /// Standalone VMs allow compilation by default. Validation and instantiation
    /// of an already compiled module are unaffected.
    pub fn set_wasm_compilation_allowed(&mut self, allowed: bool) {
        self.realm.wasm_compilation_allowed = allowed;
    }
    pub fn check_syntax(&mut self, source: &str) -> Result<(), JsError> {
        let chunk = bytecode::compile(source, self.compilation_budget(source)?)?;
        self.ensure_allocation(bytecode::retained_size_with_chunks(Vec::new(), vec![chunk]))
    }
    pub(crate) fn compilation_budget(&self, source: &str) -> Result<usize, JsError> {
        self.compilation_budget_for_bytes(source.len())
    }
    fn compilation_budget_for_bytes(&self, bytes: usize) -> Result<usize, JsError> {
        if bytes > self.limits.max_source_bytes {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Script source exceeds configured byte limit",
            ));
        }
        // UTF-32 lexer input and decoded character data, including temporary
        // UTF-16 buffers during literal conversion. The lexer accounts for
        // per-token structural allocations as it consumes the source.
        let input_bytes = bytes.saturating_mul(16).saturating_add(4096);
        self.ensure_allocation(input_bytes)?;
        Ok(self
            .limits
            .max_heap_bytes
            .saturating_sub(self.estimated_heap_bytes())
            .saturating_sub(input_bytes))
    }
    pub(crate) fn materialize_body(&mut self, code: &Code) -> Result<Rc<Chunk>, JsError> {
        if let Some(body) = code.body.get() {
            return Ok(body.clone());
        }
        // The lexer reuses the already-retained span. Reserve transient decoded
        // literal buffers, then charge structural allocations against the free heap.
        let scratch = code.source.len().saturating_mul(8).saturating_add(4096);
        self.ensure_allocation(scratch)?;
        let budget = self.limits.max_heap_bytes
            .saturating_sub(self.estimated_heap_bytes())
            .saturating_sub(scratch);
        let body = bytecode::recompile_body(code, budget)?;
        let retained = bytecode::retained_size(vec![], vec![body.clone()], Some(code.source.storage().0));
        self.ensure_allocation(retained)?;
        self.code_bytes = self.code_bytes.saturating_add(retained);
        let _ = code.body.set(body.clone());
        Ok(body)
    }

    pub(crate) fn retain_compiled_chunk(&mut self, chunk: &Rc<Chunk>) -> Result<(), JsError> {
        let bytes = bytecode::retained_size_with_chunks(Vec::new(), vec![chunk.clone()]);
        self.ensure_allocation(bytes)?;
        self.code_bytes = self.code_bytes.saturating_add(bytes);
        Ok(())
    }
    pub fn eval(&mut self, source: &str) -> Result<Value, JsError> {
        self.eval_with_referrer(source, None, None)
    }

    /// Execute a classic script whose dynamic imports and functions use the given
    /// source identifier as their referrer. The host interprets the identifier.
    pub fn eval_with_identifier(
        &mut self,
        source: &str,
        identifier: &str,
    ) -> Result<Value, JsError> {
        self.ensure_string_allocation(identifier.encode_utf16().count())?;
        let identifier = self.account_value(Value::from(identifier))?;
        let Value::String(identifier) = identifier else {
            unreachable!()
        };
        self.eval_with_referrer(source, Some(SourceReferrer::Script(identifier)), None)
    }

    /// An embedded parser may synchronously execute a child script while its
    /// caller is still on the stack. Set aside part of the *existing* task
    /// instruction, CPU and heap headroom so the host can deliver a child resource
    /// error and resume its caller. Work and live objects remain charged to
    /// the enclosing task. Host interruption and internal errors propagate.
    pub fn eval_embedded_script(
        &mut self,
        source: &str,
        identifier: &str,
    ) -> (Result<Value, JsError>, bool) {
        if self.active == 0 {
            return (self.eval_with_identifier(source, identifier), false);
        }
        let child_budget = (self.remaining - self.remaining / 4).min(self.limits.max_instructions);
        let reserved = self.remaining - child_budget;
        let parent_cells = self.limits.max_heap_objects;
        let parent_bytes = self.limits.max_heap_bytes;
        let reserved_cells = parent_cells.saturating_sub(self.heap.live) / 4;
        let reserved_bytes = parent_bytes.saturating_sub(self.estimated_heap_bytes()) / 4;
        self.remaining -= reserved;
        self.limits.max_heap_objects -= reserved_cells;
        self.limits.max_heap_bytes -= reserved_bytes;
        let child_deadline = self.task_deadline.as_ref().and_then(TaskDeadline::for_child);
        let parent_deadline = child_deadline.and_then(|child| self.task_deadline.replace(child));
        let result = self.eval_with_identifier(source, identifier);
        if let Some(parent) = parent_deadline {
            self.task_deadline = Some(parent);
        }
        self.remaining += reserved;
        self.limits.max_heap_objects = parent_cells;
        self.limits.max_heap_bytes = parent_bytes;
        // Fixed operation bounds (for example RegExp nesting) may fail with
        // fuel left. Keep the original error and report it in the child when
        // the enclosing task still has resources to deliver that report.
        let recoverable_child_limit = result.as_ref().is_err_and(|error| {
            matches!(error.kind, ErrorKind::ExecutionLimit | ErrorKind::HeapLimit)
        }) && self.remaining > 0
            && self.check_interrupt().is_ok()
            && self.heap.live < parent_cells
            && self.ensure_allocation(0).is_ok();
        (result, recoverable_child_limit)
    }

    fn eval_with_referrer(
        &mut self,
        source: &str,
        referrer: Option<SourceReferrer>,
        inspected_node: Option<Value>,
    ) -> Result<Value, JsError> {
        self.check_interrupt()?;
        let chunk = bytecode::compile(source, self.compilation_budget(source)?)?;
        self.retain_compiled_chunk(&chunk)?;
        if self.active == 0 {
            self.remaining = self.limits.max_instructions;
        }
        self.active += 1;
        let result = self
            .with_referrer(referrer, |vm| match inspected_node {
                Some(node) => vm.run_inspected_command(&chunk, node),
                None => vm.run_chunk(&chunk, vm.realm.global_env),
            })
            .and_then(|c| match c {
                Completion::Normal(v) => Ok(v.unwrap_or_default()),
                _ => Err(JsError::new(
                    ErrorKind::InternalError,
                    "Unexpected top-level abrupt completion",
                )),
            });
        self.active -= 1;
        result
    }
    pub fn call(&mut self, function: Value, this: Value, args: &[Value]) -> Result<Value, JsError> {
        if self.active == 0 {
            self.check_interrupt()?;
            self.remaining = self.limits.max_instructions;
        }
        self.active += 1;
        let result = self.invoke(function, this, args);
        self.active -= 1;
        result
    }
    /// One browser task and callback/microtask checkpoint share one budget.
    /// Collection is deferred until this scope returns, including on an error.
    pub fn with_task<T, F>(&mut self, callback: F) -> Result<T, JsError>
    where
        F: FnOnce(&mut Vm) -> Result<T, JsError>,
    {
        self.with_task_limits(self.limits.max_instructions, None, callback)
    }

    /// A host may give an outer scheduled callback more work with a wall-clock
    /// deadline. Nested tasks and their microtasks always share the outer limits.
    pub fn with_task_limits<T, F>(
        &mut self,
        instructions: u64,
        deadline: Option<Instant>,
        callback: F,
    ) -> Result<T, JsError>
    where
        F: FnOnce(&mut Vm) -> Result<T, JsError>,
    {
        self.run_task(instructions, deadline.map(TaskDeadline::Wall), callback)
    }

    /// Charge only execution on this thread to the CPU allowance. The absolute
    /// wall deadline also bounds blocked callbacks. Nested work shares both limits.
    pub fn with_task_cpu_budget<T, F>(
        &mut self,
        instructions: u64,
        cpu_budget: Duration,
        wall_deadline: Instant,
        callback: F,
    ) -> Result<T, JsError>
    where
        F: FnOnce(&mut Vm) -> Result<T, JsError>,
    {
        self.run_task(
            instructions,
            Some(TaskDeadline::for_thread(cpu_budget, wall_deadline)),
            callback,
        )
    }

    fn run_task<T, F>(
        &mut self,
        instructions: u64,
        deadline: Option<TaskDeadline>,
        callback: F,
    ) -> Result<T, JsError>
    where
        F: FnOnce(&mut Vm) -> Result<T, JsError>,
    {
        let outer = self.active == 0;
        if outer {
            self.check_interrupt()?;
            self.remaining = instructions;
            self.task_deadline = deadline;
        }
        self.active += 1;
        let result = callback(self);
        self.active -= 1;
        if outer {
            self.task_deadline = None;
        }
        result
    }
    /// Allow a worker's owning thread to request cooperative termination.
    pub fn set_interrupt_flag(&mut self, flag: Arc<AtomicBool>) {
        self.interrupt = Some(flag);
    }

    fn check_interrupt(&self) -> Result<(), JsError> {
        if let Some(message) = self.task_deadline.as_ref().and_then(TaskDeadline::exceeded) {
            return Err(JsError::new(ErrorKind::ExecutionLimit, message));
        }
        if self
            .interrupt
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Worker execution terminated",
            ));
        }
        Ok(())
    }

    pub fn global(&self) -> Value {
        Value::Object(self.realm.global_object)
    }
    pub(crate) fn global_this(&self) -> Value {
        self.heap
            .environment(self.realm.global_env)
            .this
            .clone()
            .expect("global environment has a this binding")
    }

    pub(crate) fn install_global_this(&mut self) -> Result<(), JsError> {
        self.define_data_property(
            self.global(),
            "globalThis",
            self.global_this(),
            true,
            false,
            true,
        )
    }

    pub fn get_global(&mut self, key: &str) -> Result<Value, JsError> {
        let reference = self.resolve(self.realm.global_env, &key.into())?;
        if matches!(reference, Reference::Unresolvable(_)) {
            Ok(Value::Undefined)
        } else {
            self.read_reference(reference)
        }
    }
    pub fn set_global(&mut self, key: &str, value: Value) -> Result<(), JsError> {
        let reference = self.resolve(self.realm.global_env, &key.into())?;
        self.write_reference(reference, value, false)
    }
    pub fn new_object(&mut self) -> Value {
        Value::Object(self.heap.allocate(Cell::Object(Object::new(
            ObjectKind::Ordinary,
            Some(self.realm.object_prototype),
        ))))
    }
    /// Host metadata must contain Rust data only, with its own host byte budget.
    /// JavaScript values retained by the host require separate explicit roots.
    pub fn set_host_data<T: 'static>(&mut self, object: Value, data: Rc<T>) -> Result<(), JsError> {
        let id = object
            .as_object()
            .ok_or_else(|| JsError::type_error("Host data target must be an object"))?;
        self.heap.object_mut(id)?.host_data = Some(data);
        Ok(())
    }
    pub fn host_data<T: 'static>(&self, object: &Value) -> Option<Rc<T>> {
        self.heap
            .object(object.as_object()?)
            .ok()?
            .host_data
            .as_ref()?
            .clone()
            .downcast::<T>()
            .ok()
    }
    pub fn set_host_slot(
        &mut self,
        object: Value,
        index: u32,
        value: Value,
    ) -> Result<(), JsError> {
        let id = object
            .as_object()
            .ok_or_else(|| JsError::type_error("Host slot target must be an object"))?;
        if !self.heap.object(id)?.host_slots.contains_key(&index) {
            if self.heap.object(id)?.host_slots.len() >= 32 {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Host slot count budget exhausted",
                ));
            }
            self.reserve_allocation(128)?;
        }
        let value = self.account_value(value)?;
        self.heap.object_mut(id)?.host_slots.insert(index, value);
        Ok(())
    }
    pub fn host_slot(&self, object: &Value, index: u32) -> Option<Value> {
        self.heap
            .object(object.as_object()?)
            .ok()?
            .host_slots
            .get(&index)
            .cloned()
    }
    pub fn is_array(&self, value: &Value) -> bool {
        value
            .as_object()
            .and_then(|id| self.heap.object(id).ok())
            .is_some_and(|o| matches!(o.kind, ObjectKind::Array { .. }))
    }
    /// Ordinary storage data has no platform-owned or exotic internal slots.
    pub fn supports_ordinary_clone(&self, value: &Value) -> bool {
        value
            .as_object()
            .and_then(|id| self.heap.object(id).ok())
            .is_some_and(|object| {
                matches!(object.kind, ObjectKind::Ordinary | ObjectKind::Array { .. })
                    && !object.is_platform_object()
            })
    }
    pub fn enumerable_own_keys(&mut self, value: Value) -> Result<Vec<JsString>, JsError> {
        self.own_keys(value, true)
    }
    pub fn new_object_with_prototype(&mut self, prototype: Value) -> Result<Value, JsError> {
        if prototype != Value::Null && prototype.as_object().is_none() {
            return Err(JsError::type_error("Prototype must be an object or null"));
        }
        if let Some(id) = prototype.as_object() {
            self.heap.object(id)?;
        }
        Ok(Value::Object(self.heap.allocate(Cell::Object(
            Object::new(ObjectKind::Ordinary, prototype.as_object()),
        ))))
    }
    pub fn new_array(&mut self, values: &[Value]) -> Value {
        self.new_array_with_prototype(values, self.realm.array_prototype)
    }
    fn new_array_with_prototype(&mut self, values: &[Value], prototype: ObjectId) -> Value {
        let mut object = Object::new(
            ObjectKind::Array {
                length: values.len().min(u32::MAX as usize) as u32,
                length_writable: true,
            },
            Some(prototype),
        );
        for (i, v) in values.iter().enumerate() {
            object.insert(i.to_string().into(), Property::data(v.clone()));
        }
        Value::Object(self.heap.allocate(Cell::Object(object)))
    }
    /// Constructs an ordinary array while charging its elements and storage.
    pub fn try_new_array(&mut self, values: &[Value]) -> Result<Value, JsError> {
        self.try_new_array_with_prototype(values, self.realm.array_prototype)
    }
    pub(crate) fn try_new_array_with_prototype(
        &mut self,
        values: &[Value],
        prototype: ObjectId,
    ) -> Result<Value, JsError> {
        if values.len() > self.limits.max_array_length as usize {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Array length budget exhausted",
            ));
        }
        self.ensure_allocation(256usize.saturating_add(values.len().saturating_mul(256)))?;
        for value in values {
            self.account_value(value.clone())?;
        }
        Ok(self.new_array_with_prototype(values, prototype))
    }
    pub(crate) fn source_allocation(bytes: usize) -> usize {
        bytes.saturating_mul(512).saturating_add(4096)
    }
    pub fn estimated_heap_bytes(&self) -> usize {
        self.heap
            .estimated_bytes
            .saturating_add(self.inactive_realms.len() * std::mem::size_of::<realm::RealmState>())
            .saturating_add(self.string_bytes)
            .saturating_add(self.code_bytes)
            .saturating_add(self.bigint_storage.bytes)
    }
    pub(crate) fn ensure_allocation(&self, bytes: usize) -> Result<(), JsError> {
        if !self.initializing
            && self.estimated_heap_bytes().saturating_add(bytes) > self.limits.max_heap_bytes
        {
            Err(JsError::new(
                ErrorKind::HeapLimit,
                "Aggregate allocation byte budget exhausted; collect between script tasks",
            ))
        } else {
            Ok(())
        }
    }
    #[cfg_attr(feature = "twitch-bootstrap-profile", track_caller)]
    pub(crate) fn reserve_allocation(&mut self, bytes: usize) -> Result<(), JsError> {
        #[cfg(feature = "twitch-bootstrap-profile")]
        qa_charge("reserve_allocation", bytes);
        self.ensure_allocation(bytes)?;
        self.heap.estimated_bytes = self.heap.estimated_bytes.saturating_add(bytes);
        Ok(())
    }
    pub(crate) fn ensure_dynamic_source(&self, units: usize) -> Result<(), JsError> {
        let bytes = units.saturating_mul(3);
        if bytes > self.limits.max_source_bytes {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Dynamic source byte budget exhausted",
            ));
        }
        self.ensure_allocation(Self::source_allocation(bytes))
    }
    fn account_string(&mut self, string: &JsString) -> Result<(), JsError> {
        let mut part = Some(string.clone());
        while let Some(string) = part {
            let bytes = string.allocation_bytes();
            let old = self
                .string_storage
                .get(&string.storage_id())
                .copied()
                .unwrap_or(0);
            if bytes <= old {
                break;
            }
            self.ensure_allocation(bytes - old)?;
            self.string_bytes = self.string_bytes.saturating_add(bytes - old);
            self.string_storage.insert(string.storage_id(), bytes);
            part = string.prefix();
        }
        Ok(())
    }
    pub(crate) fn account_value(&mut self, value: Value) -> Result<Value, JsError> {
        if let Value::BigInt(integer) = &value {
            self.account_bigint(integer)?;
        }
        if let Value::Symbol(symbol) = &value {
            self.account_symbol(symbol)?;
        }
        if let Value::String(string) = &value {
            if string.len() > self.limits.max_string_units {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "String code-unit budget exhausted",
                ));
            }
            self.account_string(string)?;
        }
        Ok(value)
    }
    pub(crate) fn check_string_length(&self, units: usize) -> Result<(), JsError> {
        if units > self.limits.max_string_units {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "String code-unit budget exhausted",
            ));
        }
        Ok(())
    }
    pub fn ensure_string_allocation(&self, units: usize) -> Result<(), JsError> {
        self.check_string_length(units)?;
        // Include both a growing UTF-16 Vec and conversion into Rc storage.
        self.ensure_allocation(units.saturating_mul(6).saturating_add(128))
    }
    pub fn new_native_function<F>(&mut self, name: &str, callback: F) -> Value
    where
        F: Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
    {
        self.native(name, 0, false, Rc::new(callback))
    }
    /// Creates a callable host exotic with the Annex B [[IsHTMLDDA]] slot.
    /// Mark the handle before it is exposed to script or retained by the host.
    pub fn new_html_dda<F>(&mut self, callback: F) -> Value
    where
        F: Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
    {
        let mut id = self.new_native_function("", callback).as_object().unwrap();
        id.heap_id |= ObjectId::HTML_DDA;
        Value::Object(id)
    }

    /// A native call/construct pair. Construction passes `true`, an undefined
    /// receiver, and requires an object result; subclass/newTarget is not exposed.
    pub fn new_native_constructor<F>(&mut self, name: &str, callback: F) -> Value
    where
        F: Fn(&mut Vm, Value, &[Value], bool) -> Result<Value, JsError> + 'static,
    {
        let callback = Rc::new(callback);
        let call = callback.clone();
        let function = self.native(
            name,
            0,
            false,
            Rc::new(move |vm, this, args| call(vm, this, args, false)),
        );
        crate::builtins::replace_construct(self, function.clone(), move |vm, _, args, _| {
            let value = callback(vm, Value::Undefined, args, true)?;
            if value.as_object().is_none() {
                return Err(JsError::type_error(
                    "Native constructor must return an object",
                ));
            }
            Ok(value)
        });
        function
    }
    pub fn new_native_constructor_with_new_target<F>(&mut self, name: &str, callback: F) -> Value
    where
        F: Fn(&mut Vm, Value, &[Value], Option<Value>) -> Result<Value, JsError> + 'static,
    {
        let callback = Rc::new(callback);
        let call = callback.clone();
        let function = self.native(
            name,
            0,
            false,
            Rc::new(move |vm, this, args| call(vm, this, args, None)),
        );
        crate::builtins::replace_construct(self, function.clone(), move |vm, _, args, target| {
            let value = callback(vm, Value::Undefined, args, Some(target))?;
            if value.as_object().is_none() {
                return Err(JsError::type_error(
                    "Native constructor must return an object",
                ));
            }
            Ok(value)
        });
        function
    }
    pub(crate) fn native(
        &mut self,
        name: &str,
        length: usize,
        constructable: bool,
        callback: crate::heap::NativeCallback,
    ) -> Value {
        let construct: Option<crate::heap::NativeConstruct> = if constructable {
            let call = callback.clone();
            Some(Rc::new(move |vm, _, args, _| {
                call(vm, Value::Undefined, args)
            }))
        } else {
            None
        };
        let initial_name: JsString = name.into();
        let mut object = Object::new(
            ObjectKind::Native {
                initial_name: initial_name.clone(),
                callback,
                construct,
                realm_environment: self.realm.global_env,
            },
            Some(self.realm.function_prototype),
        );
        object.insert(
            "length".into(),
            Property {
                kind: PropertyKind::Data {
                    value: Value::from(length),
                    writable: false,
                },
                enumerable: false,
                configurable: true,
            },
        );
        object.insert(
            "name".into(),
            Property {
                kind: PropertyKind::Data {
                    value: Value::String(initial_name),
                    writable: false,
                },
                enumerable: false,
                configurable: true,
            },
        );
        Value::Object(self.heap.allocate(Cell::Object(object)))
    }
    pub fn is_callable(&self, value: &Value) -> bool {
        value
            .as_object()
            .and_then(|id| self.heap.object(id).ok())
            .is_some_and(|o| {
                matches!(
                    o.kind,
                    ObjectKind::Proxy(crate::proxy::Proxy { callable: true, .. })
                        | ObjectKind::Function { .. }
                        | ObjectKind::Native { .. }
                        | ObjectKind::Bound { .. }
                        | ObjectKind::PromiseFunction(..)
                )
            })
    }
    pub fn get_property(&mut self, object: Value, key: &str) -> Result<Value, JsError> {
        self.get(object, &key.into())
    }
    /// Compare actual prototype identities without invoking user getters or hooks.
    pub fn has_prototype(&mut self, value: &Value, prototype: &Value) -> Result<bool, JsError> {
        let prototype = prototype
            .as_object()
            .ok_or_else(|| JsError::type_error("Prototype must be an object"))?;
        self.heap.object(prototype)?;
        let Some(id) = value.as_object() else {
            return Ok(false);
        };
        let mut current = self.object_prototype_of(id)?;
        for _ in 0..1024 {
            self.consume_work(1)?;
            let Some(id) = current else {
                return Ok(false);
            };
            if id == prototype {
                return Ok(true);
            }
            current = self.object_prototype_of(id)?;
        }
        Err(JsError::new(
            ErrorKind::RangeError,
            "Prototype chain limit exhausted",
        ))
    }
    pub fn set_property(&mut self, object: Value, key: &str, value: Value) -> Result<(), JsError> {
        self.set(object, &key.into(), value, true)
    }
    pub fn define_accessor(
        &mut self,
        object: Value,
        key: &str,
        getter: Option<Value>,
        setter: Option<Value>,
    ) -> Result<(), JsError> {
        for function in getter.iter().chain(setter.iter()) {
            if !self.is_callable(function) {
                return Err(JsError::type_error("Accessor must be callable"));
            }
        }
        self.define_property(
            object,
            key.into(),
            Property {
                kind: PropertyKind::Accessor {
                    get: getter,
                    set: setter,
                },
                enumerable: true,
                configurable: true,
            },
        )
    }
    pub fn define_data_property(
        &mut self,
        object: Value,
        key: &str,
        value: Value,
        writable: bool,
        enumerable: bool,
        configurable: bool,
    ) -> Result<(), JsError> {
        self.define_property(
            object,
            key.into(),
            Property {
                kind: PropertyKind::Data { value, writable },
                enumerable,
                configurable,
            },
        )
    }
    pub fn root(&mut self, value: Value) -> RootId {
        let id = RootId(self.next_root);
        self.next_root = self
            .next_root
            .checked_add(1)
            .expect("root identifier space exhausted");
        self.roots.insert(id, value);
        id
    }
    pub fn root_value(&self, id: RootId) -> Option<Value> {
        self.roots.get(&id).cloned()
    }
    pub fn unroot(&mut self, id: RootId) -> Option<Value> {
        self.roots.remove(&id)
    }
    pub fn collect_garbage(&mut self) -> GcStats {
        let before = self.heap.live;
        if self.active > 0 || self.realm_depth > 0 {
            return GcStats {
                before,
                after: before,
                collected: 0,
                skipped_active_execution: true,
            };
        }
        let mut roots = self.heap.trace_roots();
        roots.push(self.realm.global_env.0);
        roots.extend(self.unhandled_promises.iter().copied());
        roots.extend(self.atomic_waiters.timed_roots());
        for job in &self.jobs {
            job.trace(&mut roots);
        }
        for value in self.roots.values() {
            roots.value(value);
        }
        for symbol in self.well_known_symbols.values() {
            roots.symbol(symbol);
        }
        self.template_objects
            .retain(|_, (site, _)| site.strong_count() > 0);
        roots.extend(
            self.template_objects
                .values()
                .filter_map(|(_, value)| value.as_object()),
        );
        let (collected, cleanup) = self.heap.collect(roots, self.atomic_waiters.indefinite_edges());
        self.atomic_waiters.retain_live(&self.heap);
        self.schedule_registry_cleanup(cleanup);
        self.inactive_realms
            .retain(|realm, _| self.heap.realm_core(*realm).is_some());
        self.bigint_storage.reclaim();
        self.recompute_memory();
        GcStats {
            before,
            after: self.heap.live,
            collected,
            skipped_active_execution: false,
        }
    }
    pub fn heap_cell_count(&self) -> usize {
        self.heap.live
    }
    fn recompute_memory(&mut self) {
        let mut strings = Vec::new();
        let mut codes = Vec::new();
        let mut chunks = Vec::new();
        fn value(v: &Value, strings: &mut Vec<JsString>) {
            if let Value::String(s) = v {
                strings.push(s.clone());
            }
        }
        for cell in self.heap.live_cells() {
            match cell {
                Cell::Environment(env) => {
                    if let Some(args) = &env.pending_arguments {
                        for arg in args.iter() { value(arg, &mut strings); }
                    }
                    strings.extend(env.private_names.keys().cloned());
                    for (key, (_, target)) in &env.aliases {
                        strings.push(key.clone());
                        strings.push(target.clone());
                    }
                    for (key, binding) in &env.bindings {
                        strings.push(key.clone());
                        value(&binding.value, &mut strings);
                    }
                    if let Some(this) = &env.this {
                        value(this, &mut strings);
                    }
                }
                Cell::Object(object) => {
                    if let Some(data) = &object.class_data {
                        for field in &data.fields {
                            match &field.key {
                                crate::classes::ElementName::Public(PropertyKey::String(key))
                                | crate::classes::ElementName::Private(_, key) => {
                                    strings.push(key.clone());
                                }
                                crate::classes::ElementName::Public(PropertyKey::Symbol(_)) => {}
                            }
                            chunks.extend(field.value.iter().cloned());
                        }
                    }
                    for element in object.private_elements.values() {
                        element.visit(|v| value(v, &mut strings));
                    }
                    for v in object.host_slots.values() {
                        value(v, &mut strings);
                    }
                    for (key, p) in &object.properties {
                        strings.push(key.clone());
                        if let PropertyKind::Data { value: v, .. } = &p.kind {
                            value(v, &mut strings);
                        }
                    }
                    for p in object.symbols.values() {
                        if let PropertyKind::Data { value: v, .. } = &p.kind {
                            value(v, &mut strings);
                        }
                    }
                    match &object.kind {
                        ObjectKind::Function { code, referrer, .. } => {
                            codes.push(code.clone());
                            if let Some(SourceReferrer::Script(identifier)) = referrer {
                                strings.push(identifier.clone());
                            }
                        }
                        ObjectKind::Bound { this, args, .. } => {
                            value(this, &mut strings);
                            for v in args {
                                value(v, &mut strings);
                            }
                        }
                        ObjectKind::Arguments { mapped, .. } => strings.extend(mapped.values().cloned()),
                        ObjectKind::Boxed(v) => value(v, &mut strings),
                        ObjectKind::Error(_, trace) => strings.push(trace.clone()),
                        ObjectKind::Promise(data) => data.strings(&mut strings),
                        ObjectKind::PromiseFunction(function, _) => function.strings(&mut strings),
                        ObjectKind::Iterator(data) => data.strings(&mut strings),
                        ObjectKind::RegExpStringIterator(
                            crate::regexp::StringIterator::Active { input, .. },
                        ) => strings.push(input.clone()),
                        ObjectKind::Collection(data) => data.strings(&mut strings),
                        ObjectKind::WeakCollection(data) => data.strings(&mut strings),
                        ObjectKind::FinalizationRegistry(registry) => registry.strings(&mut strings),
                        ObjectKind::PromiseAggregate(data) => data.strings(&mut strings),
                        ObjectKind::Generator(state) => state.visit(|v| value(v, &mut strings)),
                        ObjectKind::AsyncGenerator(state) => {
                            state.visit(|v| value(v, &mut strings))
                        }
                        ObjectKind::AsyncFromSyncIterator(state) => {
                            state.visit(|v| value(v, &mut strings))
                        }
                        ObjectKind::Module(data) => {
                            data.strings(&mut strings);
                            chunks.push(data.code.body.clone());
                        }
                        ObjectKind::Native {initial_name, ..} => strings.push(initial_name.clone()),
                        ObjectKind::ModuleNamespace(data) => {
                            for (key, target) in &data.exports {
                                strings.push(key.clone());
                                if let crate::modules::ExportTarget::Binding(_, name) = target {
                                    strings.push(name.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        for v in self.roots.values() {
            value(v, &mut strings);
        }
        self.string_storage.clear();
        self.symbol_storage
            .retain(|_, symbol| symbol.strong_count() > 0);
        for symbol in self
            .symbol_storage
            .values()
            .filter_map(std::rc::Weak::upgrade)
        {
            if let Some(description) = &symbol.description {
                strings.push(description.clone());
            }
        }
        self.heap.estimated_bytes = self
            .heap
            .estimated_bytes
            .saturating_add(self.symbol_storage.len() * 128 + self.symbol_registry.len() * 128);
        for job in &self.jobs {
            job.strings(&mut strings);
        }
        self.heap.estimated_bytes = self
            .heap
            .estimated_bytes
            .saturating_add(self.atomic_waiters.estimated_bytes())
            .saturating_add(self.jobs.capacity() * std::mem::size_of::<crate::promise::Job>())
            .saturating_add(
                self.jobs
                    .iter()
                    .map(crate::promise::Job::extra_bytes)
                    .sum::<usize>(),
            )
            .saturating_add(self.unhandled_promises.capacity() * std::mem::size_of::<ObjectId>());
        self.heap.estimated_bytes = self
            .heap
            .estimated_bytes
            .saturating_add(self.template_objects.len() * 128);
        self.string_bytes = 0;
        for s in &strings {
            let mut part = Some(s.clone());
            while let Some(string) = part {
                let bytes = string.allocation_bytes();
                if self
                    .string_storage
                    .insert(string.storage_id(), bytes)
                    .is_some()
                {
                    break;
                }
                self.string_bytes = self.string_bytes.saturating_add(bytes);
                part = string.prefix();
            }
        }
        self.code_bytes = bytecode::retained_size_with_chunks(codes, chunks);
    }
    #[cfg_attr(feature = "twitch-bootstrap-profile", track_caller)]
    pub(crate) fn reserve_binding(&mut self, key: &JsString) -> Result<(), JsError> {
        let bytes = 192usize.saturating_add(key.len().saturating_mul(2));
        #[cfg(feature = "twitch-bootstrap-profile")]
        qa_charge("reserve_binding", bytes);
        self.ensure_allocation(bytes)?;
        self.heap.estimated_bytes = self.heap.estimated_bytes.saturating_add(bytes);
        Ok(())
    }
    #[cfg_attr(feature = "twitch-bootstrap-profile", track_caller)]
    pub(crate) fn tick(&mut self) -> Result<(), JsError> {
        #[cfg(feature = "twitch-bootstrap-profile")]
        qa_charge("tick", 1);
        if self.initializing {
            return Ok(());
        }
        self.ensure_allocation(0)?;
        if self.remaining & 255 == 0 {
            self.check_interrupt()?;
        }
        if self.remaining == 0 {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Instruction budget exhausted",
            ));
        }
        self.remaining -= 1;
        if self.heap.live > self.limits.max_heap_objects {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Heap cell budget exhausted",
            ));
        }
        Ok(())
    }
    #[cfg_attr(feature = "twitch-bootstrap-profile", track_caller)]
    pub(crate) fn consume_work(&mut self, units: usize) -> Result<(), JsError> {
        #[cfg(feature = "twitch-bootstrap-profile")]
        qa_charge("consume_work", units);
        self.check_interrupt()?;
        let units = units as u64;
        if units > self.remaining {
            self.remaining = 0;
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Native work budget exhausted",
            ));
        }
        self.remaining -= units;
        Ok(())
    }
    /// Charge native DOM/host work to the current shared execution budget.
    pub fn charge_work(&mut self, units: usize) -> Result<(), JsError> {
        self.consume_work(units)
    }
    /// Charge a linear scan or copy of already bounded bytes / UTF-16 units.
    /// One nonempty operation always costs at least one instruction.
    pub fn charge_buffer_work(&mut self, units: usize) -> Result<(), JsError> {
        self.consume_work(units.div_ceil(self.limits.buffer_units_per_instruction))
    }
    fn compare_strings(&mut self, a: &JsString, b: &JsString) -> Result<(), JsError> {
        if a.storage_id() != b.storage_id() {
            self.consume_work(a.len().min(b.len()))?;
        }
        Ok(())
    }
    pub(crate) fn checked_string(&mut self, s: JsString) -> Result<Value, JsError> {
        if s.len() > self.limits.max_string_units {
            Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "String code-unit budget exhausted",
            ))
        } else {
            self.account_value(Value::String(s))
        }
    }
    /// Account for native output whose writes have not been charged separately.
    pub fn finish_string(&mut self, units: Vec<u16>) -> Result<Value, JsError> {
        self.ensure_string_allocation(units.len())?;
        self.consume_work(units.len())?;
        self.checked_string(JsString::from_utf16(units))
    }
    /// Finish a builder whose UTF-16 writes were already charged. Spare
    /// capacity can require a reallocating shrink when forming a boxed slice.
    pub(crate) fn finish_written_string(&mut self, units: Vec<u16>) -> Result<Value, JsError> {
        self.ensure_string_allocation(units.len())?;
        if !JsString::adopts_buffer(units.len(), units.capacity()) {
            self.consume_work(units.len())?;
        }
        self.checked_string(JsString::from_utf16(units))
    }
    /// Transfer a native, bounded data buffer into a JS string. Script
    /// string builders retain their per-unit costs through `finish_string`.
    pub fn finish_buffer_string(&mut self, units: Vec<u16>) -> Result<Value, JsError> {
        self.check_string_length(units.len())?;
        let peak = units
            .capacity()
            .saturating_add(units.len())
            .saturating_mul(2);
        self.ensure_allocation(peak.saturating_add(128))?;
        self.charge_buffer_work(units.len())?;
        self.checked_string(JsString::from_utf16(units))
    }
    /// Copy a borrowed slice directly into its immutable UTF-16 storage.
    pub(crate) fn copy_string(&mut self, units: &[u16]) -> Result<Value, JsError> {
        self.ensure_string_allocation(units.len())?;
        self.consume_work(units.len())?;
        self.checked_string(JsString::from_utf16_slice(units))
    }
    pub(crate) fn get(&mut self, receiver: Value, key: &JsString) -> Result<Value, JsError> {
        if receiver.is_nullish() {
            return Err(JsError::type_error(
                "Cannot read a property of null or undefined",
            ));
        }
        if let Value::String(s) = &receiver {
            if key == &JsString::from("length") {
                return Ok(Value::from(s.len()));
            }
            if let Some(index) = array_index(key)
                && let Some(unit) = s.as_utf16().get(index as usize)
            {
                return Ok(Value::String(JsString::from_utf16(vec![*unit])));
            }
        }
        let mut current = match &receiver {
            Value::Object(id) => Some(*id),
            Value::String(_) => Some(self.realm.string_prototype),
            Value::Number(_) => Some(self.realm.number_prototype),
            Value::BigInt(_) => self.realm.bigint_prototype,
            Value::Bool(_) => Some(self.realm.boolean_prototype),
            Value::Symbol(_) => self.realm.symbol_prototype,
            _ => None,
        };
        let mut depth = 0;
        while let Some(id) = current {
            depth += 1;
            if depth > 1024 {
                return Err(JsError::new(
                    ErrorKind::RangeError,
                    "Prototype chain limit exceeded",
                ));
            }
            if self.proxy(id) {
                return self.proxy_get(id, &PropertyKey::String(key.clone()), receiver);
            }
            let object = self.heap.object(id)?;
            if matches!(object.kind, ObjectKind::ModuleNamespace(_)) {
                return Ok(self
                    .namespace_descriptor(id, key)?
                    .map(|p| {
                        if let PropertyKind::Data { value, .. } = p.kind {
                            value
                        } else {
                            Value::Undefined
                        }
                    })
                    .unwrap_or_default());
            }
            if matches!(object.kind, ObjectKind::TypedArray(_))
                && let Some(index) = crate::binary::numeric_index(key)
            {
                return self.typed_index_get(id, index);
            }
            if key == &JsString::from("length") {
                if let ObjectKind::Array { length, .. } = &object.kind {
                    return Ok(Value::from(*length));
                }
                if let ObjectKind::Boxed(Value::String(s)) = &object.kind {
                    return Ok(Value::from(s.len()));
                }
            }
            if let ObjectKind::Boxed(Value::String(s)) = &object.kind
                && let Some(index) = array_index(key)
                && let Some(unit) = s.as_utf16().get(index as usize)
            {
                return Ok(Value::String(JsString::from_utf16(vec![*unit])));
            }
            let property = self.stored_property(id, object, key);
            current = object.prototype;
            let indexed = object.indexed_properties.is_some();
            let named = object.named_properties.is_some();
            if indexed && let Some(value) = self.host_indexed_get(id, key)? {
                return Ok(value);
            }
            if named && let Some(value) = self.host_named_value(id, key)? {
                return self.account_value(value);
            }
            if let Some(property) = property {
                return match property.kind {
                    PropertyKind::Data { value, .. } => {
                        crate::temporal::force_named(self, key, &value)?;
                        Ok(value)
                    }
                    PropertyKind::Accessor {
                        get: Some(getter), ..
                    } => self.invoke(getter, receiver, &[]),
                    PropertyKind::Accessor { get: None, .. } => Ok(Value::Undefined),
                };
            }
        }
        Ok(Value::Undefined)
    }
    pub(crate) fn has(&mut self, receiver: &Value, key: &JsString) -> Result<bool, JsError> {
        let Some(mut id) = receiver.as_object() else {
            return Err(JsError::type_error(
                "Right-hand side of 'in' is not an object",
            ));
        };
        for _ in 0..1024 {
            if self.proxy(id) {
                return self.proxy_has(id, &PropertyKey::String(key.clone()));
            }
            let o = self.heap.object(id)?;
            if let ObjectKind::ModuleNamespace(ns) = &o.kind {
                return Ok(ns.exports.contains_key(key));
            }
            if matches!(o.kind, ObjectKind::TypedArray(_))
                && let Some(index) = crate::binary::numeric_index(key)
            {
                return self.typed_element_exists(id, index);
            }
            if self.host_indexed_has(id, key)?
                || self.own_descriptor(id, key)?.is_some()
                || o.named_properties.is_some() && self.host_named_value(id, key)?.is_some()
            {
                return Ok(true);
            }
            if let Some(p) = o.prototype {
                id = p;
            } else {
                return Ok(false);
            }
        }
        Err(JsError::new(
            ErrorKind::RangeError,
            "Prototype chain limit exceeded",
        ))
    }
    pub(crate) fn own_descriptor(
        &self,
        id: ObjectId,
        key: &JsString,
    ) -> Result<Option<Property>, JsError> {
        let o = self.heap.object(id)?;
        if matches!(o.kind, ObjectKind::ModuleNamespace(_)) {
            return self.namespace_descriptor(id, key);
        }
        if matches!(o.kind, ObjectKind::TypedArray(_))
            && let Some(index) = crate::binary::numeric_index(key)
        {
            if !self.typed_element_exists(id, index)? {
                return Ok(None);
            }
            let mutable = !self.typed_buffer_immutable(id)?;
            return Ok(Some(Property {kind:PropertyKind::Data { value:self.typed_index_get(id, index)?, writable:mutable }, configurable:mutable, enumerable:true}));
        }
        if key == &JsString::from("length") {
            match &o.kind {
                ObjectKind::Array {
                    length,
                    length_writable,
                } => {
                    return Ok(Some(Property {
                        kind: PropertyKind::Data {
                            value: Value::from(*length),
                            writable: *length_writable,
                        },
                        enumerable: false,
                        configurable: false,
                    }));
                }
                ObjectKind::Boxed(Value::String(s)) => {
                    return Ok(Some(Property {
                        kind: PropertyKind::Data {
                            value: Value::from(s.len()),
                            writable: false,
                        },
                        enumerable: false,
                        configurable: false,
                    }));
                }
                _ => {}
            }
        }
        if let ObjectKind::Boxed(Value::String(s)) = &o.kind
            && let Some(i) = array_index(key)
            && let Some(unit) = s.as_utf16().get(i as usize)
        {
            return Ok(Some(Property {
                kind: PropertyKind::Data {
                    value: JsString::from_utf16(vec![*unit]).into(),
                    writable: false,
                },
                enumerable: true,
                configurable: false,
            }));
        }
        Ok(self.stored_property(id, o, key))
    }
    fn stored_property(&self, id: ObjectId, object: &Object, key: &JsString) -> Option<Property> {
        let mut property = self.stored_argument_property(object, key)?;
        if let ObjectKind::Function { code, .. } = &object.kind
            && code.constructible() && !code.strict && key == &JsString::from("caller")
            && let PropertyKind::Data { value, .. } = &mut property.kind
        {
            // Expose only the adjacent, non-strict interpreted caller from this realm.
            // Native entry points and suspended executions are boundaries.
            *value = self.legacy_calls.windows(2).rev()
                .find(|frames| frames[1].function == id)
                .filter(|frames| frames[0].visible
                    && frames[0].depth + 1 == frames[1].depth
                    && frames[0].realm == self.realm.global_env
                    && frames[1].realm == self.realm.global_env)
                .map_or(Value::Null, |frames| Value::Object(frames[0].function));
        }
        Some(property)
    }

    fn enter_legacy_call(&mut self, function: ObjectId, code: &Code) {
        self.legacy_calls.push(LegacyCall {
            function,
            realm: self.realm.global_env,
            depth: self.call_depth,
            visible: !code.strict && code.constructible(),
        });
    }
    fn primitive_string_has_own_key(receiver: &Value, key: &JsString) -> bool {
        matches!(receiver, Value::String(s) if
            key == &JsString::from("length")
                || array_index(key).is_some_and(|index| (index as usize) < s.len()))
    }
    pub(crate) fn set(
        &mut self,
        receiver: Value,
        key: &JsString,
        value: Value,
        strict: bool,
    ) -> Result<(), JsError> {
        let Some(id) = receiver.as_object() else {
            if receiver.is_nullish() || strict {
                return Err(JsError::type_error(
                    "Cannot assign a property on this primitive",
                ));
            }
            return Ok(());
        };
        if let Some(provider) = self.host_named_setter(id)? {
            return provider.set(self, key.clone(), value);
        }
        if matches!(self.heap.object(id)?.kind, ObjectKind::ModuleNamespace(_)) {
            return if strict {
                Err(JsError::type_error("Module namespace is read-only"))
            } else {
                Ok(())
            };
        }
        let mut current = Some(id);
        for _ in 0..1024 {
            let Some(owner) = current else {
                break;
            };
            if self.proxy(owner) {
                let ok =
                    self.proxy_set(owner, &PropertyKey::String(key.clone()), value, receiver)?;
                return if !ok && strict {
                    Err(JsError::type_error("Proxy rejected assignment"))
                } else {
                    Ok(())
                };
            }
            if matches!(self.heap.object(owner)?.kind, ObjectKind::TypedArray(_))
                && let Some(index) = crate::binary::numeric_index(key)
            {
                if self.typed_buffer_immutable(owner)? {
                    return if strict { Err(JsError::type_error("ArrayBuffer is immutable")) } else { Ok(()) };
                }
                if owner == id {
                    return self.typed_index_set(id, index, &value);
                }
                if !self.typed_element_exists(owner, index)? {
                    return Ok(());
                }
            }
            if matches!(
                self.heap.object(owner)?.kind,
                ObjectKind::ModuleNamespace(_)
            ) {
                return if strict {
                    Err(JsError::type_error("Module namespace is read-only"))
                } else {
                    Ok(())
                };
            }
            if self.host_indexed_has(owner, key)? {
                return if strict {
                    Err(JsError::type_error("Host index is not writable"))
                } else {
                    Ok(())
                };
            }
            let descriptor = self.own_descriptor(owner, key)?;
            current = self.heap.object(owner)?.prototype;
            if let Some(property) = descriptor {
                match property.kind {
                    PropertyKind::Accessor {
                        set: Some(setter), ..
                    } => {
                        self.invoke(setter, receiver, &[value])?;
                        return Ok(());
                    }
                    PropertyKind::Accessor { set: None, .. }
                    | PropertyKind::Data {
                        writable: false, ..
                    } => {
                        return if strict {
                            Err(JsError::type_error("Property is not writable"))
                        } else {
                            Ok(())
                        };
                    }
                    PropertyKind::Data { writable: true, .. } => {
                        if owner == id {
                            if key == &JsString::from("length")
                                && matches!(self.heap.object(id)?.kind, ObjectKind::Array { .. })
                            {
                                let success = crate::proxy::PartialDescriptor::value_only(value)
                                    .apply_array_length(self, id)?;
                                return if !success && strict {
                                    Err(JsError::type_error("Cannot assign array length"))
                                } else {
                                    Ok(())
                                };
                            }
                            let mut property = property;
                            property.kind = PropertyKind::Data {
                                value,
                                writable: true,
                            };
                            return self.define_property(receiver, key.clone(), property);
                        }
                        break;
                    }
                }
            }
        }
        if self.host_indexed_key(id, key)?.is_some() {
            return if strict {
                Err(JsError::type_error("Cannot define a host index"))
            } else {
                Ok(())
            };
        }
        if !self.heap.object(id)?.extensible {
            return if strict {
                Err(JsError::type_error("Object is not extensible"))
            } else {
                Ok(())
            };
        }
        if !strict && self.array_index_blocked(id, key)? {
            return Ok(());
        }
        self.define_property(receiver, key.clone(), Property::data(value))
    }
    pub(crate) fn resize_sparse_array(
        &mut self,
        id: ObjectId,
        length: Option<u32>,
        writable: Option<bool>,
    ) -> Result<bool, JsError> {
        let object = self.heap.object(id)?;
        let ObjectKind::Array {
            length: old,
            length_writable,
        } = object.kind
        else {
            unreachable!()
        };
        let requested = length.unwrap_or(old);
        if !length_writable {
            return Ok(writable != Some(true) && requested == old);
        }
        let mut final_length = requested;
        if requested < old {
            self.consume_work(object.properties.len().saturating_mul(2))?;
            let object = self.heap.object_mut(id)?;
            // Descending deletion stops at the highest non-configurable index.
            // Index keys have no delete hooks; both scans only visit stored entries.
            for (key, property) in &object.properties {
                if !property.configurable
                    && let Some(index) = array_index(key)
                {
                    final_length = final_length.max(index + 1);
                }
            }
            object
                .properties
                .retain(|key, _| array_index(key).is_none_or(|index| index < final_length));
        }
        self.heap.object_mut(id)?.kind = ObjectKind::Array {
            length: final_length,
            length_writable: writable.unwrap_or(true),
        };
        Ok(final_length == requested)
    }
    pub(crate) fn array_index_blocked(
        &self,
        id: ObjectId,
        key: &JsString,
    ) -> Result<bool, JsError> {
        Ok(
            matches!(self.heap.object(id)?.kind, ObjectKind::Array { length, length_writable: false }
            if array_index(key).is_some_and(|index| index >= length)),
        )
    }

    pub(crate) fn define_property(
        &mut self,
        receiver: Value,
        key: JsString,
        property: Property,
    ) -> Result<(), JsError> {
        let id = receiver
            .as_object()
            .ok_or_else(|| JsError::type_error("Property target is not an object"))?;
        if self.proxy(id) {
            return if self.proxy_define(
                id,
                &PropertyKey::String(key),
                crate::proxy::PartialDescriptor::full(property),
            )? {
                Ok(())
            } else {
                Err(JsError::type_error("Proxy rejected property definition"))
            };
        }
        if let Some(provider) = self.host_named_definition(id, &key)? {
            if let PropertyKind::Data { value, .. } = property.kind {
                return provider.set(self, key, value);
            }
            return Err(JsError::type_error(
                "Named host properties require a data descriptor",
            ));
        }
        if self.host_indexed_key(id, &key)?.is_some() {
            return Err(JsError::type_error("Cannot define a host index"));
        }
        if matches!(self.heap.object(id)?.kind, ObjectKind::TypedArray(_))
            && let Some(index) = crate::binary::numeric_index(&key)
        {
            return self.define_typed_index(id, index, &property);
        }
        if let PropertyKind::Data { value, .. } = &property.kind {
            self.account_value(value.clone())?;
        }
        if matches!(self.heap.object(id)?.kind, ObjectKind::ModuleNamespace(_)) {
            return self.define_namespace_property(id, &key, &property);
        }

        if key == JsString::from("length")
            && matches!(self.heap.object(id)?.kind, ObjectKind::Array { .. })
        {
            let success =
                crate::proxy::PartialDescriptor::full(property).apply_array_length(self, id)?;
            return if success {
                Ok(())
            } else {
                Err(JsError::type_error("Cannot redefine array length"))
            };
        }
        let previous = self.own_descriptor(id, &key)?;
        if previous.is_none() {
            self.account_string(&key)?;
            let bytes = 192usize.saturating_add(key.len().saturating_mul(2));
            self.ensure_allocation(bytes)?;
            self.heap.estimated_bytes = self.heap.estimated_bytes.saturating_add(bytes);
        }
        if let Some(old) = &previous {
            if !old.configurable {
                if property.configurable || property.enumerable != old.enumerable {
                    return Err(JsError::type_error(
                        "Cannot redefine non-configurable property",
                    ));
                }
                match (&old.kind, &property.kind) {
                    (
                        PropertyKind::Data {
                            value: a,
                            writable: false,
                        },
                        PropertyKind::Data { value: b, writable },
                    ) => {
                        self.charge_bigint_comparison(a, b)?;
                        if *writable || !same_value(a, b) {
                            return Err(JsError::type_error(
                                "Cannot redefine non-writable property",
                            ));
                        }
                    }
                    (PropertyKind::Data { .. }, PropertyKind::Accessor { .. })
                    | (PropertyKind::Accessor { .. }, PropertyKind::Data { .. }) => {
                        return Err(JsError::type_error(
                            "Cannot change non-configurable property kind",
                        ));
                    }
                    (
                        PropertyKind::Accessor { get: a, set: b },
                        PropertyKind::Accessor { get: c, set: d },
                    ) if a != c || b != d => {
                        return Err(JsError::type_error("Cannot redefine accessor"));
                    }
                    _ => {}
                }
                // A compatible redefinition of an immutable String property
                // must not add a second, stored copy of the implicit key.
                if let ObjectKind::Boxed(Value::String(text)) = &self.heap.object(id)?.kind
                    && (key == JsString::from("length")
                        || array_index(&key).is_some_and(|i| (i as usize) < text.len()))
                {
                    return Ok(());
                }
            }
        } else if !self.heap.object(id)?.extensible {
            return Err(JsError::type_error("Object is not extensible"));
        }
        if let Some(index) = array_index(&key) {
            let o = self.heap.object_mut(id)?;
            if let ObjectKind::Array {
                length,
                length_writable,
            } = &mut o.kind
            {
                if index >= *length && !*length_writable {
                    return Err(JsError::type_error("Array length is not writable"));
                }
                // The array stores only present indices. Named keys are tracked in order.
                let occupied = o.properties.len() - o.order.len();
                if previous.is_none() && occupied >= self.limits.max_array_length as usize {
                    return Err(JsError::new(
                        ErrorKind::ExecutionLimit,
                        "Array element budget exhausted",
                    ));
                }
                *length = (*length).max(index + 1);
            }
        }
        let property = self.update_argument_mapping(id, &key, property)?;
        self.heap.object_mut(id)?.insert(key, property);
        Ok(())
    }
    pub(crate) fn delete(
        &mut self,
        receiver: Value,
        key: &JsString,
        strict: bool,
    ) -> Result<bool, JsError> {
        if let Some(id) = receiver.as_object()
            && self.proxy(id)
        {
            let ok = self.proxy_delete(id, &PropertyKey::String(key.clone()))?;
            return if !ok && strict {
                Err(JsError::type_error("Proxy rejected deletion"))
            } else {
                Ok(ok)
            };
        }
        let Some(id) = receiver.as_object() else {
            if receiver.is_nullish() {
                return Err(JsError::type_error(
                    "Cannot delete a property of null or undefined",
                ));
            }
            if Self::primitive_string_has_own_key(&receiver, key) {
                return if strict {
                    Err(JsError::type_error(
                        "Cannot delete non-configurable property",
                    ))
                } else {
                    Ok(false)
                };
            }
            return Ok(true);
        };
        if let Some(provider) = self.host_named_setter(id)?
            && self.host_named_value(id, key)?.is_some()
        {
            provider.delete(self, key)?;
            return Ok(true);
        }
        if let ObjectKind::ModuleNamespace(ns) = &self.heap.object(id)?.kind {
            return if !ns.exports.contains_key(key) {
                Ok(true)
            } else if strict {
                Err(JsError::type_error(
                    "Cannot delete a module namespace export",
                ))
            } else {
                Ok(false)
            };
        }
        if matches!(self.heap.object(id)?.kind, ObjectKind::TypedArray(_))
            && let Some(index) = crate::binary::numeric_index(key)
        {
            return if !self.typed_element_exists(id, index)? {
                Ok(true)
            } else if strict {
                Err(JsError::type_error("Cannot delete a TypedArray index"))
            } else {
                Ok(false)
            };
        }
        if self.host_indexed_key(id, key)?.is_some() {
            return if !self.host_indexed_has(id, key)? {
                Ok(true)
            } else if strict {
                Err(JsError::type_error("Cannot delete a supported host index"))
            } else {
                Ok(false)
            };
        }
        if self
            .own_descriptor(id, key)?
            .is_some_and(|p| !p.configurable)
        {
            return if strict {
                Err(JsError::type_error(
                    "Cannot delete non-configurable property",
                ))
            } else {
                Ok(false)
            };
        }
        if array_index(key).is_none() && self.heap.object(id)?.properties.contains_key(key) {
            let work = self
                .heap
                .object(id)?
                .order
                .iter()
                .fold(0usize, |work, key| {
                    work.saturating_add(key.len()).saturating_add(1)
                });
            self.consume_work(work)?;
            self.heap.object_mut(id)?.order.retain(|old| old != key);
        }
        let object = self.heap.object_mut(id)?;
        if let ObjectKind::Arguments { mapped, environment } = &mut object.kind
            && let Some(index) = array_index(key) {
            mapped.remove(&index);
            if mapped.is_empty() { *environment = None; }
        }
        object.properties.remove(key);
        Ok(true)
    }
    /// Keep a host object's prototype fixed while allowing new own properties.
    pub fn make_prototype_immutable(&mut self, object: &Value) -> Result<(), JsError> {
        let id = object
            .as_object()
            .ok_or_else(|| JsError::type_error("Expected an object"))?;
        self.heap.object_mut(id)?.immutable_prototype = true;
        Ok(())
    }

    /// Set a prototype using the ordinary ECMAScript invariants, including extensibility.
    pub fn set_prototype(
        &mut self,
        target: ObjectId,
        prototype: Option<ObjectId>,
    ) -> Result<(), JsError> {
        if self.try_set_prototype(target, prototype)? {
            return Ok(());
        }
        Err(JsError::type_error("Cannot set object prototype"))
    }
    pub(crate) fn try_set_prototype(
        &mut self,
        target: ObjectId,
        prototype: Option<ObjectId>,
    ) -> Result<bool, JsError> {
        if self.proxy(target) {
            return self.proxy_set_proto(target, prototype);
        }
        let object = self.heap.object(target)?;
        if object.prototype == prototype {
            return Ok(true);
        }
        if object.immutable_prototype || target == self.realm.object_prototype || !object.extensible
        {
            return Ok(false);
        }
        let mut current = prototype;
        for _ in 0..1024 {
            let Some(id) = current else {
                self.heap.object_mut(target)?.prototype = prototype;
                return Ok(true);
            };
            if id == target {
                return Ok(false);
            }
            current = self.heap.object(id)?.prototype;
        }
        Err(JsError::new(
            ErrorKind::RangeError,
            "Prototype chain limit exceeded",
        ))
    }
    pub fn to_string(&mut self, value: &Value) -> Result<JsString, JsError> {
        Ok(match value {
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(b) => if *b { "true" } else { "false" }.into(),
            Value::Number(n) => number_string(*n).into(),
            Value::BigInt(integer) => return self.bigint_text(integer, 10),
            Value::String(s) => s.clone(),
            Value::Symbol(_) => {
                return Err(JsError::type_error("Cannot convert a Symbol to a string"));
            }
            Value::Object(_) => {
                let primitive = self.coerce_primitive(value.clone(), true)?;
                return self.to_string(&primitive);
            }
        })
    }
    pub fn to_number(&mut self, value: &Value) -> Result<f64, JsError> {
        Ok(match value {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            Value::Number(n) => *n,
            Value::BigInt(_) => {
                return Err(JsError::type_error("Cannot convert a BigInt to a number"));
            }
            Value::Symbol(_) => {
                return Err(JsError::type_error("Cannot convert a Symbol to a number"));
            }
            Value::String(s) => {
                self.consume_work(s.len())?;
                parse_number(s)
            }
            Value::Object(_) => {
                let primitive = self.coerce_primitive(value.clone(), false)?;
                return self.to_number(&primitive);
            }
        })
    }
    pub(crate) fn coerce_primitive(
        &mut self,
        value: Value,
        string_hint: bool,
    ) -> Result<Value, JsError> {
        self.primitive_hint(value, if string_hint { "string" } else { "number" })
    }
    pub(crate) fn ordinary_primitive(
        &mut self,
        value: Value,
        string_hint: bool,
    ) -> Result<Value, JsError> {
        if !matches!(value, Value::Object(_)) {
            return Ok(value);
        }
        for name in if string_hint {
            ["toString", "valueOf"]
        } else {
            ["valueOf", "toString"]
        } {
            let method = self.get_property(value.clone(), name)?;
            if self.is_callable(&method) {
                let result = self.invoke(method, value.clone(), &[])?;
                if !matches!(result, Value::Object(_)) {
                    return Ok(result);
                }
            }
        }
        Err(JsError::type_error(
            "Cannot convert object to primitive value",
        ))
    }
    pub(crate) fn coerce_object(&mut self, value: Value) -> Result<Value, JsError> {
        match &value {
            Value::Object(_) => Ok(value),
            Value::Undefined | Value::Null => Err(JsError::type_error(
                "Cannot convert null or undefined to object",
            )),
            Value::String(_)
            | Value::Number(_)
            | Value::BigInt(_)
            | Value::Bool(_)
            | Value::Symbol(_) => {
                let prototype = match value {
                    Value::String(_) => self.realm.string_prototype,
                    Value::Number(_) => self.realm.number_prototype,
                    Value::BigInt(_) => self.realm.bigint_prototype.unwrap(),
                    Value::Symbol(_) => self.realm.symbol_prototype.unwrap(),
                    _ => self.realm.boolean_prototype,
                };
                Ok(Value::Object(self.heap.allocate(Cell::Object(
                    Object::new(ObjectKind::Boxed(value), Some(prototype)),
                ))))
            }
        }
    }
    pub(crate) fn own_keys(
        &mut self,
        value: Value,
        enumerable: bool,
    ) -> Result<Vec<JsString>, JsError> {
        let value = self.coerce_object(value)?;
        let id = value.as_object().unwrap();
        if self.proxy(id) {
            let mut result = Vec::new();
            for key in self.proxy_keys(id)? {
                if let PropertyKey::String(name) = &key
                    && (!enumerable || self.own_enumerability(id, &key)?.unwrap_or(false))
                {
                    result.push(name.clone())
                }
            }
            return Ok(result);
        }
        if let ObjectKind::ModuleNamespace(ns) = &self.heap.object(id)?.kind {
            let count = ns.exports.len();
            if count > self.limits.max_array_length as usize {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Property key count budget exhausted",
                ));
            }
            self.reserve_allocation(count.saturating_mul(128))?;
            self.consume_work(count)?;
            let ObjectKind::ModuleNamespace(ns) = &self.heap.object(id)?.kind else {
                unreachable!()
            };
            let keys: Vec<_> = ns.exports.keys().cloned().collect();
            if enumerable {
                for key in &keys {
                    self.namespace_descriptor(id, key)?;
                }
            }
            return Ok(keys);
        }
        let (properties, string_length, include_length) = {
            let object = self.heap.object(id)?;
            (
                object.properties.len(),
                if let ObjectKind::Boxed(Value::String(s)) = &object.kind {
                    s.len()
                } else {
                    0
                },
                !enumerable
                    && matches!(
                        object.kind,
                        ObjectKind::Array { .. } | ObjectKind::Boxed(Value::String(_))
                    ),
            )
        };
        let indexed = string_length
            .saturating_add(self.host_indexed_len(id)?)
            .saturating_add(self.typed_array_key_count(id)?);
        let named = self.host_named_keys(id)?;
        let count = properties
            .saturating_add(indexed)
            .saturating_add(named.len())
            .saturating_add(usize::from(include_length));
        if count > self.limits.max_array_length as usize {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Property key count budget exhausted",
            ));
        }
        self.reserve_allocation(
            properties
                .saturating_mul(128)
                .saturating_add(named.len().saturating_mul(128))
                .saturating_add(indexed.saturating_mul(256)),
        )?;
        self.consume_work(indexed)?;
        let object = self.heap.object(id)?;
        let mut keys = object.keys(enumerable);
        if indexed > 0 || !named.is_empty() {
            let mut prefix = Vec::with_capacity(count);
            for i in 0..indexed {
                prefix.push(JsString::from(i.to_string()));
            }
            prefix.extend(named);
            prefix.extend(keys);
            keys = prefix;
        }
        Ok(keys)
    }
    pub(crate) fn with_scope(&mut self, parent: EnvId, value: Value) -> Result<EnvId, JsError> {
        let binding_object = self.coerce_object(value)?.as_object().unwrap();
        self.ensure_allocation(std::mem::size_of::<Environment>())?;
        Ok(self.heap.allocate_env(Environment::new(
            Some(parent),
            EnvKind::With { binding_object },
            None,
        )))
    }
    pub(crate) fn resolve(&mut self, mut env: EnvId, key: &JsString) -> Result<Reference, JsError> {
        loop {
            let e = self.heap.environment(env);
            if e.aliases.contains_key(key) {
                return Ok(Reference::Import(env, key.clone()));
            }
            if e.bindings.contains_key(key) {
                return Ok(Reference::Binding(env, key.clone()));
            }
            let parent = e.parent;
            let kind = e.kind;
            if let EnvKind::With { binding_object } = kind {
                let object = Value::Object(binding_object);
                if self.has(&object, key)? {
                    let block =
                        self.get_property_key(object.clone(), self.well_known_key("unscopables"))?;
                    if block.as_object().is_none() || !self.get(block, key)?.is_truthy() {
                        return Ok(Reference::With(binding_object, key.clone()));
                    }
                }
            }
            if let EnvKind::Global { binding_object } = kind
                && self.has(&Value::Object(binding_object), key)?
            {
                return Ok(Reference::Global(binding_object, key.clone()));
            }
            if let Some(p) = parent {
                env = p;
            } else {
                return Ok(Reference::Unresolvable(key.clone()));
            }
        }
    }
    fn normalize_reference(&mut self, reference: &mut Reference) -> Result<(), JsError> {
        let (base, key) = match reference {
            Reference::Property(base, key) | Reference::Super(base, _, key) => (base, key),
            _ => return Ok(()),
        };
        if !base.is_nullish() && !matches!(key, Value::String(_) | Value::Symbol(_)) {
            // GetValue canonicalizes the reference's key. Preserve it when an
            // operation will reuse the same reference for PutValue.
            *key = match self.to_property_key(key)? {
                PropertyKey::String(name) => Value::String(name),
                PropertyKey::Symbol(symbol) => Value::Symbol(symbol),
            };
        }
        Ok(())
    }
    fn property_reference_key(
        &mut self,
        base: &Value,
        key: &Value,
    ) -> Result<PropertyKey, JsError> {
        if base.is_nullish() {
            return Err(JsError::type_error(
                "Cannot access a property of null or undefined",
            ));
        }
        let key = self.to_property_key(key)?;
        if let PropertyKey::String(name) = &key {
            self.consume_work(name.len())?;
        }
        Ok(key)
    }
    pub(crate) fn read_reference(&mut self, reference: Reference) -> Result<Value, JsError> {
        match reference {
            Reference::Binding(env, key) => {
                if self.heap.environment(env).has_lazy_arguments(&key) {
                    self.materialize_arguments(env)?;
                }
                self.heap.environment(env).bindings.get(&key)
                    .ok_or_else(|| JsError::new(ErrorKind::ReferenceError, format!("Binding '{key}' was deleted")))?
                    .read(&key)
            }
            Reference::Import(env, key) => self.read_import(env, &key),
            Reference::Super(base, receiver, key) => {
                if base.is_nullish() {
                    return Err(JsError::type_error("Super property base is null"));
                }
                let key = self.to_property_key(&key)?;
                self.get_from(base, &key, receiver)
            }
            Reference::With(object, key) => {
                let object = Value::Object(object);
                if self.has(&object, &key)? {
                    self.get(object, &key)
                } else {
                    Ok(Value::Undefined)
                }
            }
            Reference::Global(object, key) => self.get(Value::Object(object), &key),
            Reference::Property(object, key) => {
                let key = self.property_reference_key(&object, &key)?;
                self.get_property_key(object, key)
            }
            Reference::Private(object, id) => self.private_get(object, id),
            Reference::Unresolvable(key) => Err(JsError::new(
                ErrorKind::ReferenceError,
                format!("{key} is not defined"),
            )),
        }
    }
    pub(crate) fn write_reference(
        &mut self,
        reference: Reference,
        value: Value,
        strict: bool,
    ) -> Result<(), JsError> {
        let value = self.account_value(value)?;
        match reference {
            Reference::Binding(env, key) => {
                // The right-hand side or eval can delete a configurable binding after
                // resolving the left-hand side. Declarative SetMutableBinding
                // recreates that binding for a non-strict reference.
                if !self.heap.environment(env).bindings.contains_key(&key) {
                    if strict {
                        return Err(JsError::new(ErrorKind::ReferenceError, format!("Binding '{key}' was deleted")));
                    }
                    self.reserve_binding(&key)?;
                    self.heap.environment_mut(env).bindings.insert(key, Binding {
                        value, mutable: true, initialized: true, lexical: false, strict_immutable: true, deletable: true,
                    });
                    return Ok(());
                }
                let b = self.heap.environment_mut(env).bindings.get_mut(&key).expect("existing binding");
                if !b.initialized {
                    return Err(JsError::new(
                        ErrorKind::ReferenceError,
                        format!("Cannot access '{key}' before initialization"),
                    ));
                }
                if !b.mutable {
                    if strict || b.strict_immutable {
                        return Err(JsError::type_error("Assignment to constant binding"));
                    }
                    return Ok(());
                }
                b.value = value;
                if self.heap.environment(env).has_lazy_arguments(&key) {
                    self.heap.environment_mut(env).pending_arguments = None;
                }
                Ok(())
            }
            Reference::Import(..) => Err(JsError::type_error("Assignment to an import binding")),
            Reference::Super(base, receiver, key) => {
                if base.is_nullish() {
                    return Err(JsError::type_error("Super property base is null"));
                }
                let key = self.to_property_key(&key)?;
                if !self.set_from(base, &key, value, receiver)? && strict {
                    return Err(JsError::type_error("Super property is not writable"));
                }
                Ok(())
            }
            Reference::With(object, key) | Reference::Global(object, key) => {
                let global = Value::Object(object);
                // A getter, coercion, or right-hand side may have removed a
                // binding since the identifier was resolved.
                if strict && !self.has(&global, &key)? {
                    return Err(JsError::new(
                        ErrorKind::ReferenceError,
                        format!("{key} is not defined"),
                    ));
                }
                self.set(global, &key, value, strict)
            }
            Reference::Property(object, key) => {
                let key = self.property_reference_key(&object, &key)?;
                if object.as_object().is_some() {
                    return self.set_key(object, &key, value, strict);
                }
                // Only string wrappers have own properties for primitive
                // bases. Otherwise start the lookup at the wrapper prototype
                // without allocating a wrapper or changing the receiver.
                let string_own_key = matches!(&key, PropertyKey::String(name)
                    if Self::primitive_string_has_own_key(&object, name));
                let succeeded = match self.property_start(&object) {
                    Some(prototype) if !string_own_key => {
                        self.set_from(Value::Object(prototype), &key, value, object)?
                    }
                    _ => false,
                };
                if !succeeded && strict {
                    return Err(JsError::type_error(
                        "Cannot assign a property on this primitive",
                    ));
                }
                Ok(())
            }
            Reference::Private(object, id) => self.private_set(object, id, value),
            Reference::Unresolvable(key) => {
                if strict {
                    Err(JsError::new(
                        ErrorKind::ReferenceError,
                        format!("{key} is not defined"),
                    ))
                } else {
                    self.set(self.global(), &key, value, false)
                }
            }
        }
    }
    pub(crate) fn get_this(&self, mut env: EnvId) -> Result<Value, JsError> {
        loop {
            let e = self.heap.environment(env);
            if let Some(this) = &e.this {
                if !e.this_initialized {
                    return Err(JsError::new(
                        ErrorKind::ReferenceError,
                        "this is uninitialized before super()",
                    ));
                }
                return Ok(this.clone());
            }
            if let Some(parent) = e.parent {
                env = parent;
            } else {
                return Ok(Value::Undefined);
            }
        }
    }
    fn annex_variable_environment(&mut self, mut env: EnvId, name: &JsString) -> Result<Option<EnvId>, JsError> {
        loop {
            self.tick()?;
            let record = self.heap.environment(env);
            if record.kind != EnvKind::SimpleCatch && record.bindings.get(name).is_some_and(|binding| binding.lexical) {
                return Ok(None);
            }
            match record.kind {
                EnvKind::Global { binding_object } => {
                    if self.own_descriptor(binding_object, name)?.is_none() && !self.object_extensible(binding_object)? {
                        return Ok(None);
                    }
                    return Ok(Some(env));
                }
                EnvKind::Function => return Ok(Some(env)),
                EnvKind::Module => return Ok(None),
                _ => env = record.parent.unwrap(),
            }
        }
    }

    fn prepare_annex_function(&mut self, env: EnvId, name: &JsString) -> Result<(), JsError> {
        let Some(target) = self.annex_variable_environment(env, name)? else { return Ok(()) };
        let deletable = self.heap.environment(env).kind == EnvKind::SloppyEval;
        if let EnvKind::Global { binding_object } = self.heap.environment(target).kind {
            if self.own_descriptor(binding_object, name)?.is_none() {
                self.define_data_property_key(Value::Object(binding_object), name.clone().into(), Value::Undefined, true, true, deletable)?;
            }
        } else if !self.heap.environment(target).bindings.contains_key(name) {
            self.reserve_binding(name)?;
            self.heap.environment_mut(target).bindings.insert(name.clone(), Binding {
                value: Value::Undefined, mutable: true, initialized: true, lexical: false, strict_immutable: true, deletable,
            });
        }
        Ok(())
    }

    fn copy_annex_function(&mut self, env: EnvId, name: &JsString) -> Result<(), JsError> {
        // A with object can shadow the name. The declaration copies the value
        // directly between its lexical and variable environment records.
        let mut boundary = env;
        loop {
            self.tick()?;
            let record = self.heap.environment(boundary);
            if matches!(record.kind, EnvKind::SloppyEval | EnvKind::Global { .. } | EnvKind::Function | EnvKind::Module) {
                break;
            }
            boundary = record.parent.unwrap();
        }
        if let Some(target) = self.annex_variable_environment(boundary, name)? {
            let reference = match self.heap.environment(target).kind {
                EnvKind::Global { binding_object } => Reference::Global(binding_object, name.clone()),
                _ => Reference::Binding(target, name.clone()),
            };
            let value = self.read_reference(Reference::Binding(env, name.clone()))?;
            self.write_reference(reference, value, false)?;
        }
        Ok(())
    }

    pub(crate) fn block_environment(&mut self, chunk: &Chunk, parent: EnvId) -> Result<EnvId, JsError> {
        // A block without declarations introduces no bindings and cannot
        // affect name resolution, including direct eval and escaping closures.
        if chunk.scope.is_empty() {
            return Ok(parent);
        }
        self.ensure_allocation(384)?;
        Ok(self.heap.allocate_env(Environment::new(Some(parent), EnvKind::Block, None)))
    }

    pub(crate) fn prepare(&mut self, scope: &ScopePlan, env: EnvId) -> Result<(), JsError> {
        if scope.is_empty() {
            return Ok(());
        }
        let declarations = self.declaration_environment(env);
        for (name, _) in &scope.lexical {
            if self
                .heap
                .environment(declarations)
                .bindings
                .contains_key(name)
            {
                return Err(JsError::new(
                    ErrorKind::SyntaxError,
                    format!("Identifier '{name}' has already been declared"),
                ));
            }
            if declarations == self.realm.global_env
                && self
                    .own_descriptor(self.realm.global_object, name)?
                    .is_some_and(|p| !p.configurable)
            {
                return Err(JsError::new(
                    ErrorKind::SyntaxError,
                    "Global lexical binding conflicts with non-configurable property",
                ));
            }
        }
        let deletable = self.heap.environment(declarations).kind == EnvKind::SloppyEval;
        let mut target = declarations;
        if !scope.vars.is_empty() {
            loop {
                let record = self.heap.environment(target);
                if !matches!(record.kind, EnvKind::Block | EnvKind::SimpleCatch | EnvKind::SloppyEval | EnvKind::Inspector | EnvKind::With { .. }) { break; }
                if record.kind != EnvKind::SimpleCatch && scope.vars.iter().any(|name| record.bindings.contains_key(name)) {
                    return Err(JsError::new(ErrorKind::SyntaxError, "Eval var conflicts with lexical binding"));
                }
                target = record.parent.unwrap();
            }
        }
        let global = match self.heap.environment(target).kind {
            EnvKind::Global { binding_object } => Some(binding_object),
            _ => None,
        };
        // Validate the entire declaration list before creating any binding.
        for name in &scope.vars {
            self.ensure_allocation(256 + name.len() * 2)?;
            if self.heap.environment(target).bindings.get(name).is_some_and(|binding| binding.lexical) {
                return Err(JsError::new(ErrorKind::SyntaxError, "Var conflicts with lexical binding"));
            }
            if let Some(object) = global && self.own_descriptor(object, name)?.is_none() && !self.object_extensible(object)? {
                return Err(JsError::type_error("Cannot declare a variable on a non-extensible global object"));
            }
        }
        if let Some(object) = global {
            for (name, _, is_var) in &scope.functions {
                if *is_var && let Some(property) = self.own_descriptor(object, name)? && !property.configurable
                    && !(property.enumerable && matches!(property.kind, PropertyKind::Data { writable: true, .. })) {
                    return Err(JsError::type_error("Cannot declare a function on a restricted global property"));
                }
            }
        }
        for name in &scope.vars {
            if let Some(object) = global {
                if self.own_descriptor(object, name)?.is_none() {
                    self.define_data_property_key(Value::Object(object), name.clone().into(), Value::Undefined, true, true, deletable)?;
                }
            } else if !self.heap.environment(target).bindings.contains_key(name) {
                self.reserve_binding(name)?;
                self.heap.environment_mut(target).bindings.insert(name.clone(), Binding {
                    value: Value::Undefined, mutable: true, initialized: true, lexical: false, strict_immutable: true, deletable,
                });
            }
        }
        for name in &scope.annex_vars {
            self.prepare_annex_function(declarations, name)?;
        }
        for (name, mutable) in &scope.lexical {
            self.reserve_binding(name)?;
            self.heap.environment_mut(declarations).bindings.insert(
                name.clone(),
                Binding {
                    value: Value::Undefined,
                    mutable: *mutable,
                    initialized: false,
                    lexical: true,
                    strict_immutable: true,
                    deletable: false,
                },
            );
        }
        for (name, code, is_var) in &scope.functions {
            let function = self.make_function(code.clone(), env)?;
            if *is_var {
                if let Some(object) = global {
                    let configurable = self.own_descriptor(object, name)?.is_some_and(|p| p.configurable) && deletable;
                    self.define_data_property_key(Value::Object(object), name.clone().into(), function, true, true, configurable)?;
                } else {
                    self.write_reference(Reference::Binding(target, name.clone()), function, false)?;
                }
            } else {
                let binding = self
                    .heap
                    .environment_mut(declarations)
                    .bindings
                    .get_mut(name)
                    .unwrap();
                binding.value = function;
                binding.initialized = true;
            }
        }
        Ok(())
    }
    pub(crate) fn initialize(
        &mut self,
        env: EnvId,
        kind: VarKind,
        key: &JsString,
        value: Value,
        has_initializer: bool,
    ) -> Result<(), JsError> {
        let value = self.account_value(value)?;
        if kind == VarKind::Var {
            if has_initializer {
                let r = self.resolve(env, key)?;
                self.write_reference(r, value, false)?;
            }
            Ok(())
        } else {
            let declarations = self.declaration_environment(env);
            let binding = self
                .heap
                .environment_mut(declarations)
                .bindings
                .get_mut(key)
                .ok_or_else(|| JsError::new(ErrorKind::InternalError, "Missing lexical binding"))?;
            if binding.initialized {
                return Err(JsError::new(
                    ErrorKind::ReferenceError,
                    "Binding already initialized",
                ));
            }
            binding.value = value;
            binding.initialized = true;
            Ok(())
        }
    }
    pub(crate) fn make_function(
        &mut self,
        code: Rc<Code>,
        environment: EnvId,
    ) -> Result<Value, JsError> {
        let environment = if code.self_binding {
            self.heap
                .allocate_env(Environment::new(Some(environment), EnvKind::Block, None))
        } else {
            environment
        };
        let constructible = code.constructible();
        let legacy_caller = constructible && !code.strict;
        let generator_prototype = match code.kind {
            FunctionKind::Generator => self.realm.generator_prototype,
            FunctionKind::AsyncGenerator => self.realm.async_generator_prototype,
            _ => None,
        };
        let name = code.name.clone().unwrap_or_default();
        let self_binding = code.self_binding;
        let length = code.length;
        let prototype = self.function_object_prototype(code.kind);
        let id = self.heap.allocate(Cell::Object(Object::new(
            ObjectKind::Function {
                code,
                environment,
                referrer: self.realm.active_referrer.clone(),
            },
            Some(prototype),
        )));
        let function = Value::Object(id);
        if self_binding {
            self.reserve_binding(&name)?;
            self.heap.environment_mut(environment).bindings.insert(
                name.clone(),
                Binding {
                    value: function.clone(),
                    mutable: false,
                    initialized: true,
                    lexical: true,
                    strict_immutable: false,
                    deletable: false,
                },
            );
        }
        self.define_data_property(
            function.clone(),
            "length",
            Value::from(length),
            false,
            false,
            true,
        )?;
        self.define_data_property(
            function.clone(),
            "name",
            Value::String(name),
            false,
            false,
            true,
        )?;
        if legacy_caller {
            self.define_data_property(function.clone(), "caller", Value::Null, false, false, false)?;
        }
        if constructible {
            let prototype = self.new_object();
            self.define_data_property(
                prototype.clone(),
                "constructor",
                function.clone(),
                true,
                false,
                true,
            )?;
            self.define_data_property(
                function.clone(),
                "prototype",
                prototype,
                true,
                false,
                false,
            )?;
        } else if let Some(prototype) = generator_prototype {
            let prototype = self.new_object_with_prototype(Value::Object(prototype))?;
            self.define_data_property(
                function.clone(),
                "prototype",
                prototype,
                true,
                false,
                false,
            )?;
        }
        Ok(function)
    }
    pub(crate) fn invoke(
        &mut self,
        function: Value,
        this: Value,
        args: &[Value],
    ) -> Result<Value, JsError> {
        if self.active == 0 {
            self.remaining = self.limits.max_instructions;
        }
        self.active += 1;
        for arg in args {
            if let Err(error) = self.account_value(arg.clone()) {
                self.active -= 1;
                return Err(error);
            }
        }
        let result = self
            .invoke_inner(function, this, args)
            .and_then(|value| self.account_value(value));
        self.active -= 1;
        result
    }
    fn invoke_inner(
        &mut self,
        function: Value,
        this: Value,
        args: &[Value],
    ) -> Result<Value, JsError> {
        self.tick()?;
        if !self.can_enter_call() {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Call depth budget exhausted",
            ));
        }
        let id = function
            .as_object()
            .ok_or_else(|| JsError::type_error("Value is not callable"))?;
        if self.proxy(id) {
            return self.proxy_call(id, this, args);
        }
        self.with_function_realm(id, |vm| vm.invoke_in_own_realm(id, function, this, args))
    }

    fn invoke_in_own_realm(
        &mut self,
        id: ObjectId,
        function: Value,
        this: Value,
        args: &[Value],
    ) -> Result<Value, JsError> {
        if let ObjectKind::PromiseFunction(function, _) = &self.heap.object(id)?.kind {
            let function = function.clone();
            self.call_depth += 1;
            let result = self.invoke_promise_function(id, function, this, args);
            self.call_depth -= 1;
            return result;
        }
        if let ObjectKind::Bound {
            target,
            this,
            args: bound_args,
        } = &self.heap.object(id)?.kind
        {
            let target = target.clone();
            let this = this.clone();
            self.ensure_allocation(
                bound_args
                    .len()
                    .saturating_add(args.len())
                    .saturating_mul(std::mem::size_of::<Value>()),
            )?;
            let mut all = bound_args.clone();
            all.extend_from_slice(args);
            self.call_depth += 1;
            let result = self.invoke(target, this, &all);
            self.call_depth -= 1;
            return result;
        }
        if self.heap.object(id)?.class_data.is_some() {
            return Err(JsError::type_error(
                "Class constructor cannot be called without new",
            ));
        }
        let (native, interpreted) = match &self.heap.object(id)?.kind {
            ObjectKind::Native { callback, .. } => (Some(callback.clone()), None),
            ObjectKind::Function {
                code,
                environment,
                referrer,
            } => (None, Some((code.clone(), *environment, referrer.clone()))),
            _ => return Err(JsError::type_error("Value is not callable")),
        };
        self.call_depth += 1;
        let result = if let Some(callback) = native {
            callback(self, this, args)
        } else {
            let (code, parent, referrer) = interpreted.unwrap();
            self.active_calls.push(code.name.clone());
            self.enter_legacy_call(id, &code);
            let mut result = self.with_referrer(referrer, |vm| {
                let invocation = Invocation {
                    function,
                    this,
                    args,
                    new_target: Value::Undefined,
                    this_initialized: true,
                };
                match code.kind {
                    FunctionKind::Async => return vm.invoke_async(code, parent, invocation),
                    FunctionKind::Generator | FunctionKind::AsyncGenerator => {
                        return vm.invoke_generator(code, parent, invocation);
                    }
                    FunctionKind::Ordinary => {}
                }
                let (body_env, body) = vm.function_environment(&code, parent, invocation)?;
                vm.run_chunk(&body, body_env)
                    .and_then(|completion| match completion {
                        Completion::Return(v) => Ok(v),
                        Completion::Normal(_) => Ok(Value::Undefined),
                        _ => Err(JsError::new(
                            ErrorKind::InternalError,
                            "Unexpected function abrupt completion",
                        )),
                    })
            });
            if let Err(error) = &mut result
                && error.inspected_calls.is_none()
            {
                error.inspected_calls = self.inspect_call_stack();
            }
            self.legacy_calls.pop();
            self.active_calls.pop();
            result
        };
        self.call_depth -= 1;
        result
    }
    pub fn construct(&mut self, function: Value, args: &[Value]) -> Result<Value, JsError> {
        self.construct_with_new_target(function.clone(), args, function)
    }
    pub fn construct_with_new_target(
        &mut self,
        function: Value,
        args: &[Value],
        new_target: Value,
    ) -> Result<Value, JsError> {
        self.with_task(|vm| vm.construct_target(function, args, new_target))
    }
    pub(crate) fn prototype_from_constructor(
        &mut self,
        new_target: Value,
        fallback: ObjectId,
    ) -> Result<ObjectId, JsError> {
        if let Some(prototype) = self
            .get_property(new_target.clone(), "prototype")?
            .as_object()
        {
            return Ok(prototype);
        }
        self.constructor_realm_prototype(
            new_target
                .as_object()
                .ok_or_else(|| JsError::type_error("Expected a constructor"))?,
            fallback,
        )
    }
    pub(crate) fn allocate_constructed(
        &mut self,
        kind: ObjectKind,
        prototype: ObjectId,
    ) -> Result<Value, JsError> {
        self.ensure_allocation(384)?;
        Ok(Value::Object(self.heap.allocate(Cell::Object(
            Object::new(kind, Some(prototype)),
        ))))
    }
    pub(crate) fn construct_target(
        &mut self,
        function: Value,
        args: &[Value],
        new_target: Value,
    ) -> Result<Value, JsError> {
        self.tick()?;
        if !self.can_enter_call() {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Constructor depth budget exhausted",
            ));
        }
        if !self.is_constructor(&new_target) {
            return Err(JsError::type_error("newTarget is not a constructor"));
        }
        let id = function
            .as_object()
            .ok_or_else(|| JsError::type_error("Value is not a constructor"))?;
        if self.proxy(id) {
            return self.proxy_construct(id, args, new_target);
        }
        self.with_function_realm(id, |vm| {
            vm.construct_in_own_realm(id, function, args, new_target)
        })
    }

    fn construct_in_own_realm(
        &mut self,
        id: ObjectId,
        function: Value,
        args: &[Value],
        new_target: Value,
    ) -> Result<Value, JsError> {
        if self.heap.object(id)?.class_data.is_some() {
            let referrer = self.referrer_for_owner(id)?;
            return self.with_referrer(referrer, |vm| vm.construct_class(id, args, new_target));
        }
        if let ObjectKind::Bound {
            target,
            args: bound_args,
            ..
        } = &self.heap.object(id)?.kind
        {
            let target = target.clone();
            self.ensure_allocation(
                bound_args
                    .len()
                    .saturating_add(args.len())
                    .saturating_mul(std::mem::size_of::<Value>()),
            )?;
            let mut all = bound_args.clone();
            all.extend_from_slice(args);
            self.call_depth += 1;
            let new_target = if new_target == function {
                target.clone()
            } else {
                new_target
            };
            let result = self.construct_target(target, &all, new_target);
            self.call_depth -= 1;
            return result;
        }
        match &self.heap.object(id)?.kind {
            ObjectKind::Function { code, .. } if code.constructible() => {}
            ObjectKind::Native {
                construct: Some(callback),
                ..
            } => {
                let callback = callback.clone();
                for value in args {
                    self.account_value(value.clone())?;
                }
                self.call_depth += 1;
                let result = callback(self, function, args, new_target).and_then(|value| {
                    if value.as_object().is_none() {
                        return Err(JsError::type_error(
                            "Native constructor must return an object",
                        ));
                    }
                    self.account_value(value)
                });
                self.call_depth -= 1;
                return result;
            }
            _ => return Err(JsError::type_error("Value is not a constructor")),
        }
        let prototype =
            self.prototype_from_constructor(new_target.clone(), self.realm.object_prototype)?;
        let object = self.allocate_constructed(ObjectKind::Ordinary, prototype)?;
        let (code, parent, referrer) = if let ObjectKind::Function {
            code,
            environment,
            referrer,
        } = &self.heap.object(id)?.kind
        {
            (code.clone(), *environment, referrer.clone())
        } else {
            unreachable!()
        };
        self.call_depth += 1;
        self.enter_legacy_call(id, &code);
        let result = self.with_referrer(referrer, |vm| {
            let (env, body) = vm.function_environment(
                &code,
                parent,
                Invocation {
                    function,
                    this: object.clone(),
                    args,
                    new_target,
                    this_initialized: true,
                },
            )?;
            vm.run_chunk(&body, env).and_then(|c| match c {
                Completion::Return(v) => Ok(v),
                Completion::Normal(_) => Ok(Value::Undefined),
                _ => Err(JsError::new(
                    ErrorKind::InternalError,
                    "Invalid constructor completion",
                )),
            })
        });
        self.legacy_calls.pop();
        self.call_depth -= 1;
        let result = result?;
        if matches!(result, Value::Object(_)) {
            Ok(result)
        } else {
            Ok(object)
        }
    }
    pub(crate) fn value(&mut self, item: Item) -> Result<Value, JsError> {
        match item {
            Item::Value(v) => Ok(v),
            Item::Reference(r) => self.read_reference(r),
            _ => Err(JsError::new(
                ErrorKind::InternalError,
                "Expected an expression value",
            )),
        }
    }
    pub(crate) fn pop_value(&mut self, stack: &mut Vec<Item>) -> Result<Value, JsError> {
        let item = stack
            .pop()
            .ok_or_else(|| JsError::new(ErrorKind::InternalError, "Bytecode stack underflow"))?;
        self.value(item)
    }
    pub(crate) fn expression_value(&mut self, chunk: &Chunk, env: EnvId) -> Result<Value, JsError> {
        match self.run_chunk(chunk, env)? {
            Completion::Normal(value) => Ok(value.unwrap_or_default()),
            _ => Err(JsError::new(
                ErrorKind::InternalError,
                "Abrupt completion in expression bytecode",
            )),
        }
    }
    pub(crate) fn append_array_literal(
        &mut self,
        array: Value,
        value: Option<Value>,
    ) -> Result<(), JsError> {
        let id = array.as_object().unwrap();
        let ObjectKind::Array { length, .. } = self.heap.object(id)?.kind else {
            unreachable!()
        };
        if length >= self.limits.max_array_length {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Array literal length budget exhausted",
            ));
        }
        if let Some(value) = value {
            self.define_property(
                array,
                JsString::from(length.to_string()),
                Property::data(value),
            )?;
        } else if let ObjectKind::Array { length, .. } = &mut self.heap.object_mut(id)?.kind {
            *length += 1;
        }
        Ok(())
    }
    fn append_argument(&mut self, args: &mut Vec<Value>, value: Value) -> Result<(), JsError> {
        if args.len() >= self.limits.max_array_length as usize {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Argument list budget exhausted",
            ));
        }
        self.reserve_allocation(std::mem::size_of::<Value>() * 2)?;
        self.account_value(value.clone())?;
        self.tick()?;
        args.push(value);
        Ok(())
    }
    pub(crate) fn set_literal_function_name(
        &mut self,
        value: Value,
        key: &PropertyKey,
    ) -> Result<(), JsError> {
        let name = match key {
            PropertyKey::String(name) => name.clone(),
            PropertyKey::Symbol(symbol) => {
                if let Some(description) = symbol.description() {
                    self.ensure_string_allocation(description.len().saturating_add(2))?;
                    self.consume_work(description.len().saturating_add(2))?;
                    let mut units = Vec::with_capacity(description.len() + 2);
                    units.push(91);
                    units.extend_from_slice(description.as_utf16());
                    units.push(93);
                    self.finish_string(units)?.as_string().unwrap().clone()
                } else {
                    JsString::default()
                }
            }
        };
        self.define_data_property(value, "name", Value::String(name), false, false, true)
    }
    pub(crate) fn compile_eval_string(
        &mut self,
        source: &JsString,
        strict: bool,
        context: crate::parser::EvalContext,
    ) -> Result<Rc<Chunk>, JsError> {
        self.consume_work(source.len().saturating_mul(2))?;
        // An isolated surrogate occupies three bytes in the same byte-based
        // source budget used for well-formed UTF-8 script inputs.
        let bytes = char::decode_utf16(source.as_utf16().iter().copied())
            .map(|entry| entry.map_or(3, char::len_utf8)).sum();
        let budget = self.compilation_budget_for_bytes(bytes)?;
        let lexer = crate::lexer::Lexer::from_utf16(source.as_utf16(), budget);
        bytecode::compile_eval(lexer, bytes, strict, context)
    }
    pub(crate) fn direct_eval(
        &mut self,
        value: Value,
        env: EnvId,
        strict: bool,
    ) -> Result<Value, JsError> {
        let Value::String(source) = value else {
            return Ok(value);
        };
        let context = self.eval_context(env)?;
        let chunk = self.compile_eval_string(&source, strict, context)?;
        self.retain_compiled_chunk(&chunk)?;
        if !self.can_enter_call() {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Eval depth budget exhausted",
            ));
        }
        self.call_depth += 1;
        let kind = if chunk.strict {
            EnvKind::Function
        } else {
            EnvKind::SloppyEval
        };
        let scope = self
            .heap
            .allocate_env(Environment::new(Some(env), kind, None));
        let result = self.run_chunk(&chunk, scope).and_then(|c| match c {
            Completion::Normal(v) => Ok(v.unwrap_or_default()),
            _ => Err(JsError::new(
                ErrorKind::InternalError,
                "Invalid eval completion",
            )),
        });
        self.call_depth -= 1;
        result
    }
    fn function_object_prototype(&self, kind: FunctionKind) -> ObjectId {
        match kind {
            FunctionKind::Ordinary => self.realm.function_prototype,
            FunctionKind::Async => self.realm.async_function_prototype.unwrap(),
            FunctionKind::Generator => self.realm.generator_function_prototype.unwrap(),
            FunctionKind::AsyncGenerator => self.realm.async_generator_function_prototype.unwrap(),
        }
    }

    pub(crate) fn dynamic_function(
        &mut self,
        parameters: &str,
        body: &str,
        new_target: Option<Value>,
        kind: FunctionKind,
    ) -> Result<Value, JsError> {
        if parameters.len().saturating_add(body.len()) > self.limits.max_source_bytes {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Function source exceeds byte budget",
            ));
        }
        let allocation = Self::source_allocation(parameters.len().saturating_add(body.len()));
        self.ensure_allocation(allocation)?;
        self.code_bytes = self.code_bytes.saturating_add(allocation);
        let code = bytecode::compile_dynamic_function(parameters, body, kind)?;
        let fallback = self.function_object_prototype(kind);
        let prototype = if let Some(target) = new_target {
            self.prototype_from_constructor(target, fallback)?
        } else {
            fallback
        };
        let function = self.make_function(code, self.realm.global_env)?;
        self.heap
            .object_mut(function.as_object().unwrap())?
            .prototype = Some(prototype);
        Ok(function)
    }
    #[inline]
    pub(crate) fn can_enter_call(&self) -> bool {
        self.call_depth < self.limits.max_call_depth
            && (self.call_depth < DEFAULT_CALL_DEPTH || native_stack_headroom())
    }

    #[inline]
    pub(crate) fn permits_bytecode_depth(&self, depth: usize) -> bool {
        depth <= self.limits.max_bytecode_depth
            && (depth <= DEFAULT_EXECUTION_DEPTH || native_stack_headroom())
    }

    pub(crate) fn run_chunk(&mut self, chunk: &Chunk, env: EnvId) -> Result<Completion, JsError> {
        if !self.permits_bytecode_depth(self.execution_depth + 1) {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Nested bytecode depth budget exhausted",
            ));
        }
        self.execution_depth += 1;
        let result = self.run_chunk_inner(chunk, env, true);
        self.execution_depth -= 1;
        result
    }
    pub(crate) fn run_prepared_chunk(
        &mut self,
        chunk: &Chunk,
        env: EnvId,
    ) -> Result<Completion, JsError> {
        if !self.permits_bytecode_depth(self.execution_depth + 1) {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Nested bytecode depth budget exhausted",
            ));
        }
        self.execution_depth += 1;
        let result = self.run_chunk_inner(chunk, env, false);
        self.execution_depth -= 1;
        result
    }
    fn run_chunk_inner(
        &mut self,
        chunk: &Chunk,
        env: EnvId,
        prepare: bool,
    ) -> Result<Completion, JsError> {
        if prepare {
            self.prepare(&chunk.scope, env)?;
        }
        let mut stack = Vec::<Item>::new();
        let mut result = None;
        let mut pc = 0;
        while pc < chunk.ops.len() {
            self.tick()?;
            let op = &chunk.ops[pc];
            pc += 1;
            if self.run_stack_op(op, &mut stack, env, chunk.strict, &mut pc)? {
                continue;
            }
            match self.run_control_op(op, env, chunk.strict)? {
                Completion::Normal(value) => {
                    if value.is_some() {
                        result = value;
                    }
                }
                abrupt => return Ok(abrupt.update_empty(result)),
            }
        }
        if let Some(item) = stack.pop() {
            result = Some(self.value(item)?);
        }
        Ok(Completion::Normal(result))
    }

    #[inline(never)]
    pub(crate) fn run_stack_op(
        &mut self,
        op: &Op,
        stack: &mut Vec<Item>,
        env: EnvId,
        strict: bool,
        pc: &mut usize,
    ) -> Result<bool, JsError> {
        match op {
            Op::Push(v) => {
                let value = self.account_value(v.clone())?;
                stack.push(Item::Value(value));
            }
            Op::Name(name) => {
                self.consume_work(name.len())?;
                stack.push(Item::Reference(self.resolve(env, name)?));
            }
            Op::LoadName(name) => {
                self.consume_work(name.len())?;
                let scope = self.heap.environment(env);
                let value = if scope.aliases.is_empty()
                    && !scope.has_lazy_arguments(name)
                    && let Some(binding) = scope.bindings.get(name)
                {
                    binding.read(name)?
                } else {
                    let reference = self.resolve(env, name)?;
                    self.read_reference(reference)?
                };
                stack.push(Item::Value(value));
            }
            Op::This => stack.push(Item::Value(self.get_this(env)?)),
            op @ (Op::NewTarget
            | Op::ImportMeta
            | Op::Class(..)
            | Op::PrivateProperty(..)
            | Op::PrivateIn(..)
            | Op::SuperProperty
            | Op::SuperConstructor
            | Op::SuperConstruct) => self.run_class_op(op, stack, env)?,
            op @ (Op::Property
            | Op::Get
            | Op::NormalizeReference
            | Op::InvalidCallAssignment
            | Op::Set
            | Op::Delete
            | Op::Update { .. }
            | Op::TypeofReference
            | Op::Unary(..)
            | Op::Binary(..)) => self.run_reference_op(op, stack, strict)?,
            Op::Dup => stack.push(stack.last().unwrap().clone()),
            Op::Swap => {
                let len = stack.len();
                stack.swap(len - 1, len - 2);
            }
            Op::OptionalCheck(target, call) => {
                let nullish = if *call {
                    if let Some(Item::CallTarget { function, .. }) = stack.last() {
                        function.is_nullish()
                    } else {
                        unreachable!()
                    }
                } else {
                    if let Some(Item::Value(value)) = stack.last() {
                        value.is_nullish()
                    } else {
                        unreachable!()
                    }
                };
                if nullish {
                    stack.pop();
                    stack.push(Item::Value(Value::Undefined));
                    *pc = *target;
                }
            }
            Op::Pop => {
                stack.pop();
            }
            Op::Jump(target) => *pc = *target,
            Op::JumpFalse(target) => {
                if !self.pop_value(stack)?.is_truthy() {
                    *pc = *target;
                }
            }
            Op::JumpTrue(target) => {
                if self.pop_value(stack)?.is_truthy() {
                    *pc = *target;
                }
            }
            Op::JumpNotNullish(target) => {
                if !self.pop_value(stack)?.is_nullish() {
                    *pc = *target;
                }
            }
            op @ (Op::RegExp(..)
            | Op::Array
            | Op::ArrayHole
            | Op::ArrayElement(..)
            | Op::Object
            | Op::ObjectSpread
            | Op::Destructure(..)
            | Op::TemplateObject(..)
            | Op::TemplateAppend(..)
            | Op::PropertyKey
            | Op::ObjectProperty { .. }
            | Op::Function(..)
            | Op::InferName(..)) => self.run_literal_op(op, stack, env, strict)?,
            op @ (Op::PrepareCall(..)
            | Op::Arguments
            | Op::Argument(..)
            | Op::Call
            | Op::Construct
            | Op::DynamicImport(_)) => self.run_call_op(op, stack, env, strict)?,
            Op::Await | Op::Yield { .. } => {
                return Err(JsError::new(
                    ErrorKind::InternalError,
                    "Suspension requires an owned execution frame",
                ));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    // Keep disjoint opcode temporaries in separate native frames. In debug
    // builds a single large match reserves stack for every arm on each entry.
    #[inline(never)]
    fn run_class_op(&mut self, op: &Op, stack: &mut Vec<Item>, env: EnvId) -> Result<(), JsError> {
        match op {
            Op::PrivateProperty(name) => {
                let receiver = self.pop_value(stack)?;
                stack.push(Item::Reference(
                    self.private_reference(env, receiver, name)?,
                ));
            }
            Op::PrivateIn(name) => {
                let object = self.pop_value(stack)?;
                stack.push(Item::Value(Value::Bool(
                    self.private_in(env, object, name)?,
                )));
            }
            Op::NewTarget => stack.push(Item::Value(self.get_new_target(env))),
            Op::ImportMeta => {
                let value = self.import_meta_for_env(env)?;
                stack.push(Item::Value(value));
            }
            Op::Class(code, name) => {
                let key = match name {
                    ClassName::Intrinsic => None,
                    ClassName::Binding(name) => Some(PropertyKey::String(name.clone())),
                    ClassName::Property => match stack.last() {
                        Some(Item::Value(Value::String(name))) => {
                            Some(PropertyKey::String(name.clone()))
                        }
                        Some(Item::Value(Value::Symbol(symbol))) => {
                            Some(PropertyKey::Symbol(symbol.clone()))
                        }
                        _ => unreachable!(),
                    },
                };
                let value = self.make_class(code.clone(), env, key.as_ref())?;
                stack.push(Item::Value(value));
            }
            Op::SuperProperty => {
                let key = self.pop_value(stack)?;
                let receiver = self.pop_value(stack)?;
                let reference = self.super_reference(env, receiver, key)?;
                stack.push(Item::Reference(reference));
            }
            Op::SuperConstructor => stack.push(Item::Value(self.super_constructor(env)?)),
            Op::SuperConstruct => {
                let Some(Item::Arguments(args)) = stack.pop() else {
                    unreachable!()
                };
                let target = self.pop_value(stack)?;
                let value = self.super_construct(env, target, &args)?;
                stack.push(Item::Value(value));
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    #[inline(never)]
    fn run_reference_op(
        &mut self,
        op: &Op,
        stack: &mut Vec<Item>,
        strict: bool,
    ) -> Result<(), JsError> {
        match op {
            Op::Property => {
                let key = self.pop_value(stack)?;
                let object = self.pop_value(stack)?;
                // Reference creation evaluates both operands without checking the
                // base or coercing the key. A simple assignment does those after
                // evaluating its right-hand side, when PutValue consumes it.
                stack.push(Item::Reference(Reference::Property(object, key)));
            }
            Op::Get => {
                let v = self.pop_value(stack)?;
                stack.push(Item::Value(v));
            }
            Op::InvalidCallAssignment => {
                return Err(JsError::new(ErrorKind::ReferenceError, "Cannot assign to a call result"));
            }
            Op::NormalizeReference => {
                let Some(Item::Reference(reference)) = stack.last_mut() else {
                    unreachable!()
                };
                self.normalize_reference(reference)?;
            }
            Op::Set => {
                let v = self.pop_value(stack)?;
                let Some(Item::Reference(reference)) = stack.pop() else {
                    return Err(JsError::new(ErrorKind::InternalError, "Expected reference"));
                };
                self.write_reference(reference, v.clone(), strict)?;
                stack.push(Item::Value(v));
            }
            Op::Delete => {
                let item = stack.pop().unwrap();
                let deleted = match item {
                    Item::Reference(Reference::Property(object, key)) => {
                        let key = self.property_reference_key(&object, &key)?;
                        self.delete_key(object, &key, strict)?
                    }
                    Item::Reference(
                        Reference::Global(object, key) | Reference::With(object, key),
                    ) => self.delete(Value::Object(object), &key, strict)?,
                    Item::Reference(Reference::Binding(env, key)) => {
                        let bindings = &mut self.heap.environment_mut(env).bindings;
                        if bindings.get(&key).is_some_and(|binding| binding.deletable) {
                            bindings.remove(&key);
                            true
                        } else { false }
                    }
                    Item::Reference(Reference::Import(_, _)) => false,
                    Item::Reference(Reference::Super(..)) => {
                        return Err(JsError::new(
                            ErrorKind::ReferenceError,
                            "Cannot delete a super property",
                        ));
                    }
                    Item::Reference(Reference::Private(..)) => {
                        return Err(JsError::new(
                            ErrorKind::InternalError,
                            "Private element delete escaped parser validation",
                        ));
                    }
                    _ => true,
                };
                stack.push(Item::Value(Value::Bool(deleted)));
            }
            Op::Update { increment, prefix } => {
                let Some(Item::Reference(mut reference)) = stack.pop() else {
                    return Err(JsError::new(
                        ErrorKind::InternalError,
                        "Expected update reference",
                    ));
                };
                self.normalize_reference(&mut reference)?;
                let old = self.read_reference(reference.clone())?;
                let old = self.coerce_numeric(&old)?;
                let new = if let Value::BigInt(value) = &old {
                    let delta = crate::JsBigInt::from_i64(if *increment { 1 } else { -1 });
                    self.bigint_binary("+", value, &delta)?
                } else {
                    Value::Number(old.as_number().unwrap() + if *increment { 1.0 } else { -1.0 })
                };
                self.write_reference(reference, new.clone(), strict)?;
                stack.push(Item::Value(if *prefix { new } else { old }));
            }
            Op::TypeofReference => {
                let item = stack.pop().unwrap();
                let value = if matches!(item, Item::Reference(Reference::Unresolvable(_))) {
                    Value::Undefined
                } else {
                    self.value(item)?
                };
                let kind = self.typeof_value(&value);
                stack.push(Item::Value(Value::from(kind)));
            }
            Op::Unary(op) => {
                let value = self.pop_value(stack)?;
                let value = self.unary(op, value)?;
                stack.push(Item::Value(value));
            }
            Op::Binary(op) => {
                let right = self.pop_value(stack)?;
                let left = self.pop_value(stack)?;
                let value = self.binary(op, left, right)?;
                stack.push(Item::Value(value));
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    #[inline(never)]
    fn run_literal_op(
        &mut self,
        op: &Op,
        stack: &mut Vec<Item>,
        env: EnvId,
        strict: bool,
    ) -> Result<(), JsError> {
        match op {
            Op::RegExp(program) => {
                let value = crate::regexp::literal(self, program.clone())?;
                stack.push(Item::Value(value));
            }
            Op::Array => {
                let value = self.try_new_array(&[])?;
                stack.push(Item::Value(value));
            }
            Op::ArrayHole => {
                let array = self.value(stack.last().unwrap().clone())?;
                self.append_array_literal(array, None)?;
            }
            Op::ArrayElement(spread) => {
                let value = self.pop_value(stack)?;
                let array = self.value(stack.last().unwrap().clone())?;
                if *spread {
                    let mut iterator = self.get_iterator(value)?;
                    while let Some(value) = self.iterator_step(&mut iterator)? {
                        self.append_array_literal(array.clone(), Some(value))?;
                    }
                } else {
                    self.append_array_literal(array, Some(value))?;
                }
            }
            Op::Object => {
                self.ensure_allocation(256)?;
                let object = self.new_object();
                stack.push(Item::Value(object));
            }
            Op::ObjectSpread => {
                let source = self.pop_value(stack)?;
                let target = self.value(stack.last().unwrap().clone())?;
                self.copy_data_properties(target, source, &[])?;
            }
            Op::Destructure(pattern) => {
                let value = self.pop_value(stack)?;
                self.bind_pattern(
                    pattern,
                    value.clone(),
                    env,
                    PatternMode::Assignment,
                    strict,
                    None,
                )?;
                stack.push(Item::Value(value));
            }
            Op::TemplateObject(site) => {
                let object = self.template_object(site.clone())?;
                stack.push(Item::Value(object));
            }
            Op::TemplateAppend(tail) => {
                let value = self.pop_value(stack)?;
                let prefix = self.pop_value(stack)?;
                let prefix = prefix.as_string().unwrap();
                let value = self.to_string(&value)?;
                let length = prefix
                    .len()
                    .saturating_add(value.len())
                    .saturating_add(tail.len());
                self.ensure_string_allocation(length)?;
                self.consume_work(length)?;
                let mut units = Vec::with_capacity(length);
                units.extend_from_slice(prefix.as_utf16());
                units.extend_from_slice(value.as_utf16());
                units.extend_from_slice(tail.as_utf16());
                let value = self.finish_string(units)?;
                stack.push(Item::Value(value));
            }
            Op::PropertyKey => {
                let value = self.pop_value(stack)?;
                let key = self.to_property_key(&value)?;
                stack.push(Item::Value(match key {
                    PropertyKey::String(s) => Value::String(s),
                    PropertyKey::Symbol(s) => Value::Symbol(s),
                }));
            }
            Op::ObjectProperty {
                prototype,
                infer_name,
                method,
            } => {
                let value = self.pop_value(stack)?;
                let key = self.pop_value(stack)?;
                let key = self.to_property_key(&key)?;
                let object = self.pop_value(stack)?;
                if *prototype {
                    if let Value::Object(proto) = value {
                        self.set_prototype(object.as_object().unwrap(), Some(proto))?;
                    } else if value == Value::Null {
                        self.set_prototype(object.as_object().unwrap(), None)?;
                    }
                } else if let Some(method) = method {
                    self.heap
                        .object_mut(value.as_object().unwrap())?
                        .home_object = object.as_object();
                    self.name_property_method(value.clone(), &key, *method)?;
                    self.define_method_property(object, key, value, *method, true)?;
                } else {
                    if *infer_name {
                        self.set_literal_function_name(value.clone(), &key)?;
                    }
                    self.define_key(object, key, Property::data(value))?;
                }
            }
            Op::Function(code) => {
                let function = self.make_function(code.clone(), env)?;
                stack.push(Item::Value(function));
            }
            Op::InferName(name) => {
                let value = self.pop_value(stack)?;
                self.set_literal_function_name(value.clone(), &PropertyKey::String(name.clone()))?;
                stack.push(Item::Value(value));
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    #[inline(never)]
    fn run_call_op(
        &mut self,
        op: &Op,
        stack: &mut Vec<Item>,
        env: EnvId,
        strict: bool,
    ) -> Result<(), JsError> {
        match op {
            Op::DynamicImport(phase) => {
                let options = self.pop_value(stack)?;
                let value = self.pop_value(stack)?;
                let promise = self.dynamic_import(*phase, value, options)?;
                stack.push(Item::Value(promise));
            }
            Op::PrepareCall(allow_direct_eval) => {
                let callee = stack.pop().unwrap();
                let direct_eval = *allow_direct_eval
                    && match &callee {
                        Item::Reference(
                            Reference::Binding(_, name)
                            | Reference::With(_, name)
                            | Reference::Global(_, name)
                            | Reference::Unresolvable(name),
                        ) => name == &JsString::from("eval"),
                        _ => false,
                    };
                let this = match &callee {
                    Item::Reference(
                        Reference::Property(object, _) | Reference::Private(object, _),
                    ) => object.clone(),
                    Item::Reference(Reference::With(object, _)) => Value::Object(*object),
                    Item::Reference(Reference::Super(_, receiver, _)) => receiver.clone(),
                    _ => Value::Undefined,
                };
                let function = self.value(callee)?;
                stack.push(Item::CallTarget {
                    function,
                    this,
                    direct_eval,
                });
            }
            Op::Arguments => stack.push(Item::Arguments(Vec::new())),
            Op::Argument(spread) => {
                let value = self.pop_value(stack)?;
                let Some(Item::Arguments(args)) = stack.last_mut() else {
                    unreachable!()
                };
                if *spread {
                    let mut iterator = self.get_iterator(value)?;
                    while let Some(value) = self.iterator_step(&mut iterator)? {
                        self.append_argument(args, value)?;
                    }
                } else {
                    self.append_argument(args, value)?;
                }
            }
            Op::Call => {
                let Some(Item::Arguments(args)) = stack.pop() else {
                    unreachable!()
                };
                let Some(Item::CallTarget {
                    function,
                    this,
                    direct_eval,
                }) = stack.pop()
                else {
                    unreachable!()
                };
                let value = if direct_eval && function.as_object() == self.realm.intrinsic_eval {
                    self.direct_eval(args.first().cloned().unwrap_or_default(), env, strict)?
                } else {
                    self.invoke(function, this, &args)?
                };
                stack.push(Item::Value(value));
            }
            Op::Construct => {
                let Some(Item::Arguments(args)) = stack.pop() else {
                    unreachable!()
                };
                let function = self.pop_value(stack)?;
                let value = self.construct_target(function.clone(), &args, function)?;
                stack.push(Item::Value(value));
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    pub(crate) fn thrown_error(&mut self, value: Value) -> Result<JsError, JsError> {
        let mut kind = ErrorKind::Thrown;
        let diagnostic = match &value {
            Value::Object(id) => {
                let object = self.heap.object(*id)?;
                if let ObjectKind::Error(error_kind, _) = object.kind {
                    kind = error_kind;
                }
                object
                    .properties
                    .get(&JsString::from("message"))
                    .and_then(|p| {
                        if let PropertyKind::Data {
                            value: Value::String(message),
                            ..
                        } = &p.kind
                        {
                            Some(message.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "<thrown object>".into())
            }
            Value::Symbol(_) => "<thrown symbol>".into(),
            _ => self.to_string(&value)?,
        };
        let units = diagnostic.as_utf16();
        let shown = &units[..units.len().min(2048)];
        self.consume_work(shown.len())?;
        let mut message = String::from_utf16_lossy(shown);
        if shown.len() < units.len() {
            message.push('…');
        }
        Ok(JsError {
            kind,
            message,
            line: None,
            column: None,
            thrown: Some(value),
            inspected_calls: None,
        })
    }

    pub(crate) fn next_for_iteration(
        &mut self,
        outer: EnvId,
        current: EnvId,
        lexical: &[(JsString, bool)],
    ) -> Result<EnvId, JsError> {
        // Only a `let` initializer creates per-iteration bindings. Keep its
        // own environment (captured by closures in the initializer) separate.
        if !lexical.iter().any(|(_, mutable)| *mutable) {
            return Ok(current);
        }
        let bindings = &self.heap.environment(current).bindings;
        let bytes: usize = bindings.keys().map(|key| 192 + key.len() * 2).sum();
        self.ensure_allocation(384 + bytes)?;
        let bindings = bindings.clone();
        for key in bindings.keys() {
            self.reserve_binding(key)?;
        }
        let mut next = Environment::new(Some(outer), EnvKind::Block, None);
        next.bindings = bindings;
        Ok(self.heap.allocate_env(next))
    }

    // Keep control-flow temporaries off the recursive expression-dispatch frame.
    pub(crate) fn run_control_op(
        &mut self,
        op: &Op,
        env: EnvId,
        strict: bool,
    ) -> Result<Completion, JsError> {
        let mut result = None;
        match op {
            Op::CopyAnnexFunction(name) => self.copy_annex_function(env, name)?,
            Op::Evaluate(expression) => result = Some(self.expression_value(expression, env)?),
            Op::Initialize(kind, key, expression) => {
                let value = if let Some(e) = expression {
                    self.expression_value(e, env)?
                } else {
                    Value::Undefined
                };
                if expression.is_some() || *kind != VarKind::Var {
                    self.bind_pattern(key, value, env, PatternMode::Binding(*kind), strict, None)?;
                }
            }
            Op::Block(block) => {
                let scope = self.block_environment(block, env)?;
                match self.run_chunk(block, scope)? {
                    Completion::Normal(v) => {
                        if v.is_some() {
                            result = v;
                        }
                    }
                    c => return Ok(c),
                }
            }
            Op::Label(label, body) => return Ok(self.run_chunk(body, env)?.leave_label(*label)),
            Op::With(object, body) => {
                let object = self.expression_value(object, env)?;
                let scope = self.with_scope(env, object)?;
                return Ok(self
                    .run_chunk(body, scope)?
                    .update_empty(Some(Value::Undefined)));
            }
            Op::Branch(arms, no) => {
                let mut selected = no.as_ref();
                for (test, body) in arms.iter() {
                    if self.expression_value(test, env)?.is_truthy() {
                        selected = Some(body);
                        break;
                    }
                }
                return Ok(if let Some(selected) = selected {
                    self.run_chunk(selected, env)?
                        .update_empty(Some(Value::Undefined))
                } else {
                    Completion::Normal(Some(Value::Undefined))
                });
            }
            Op::While(test, body, label) => {
                result = Some(Value::Undefined);
                while self.expression_value(test, env)?.is_truthy() {
                    self.tick()?;
                    if let Some(completion) = self
                        .run_chunk(body, env)?
                        .after_loop_body(*label, &mut result)
                    {
                        return Ok(completion);
                    }
                }
            }
            Op::DoWhile(body, test, label) => {
                result = Some(Value::Undefined);
                loop {
                    self.tick()?;
                    if let Some(completion) = self
                        .run_chunk(body, env)?
                        .after_loop_body(*label, &mut result)
                    {
                        return Ok(completion);
                    }
                    if !self.expression_value(test, env)?.is_truthy() {
                        break;
                    }
                }
            }
            Op::For {
                initial,
                test,
                update,
                body,
                lexical,
                label,
            } => {
                result = Some(Value::Undefined);
                let mut iteration = if lexical.is_empty() {
                    env
                } else {
                    let scope =
                        self.heap
                            .allocate_env(Environment::new(Some(env), EnvKind::Block, None));
                    for (name, mutable) in lexical {
                        self.reserve_binding(name)?;
                        self.heap.environment_mut(scope).bindings.insert(
                            name.clone(),
                            Binding {
                                value: Value::Undefined,
                                mutable: *mutable,
                                initialized: false,
                                lexical: true,
                                strict_immutable: true,
                                deletable: false,
                            },
                        );
                    }
                    scope
                };
                if let Some(init) = initial {
                    self.run_chunk(init, iteration)?;
                }
                iteration = self.next_for_iteration(env, iteration, lexical)?;
                loop {
                    self.tick()?;
                    if let Some(test) = test
                        && !self.expression_value(test, iteration)?.is_truthy()
                    {
                        break;
                    }
                    if let Some(completion) = self
                        .run_chunk(body, iteration)?
                        .after_loop_body(*label, &mut result)
                    {
                        return Ok(completion);
                    }
                    iteration = self.next_for_iteration(env, iteration, lexical)?;
                    if let Some(update) = update {
                        self.expression_value(update, iteration)?;
                    }
                }
            }
            Op::ForIn(kind, name, object, body, label) => {
                result = Some(Value::Undefined);
                let head = self.iteration_scope(env, *kind, name)?;
                let object = self.expression_value(object, head)?;
                let mut keys = ForInKeys::new(self, object)?;
                while let Some(key) = keys.next(self)? {
                    let scope =
                        self.bind_iteration_value(env, *kind, name, Value::String(key), strict)?;
                    if let Some(completion) = self
                        .run_chunk(body, scope)?
                        .after_loop_body(*label, &mut result)
                    {
                        return Ok(completion);
                    }
                }
            }
            Op::ForOf(kind, name, iterable, body, false, label) => {
                result = Some(Value::Undefined);
                let head = self.iteration_scope(env, *kind, name)?;
                let iterable = self.expression_value(iterable, head)?;
                let mut iterator = self.get_iterator(iterable)?;
                while let Some(value) = self.iterator_step(&mut iterator)? {
                    let completion = (|| {
                        let scope = self.bind_iteration_value(env, *kind, name, value, strict)?;
                        self.run_chunk(body, scope)
                    })();
                    match completion {
                        Ok(completion) => {
                            if let Some(completion) =
                                completion.after_loop_body(*label, &mut result)
                            {
                                self.iterator_close(&mut iterator, Ok(Value::Undefined))?;
                                return Ok(completion);
                            }
                        }
                        Err(error) => {
                            return self
                                .iterator_close(&mut iterator, Err(error))
                                .map(|_| Completion::Normal(None));
                        }
                    }
                }
            }
            Op::Switch(test, cases, plan) => {
                result = Some(Value::Undefined);
                let value = self.expression_value(test, env)?;
                let scope =
                    self.heap
                        .allocate_env(Environment::new(Some(env), EnvKind::Block, None));
                self.prepare(plan, scope)?;
                let mut start = None;
                let mut default = None;
                for (i, (test, _)) in cases.iter().enumerate() {
                    if let Some(test) = test {
                        if self.expression_value(test, scope)? == value {
                            start = Some(i);
                            break;
                        }
                    } else {
                        default = Some(i);
                    }
                }
                if let Some(start) = start.or(default) {
                    for (_, body) in &cases[start..] {
                        match self.run_chunk(body, scope)?.update_empty(result.take()) {
                            Completion::Normal(value) => result = value,
                            Completion::Break(None, value) => return Ok(Completion::Normal(value)),
                            c => return Ok(c),
                        }
                    }
                }
            }
            Op::Return(e) => {
                return Ok(Completion::Return(if let Some(e) = e {
                    self.expression_value(e, env)?
                } else {
                    Value::Undefined
                }));
            }
            Op::Break(target) => return Ok(Completion::Break(*target, None)),
            Op::Continue(target) => return Ok(Completion::Continue(*target, None)),
            Op::Throw(e) => {
                let value = self.expression_value(e, env)?;
                return Err(self.thrown_error(value)?);
            }
            Op::Try(body, catch, finally) => {
                let mut completion = self.run_chunk(body, env);
                if let Err(error) = &completion {
                    if error.uncatchable() {
                        return completion;
                    }
                    if let Some((name, handler)) = catch {
                        let value = if let Some(value) = &error.thrown {
                            value.clone()
                        } else {
                            self.error_value(error)?
                        };
                        let scope = self.heap.allocate_env(Environment::new(
                            Some(env),
                            if matches!(name.as_deref(), Some(bytecode::PatternCode::Name(_))) { EnvKind::SimpleCatch } else { EnvKind::Block },
                            None,
                        ));
                        if let Some(name) = name {
                            self.declare_pattern(name, scope, true, true)?;
                        }
                        completion = if let Some(pattern) = name {
                            self.bind_pattern(
                                pattern,
                                value,
                                scope,
                                PatternMode::Parameter,
                                strict,
                                None,
                            )
                            .and_then(|_| self.run_chunk(handler, scope))
                        } else {
                            self.run_chunk(handler, scope)
                        };
                    }
                }
                if let Some(finally) = finally {
                    match self.run_chunk(finally, env) {
                        Ok(Completion::Normal(_)) => {}
                        other => completion = other,
                    }
                }
                return completion.map(|value| value.update_empty(Some(Value::Undefined)));
            }
            _ => unreachable!(),
        }
        Ok(Completion::Normal(result))
    }
    pub fn error_value(&mut self, error: &JsError) -> Result<Value, JsError> {
        let prototype = self
            .realm
            .error_prototypes
            .get(&error.kind)
            .copied()
            .unwrap_or(self.realm.error_prototype);
        let value = self.allocate_error(error.kind, prototype)?;
        self.define_data_property(
            value.clone(),
            "message",
            Value::from(error.message.clone()),
            true,
            false,
            true,
        )?;
        Ok(value)
    }
    fn typeof_value(&self, value: &Value) -> &'static str {
        match value {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::BigInt(_) => "bigint",
            Value::String(_) => "string",
            Value::Symbol(_) => "symbol",
            Value::Object(id) if id.is_html_dda() => "undefined",
            Value::Object(_) => {
                if self.is_callable(value) {
                    "function"
                } else {
                    "object"
                }
            }
        }
    }
    fn unary(&mut self, op: &str, value: Value) -> Result<Value, JsError> {
        Ok(match op {
            "!" => Value::Bool(!value.is_truthy()),
            "+" => Value::Number(self.to_number(&value)?),
            "-" | "~" => match self.coerce_numeric(&value)? {
                Value::BigInt(value) => self.bigint_unary(op, &value)?,
                Value::Number(value) if op == "-" => Value::Number(-value),
                Value::Number(value) => Value::Number((!to_i32(value)) as f64),
                _ => unreachable!("ToNumeric returned a numeric primitive"),
            },
            "void" => Value::Undefined,
            "typeof" => Value::from(self.typeof_value(&value)),
            _ => {
                return Err(JsError::new(
                    ErrorKind::InternalError,
                    "Unknown unary bytecode",
                ));
            }
        })
    }
    fn binary(&mut self, op: &str, left: Value, right: Value) -> Result<Value, JsError> {
        if matches!(op, "===" | "!==") {
            self.charge_bigint_comparison(&left, &right)?;
            if let (Value::String(a), Value::String(b)) = (&left, &right) {
                self.compare_strings(a, b)?;
            }
            let equal = left == right;
            return Ok(Value::Bool(if op == "===" { equal } else { !equal }));
        }
        if matches!(op, "==" | "!=") {
            let equal = self.abstract_equal(left, right)?;
            return Ok(Value::Bool(if op == "==" { equal } else { !equal }));
        }
        if op == "in" {
            if right.as_object().is_none() {
                return Err(JsError::type_error(
                    "Right-hand side of in is not an object",
                ));
            }
            let key = self.to_property_key(&left)?;
            if let PropertyKey::String(key) = &key {
                self.consume_work(key.len())?;
            }
            return Ok(Value::Bool(self.has_key(&right, &key)?));
        }
        if op == "instanceof" {
            return self.instanceof_operator(left, right);
        }
        let (a, b) = if op == "+" {
            (
                self.primitive_hint(left, "default")?,
                self.primitive_hint(right, "default")?,
            )
        } else if matches!(op, "<" | ">" | "<=" | ">=") {
            (
                self.coerce_primitive(left, false)?,
                self.coerce_primitive(right, false)?,
            )
        } else {
            (self.coerce_numeric(&left)?, self.coerce_numeric(&right)?)
        };
        if op == "+" && (matches!(a, Value::String(_)) || matches!(b, Value::String(_))) {
            let a = self.to_string(&a)?;
            let b = self.to_string(&b)?;
            if a.len().saturating_add(b.len()) > self.limits.max_string_units {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "String code-unit budget exhausted",
                ));
            }
            if a.is_empty() {
                return self.checked_string(b);
            }
            if b.is_empty() {
                return self.checked_string(a);
            }
            let len = a.len().saturating_add(b.len());
            self.ensure_string_allocation(len)?;
            if let Some((result, copied)) = a.append_short(&b) {
                self.consume_work(copied)?;
                return self.checked_string(result);
            }
            self.charge_buffer_work(len)?;
            return self.checked_string(a.concat(&b));
        }
        if matches!(op, "<" | ">" | "<=" | ">=") {
            let comparison = match (&a, &b) {
                (Value::String(a), Value::String(b)) => {
                    self.compare_strings(a, b)?;
                    Some(a.cmp(b))
                }
                (Value::BigInt(x), Value::BigInt(y)) => {
                    self.charge_bigint_comparison(&a, &b)?;
                    Some(x.cmp(y))
                }
                (Value::BigInt(x), Value::String(y)) => self.compare_bigint_string(x, y)?,
                (Value::String(x), Value::BigInt(y)) => self
                    .compare_bigint_string(y, x)?
                    .map(std::cmp::Ordering::reverse),
                (Value::BigInt(x), _) => {
                    let number = self.to_number(&b)?;
                    self.compare_bigint_number(x, number)?
                }
                (_, Value::BigInt(y)) => {
                    let number = self.to_number(&a)?;
                    self.compare_bigint_number(y, number)?
                        .map(std::cmp::Ordering::reverse)
                }
                _ => self.to_number(&a)?.partial_cmp(&self.to_number(&b)?),
            };
            return Ok(Value::Bool(comparison.is_some_and(|c| match op {
                "<" => c.is_lt(),
                ">" => c.is_gt(),
                "<=" => c.is_le(),
                _ => c.is_ge(),
            })));
        }
        let (a, b) = if op == "+" {
            (self.numeric_primitive(a)?, self.numeric_primitive(b)?)
        } else {
            (a, b)
        };
        let (a, b) = match (a, b) {
            (Value::Number(a), Value::Number(b)) => (a, b),
            (Value::BigInt(a), Value::BigInt(b)) => return self.bigint_binary(op, &a, &b),
            _ => return Err(JsError::type_error("Cannot mix BigInt and Number operands")),
        };
        Ok(Value::Number(match op {
            "+" => a + b,
            "-" => a - b,
            "*" => a * b,
            "/" => a / b,
            "%" => a % b,
            "**" => crate::builtins::math::pow(a, b),
            "|" => (to_i32(a) | to_i32(b)) as f64,
            "&" => (to_i32(a) & to_i32(b)) as f64,
            "^" => (to_i32(a) ^ to_i32(b)) as f64,
            "<<" => to_i32(a).wrapping_shl((to_u32(b)) & 31) as f64,
            ">>" => (to_i32(a) >> ((to_u32(b)) & 31)) as f64,
            ">>>" => (to_u32(a) >> ((to_u32(b)) & 31)) as f64,
            _ => {
                return Err(JsError::new(
                    ErrorKind::InternalError,
                    "Unknown binary bytecode",
                ));
            }
        }))
    }
    fn abstract_equal(&mut self, a: Value, b: Value) -> Result<bool, JsError> {
        if std::mem::discriminant(&a) == std::mem::discriminant(&b) {
            self.charge_bigint_comparison(&a, &b)?;
            if let (Value::String(a), Value::String(b)) = (&a, &b) {
                self.compare_strings(a, b)?;
            }
            return Ok(a == b);
        }
        if (a.is_nullish() && (b.is_nullish() || b.as_object().is_some_and(|id| id.is_html_dda())))
            || (b.is_nullish() && a.as_object().is_some_and(|id| id.is_html_dda()))
        {
            return Ok(true);
        }
        match (&a, &b) {
            (Value::BigInt(a), Value::Number(b)) | (Value::Number(b), Value::BigInt(a)) => Ok(self
                .compare_bigint_number(a, *b)?
                .is_some_and(|order| order.is_eq())),
            (Value::BigInt(a), Value::String(b)) | (Value::String(b), Value::BigInt(a)) => Ok(self
                .compare_bigint_string(a, b)?
                .is_some_and(|order| order.is_eq())),
            (Value::Number(_), Value::String(_)) | (Value::String(_), Value::Number(_)) => {
                Ok(self.to_number(&a)? == self.to_number(&b)?)
            }
            (Value::Bool(_), _) => {
                let n = Value::Number(self.to_number(&a)?);
                self.abstract_equal(n, b)
            }
            (_, Value::Bool(_)) => {
                let n = Value::Number(self.to_number(&b)?);
                self.abstract_equal(a, n)
            }
            (
                Value::Object(_),
                Value::String(_) | Value::Number(_) | Value::BigInt(_) | Value::Symbol(_),
            ) => {
                let primitive = self.primitive_hint(a, "default")?;
                self.abstract_equal(primitive, b)
            }
            (
                Value::String(_) | Value::Number(_) | Value::BigInt(_) | Value::Symbol(_),
                Value::Object(_),
            ) => {
                let primitive = self.primitive_hint(b, "default")?;
                self.abstract_equal(a, primitive)
            }
            _ => Ok(false),
        }
    }
}
pub(crate) fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => {
            a.is_nan() && b.is_nan() || (a == b && a.is_sign_negative() == b.is_sign_negative())
        }
        _ => a == b,
    }
}
pub(crate) fn to_u32(n: f64) -> u32 {
    if !n.is_finite() || n == 0.0 {
        0
    } else {
        n.trunc().rem_euclid(4294967296.0) as u32
    }
}
pub(crate) fn to_i32(n: f64) -> i32 {
    to_u32(n) as i32
}
pub(crate) fn number_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n == f64::INFINITY {
        return "Infinity".into();
    }
    if n == f64::NEG_INFINITY {
        return "-Infinity".into();
    }
    if n == 0.0 {
        return "0".into();
    }
    let raw = n.to_string();
    if n.abs() >= 1e-6 && n.abs() < 1e21 {
        return raw;
    }
    let unsigned = raw.strip_prefix('-').unwrap_or(&raw);
    let decimal = unsigned.find('.').unwrap_or(unsigned.len());
    let all: String = unsigned.chars().filter(|c| *c != '.').collect();
    let leading = all.bytes().take_while(|b| *b == b'0').count();
    let digits = all[leading..].trim_end_matches('0');
    let exponent = decimal as i32 - leading as i32 - 1;
    let mut out = String::new();
    if n < 0.0 {
        out.push('-');
    }
    out.push(digits.as_bytes()[0] as char);
    if digits.len() > 1 {
        out.push('.');
        out.push_str(&digits[1..]);
    }
    out.push('e');
    if exponent >= 0 {
        out.push('+');
    }
    out.push_str(&exponent.to_string());
    out
}
fn parse_number(s: &JsString) -> f64 {
    let s = s.to_string_lossy();
    let s = s.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}');
    if s.is_empty() {
        return 0.0;
    }
    if s == "Infinity" || s == "+Infinity" {
        return f64::INFINITY;
    }
    if s == "-Infinity" {
        return f64::NEG_INFINITY;
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ] {
        if let Some(d) = s.strip_prefix(prefix) {
            if d.is_empty() {
                return f64::NAN;
            }
            let mut n = 0f64;
            for c in d.chars() {
                let Some(v) = c.to_digit(radix) else {
                    return f64::NAN;
                };
                n = n * radix as f64 + v as f64;
            }
            return n;
        }
    }
    if s.bytes().any(|b| matches!(b, b'i' | b'I')) {
        f64::NAN
    } else {
        s.parse().unwrap_or(f64::NAN)
    }
}

#[cfg(test)]
mod task_limit_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn embedded_errors_propagate_explicit_interrupts_and_expired_task_deadlines() {
        let mut vm = Vm::new();
        let flag = Arc::new(AtomicBool::new(false));
        vm.set_interrupt_flag(flag.clone());
        vm.with_task(|vm| {
            flag.store(true, Ordering::Relaxed);
            let (result, recoverable) = vm.eval_embedded_script("0", "child");
            assert_eq!(result.unwrap_err().kind, ErrorKind::ExecutionLimit);
            assert!(!recoverable);
            flag.store(false, Ordering::Relaxed);
            Ok(())
        })
        .unwrap();

        vm.with_task_limits(100, Some(Instant::now()), |vm| {
            let (result, recoverable) = vm.eval_embedded_script("0", "child");
            assert_eq!(result.unwrap_err().kind, ErrorKind::ExecutionLimit);
            assert!(!recoverable);
            Ok(())
        })
        .unwrap();
        assert_eq!(vm.eval("4 + 3").unwrap(), 7.into());
    }

    #[test]
    fn nested_callbacks_share_fuel_and_deadlines_and_next_tasks_recover() {
        let mut vm = Vm::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        let failure = vm
            .with_task_limits(100, Some(deadline), |vm| {
                vm.charge_work(80)?;
                vm.with_task_limits(150, None, |vm| vm.charge_work(30))
            })
            .unwrap_err();
        assert_eq!(failure.kind, ErrorKind::ExecutionLimit);

        let expired = Instant::now();
        let failure = vm
            .with_task_limits(u64::MAX, Some(expired), |vm| {
                vm.with_task(|vm| vm.eval("while(true){}"))
            })
            .unwrap_err();
        assert_eq!(failure.kind, ErrorKind::ExecutionLimit);
        assert_eq!(vm.eval("4 + 3").unwrap(), 7.into());
    }
}

#[cfg(feature = "twitch-bootstrap-profile")]
type ProfileHistogram =
    std::collections::HashMap<(u64, &'static str, &'static str, u32), (u64, u64)>;

#[cfg(feature = "twitch-bootstrap-profile")]
thread_local! {
    static QA_DATA: std::cell::RefCell<Option<ProfileHistogram>> = Default::default();
    static QA_STACK: std::cell::RefCell<Vec<u64>> = Default::default();
}

#[cfg(feature = "twitch-bootstrap-profile")]
#[track_caller]
pub(crate) fn qa_charge(kind: &'static str, units: usize) {
    let location = std::panic::Location::caller();
    QA_DATA.with_borrow_mut(|histogram| {
        if let Some(histogram) = histogram.as_mut() {
            let module = QA_STACK.with_borrow(|stack| stack.last().copied().unwrap_or_default());
            let module = match module {
                69861 | 174790 | 846451 | 261689 | 995497 | 925098 => module,
                _ => 0,
            };
            let (total, count) = histogram
                .entry((module, kind, location.file(), location.line()))
                .or_default();
            *total += units as u64;
            *count += 1;
        }
    });
}
