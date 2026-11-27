//! Native promise records and a single traced FIFO queue for ECMAScript jobs and
//! browser queueMicrotask callbacks. No JavaScript references hide in Rust closures.
use crate::{
    ErrorKind, JsError, JsString, ObjectId, PropertyKey, Value, Vm,
    async_iterator::Continuation as AsyncIteratorContinuation,
    builtins::{method, replace_construct},
    execution::AsyncExecution,
    generator::GeneratorAwait,
    heap::{Cell, Object, ObjectKind, TraceEdges},
    modules::ModuleId,
};
use std::{cell::Cell as Flag, rc::Rc};

#[derive(Clone, Debug)]
pub struct PromiseCapability {
    pub promise: Value,
    pub resolve: Value,
    pub reject: Value,
}
#[derive(Clone)]
pub(crate) enum PromiseState {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}
pub(crate) struct PromiseData {
    pub state: PromiseState,
    pub reactions: Vec<ReactionPair>,
    pub handled: bool,
}
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum AggregateKind {
    All,
    AllSettled,
    AllKeyed,
    AllSettledKeyed,
    Any,
}
pub(crate) struct AggregateData {
    pub values: Vec<Value>,
    pub keys: Vec<PropertyKey>,
    kind: AggregateKind,
    remaining: usize,
    capability: PromiseCapability,
}
impl AggregateData {
    pub(crate) fn trace(&self, out: &mut TraceEdges) {
        self.capability.visit(|v| out.value(v));
        for key in &self.keys {
            if let PropertyKey::Symbol(symbol) = key {
                out.symbol(symbol);
            }
        }
        for value in &self.values {
            out.value(value);
        }
    }
    pub(crate) fn strings(&self, out: &mut Vec<JsString>) {
        self.capability.visit(|v| string(v, out));
        for key in &self.keys {
            if let PropertyKey::String(name) = key {
                out.push(name.clone());
            }
        }
        for value in &self.values {
            string(value, out);
        }
    }
}
pub(crate) enum ReactionPair {
    Then {
        capability: PromiseCapability,
        fulfilled: Option<Value>,
        rejected: Option<Value>,
    },
    Resume(Box<AsyncExecution>),
    AsyncFromSync(AsyncIteratorContinuation),
    AsyncGenerator {
        generator: ObjectId,
        waiting: GeneratorAwait,
    },
    ModuleEvaluation(ModuleId),
    DynamicImport {
        module: ModuleId,
        capability: PromiseCapability,
    },
}
#[derive(Clone)]
pub(crate) enum PromiseFunction {
    Resolving {
        promise: ObjectId,
        reject: bool,
        already: Rc<Flag<bool>>,
    },
    CapabilityExecutor {
        resolve: Value,
        reject: Value,
    },
    Finally {
        callback: Value,
        constructor: Value,
        rejected: bool,
    },
    Thunk {
        value: Value,
        rejected: bool,
    },
    AggregateElement {
        state: ObjectId,
        index: usize,
        rejected: bool,
        already: Rc<Flag<bool>>,
    },
}
pub(crate) enum Job {
    CleanupRegistry(ObjectId),
    Microtask(Value),
    Call {
        callback: Value,
        this: Value,
        args: Vec<Value>,
    },
    JsonParse {
        resolve: Value,
        reject: Value,
        source: JsString,
    },
    Reaction {
        capability: PromiseCapability,
        handler: Option<Value>,
        argument: Value,
        rejected: bool,
    },
    Resume {
        execution: Box<AsyncExecution>,
        argument: Value,
        rejected: bool,
    },
    AsyncFromSync {
        continuation: AsyncIteratorContinuation,
        argument: Value,
        rejected: bool,
    },
    AsyncGenerator {
        generator: ObjectId,
        waiting: GeneratorAwait,
        argument: Value,
        rejected: bool,
    },
    ModuleEvaluation {
        module: ModuleId,
        argument: Value,
        rejected: bool,
    },
    DynamicImport {
        module: ModuleId,
        capability: PromiseCapability,
        argument: Value,
        rejected: bool,
    },
    Thenable {
        promise: ObjectId,
        thenable: Value,
        then: Value,
    },
}
fn string(value: &Value, out: &mut Vec<JsString>) {
    if let Value::String(s) = value {
        out.push(s.clone());
    }
}
impl PromiseCapability {
    pub(crate) fn visit(&self, mut f: impl FnMut(&Value)) {
        f(&self.promise);
        f(&self.resolve);
        f(&self.reject);
    }
}
impl ReactionPair {
    fn visit(&self, mut f: impl FnMut(&Value)) {
        match self {
            Self::Then {
                capability,
                fulfilled,
                rejected,
            } => {
                capability.visit(&mut f);
                for value in fulfilled.iter().chain(rejected.iter()) {
                    f(value);
                }
            }
            Self::Resume(execution) => execution.visit(f),
            Self::AsyncFromSync(continuation) => continuation.visit(f),
            Self::AsyncGenerator { generator, waiting } => {
                f(&Value::Object(*generator));
                waiting.visit(f);
            }
            Self::ModuleEvaluation(module) => f(&Value::Object(module.0)),
            Self::DynamicImport { module, capability } => {
                f(&Value::Object(module.0));
                capability.visit(f);
            }
        }
    }

