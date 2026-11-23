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
};

use crate::dynamic_import::{DynamicImportHook, SourceReferrer};

#[cfg(test)]
mod global_environment_tests;
#[cfg(test)]
mod global_reference_tests;

const DEFAULT_EXECUTION_DEPTH: usize = 16;
const DEFAULT_CALL_DEPTH: usize = 24;
const MAX_NATIVE_DEPTH: usize = 64;
const MAX_CALL_DEPTH: usize = if cfg!(windows) {
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
    #[cfg(not(windows))]
    {
        false
    }
}

#[inline]
pub(crate) fn permits_bytecode_depth(depth: usize) -> bool {
    depth <= DEFAULT_EXECUTION_DEPTH || (depth <= MAX_NATIVE_DEPTH && native_stack_headroom())
}

#[derive(Clone, Debug)]
pub struct VmLimits {
    pub max_instructions: u64,
    pub max_call_depth: usize,
    pub max_heap_objects: usize,
    /// Conservative aggregate allocation allowance, reset to traced live usage by GC.
    pub max_heap_bytes: usize,
    pub max_source_bytes: usize,
    pub max_string_units: usize,
    pub max_array_length: u32,
    pub max_pending_jobs: usize,
}
impl Default for VmLimits {
    fn default() -> Self {
        Self {
            max_instructions: 1_000_000,
            max_call_depth: MAX_CALL_DEPTH,
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
    /// Binding object of `global_env`; it need not be the global this value.
    pub(crate) global_object: ObjectId,
    pub(crate) global_env: EnvId,
    pub(crate) object_prototype: ObjectId,
    pub(crate) function_prototype: ObjectId,
    pub(crate) async_function_prototype: Option<ObjectId>,
    pub(crate) generator_function_prototype: Option<ObjectId>,
    pub(crate) generator_prototype: Option<ObjectId>,
    pub(crate) async_generator_function_prototype: Option<ObjectId>,
    pub(crate) async_generator_prototype: Option<ObjectId>,
    pub(crate) async_iterator_prototype: Option<ObjectId>,
    pub(crate) async_from_sync_prototype: Option<ObjectId>,
    pub(crate) array_prototype: ObjectId,
    pub(crate) binary: crate::binary::Intrinsics,
    pub(crate) clone_intrinsics: crate::structured_clone::Intrinsics,
    pub(crate) string_prototype: ObjectId,
    pub(crate) regexp_prototype: Option<ObjectId>,
    pub(crate) regexp_constructor: Option<ObjectId>,
    pub(crate) regexp_iterator_prototype: Option<ObjectId>,
    pub(crate) number_prototype: ObjectId,
    pub(crate) bigint_prototype: Option<ObjectId>,
    pub(crate) bigint_storage: crate::bigint::Storage,
    pub(crate) boolean_prototype: ObjectId,
    pub(crate) error_prototype: ObjectId,
    pub(crate) error_prototypes: BTreeMap<ErrorKind, ObjectId>,
    pub(crate) intrinsic_eval: Option<ObjectId>,
    pub(crate) throw_type_error: Option<ObjectId>,
    roots: BTreeMap<RootId, Value>,
    next_root: u64,
    pub(crate) next_private_id: u64,
    pub(crate) limits: VmLimits,
    remaining: u64,
    interrupt: Option<Arc<AtomicBool>>,
    active: usize,
    pub(crate) call_depth: usize,
    pub(crate) execution_depth: usize,
    string_storage: BTreeMap<usize, usize>,
    string_bytes: usize,
    pub(crate) code_bytes: usize,
    initializing: bool,
    pub(crate) promise_prototype: Option<ObjectId>,
    pub(crate) promise_constructor: Option<ObjectId>,
    pub(crate) jobs: VecDeque<crate::promise::Job>,
    pub(crate) unhandled_promises: Vec<ObjectId>,
    pub(crate) json_depth: usize,
    pub(crate) symbol_prototype: Option<ObjectId>,
    pub(crate) well_known_symbols: BTreeMap<&'static str, SymbolId>,
    pub(crate) symbol_registry: BTreeMap<JsString, SymbolId>,
    pub(crate) symbol_storage: BTreeMap<usize, std::rc::Weak<crate::value::SymbolData>>,
    pub(crate) iterator_prototype: Option<ObjectId>,
    pub(crate) array_iterator_prototype: Option<ObjectId>,
    pub(crate) string_iterator_prototype: Option<ObjectId>,
    pub(crate) template_objects: BTreeMap<usize, (std::rc::Weak<bytecode::TemplateSite>, Value)>,
    pub(crate) pattern_depth: usize,
    pub(crate) module_evaluation_active: bool,
    pub(crate) module_async_order: u64,
    pub(crate) module_realm: u64,
    pub(crate) active_referrer: Option<SourceReferrer>,
    pub(crate) dynamic_import_hook: Option<DynamicImportHook>,
    pub(crate) wasm_compilation_allowed: bool,
}
#[derive(Clone)]
pub(crate) enum Reference {
    Binding(EnvId, JsString),
    Global(ObjectId, JsString),
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
        // This first interpreter still uses bounded Rust calls for nested chunks.
        // Keep the host stack safe even when an embedder requests a larger limit.
        limits.max_call_depth = limits.max_call_depth.min(MAX_CALL_DEPTH);
        let mut heap = Heap::default();
        let object_prototype = heap.allocate(Cell::Object(Object::new(ObjectKind::Ordinary, None)));
        let global_object = heap.allocate(Cell::Object(Object::new(
            ObjectKind::Ordinary,
            Some(object_prototype),
        )));
        let global_env = heap.allocate_env(Environment::new_global(global_object, global_object));
        let function_prototype = heap.allocate(Cell::Object(Object::new(
            ObjectKind::Native {
                callback: Rc::new(|_, _, _| Ok(Value::Undefined)),
                construct: None,
                realm_environment: global_env,
            },
            Some(object_prototype),
        )));
        let array_prototype = heap.allocate(Cell::Object(Object::new(
            ObjectKind::Array {
                length: 0,
                length_writable: true,
            },
            Some(object_prototype),
        )));
        let string_prototype = heap.allocate(Cell::Object(Object::new(
            ObjectKind::Boxed(Value::from("")),
            Some(object_prototype),
        )));
        let number_prototype = heap.allocate(Cell::Object(Object::new(
            ObjectKind::Boxed(Value::Number(0.0)),
            Some(object_prototype),
        )));
        let boolean_prototype = heap.allocate(Cell::Object(Object::new(
            ObjectKind::Boxed(Value::Bool(false)),
            Some(object_prototype),
        )));
        let error_prototype = heap.allocate(Cell::Object(Object::new(
            ObjectKind::Ordinary,
            Some(object_prototype),
        )));
        let mut vm = Self {
            heap,
            global_object,
            global_env,
            object_prototype,
            function_prototype,
            async_function_prototype: None,
            generator_function_prototype: None,
            generator_prototype: None,
            async_generator_function_prototype: None,
            async_generator_prototype: None,
            async_iterator_prototype: None,
            async_from_sync_prototype: None,
            array_prototype,
            binary: crate::binary::Intrinsics::default(),
            clone_intrinsics: crate::structured_clone::Intrinsics::default(),
            string_prototype,
            regexp_prototype: None,
            regexp_constructor: None,
            regexp_iterator_prototype: None,
            number_prototype,
            bigint_prototype: None,
            bigint_storage: crate::bigint::Storage::default(),
            boolean_prototype,
            error_prototype,
            error_prototypes: BTreeMap::new(),
            intrinsic_eval: None,
            throw_type_error: None,
            roots: BTreeMap::new(),
            next_root: 1,
            next_private_id: 1,
            remaining: limits.max_instructions,
            limits,
            interrupt: None,
            active: 0,
            call_depth: 0,
            execution_depth: 0,
            string_storage: BTreeMap::new(),
            string_bytes: 0,
            code_bytes: 0,
            initializing: true,
            promise_prototype: None,
            promise_constructor: None,
            jobs: VecDeque::new(),
            unhandled_promises: Vec::new(),
            json_depth: 0,
            symbol_prototype: None,
            well_known_symbols: BTreeMap::new(),
            symbol_registry: BTreeMap::new(),
            symbol_storage: BTreeMap::new(),
            iterator_prototype: None,
            array_iterator_prototype: None,
            string_iterator_prototype: None,
            template_objects: BTreeMap::new(),
            pattern_depth: 0,
            module_evaluation_active: false,
            module_async_order: 0,
            module_realm: crate::modules::module_realm_id(),
            active_referrer: None,
            dynamic_import_hook: None,
            wasm_compilation_allowed: true,
        };
        crate::builtins::install(&mut vm);
        vm.initializing = false;
        vm
    }
    pub fn limits(&self) -> &VmLimits {
        &self.limits
    }
    /// Controls whether this VM can compile new WebAssembly modules from bytes.
    ///
    /// Standalone VMs allow compilation by default. Validation and instantiation
    /// of an already compiled module are unaffected.
    pub fn set_wasm_compilation_allowed(&mut self, allowed: bool) {
        self.wasm_compilation_allowed = allowed;
    }
    pub fn check_syntax(&mut self, source: &str) -> Result<(), JsError> {
        let chunk = bytecode::compile(source, self.compilation_budget(source)?)?;
        self.ensure_allocation(bytecode::retained_size_with_chunks(Vec::new(), vec![chunk]))
    }
    pub(crate) fn compilation_budget(&self, source: &str) -> Result<usize, JsError> {
        if source.len() > self.limits.max_source_bytes {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Script source exceeds configured byte limit",
            ));
        }
        // UTF-32 lexer input and decoded character data, including temporary
        // UTF-16 buffers during literal conversion. The lexer accounts for
        // per-token structural allocations as it consumes the source.
        let input_bytes = source.len().saturating_mul(16).saturating_add(4096);
        self.ensure_allocation(input_bytes)?;
        Ok(self
            .limits
            .max_heap_bytes
            .saturating_sub(self.estimated_heap_bytes())
            .saturating_sub(input_bytes))
    }
    pub(crate) fn retain_compiled_chunk(&mut self, chunk: &Rc<Chunk>) -> Result<(), JsError> {
        let bytes = bytecode::retained_size_with_chunks(Vec::new(), vec![chunk.clone()]);
        self.ensure_allocation(bytes)?;
        self.code_bytes = self.code_bytes.saturating_add(bytes);
        Ok(())
    }
    pub fn eval(&mut self, source: &str) -> Result<Value, JsError> {
        self.eval_with_referrer(source, None)
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
        self.eval_with_referrer(source, Some(SourceReferrer::Script(identifier)))
    }

    fn eval_with_referrer(
        &mut self,
        source: &str,
        referrer: Option<SourceReferrer>,
    ) -> Result<Value, JsError> {
        self.check_interrupt()?;
        let chunk = bytecode::compile(source, self.compilation_budget(source)?)?;
        self.retain_compiled_chunk(&chunk)?;
        if self.active == 0 {
            self.remaining = self.limits.max_instructions;
        }
        self.active += 1;
        let result = self
            .with_referrer(referrer, |vm| vm.run_chunk(&chunk, vm.global_env))
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
        if self.active == 0 {
            self.check_interrupt()?;
            self.remaining = self.limits.max_instructions;
        }
        self.active += 1;
        let result = callback(self);
        self.active -= 1;
        result
    }
    /// Allow a worker's owning thread to request cooperative termination.
    pub fn set_interrupt_flag(&mut self, flag: Arc<AtomicBool>) {
        self.interrupt = Some(flag);
    }

    fn check_interrupt(&self) -> Result<(), JsError> {
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
        Value::Object(self.global_object)
    }
    pub(crate) fn global_this(&self) -> Value {
        self.heap
            .environment(self.global_env)
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
        let reference = self.resolve(self.global_env, &key.into())?;
        if matches!(reference, Reference::Unresolvable(_)) {
            Ok(Value::Undefined)
        } else {
            self.read_reference(reference)
        }
    }
    pub fn set_global(&mut self, key: &str, value: Value) -> Result<(), JsError> {
        let reference = self.resolve(self.global_env, &key.into())?;
        self.write_reference(reference, value, false)
    }
    pub fn new_object(&mut self) -> Value {
        Value::Object(self.heap.allocate(Cell::Object(Object::new(
            ObjectKind::Ordinary,
            Some(self.object_prototype),
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
        self.new_array_with_prototype(values, self.array_prototype)
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
        self.try_new_array_with_prototype(values, self.array_prototype)
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
    pub(crate) fn reserve_allocation(&mut self, bytes: usize) -> Result<(), JsError> {
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
        let bytes = string.len().saturating_mul(2).saturating_add(64);
        let old = self
            .string_storage
            .get(&string.storage_id())
            .copied()
            .unwrap_or(0);
        if bytes > old {
            self.ensure_allocation(bytes - old)?;
            self.string_bytes = self.string_bytes.saturating_add(bytes - old);
            self.string_storage.insert(string.storage_id(), bytes);
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
    pub fn ensure_string_allocation(&self, units: usize) -> Result<(), JsError> {
        if units > self.limits.max_string_units {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "String code-unit budget exhausted",
            ));
        }
        // Include both a growing UTF-16 Vec and conversion into Rc storage.
        self.ensure_allocation(units.saturating_mul(6).saturating_add(128))
    }
    pub fn new_native_function<F>(&mut self, name: &str, callback: F) -> Value
    where
        F: Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
    {
        self.native(name, 0, false, Rc::new(callback))
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
        let mut object = Object::new(
            ObjectKind::Native {
                callback,
                construct,
                realm_environment: self.global_env,
            },
            Some(self.function_prototype),
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
                    value: Value::from(name),
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
                    ObjectKind::Function { .. }
                        | ObjectKind::Native { .. }
                        | ObjectKind::Bound { .. }
                        | ObjectKind::PromiseFunction(_)
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
        let mut current = self.heap.object(id)?.prototype;
        for _ in 0..1024 {
            self.consume_work(1)?;
            let Some(id) = current else {
                return Ok(false);
            };
            if id == prototype {
                return Ok(true);
            }
            current = self.heap.object(id)?.prototype;
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
        if self.active > 0 {
            return GcStats {
                before,
                after: before,
                collected: 0,
                skipped_active_execution: true,
            };
        }
        let mut roots = self.heap.trace_roots();
        roots.extend([
            self.global_env.0,
            self.object_prototype,
            self.function_prototype,
            self.array_prototype,
            self.string_prototype,
            self.number_prototype,
            self.boolean_prototype,
            self.error_prototype,
        ]);
        roots.extend(self.intrinsic_eval);
        roots.extend(self.async_function_prototype);
        roots.extend(self.throw_type_error);
        roots.extend(self.promise_constructor);
        roots.extend(self.promise_prototype);
        roots.extend(self.symbol_prototype);
        roots.extend(self.iterator_prototype);
        roots.extend(self.array_iterator_prototype);
        roots.extend(self.string_iterator_prototype);
        roots.extend(self.unhandled_promises.iter().copied());
        for job in &self.jobs {
            job.trace(&mut roots);
        }
        roots.extend(self.error_prototypes.values().copied());
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
        let collected = self.heap.collect(roots);
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
                        ObjectKind::Boxed(v) => value(v, &mut strings),
                        ObjectKind::Promise(data) => data.strings(&mut strings),
                        ObjectKind::PromiseFunction(function) => function.strings(&mut strings),
                        ObjectKind::Iterator(data) => data.strings(&mut strings),
                        ObjectKind::RegExpStringIterator(
                            crate::regexp::StringIterator::Active { input, .. },
                        ) => strings.push(input.clone()),
                        ObjectKind::Collection(data) => data.strings(&mut strings),
                        ObjectKind::WeakCollection(data) => data.strings(&mut strings),
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
        for s in strings {
            let bytes = s.len().saturating_mul(2).saturating_add(64);
            let old = self
                .string_storage
                .get(&s.storage_id())
                .copied()
                .unwrap_or(0);
            if bytes > old {
                self.string_storage.insert(s.storage_id(), bytes);
                self.string_bytes = self.string_bytes.saturating_add(bytes - old);
            }
        }
        self.code_bytes = bytecode::retained_size_with_chunks(codes, chunks);
    }
    pub(crate) fn reserve_binding(&mut self, key: &JsString) -> Result<(), JsError> {
        let bytes = 192usize.saturating_add(key.len().saturating_mul(2));
        self.ensure_allocation(bytes)?;
        self.heap.estimated_bytes = self.heap.estimated_bytes.saturating_add(bytes);
        Ok(())
    }
    pub(crate) fn tick(&mut self) -> Result<(), JsError> {
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
    pub(crate) fn consume_work(&mut self, units: usize) -> Result<(), JsError> {
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
    /// Charge the final Vec-to-Rc copy independently of storage-address reuse.
    pub fn finish_string(&mut self, units: Vec<u16>) -> Result<Value, JsError> {
        self.ensure_string_allocation(units.len())?;
        self.consume_work(units.len())?;
        self.checked_string(JsString::from_utf16(units))
    }
    /// A borrowed slice needs both a Vec copy and the final Rc-storage copy.
    pub(crate) fn copy_string(&mut self, units: &[u16]) -> Result<Value, JsError> {
        self.ensure_string_allocation(units.len())?;
        self.consume_work(units.len().saturating_mul(2))?;
        self.checked_string(JsString::from_utf16(units.to_vec()))
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
            Value::String(_) => Some(self.string_prototype),
            Value::Number(_) => Some(self.number_prototype),
            Value::BigInt(_) => self.bigint_prototype,
            Value::Bool(_) => Some(self.boolean_prototype),
            Value::Symbol(_) => self.symbol_prototype,
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
            let property = object.properties.get(key).cloned();
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
                    PropertyKind::Data { value, .. } => Ok(value),
                    PropertyKind::Accessor {
                        get: Some(getter), ..
                    } => self.invoke(getter, receiver, &[]),
                    PropertyKind::Accessor { get: None, .. } => Ok(Value::Undefined),
                };
            }
        }
        Ok(Value::Undefined)
    }
    pub(crate) fn has(&self, receiver: &Value, key: &JsString) -> Result<bool, JsError> {
        let Some(mut id) = receiver.as_object() else {
            return Err(JsError::type_error(
                "Right-hand side of 'in' is not an object",
            ));
        };
        for _ in 0..1024 {
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
            return Ok(Some(Property::data(self.typed_index_get(id, index)?)));
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
        Ok(o.properties.get(key).cloned())
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
            if matches!(self.heap.object(owner)?.kind, ObjectKind::TypedArray(_))
                && let Some(index) = crate::binary::numeric_index(key)
            {
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
        self.define_property(receiver, key.clone(), Property::data(value))
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
            }
        } else if !self.heap.object(id)?.extensible {
            return Err(JsError::type_error("Object is not extensible"));
        }
        if key == JsString::from("length")
            && matches!(self.heap.object(id)?.kind, ObjectKind::Array { .. })
        {
            let PropertyKind::Data { value, writable } = property.kind else {
                return Err(JsError::type_error("Array length cannot be an accessor"));
            };
            let number = self.to_number(&value)?;
            if !number.is_finite()
                || number < 0.0
                || number.fract() != 0.0
                || number > u32::MAX as f64
            {
                return Err(JsError::new(ErrorKind::RangeError, "Invalid array length"));
            }
            let length = number as u32;
            if length > self.limits.max_array_length {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Array length budget exhausted",
                ));
            }
            let o = self.heap.object_mut(id)?;
            let ObjectKind::Array {
                length: old,
                length_writable,
            } = &mut o.kind
            else {
                unreachable!()
            };
            if !*length_writable && *old != length {
                return Err(JsError::type_error("Array length is not writable"));
            }
            if length < *old {
                let mut doomed: Vec<(u32, JsString)> = o
                    .properties
                    .keys()
                    .filter_map(|k| {
                        array_index(k)
                            .filter(|i| *i >= length)
                            .map(|i| (i, k.clone()))
                    })
                    .collect();
                doomed.sort_by_key(|entry| std::cmp::Reverse(entry.0));
                for (index, key) in doomed {
                    if !o.properties[&key].configurable {
                        *old = index + 1;
                        if !writable {
                            *length_writable = false;
                        }
                        return Err(JsError::type_error(
                            "Cannot truncate non-configurable array element",
                        ));
                    }
                    o.properties.remove(&key);
                }
            }
            *old = length;
            *length_writable = writable;
            return Ok(());
        }
        if let Some(index) = array_index(&key) {
            let o = self.heap.object_mut(id)?;
            if let ObjectKind::Array {
                length,
                length_writable,
            } = &mut o.kind
                && index >= *length
            {
                if !*length_writable {
                    return Err(JsError::type_error("Array length is not writable"));
                }
                if index >= self.limits.max_array_length {
                    return Err(JsError::new(
                        ErrorKind::ExecutionLimit,
                        "Array length budget exhausted",
                    ));
                }
                *length = index + 1;
            }
        }
        self.heap.object_mut(id)?.insert(key, property);
        Ok(())
    }
    pub(crate) fn delete(
        &mut self,
        receiver: Value,
        key: &JsString,
        strict: bool,
    ) -> Result<bool, JsError> {
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
        self.heap.object_mut(id)?.properties.remove(key);
        Ok(true)
    }
    pub(crate) fn set_prototype(
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
        let object = self.heap.object(target)?;
        if object.prototype == prototype {
            return Ok(true);
        }
        if target == self.object_prototype || !object.extensible {
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
                    Value::String(_) => self.string_prototype,
                    Value::Number(_) => self.number_prototype,
                    Value::BigInt(_) => self.bigint_prototype.unwrap(),
                    Value::Symbol(_) => self.symbol_prototype.unwrap(),
                    _ => self.boolean_prototype,
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
    pub(crate) fn resolve(&self, mut env: EnvId, key: &JsString) -> Result<Reference, JsError> {
        loop {
            let e = self.heap.environment(env);
            if e.aliases.contains_key(key) {
                return Ok(Reference::Import(env, key.clone()));
            }
            if e.bindings.contains_key(key) {
                return Ok(Reference::Binding(env, key.clone()));
            }
            let parent = e.parent;
            if let EnvKind::Global { binding_object } = e.kind
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
                let b = &self.heap.environment(env).bindings[&key];
                if !b.initialized {
                    return Err(JsError::new(
                        ErrorKind::ReferenceError,
                        format!("Cannot access '{key}' before initialization"),
                    ));
                }
                Ok(b.value.clone())
            }
            Reference::Import(env, key) => self.read_import(env, &key),
            Reference::Super(base, receiver, key) => {
                if base.is_nullish() {
                    return Err(JsError::type_error("Super property base is null"));
                }
                let key = self.to_property_key(&key)?;
                self.get_from(base, &key, receiver)
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
                let b = self
                    .heap
                    .environment_mut(env)
                    .bindings
                    .get_mut(&key)
                    .unwrap();
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
            Reference::Global(object, key) => {
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
    pub(crate) fn prepare(&mut self, scope: &ScopePlan, env: EnvId) -> Result<(), JsError> {
        for (name, _) in &scope.lexical {
            if self.heap.environment(env).bindings.contains_key(name) {
                return Err(JsError::new(
                    ErrorKind::SyntaxError,
                    format!("Identifier '{name}' has already been declared"),
                ));
            }
            if env == self.global_env
                && self
                    .own_descriptor(self.global_object, name)?
                    .is_some_and(|p| !p.configurable)
            {
                return Err(JsError::new(
                    ErrorKind::SyntaxError,
                    "Global lexical binding conflicts with non-configurable property",
                ));
            }
        }
        for name in &scope.vars {
            self.ensure_allocation(256 + name.len() * 2)?;
            let mut target = env;
            loop {
                let e = self.heap.environment(target);
                if e.kind != EnvKind::Block {
                    break;
                }
                if e.bindings.contains_key(name) {
                    return Err(JsError::new(
                        ErrorKind::SyntaxError,
                        "Eval var conflicts with lexical binding",
                    ));
                }
                target = e.parent.unwrap();
            }
            if let Some(binding) = self.heap.environment(target).bindings.get(name)
                && binding.lexical
            {
                return Err(JsError::new(
                    ErrorKind::SyntaxError,
                    "Var conflicts with lexical binding",
                ));
            }
            if target == self.global_env {
                if self.heap.environment(target).bindings.contains_key(name) {
                    return Err(JsError::new(
                        ErrorKind::SyntaxError,
                        "Var conflicts with global lexical binding",
                    ));
                }
                if self.own_descriptor(self.global_object, name)?.is_none() {
                    self.define_data_property(
                        self.global(),
                        &name.to_string_lossy(),
                        Value::Undefined,
                        true,
                        true,
                        false,
                    )?;
                }
            } else {
                if !self.heap.environment(target).bindings.contains_key(name) {
                    self.reserve_binding(name)?;
                }
                self.heap
                    .environment_mut(target)
                    .bindings
                    .entry(name.clone())
                    .or_insert(Binding {
                        value: Value::Undefined,
                        mutable: true,
                        initialized: true,
                        lexical: false,
                        strict_immutable: true,
                    });
            }
        }
        for (name, mutable) in &scope.lexical {
            self.reserve_binding(name)?;
            self.heap.environment_mut(env).bindings.insert(
                name.clone(),
                Binding {
                    value: Value::Undefined,
                    mutable: *mutable,
                    initialized: false,
                    lexical: true,
                    strict_immutable: true,
                },
            );
        }
        for (name, code, is_var) in &scope.functions {
            let function = self.make_function(code.clone(), env)?;
            if *is_var {
                let reference = self.resolve(env, name)?;
                self.write_reference(reference, function, false)?;
            } else {
                let binding = self
                    .heap
                    .environment_mut(env)
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
            let binding = self
                .heap
                .environment_mut(env)
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
        let generator_prototype = match code.kind {
            FunctionKind::Generator => self.generator_prototype,
            FunctionKind::AsyncGenerator => self.async_generator_prototype,
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
                referrer: self.active_referrer.clone(),
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
        if let ObjectKind::PromiseFunction(function) = &self.heap.object(id)?.kind {
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
            self.with_referrer(referrer, |vm| {
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
                let body_env = vm.function_environment(&code, parent, invocation)?;
                vm.run_chunk(&code.body, body_env)
                    .and_then(|completion| match completion {
                        Completion::Return(v) => Ok(v),
                        Completion::Normal(_) => Ok(Value::Undefined),
                        _ => Err(JsError::new(
                            ErrorKind::InternalError,
                            "Unexpected function abrupt completion",
                        )),
                    })
            })
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
        Ok(self
            .get_property(new_target, "prototype")?
            .as_object()
            .unwrap_or(fallback))
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
            self.prototype_from_constructor(new_target.clone(), self.object_prototype)?;
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
        let result = self.with_referrer(referrer, |vm| {
            let env = vm.function_environment(
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
            vm.run_chunk(&code.body, env).and_then(|c| match c {
                Completion::Return(v) => Ok(v),
                Completion::Normal(_) => Ok(Value::Undefined),
                _ => Err(JsError::new(
                    ErrorKind::InternalError,
                    "Invalid constructor completion",
                )),
            })
        });
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
    pub(crate) fn direct_eval(
        &mut self,
        value: Value,
        env: EnvId,
        strict: bool,
    ) -> Result<Value, JsError> {
        let Value::String(source) = value else {
            return Ok(value);
        };
        self.consume_work(source.len().saturating_mul(2))?;
        let source = String::from_utf16(source.as_utf16()).map_err(|_| {
            JsError::new(
                ErrorKind::UnsupportedFeature,
                "Eval source containing isolated surrogate code units is not implemented",
            )
        })?;
        let chunk = bytecode::compile_eval(
            &source,
            strict,
            self.eval_context(env)?,
            self.compilation_budget(&source)?,
        )?;
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
            EnvKind::Block
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
            FunctionKind::Ordinary => self.function_prototype,
            FunctionKind::Async => self.async_function_prototype.unwrap(),
            FunctionKind::Generator => self.generator_function_prototype.unwrap(),
            FunctionKind::AsyncGenerator => self.async_generator_function_prototype.unwrap(),
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
        let function = self.make_function(code, self.global_env)?;
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

    pub(crate) fn run_chunk(&mut self, chunk: &Chunk, env: EnvId) -> Result<Completion, JsError> {
        if !permits_bytecode_depth(self.execution_depth + 1) {
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
        if !permits_bytecode_depth(self.execution_depth + 1) {
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
            | Op::DynamicImport) => self.run_call_op(op, stack, env, strict)?,
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
                    Item::Reference(Reference::Global(object, key)) => {
                        self.delete(Value::Object(object), &key, strict)?
                    }
                    Item::Reference(Reference::Binding(_, _) | Reference::Import(_, _)) => false,
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
            Op::DynamicImport => {
                let value = self.pop_value(stack)?;
                let promise = self.dynamic_import(value)?;
                stack.push(Item::Value(promise));
            }
            Op::PrepareCall(allow_direct_eval) => {
                let callee = stack.pop().unwrap();
                let direct_eval = *allow_direct_eval
                    && match &callee {
                        Item::Reference(
                            Reference::Binding(_, name)
                            | Reference::Global(_, name)
                            | Reference::Unresolvable(name),
                        ) => name == &JsString::from("eval"),
                        _ => false,
                    };
                let this = match &callee {
                    Item::Reference(
                        Reference::Property(object, _) | Reference::Private(object, _),
                    ) => object.clone(),
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
                let value = if direct_eval && function.as_object() == self.intrinsic_eval {
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
                if let ObjectKind::Error(error_kind) = object.kind {
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
                let scope =
                    self.heap
                        .allocate_env(Environment::new(Some(env), EnvKind::Block, None));
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
            Op::Branch(test, yes, no) => {
                let selected = if self.expression_value(test, env)?.is_truthy() {
                    Some(yes)
                } else {
                    no.as_ref()
                };
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
                            EnvKind::Block,
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
            .error_prototypes
            .get(&error.kind)
            .copied()
            .unwrap_or(self.error_prototype);
        let value = Value::Object(self.heap.allocate(Cell::Object(Object::new(
            ObjectKind::Error(error.kind),
            Some(prototype),
        ))));
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
            self.ensure_string_allocation(a.len().saturating_add(b.len()))?;
            self.consume_work(a.len().saturating_add(b.len()).saturating_mul(2))?;
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
            "**" => a.powf(b),
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
        if a.is_nullish() && b.is_nullish() {
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
    let s = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
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
    if matches!(s, "inf" | "+inf" | "-inf") {
        f64::NAN
    } else {
        s.parse().unwrap_or(f64::NAN)
    }
}