    fn into_job(self, argument: Value, rejected: bool) -> Job {
        match self {
            Self::Then {
                capability,
                fulfilled,
                rejected: on_rejected,
            } => Job::Reaction {
                capability,
                handler: if rejected { on_rejected } else { fulfilled },
                argument,
                rejected,
            },
            Self::Resume(execution) => Job::Resume {
                execution,
                argument,
                rejected,
            },
            Self::AsyncFromSync(continuation) => Job::AsyncFromSync {
                continuation,
                argument,
                rejected,
            },
            Self::AsyncGenerator { generator, waiting } => Job::AsyncGenerator {
                generator,
                waiting,
                argument,
                rejected,
            },
            Self::ModuleEvaluation(module) => Job::ModuleEvaluation {
                module,
                argument,
                rejected,
            },
            Self::DynamicImport { module, capability } => Job::DynamicImport {
                module,
                capability,
                argument,
                rejected,
            },
        }
    }
}
impl PromiseData {
    pub(crate) fn extra_bytes(&self) -> usize {
        self.reactions
            .iter()
            .map(|reaction| match reaction {
                ReactionPair::Resume(execution) => execution.extra_bytes(),
                ReactionPair::AsyncGenerator { waiting, .. } => waiting.extra_bytes(),
                _ => 0,
            })
            .sum()
    }
    fn visit(&self, mut f: impl FnMut(&Value)) {
        match &self.state {
            PromiseState::Fulfilled(v) | PromiseState::Rejected(v) => f(v),
            _ => {}
        }
        for reaction in &self.reactions {
            reaction.visit(&mut f);
        }
    }
    pub(crate) fn trace(&self, out: &mut TraceEdges) {
        self.visit(|v| out.value(v));
    }
    pub(crate) fn strings(&self, out: &mut Vec<JsString>) {
        self.visit(|v| string(v, out));
    }
}
impl PromiseFunction {
    fn visit(&self, mut f: impl FnMut(&Value)) {
        match self {
            Self::Resolving { promise, .. } => f(&Value::Object(*promise)),
            Self::CapabilityExecutor { resolve, reject } => {
                f(resolve);
                f(reject);
            }
            Self::Finally {
                callback,
                constructor,
                ..
            } => {
                f(callback);
                f(constructor);
            }
            Self::Thunk { value, .. } => f(value),
            Self::AggregateElement { state, .. } => f(&Value::Object(*state)),
        }
    }
    pub(crate) fn trace(&self, out: &mut TraceEdges) {
        self.visit(|v| out.value(v));
    }
    pub(crate) fn strings(&self, out: &mut Vec<JsString>) {
        self.visit(|v| string(v, out));
    }
}
impl Job {
    pub(crate) fn extra_bytes(&self) -> usize {
        match self {
            Self::Call { args, .. } => args.capacity() * std::mem::size_of::<Value>(),
            Self::Resume { execution, .. } => execution.extra_bytes(),
            Self::AsyncGenerator { waiting, .. } => waiting.extra_bytes(),
            _ => 0,
        }
    }
    fn visit(&self, mut f: impl FnMut(&Value)) {
        match self {
            Self::CleanupRegistry(id) => f(&Value::Object(*id)),
            Self::Microtask(callback) => f(callback),
            Self::Call {
                callback,
                this,
                args,
            } => {
                f(callback);
                f(this);
                for arg in args {
                    f(arg);
                }
            }
            Self::JsonParse {
                resolve,
                reject,
                source,
            } => {
                f(resolve);
                f(reject);
                f(&Value::String(source.clone()));
            }
            Self::Reaction {
                capability,
                handler,
                argument,
                ..
            } => {
                capability.visit(&mut f);
                if let Some(handler) = handler {
                    f(handler);
                }
                f(argument);
            }
            Self::Resume {
                execution,
                argument,
                ..
            } => {
                execution.visit(&mut f);
                f(argument);
            }
            Self::AsyncFromSync {
                continuation,
                argument,
                ..
            } => {
                continuation.visit(&mut f);
                f(argument);
            }
            Self::AsyncGenerator {
                generator,
                waiting,
                argument,
                ..
            } => {
                f(&Value::Object(*generator));
                waiting.visit(&mut f);
                f(argument);
            }
            Self::ModuleEvaluation {
                module, argument, ..
            } => {
                f(&Value::Object(module.0));
                f(argument);
            }
            Self::DynamicImport {
                module,
                capability,
                argument,
                ..
            } => {
                f(&Value::Object(module.0));
                capability.visit(&mut f);
                f(argument);
            }
            Self::Thenable {
                promise,
                thenable,
                then,
            } => {
                f(&Value::Object(*promise));
                f(thenable);
                f(then);
            }
        }
    }
    pub(crate) fn trace(&self, out: &mut TraceEdges) {
        self.visit(|v| out.value(v));
    }
    pub(crate) fn strings(&self, out: &mut Vec<JsString>) {
        self.visit(|v| string(v, out));
    }
}
fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}

impl Vm {
    /// The caller must root values retained beyond the current embedding operation.
    pub fn new_promise_capability(&mut self) -> Result<PromiseCapability, JsError> {
        self.with_task(|vm| vm.intrinsic_promise_capability())
    }
    pub fn is_promise(&self, value: &Value) -> bool {
        value
            .as_object()
            .and_then(|id| self.heap.object(id).ok())
            .is_some_and(|o| matches!(o.kind, ObjectKind::Promise(_)))
    }

    pub fn is_promise_pending(&self, value: &Value) -> bool {
        matches!(self.promise_state(value), Some(PromiseState::Pending))
    }
    pub fn is_promise_fulfilled(&self, value: &Value) -> bool {
        matches!(self.promise_state(value), Some(PromiseState::Fulfilled(_)))
    }
    fn promise_state(&self, value: &Value) -> Option<&PromiseState> {
        let ObjectKind::Promise(data) = &self.heap.object(value.as_object()?).ok()?.kind else {
            return None;
        };
        Some(&data.state)
    }
    /// Attach a host reaction using the intrinsic Promise constructor and methods.
    pub fn then_intrinsic(
        &mut self,
        promise: Value,
        fulfilled: Value,
        rejected: Value,
    ) -> Result<Value, JsError> {
        self.with_task(|vm| {
            let id = vm.promise_id(&promise)?;
            let capability = vm.intrinsic_promise_capability()?;
            vm.attach_then(id, capability, fulfilled, rejected)
        })
    }
    /// Directly fulfill an internal promise with a host record as its exact value.
    pub fn fulfill_promise(&mut self, promise: &Value, result: Value) -> Result<(), JsError> {
        self.with_task(|vm| vm.settle_promise(vm.promise_id(promise)?, result, false))
    }
    pub fn mark_promise_handled(&mut self, promise: &Value) -> Result<(), JsError> {
        self.with_task(|vm| {
            let id = vm.promise_id(promise)?;
            vm.consume_work(vm.unhandled_promises.len())?;
            vm.set_promise_handled(id)
        })
    }
    fn promise_id(&self, promise: &Value) -> Result<ObjectId, JsError> {
        promise
            .as_object()
            .filter(|_| self.is_promise(promise))
            .ok_or_else(|| JsError::type_error("Promise receiver is not a promise"))
    }
    pub fn has_pending_jobs(&self) -> bool {
        !self.jobs.is_empty()
            || self.next_atomic_wait_deadline().is_some_and(|time| std::time::Instant::now() >= time)
    }
    pub fn pending_job_count(&self) -> usize {
        self.jobs.len()
    }
    /// Report rejections still unhandled after a host checkpoint. Returned values
    /// follow the ordinary external-rooting contract. No user code is invoked.
    pub fn take_unhandled_rejections(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.unhandled_promises)
            .into_iter()
            .filter_map(|id| self.heap.object(id).ok())
            .filter_map(|object| {
                if let ObjectKind::Promise(PromiseData {
                    state: PromiseState::Rejected(reason),
                    handled: false,
                    ..
                }) = &object.kind
                {
                    Some(reason.clone())
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn clear_jobs(&mut self) -> usize {
        let jobs = std::mem::take(&mut self.jobs);
        let count = jobs.len();
        let discarded = JsError::new(ErrorKind::ExecutionLimit, "Module evaluation job discarded");
        for job in jobs {
            let owner = match job {
                Job::Resume { execution, .. } => execution.owner(),
                Job::ModuleEvaluation { module, .. } => module.0,
                _ => continue,
            };
            self.record_async_execution_limit(owner, &discarded);
        }
        count
    }
    pub fn enqueue_microtask(&mut self, callback: Value) -> Result<(), JsError> {
        if !self.is_callable(&callback) {
            return Err(JsError::type_error("Microtask callback is not callable"));
        }
        self.enqueue_job(Job::Microtask(callback))
    }
    pub fn enqueue_call(
        &mut self,
        callback: Value,
        this: Value,
        args: &[Value],
    ) -> Result<(), JsError> {
        if !self.is_callable(&callback) {
            return Err(JsError::type_error("Queued callback is not callable"));
        }
        if args.len() > self.limits.max_array_length as usize {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Queued argument count budget exhausted",
            ));
        }
        self.reserve_allocation(args.len().saturating_mul(std::mem::size_of::<Value>()))?;
        for arg in args {
            self.account_value(arg.clone())?;
        }
        self.account_value(this.clone())?;
        self.enqueue_job(Job::Call {
            callback,
            this,
            args: args.to_vec(),
        })
    }
    pub fn enqueue_json_parse(
        &mut self,
        resolve: Value,
        reject: Value,
        source: JsString,
    ) -> Result<(), JsError> {
        if !self.is_callable(&resolve) || !self.is_callable(&reject) {
            return Err(JsError::type_error(
                "JSON job resolving functions are not callable",
            ));
        }
        self.account_value(Value::String(source.clone()))?;
        self.enqueue_job(Job::JsonParse {
            resolve,
            reject,
            source,
        })
    }
    /// Consume a throwing microtask and leave subsequent FIFO jobs queued. Promise
    /// handler exceptions are converted to chained rejections inside reaction jobs.
    pub fn drain_jobs(&mut self, max_jobs: usize) -> Result<usize, JsError> {
        self.drain_jobs_with_collection(max_jobs, false)
    }

    /// Use heap safepoints inside interpreted promise handlers and queued
    /// callbacks when the embedder has rooted values retained across this drain.
    pub fn drain_jobs_at_callback_boundary(&mut self, max_jobs: usize) -> Result<usize, JsError> {
        self.drain_jobs_with_collection(max_jobs, true)
    }

    fn drain_jobs_with_collection(&mut self, max_jobs: usize, collectible: bool) -> Result<usize, JsError> {
        self.with_task(|vm| {
            let mut count = 0;
            while count < max_jobs {
                vm.tick()?;
                vm.poll_atomic_timeouts()?;
                let Some(job) = vm.jobs.pop_front() else {
                    break;
                };
                count += 1;
                vm.run_job(job, collectible)?;
            }
            Ok(count)
        })
    }
    pub(crate) fn schedule_registry_cleanup(&mut self, registries: Vec<ObjectId>) {
        if registries.is_empty() {
            return;
        }
        let already_queued: std::collections::BTreeSet<_> = self
            .jobs
            .iter()
            .filter_map(|job| {
                if let Job::CleanupRegistry(id) = job {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();
        for id in registries {
            if !already_queued.contains(&id) && self.enqueue_job(Job::CleanupRegistry(id)).is_err()
            {
                // The registration stays pending. A later collection retries it.
                break;
            }
        }
    }
    fn enqueue_job(&mut self, job: Job) -> Result<(), JsError> {
        if self.jobs.len() >= self.limits.max_pending_jobs {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Pending job queue budget exhausted",
            ));
        }
        self.reserve_allocation(512)?;
        self.jobs.push_back(job);
        Ok(())
    }
    fn promise_function(&mut self, kind: PromiseFunction, length: usize) -> Result<Value, JsError> {
        self.ensure_allocation(1024)?;
        let function = Value::Object(self.heap.allocate(Cell::Object(Object::new(
            ObjectKind::PromiseFunction(kind, self.realm.global_env),
            Some(self.realm.function_prototype),
        ))));
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
            Value::from(""),
            false,
            false,
            true,
        )?;
        Ok(function)
    }
    fn resolving_functions(&mut self, promise: ObjectId) -> Result<(Value, Value), JsError> {
        let already = Rc::new(Flag::new(false));
        let resolve = self.promise_function(
            PromiseFunction::Resolving {
                promise,
                reject: false,
                already: already.clone(),
            },
            1,
        )?;
        let reject = self.promise_function(
            PromiseFunction::Resolving {
                promise,
                reject: true,
                already,
            },
            1,
        )?;
        Ok((resolve, reject))
    }
    fn intrinsic_promise_capability(&mut self) -> Result<PromiseCapability, JsError> {
        self.promise_capability_with_prototype(self.realm.promise_prototype)
    }
    pub(crate) fn begin_promise_constructor(
        &mut self,
        executor: &Value,
        new_target: Value,
    ) -> Result<PromiseCapability, JsError> {
        if !self.is_callable(executor) {
            return Err(JsError::type_error("Promise executor is not callable"));
        }
        let fallback = self.realm.promise_prototype.expect("installed Promise prototype");
        let prototype = self.prototype_from_constructor(new_target, fallback)?;
        self.promise_capability_with_prototype(Some(prototype))
    }

    pub(crate) fn complete_promise_constructor(
        &mut self,
        capability: PromiseCapability,
        result: Result<Value, JsError>,
    ) -> Result<Value, JsError> {
        if let Err(error) = result {
            let reason = self.error_reason(error)?;
            self.invoke(capability.reject, Value::Undefined, &[reason])?;
        }
        Ok(capability.promise)
    }

    fn promise_capability_with_prototype(
        &mut self,
        prototype: Option<ObjectId>,
    ) -> Result<PromiseCapability, JsError> {
        self.ensure_allocation(3072)?;
        if self.heap.live.saturating_add(3) > self.limits.max_heap_objects {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Promise heap cell budget exhausted",
            ));
        }
        let promise = self.heap.allocate(Cell::Object(Object::new(
            ObjectKind::Promise(PromiseData {
                state: PromiseState::Pending,
                reactions: Vec::new(),
                handled: false,
            }),
            prototype,
        )));
        let (resolve, reject) = self.resolving_functions(promise)?;
        Ok(PromiseCapability {
            promise: Value::Object(promise),
            resolve,
            reject,
        })
    }
    fn capability_for_constructor(
        &mut self,
        constructor: Value,
    ) -> Result<PromiseCapability, JsError> {
        if constructor.as_object() == self.realm.promise_constructor {
            return self.intrinsic_promise_capability();
        }
        let executor = self.promise_function(
            PromiseFunction::CapabilityExecutor {
                resolve: Value::Undefined,
                reject: Value::Undefined,
            },
            2,
        )?;
        let promise = self.construct(constructor, std::slice::from_ref(&executor))?;
        let ObjectKind::PromiseFunction(PromiseFunction::CapabilityExecutor { resolve, reject }, _) =
            &self.heap.object(executor.as_object().unwrap())?.kind
        else {
            unreachable!()
        };
        let (resolve, reject) = (resolve.clone(), reject.clone());
        if !self.is_callable(&resolve) || !self.is_callable(&reject) {
            return Err(JsError::type_error(
                "Promise capability resolve/reject must be callable",
            ));
        }
        Ok(PromiseCapability {
            promise,
            resolve,
            reject,
        })
    }
    fn promise_species(&mut self, promise: Value) -> Result<Value, JsError> {
        let constructor = self.get_property(promise, "constructor")?;
        if !constructor.is_undefined() && constructor.as_object().is_none() {
            return Err(JsError::type_error(
                "Promise constructor property must be an object",
            ));
        }
        if constructor.is_undefined() {
            return Ok(Value::Object(self.realm.promise_constructor.unwrap()));
        }
        let species = self.get_property_key(constructor, self.well_known_key("species"))?;
        if species.is_nullish() {
            return Ok(Value::Object(self.realm.promise_constructor.unwrap()));
        }
        if !self.is_constructor(&species) {
            return Err(JsError::type_error("Promise species is not a constructor"));
        }
        Ok(species)
    }
    pub(crate) fn error_reason(&mut self, error: JsError) -> Result<Value, JsError> {
        if error.uncatchable() {
            return Err(error);
        }
        if let Some(value) = error.thrown {
            Ok(value)
        } else {
            self.error_value(&error)
        }
    }
    fn rejected_error(&mut self, promise: ObjectId, error: JsError) -> Result<(), JsError> {
        let reason = self.error_reason(error)?;
        self.settle_promise(promise, reason, true)
    }
    pub(crate) fn settle_promise(
        &mut self,
        promise: ObjectId,
        value: Value,
        rejected: bool,
    ) -> Result<(), JsError> {
        let value = self.account_value(value)?;
        let object = self.heap.object(promise)?;
        let ObjectKind::Promise(data) = &object.kind else {
            return Err(JsError::type_error("Promise record is missing"));
        };
        if !matches!(data.state, PromiseState::Pending) {
            return Ok(());
        }
        if self.jobs.len().saturating_add(data.reactions.len()) > self.limits.max_pending_jobs {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Promise reaction queue budget exhausted",
            ));
        }
        let reactions = data.reactions.len();
        let unhandled = rejected && !data.handled;
        if unhandled && self.unhandled_promises.len() >= self.limits.max_pending_jobs {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Unhandled rejection record budget exhausted",
            ));
        }
        self.ensure_allocation(reactions.saturating_mul(512))?;
        if unhandled {
            self.reserve_allocation(32)?;
        }
        let ObjectKind::Promise(data) = &mut self.heap.object_mut(promise)?.kind else {
            unreachable!()
        };
        data.state = if rejected {
            PromiseState::Rejected(value.clone())
        } else {
            PromiseState::Fulfilled(value.clone())
        };
        let reactions = std::mem::take(&mut data.reactions);
        if unhandled {
            self.unhandled_promises.push(promise);
        }
        for reaction in reactions {
            self.enqueue_job(reaction.into_job(value.clone(), rejected))?;
        }
        Ok(())
    }
    pub(crate) fn resolve_promise(
        &mut self,
        promise: ObjectId,
        value: Value,
    ) -> Result<(), JsError> {
        if value == Value::Object(promise) {
            return self.rejected_error(
                promise,
                JsError::type_error("A promise cannot resolve to itself"),
            );
        }
        if value.as_object().is_none() {
            return self.settle_promise(promise, value, false);
        }
        let then = match self.get_property(value.clone(), "then") {
            Ok(then) => then,
            Err(error) => return self.rejected_error(promise, error),
        };
        if !self.is_callable(&then) {
            self.settle_promise(promise, value, false)
        } else {
            self.enqueue_job(Job::Thenable {
                promise,
                thenable: value,
                then,
            })
        }
    }
    fn promise_resolve(&mut self, constructor: Value, value: Value) -> Result<Value, JsError> {
        if constructor.as_object().is_none() {
            return Err(JsError::type_error(
                "Promise constructor receiver must be an object",
            ));
        }
        if self.is_promise(&value)
            && self.get_property(value.clone(), "constructor")? == constructor
        {
            return Ok(value);
        }
        let capability = self.capability_for_constructor(constructor)?;
        self.invoke(capability.resolve, Value::Undefined, &[value])?;
        Ok(capability.promise)
    }
    fn perform_then(
        &mut self,
        promise: Value,
        fulfilled: Value,
        rejected: Value,
    ) -> Result<Value, JsError> {
        let id = self.promise_id(&promise)?;
        let constructor = self.promise_species(promise)?;
        let capability = self.capability_for_constructor(constructor)?;
        self.attach_then(id, capability, fulfilled, rejected)
    }

    fn attach_then(
        &mut self,
        id: ObjectId,
        capability: PromiseCapability,
        fulfilled: Value,
        rejected: Value,
    ) -> Result<Value, JsError> {
        let fulfilled = if self.is_callable(&fulfilled) {
            Some(fulfilled)
        } else {
            None
        };
        let rejected = if self.is_callable(&rejected) {
            Some(rejected)
        } else {
            None
        };
        self.register_reaction(
            id,
            ReactionPair::Then {
                capability: capability.clone(),
                fulfilled,
                rejected,
            },
        )?;
        Ok(capability.promise)
    }

    pub(crate) fn promise_for_await(&mut self, value: Value) -> Result<ObjectId, JsError> {
        let constructor = Value::Object(self.realm.promise_constructor.unwrap());
        let promise = self.promise_resolve(constructor, value)?;
        Ok(promise.as_object().unwrap())
    }

    pub(crate) fn register_reaction(
        &mut self,
        id: ObjectId,
        reaction: ReactionPair,
    ) -> Result<(), JsError> {
        let ObjectKind::Promise(data) = &self.heap.object(id)?.kind else {
            return Err(JsError::type_error("Promise record is missing"));
        };
        let state = data.state.clone();
        self.consume_work(self.unhandled_promises.len())?;
        match state {
            PromiseState::Pending => {
                let ObjectKind::Promise(data) = &self.heap.object(id)?.kind else {
                    unreachable!()
                };
                if data.reactions.len() >= self.limits.max_pending_jobs {
                    return Err(JsError::new(
                        ErrorKind::ExecutionLimit,
                        "Pending promise reaction budget exhausted",
                    ));
                }
                self.reserve_allocation(512)?;
                let ObjectKind::Promise(data) = &mut self.heap.object_mut(id)?.kind else {
                    unreachable!()
                };
                data.reactions.push(reaction);
            }
            PromiseState::Fulfilled(argument) => {
                self.enqueue_job(reaction.into_job(argument, false))?
            }
            PromiseState::Rejected(argument) => {
                self.enqueue_job(reaction.into_job(argument, true))?
            }
        }
        self.set_promise_handled(id)
    }

    fn set_promise_handled(&mut self, id: ObjectId) -> Result<(), JsError> {
        if let ObjectKind::Promise(data) = &mut self.heap.object_mut(id)?.kind {
            data.handled = true;
        }
        self.unhandled_promises.retain(|promise| *promise != id);
        Ok(())
    }

    fn run_job(&mut self, job: Job, collectible: bool) -> Result<(), JsError> {
        match job {
            Job::CleanupRegistry(id) => self.cleanup_registry(id)?,
            Job::Microtask(callback) => {
                if collectible {
                    self.invoke_collectible(callback, Value::Undefined, &[], &[])?;
                } else {
                    self.invoke(callback, Value::Undefined, &[])?;
                }
            }
            Job::Call {
                callback,
                this,
                args,
            } => {
                if collectible {
                    self.invoke_collectible(callback, this, &args, &[])?;
                } else {
                    self.invoke(callback, this, &args)?;
                }
            }
            Job::JsonParse {
                resolve,
                reject,
                source,
            } => match self.parse_json(&source) {
                Ok(value) => {
                    self.invoke(resolve, Value::Undefined, &[value])?;
                }
                Err(error) => {
                    let reason = self.error_reason(error)?;
                    self.invoke(reject, Value::Undefined, &[reason])?;
                }
            },
            Job::Reaction {
                capability,
                handler,
                argument,
                rejected,
            } => {
                let result = if let Some(handler) = handler {
                    if collectible {
                        self.invoke_collectible(
                            handler,
                            Value::Undefined,
                            &[argument],
                            &[&capability.promise, &capability.resolve, &capability.reject],
                        )
                    } else {
                        self.invoke(handler, Value::Undefined, &[argument])
                    }
                } else if rejected {
                    self.invoke(capability.reject, Value::Undefined, &[argument])?;
                    return Ok(());
                } else {
                    Ok(argument)
                };
                match result {
                    Ok(value) => {
                        self.invoke(capability.resolve, Value::Undefined, &[value])?;
                    }
                    Err(error) => {
                        let reason = self.error_reason(error)?;
                        self.invoke(capability.reject, Value::Undefined, &[reason])?;
                    }
                }
            }
            Job::Resume {
                execution,
                argument,
                rejected,
            } => {
                self.resume_async(execution, argument, rejected)?;
            }
            Job::AsyncFromSync {
                continuation,
                argument,
                rejected,
            } => continuation.settle(self, argument, rejected)?,
            Job::AsyncGenerator {
                generator,
                waiting,
                argument,
                rejected,
            } => {
                self.resume_async_generator(generator, waiting, argument, rejected)?;
            }
            Job::ModuleEvaluation {
                module,
                argument,
                rejected,
            } => {
                self.with_function_realm(module.0, |vm| vm.finish_async_module(module, argument, rejected))?;
            }
            Job::DynamicImport {
                module,
                capability,
                argument,
                rejected,
            } => {
                self.with_function_realm(module.0, |vm| {
                    if rejected {
                        vm.invoke(capability.reject, Value::Undefined, &[argument])?;
                    } else {
                        let namespace = vm.module_namespace(module)?;
                        vm.invoke(capability.resolve, Value::Undefined, &[namespace])?;
                    }
                    Ok(())
                })?;
            }
            Job::Thenable {
                promise,
                thenable,
                then,
            } => {
                let (resolve, reject) = self.resolving_functions(promise)?;
                if let Err(error) = self.invoke(then, thenable, &[resolve, reject.clone()]) {
                    let reason = self.error_reason(error)?;
                    self.invoke(reject, Value::Undefined, &[reason])?;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn invoke_promise_function(
        &mut self,
        id: ObjectId,
        function: PromiseFunction,
        _this: Value,
        args: &[Value],
    ) -> Result<Value, JsError> {
        match function {
            PromiseFunction::AggregateElement {
                state,
                index,
                rejected,
                already,
            } => {
                if already.replace(true) {
                    return Ok(Value::Undefined);
                }
                let kind = match &self.heap.object(state)?.kind {
                    ObjectKind::PromiseAggregate(data) => data.kind,
                    _ => unreachable!(),
                };
                let value = if matches!(
                    kind,
                    AggregateKind::AllSettled | AggregateKind::AllSettledKeyed
                ) {
                    self.ensure_allocation(1024)?;
                    let result = self.new_object();
                    self.define_data_property(
                        result.clone(),
                        "status",
                        Value::from(if rejected { "rejected" } else { "fulfilled" }),
                        true,
                        true,
                        true,
                    )?;
                    self.define_data_property(
                        result.clone(),
                        if rejected { "reason" } else { "value" },
                        arg(args, 0),
                        true,
                        true,
                        true,
                    )?;
                    result
                } else {
                    arg(args, 0)
                };
                self.account_value(value.clone())?;
                let ObjectKind::PromiseAggregate(data) = &mut self.heap.object_mut(state)?.kind
                else {
                    unreachable!()
                };
                data.values[index] = value;
                data.remaining -= 1;
                if data.remaining == 0 {
                    return self.finish_aggregate(state);
                }
                Ok(Value::Undefined)
            }
            PromiseFunction::Resolving {
                promise,
                reject,
                already,
            } => {
                if already.replace(true) {
                    return Ok(Value::Undefined);
                }
                if reject {
                    self.settle_promise(promise, arg(args, 0), true)?;
                } else {
                    self.resolve_promise(promise, arg(args, 0))?;
                }
                Ok(Value::Undefined)
            }
            PromiseFunction::CapabilityExecutor { resolve, reject } => {
                if !resolve.is_undefined() || !reject.is_undefined() {
                    return Err(JsError::type_error(
                        "Promise capability executor called more than once",
                    ));
                }
                let ObjectKind::PromiseFunction(function, _) = &mut self.heap.object_mut(id)?.kind
                else {
                    unreachable!()
                };
                *function = PromiseFunction::CapabilityExecutor {
                    resolve: arg(args, 0),
                    reject: arg(args, 1),
                };
                Ok(Value::Undefined)
            }
            PromiseFunction::Finally {
                callback,
                constructor,
                rejected,
            } => {
                let original = arg(args, 0);
                let result = self.invoke(callback, Value::Undefined, &[])?;
                let promise = self.promise_resolve(constructor, result)?;
                let thunk = self.promise_function(
                    PromiseFunction::Thunk {
                        value: original,
                        rejected,
                    },
                    0,
                )?;
                let then = self.get_property(promise.clone(), "then")?;
                self.invoke(then, promise, &[thunk])
            }
            PromiseFunction::Thunk { value, rejected } => {
                if rejected {
                    let kind = value
                        .as_object()
                        .and_then(|id| self.heap.object(id).ok())
                        .and_then(|o| {
                            if let ObjectKind::Error(kind, _) = o.kind {
                                Some(kind)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(ErrorKind::Thrown);
                    Err(JsError {
                        kind,
                        message: "Promise rejection".into(),
                        line: None,
                        column: None,
                        thrown: Some(value),
                        inspected_calls: None,
                    })
                } else {
                    Ok(value)
                }
            }
        }
    }
}

impl Vm {
    fn aggregate_error_values(&mut self, errors: &[Value]) -> Result<Value, JsError> {
        let list = self.try_new_array(errors)?;
        self.ensure_allocation(1024)?;
        let prototype = self.realm.error_prototypes[&ErrorKind::AggregateError];
        let result = self.allocate_error(ErrorKind::AggregateError, prototype)?;
        self.define_data_property(result.clone(), "errors", list, true, false, true)?;
        Ok(result)
    }
    fn finish_aggregate(&mut self, state: ObjectId) -> Result<Value, JsError> {
        let ObjectKind::PromiseAggregate(data) = &self.heap.object(state)?.kind else {
            unreachable!()
        };
        self.ensure_allocation(
            data.values
                .len()
                .saturating_mul(std::mem::size_of::<Value>())
                .saturating_add(
                    data.keys
                        .len()
                        .saturating_mul(std::mem::size_of::<PropertyKey>()),
                ),
        )?;
        let (kind, keys, values, capability) = (
            data.kind,
            data.keys.clone(),
            data.values.clone(),
            data.capability.clone(),
        );
        self.consume_work(values.len())?;
        let (function, result) = match kind {
            AggregateKind::Any => (capability.reject, self.aggregate_error_values(&values)?),
            AggregateKind::AllKeyed | AggregateKind::AllSettledKeyed => {
                let result = self.new_object_with_prototype(Value::Null)?;
                for (key, value) in keys.into_iter().zip(values) {
                    self.define_data_property_key(result.clone(), key, value, true, true, true)?;
                }
                (capability.resolve, result)
            }
            _ => (capability.resolve, self.try_new_array(&values)?),
        };
        self.invoke(function, Value::Undefined, &[result])
    }
    fn promise_combine(
        &mut self,
        constructor: Value,
        input: Value,
        kind: Option<AggregateKind>,
    ) -> Result<Value, JsError> {
        let capability = self.capability_for_constructor(constructor.clone())?;
        let result = (|| {
            let resolve = self.get_property(constructor.clone(), "resolve")?;
            if !self.is_callable(&resolve) {
                return Err(JsError::type_error(
                    "Promise resolve property is not callable",
                ));
            }
            let mut source = if matches!(
                kind,
                Some(AggregateKind::AllKeyed | AggregateKind::AllSettledKeyed)
            ) {
                let object = input.as_object().ok_or_else(|| {
                    JsError::type_error("Keyed promise combinators require an object")
                })?;
                let keys = self.own_property_keys(input.clone())?;
                CombineSource::Properties(object, keys.into_iter())
            } else {
                CombineSource::Iterable(self.get_iterator(input)?)
            };
            let state = if let Some(kind) = kind {
                self.ensure_allocation(512)?;
                Some(self.heap.allocate(Cell::Object(Object::new(
                    ObjectKind::PromiseAggregate(AggregateData {
                        values: Vec::new(),
                        keys: Vec::new(),
                        kind,
                        remaining: 1,
                        capability: capability.clone(),
                    }),
                    None,
                ))))
            } else {
                None
            };
            let performed = (|| {
                let mut index = 0usize;
                while let Some((key, value)) = source.next(self)? {
                    if index >= self.limits.max_array_length as usize {
                        return Err(JsError::new(
                            ErrorKind::ExecutionLimit,
                            "Promise iterable length budget exhausted",
                        ));
                    }
                    if let Some(state) = state {
                        let key_bytes = if key.is_some() {
                            std::mem::size_of::<PropertyKey>() * 2
                        } else {
                            0
                        };
                        self.reserve_allocation(std::mem::size_of::<Value>() * 2 + key_bytes)?;
                        let ObjectKind::PromiseAggregate(data) =
                            &mut self.heap.object_mut(state)?.kind
                        else {
                            unreachable!()
                        };
                        if let Some(key) = key {
                            data.keys.push(key);
                        }
                        data.values.push(Value::Undefined);
                    }
                    let promise = self.invoke(resolve.clone(), constructor.clone(), &[value])?;
                    let (fulfilled, rejected) = if let Some(state) = state {
                        let already = Rc::new(Flag::new(false));
                        let make = |vm: &mut Vm, rejected: bool, already: Rc<Flag<bool>>| {
                            vm.promise_function(
                                PromiseFunction::AggregateElement {
                                    state,
                                    index,
                                    rejected,
                                    already,
                                },
                                1,
                            )
                        };
                        let handlers = match kind.unwrap() {
                            AggregateKind::All | AggregateKind::AllKeyed => {
                                (make(self, false, already)?, capability.reject.clone())
                            }
                            AggregateKind::Any => {
                                (capability.resolve.clone(), make(self, true, already)?)
                            }
                            AggregateKind::AllSettled | AggregateKind::AllSettledKeyed => (
                                make(self, false, already.clone())?,
                                make(self, true, already)?,
                            ),
                        };
                        let ObjectKind::PromiseAggregate(data) =
                            &mut self.heap.object_mut(state)?.kind
                        else {
                            unreachable!()
                        };
                        data.remaining += 1;
                        handlers
                    } else {
                        (capability.resolve.clone(), capability.reject.clone())
                    };
                    let then = self.get_property(promise.clone(), "then")?;
                    self.invoke(then, promise, &[fulfilled, rejected])?;
                    index += 1;
                }
                if let Some(state) = state {
                    let ObjectKind::PromiseAggregate(data) = &mut self.heap.object_mut(state)?.kind
                    else {
                        unreachable!()
                    };
                    data.remaining -= 1;
                    if data.remaining == 0 {
                        self.finish_aggregate(state)?;
                    }
                }
                Ok(Value::Undefined)
            })();
            if let CombineSource::Iterable(iterator) = &mut source
                && performed.is_err()
                && !iterator.done
            {
                self.iterator_close(iterator, performed)
            } else {
                performed
            }
        })();
        if let Err(error) = result {
            let reason = self.error_reason(error)?;
            self.invoke(capability.reject, Value::Undefined, &[reason])?;
        }
        Ok(capability.promise)
    }
}

enum CombineSource {
    Iterable(crate::iterator::IteratorRecord),
    Properties(ObjectId, std::vec::IntoIter<PropertyKey>),
}

impl CombineSource {
    fn next(&mut self, vm: &mut Vm) -> Result<Option<(Option<PropertyKey>, Value)>, JsError> {
        match self {
            Self::Iterable(record) => Ok(vm.iterator_step(record)?.map(|value| (None, value))),
            Self::Properties(object, keys) => {
                for key in keys {
                    vm.tick()?;
                    if vm.own_enumerability(*object, &key)? == Some(true) {
                        let value = vm.get_property_key(Value::Object(*object), key.clone())?;
                        return Ok(Some((Some(key), value)));
                    }
                }
                Ok(None)
            }
        }
    }
}

fn install_aggregate_error(vm: &mut Vm) {
    let prototype = vm
        .new_object_with_prototype(Value::Object(vm.realm.error_prototype))
        .unwrap();
    vm.realm
        .error_prototypes
        .insert(ErrorKind::AggregateError, prototype.as_object().unwrap());
    vm.define_data_property(
        prototype.clone(),
        "name",
        Value::from("AggregateError"),
        true,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property(
        prototype.clone(),
        "message",
        Value::from(""),
        true,
        false,
        true,
    )
    .unwrap();
    let constructor = vm.native(
        "AggregateError",
        2,
        true,
        Rc::new(|vm, _, args| {
            self_aggregate_constructor(
                vm,
                args,
                vm.realm.error_prototypes[&ErrorKind::AggregateError],
            )
        }),
    );
    replace_construct(vm, constructor.clone(), |vm, _, args, target| {
        let prototype = vm.prototype_from_constructor(
            target,
            vm.realm.error_prototypes[&ErrorKind::AggregateError],
        )?;
        self_aggregate_constructor(vm, args, prototype)
    });
    vm.define_data_property(
        constructor.clone(),
        "prototype",
        prototype.clone(),
        false,
        false,
        false,
    )
    .unwrap();
    vm.define_data_property(
        prototype,
        "constructor",
        constructor.clone(),
        true,
        false,
        true,
    )
    .unwrap();
    let error = vm.get_global("Error").unwrap();
    vm.set_prototype(constructor.as_object().unwrap(), error.as_object())
        .unwrap();
    vm.define_data_property(
        vm.global(),
        "AggregateError",
        constructor,
        true,
        false,
        true,
    )
    .unwrap();
}
fn self_aggregate_constructor(
    vm: &mut Vm,
    args: &[Value],
    prototype: ObjectId,
) -> Result<Value, JsError> {
    // Message and cause are observed before iteration, as required by AggregateError.
    vm.ensure_allocation(1024)?;
    let result = vm.allocate_error(ErrorKind::AggregateError, prototype)?;
    if !arg(args, 1).is_undefined() {
        let message = vm.to_string(&arg(args, 1))?;
        vm.define_data_property(
            result.clone(),
            "message",
            Value::String(message),
            true,
            false,
            true,
        )?;
    }
    let options = arg(args, 2);
    if options.as_object().is_some() && vm.has(&options, &"cause".into())? {
        let cause = vm.get_property(options, "cause")?;
        vm.define_data_property(result.clone(), "cause", cause, true, false, true)?;
    }
    let errors = vm.iterator_to_list(arg(args, 0))?;
    let errors = vm.try_new_array(&errors)?;
    vm.define_data_property(result.clone(), "errors", errors, true, false, true)?;
    Ok(result)
}

pub(crate) fn install(vm: &mut Vm) {
    install_aggregate_error(vm);
    let prototype = vm.new_object();
    vm.realm.promise_prototype = prototype.as_object();
    let constructor = vm.native(
        "Promise",
        1,
        false,
        Rc::new(|_, _, _| Err(JsError::type_error("Promise constructor requires new"))),
    );
    vm.realm.promise_constructor = constructor.as_object();
    replace_construct(vm, constructor.clone(), |vm, _, args, target| {
        let executor = arg(args, 0);
        let capability = vm.begin_promise_constructor(&executor, target)?;
        let result = vm.invoke(
            executor,
            Value::Undefined,
            &[capability.resolve.clone(), capability.reject.clone()],
        );
        vm.complete_promise_constructor(capability, result)
    });
    vm.define_data_property(
        constructor.clone(),
        "prototype",
        prototype.clone(),
        false,
        false,
        false,
    )
    .unwrap();
    vm.define_data_property(
        prototype.clone(),
        "constructor",
        constructor.clone(),
        true,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property(
        vm.global(),
        "Promise",
        constructor.clone(),
        true,
        false,
        true,
    )
    .unwrap();
    method(vm, prototype.clone(), "then", 2, |vm, this, args| {
        vm.perform_then(this, arg(args, 0), arg(args, 1))
    });
    method(vm, prototype.clone(), "catch", 1, |vm, this, args| {
        let then = vm.get_property(this.clone(), "then")?;
        vm.invoke(then, this, &[Value::Undefined, arg(args, 0)])
    });
    vm.define_data_property_key(
        prototype.clone(),
        vm.well_known_key("toStringTag"),
        Value::from("Promise"),
        false,
        false,
        true,
    )
    .unwrap();
    let species = vm.native(
        "get [Symbol.species]",
        0,
        false,
        Rc::new(|_, this, _| Ok(this)),
    );
    vm.define_key(
        constructor.clone(),
        vm.well_known_key("species"),
        crate::heap::Property {
            kind: crate::heap::PropertyKind::Accessor {
                get: Some(species),
                set: None,
            },
            enumerable: false,
            configurable: true,
        },
    )
    .unwrap();
    method(vm, prototype, "finally", 1, |vm, this, args| {
        if this.as_object().is_none() {
            return Err(JsError::type_error(
                "Promise.finally receiver must be an object",
            ));
        }
        let constructor = vm.promise_species(this.clone())?;
        let callback = arg(args, 0);
        let (fulfilled, rejected) = if vm.is_callable(&callback) {
            (
                vm.promise_function(
                    PromiseFunction::Finally {
                        callback: callback.clone(),
                        constructor: constructor.clone(),
                        rejected: false,
                    },
                    1,
                )?,
                vm.promise_function(
                    PromiseFunction::Finally {
                        callback,
                        constructor,
                        rejected: true,
                    },
                    1,
                )?,
            )
        } else {
            (callback.clone(), callback)
        };
        let then = vm.get_property(this.clone(), "then")?;
        vm.invoke(then, this, &[fulfilled, rejected])
    });
    method(vm, constructor.clone(), "resolve", 1, |vm, this, args| {
        vm.promise_resolve(this, arg(args, 0))
    });
    method(vm, constructor.clone(), "reject", 1, |vm, this, args| {
        let capability = vm.capability_for_constructor(this)?;
        vm.invoke(capability.reject, Value::Undefined, &[arg(args, 0)])?;
        Ok(capability.promise)
    });
    method(vm, constructor.clone(), "try", 1, |vm, this, args| {
        if this.as_object().is_none() {
            return Err(JsError::type_error(
                "Promise.try receiver must be an object",
            ));
        }
        let result = vm.invoke(
            arg(args, 0),
            Value::Undefined,
            args.get(1..).unwrap_or_default(),
        );
        match result {
            Ok(value) => vm.promise_resolve(this, value),
            Err(error) => {
                let reason = vm.error_reason(error)?;
                let capability = vm.capability_for_constructor(this)?;
                vm.invoke(capability.reject, Value::Undefined, &[reason])?;
                Ok(capability.promise)
            }
        }
    });
    method(
        vm,
        constructor.clone(),
        "withResolvers",
        0,
        |vm, this, _| {
            let capability = vm.capability_for_constructor(this)?;
            let object = vm.new_object();
            for (key, value) in [
                ("promise", capability.promise),
                ("resolve", capability.resolve),
                ("reject", capability.reject),
            ] {
                vm.define_data_property(object.clone(), key, value, true, true, true)?;
            }
            Ok(object)
        },
    );
    for (name, kind) in [
        ("all", Some(AggregateKind::All)),
        ("race", None),
        ("allSettled", Some(AggregateKind::AllSettled)),
        ("any", Some(AggregateKind::Any)),
        ("allKeyed", Some(AggregateKind::AllKeyed)),
        ("allSettledKeyed", Some(AggregateKind::AllSettledKeyed)),
    ] {
        method(vm, constructor.clone(), name, 1, move |vm, this, args| {
            vm.promise_combine(this, arg(args, 0), kind)
        });
    }
}
