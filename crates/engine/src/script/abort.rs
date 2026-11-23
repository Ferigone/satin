//! Abort reasons and observable dependencies live in traced VM slots. Worker
//! cancellation uses separate thread-safe handles and never carries JS values.
use super::{arg, event_target, getter, webidl};
use rust_browser_javascript::{ErrorKind, JsError, ObjectId, Value, Vm};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::{Rc, Weak},
    time::Duration,
};

const REASON: u32 = 0;
const SOURCES: u32 = 1;
const OBSERVED: u32 = 2;
const ALGORITHM_OWNERS: u32 = 3;
const MAX_SIGNALS: usize = 8192;
const MAX_LINKS: usize = 16384;
const MAX_SOURCES: usize = 256;
const MAX_DEPENDENTS: usize = 512;
const MAX_HOOKS: usize = 256;

type Callback = Rc<dyn Fn(&mut Vm, Value) -> Result<(), JsError>>;

struct Dependent {
    slot: usize,
    signal: Weak<Signal>,
}
struct Source {
    slot: usize,
    signal: Weak<Signal>,
}
struct Algorithm {
    id: u64,
    callback: Callback,
    owner_slot: Option<usize>,
}
struct Signal {
    id: ObjectId,
    dependent: bool,
    sources: Vec<Source>,
    dependents: RefCell<Vec<Dependent>>,
    algorithms: RefCell<Vec<Algorithm>>,
    next_algorithm: Cell<u64>,
    observed: Cell<bool>,
    budget: Rc<Cell<(usize, usize)>>,
}
impl Drop for Signal {
    fn drop(&mut self) {
        let (signals, edges) = self.budget.get();
        self.budget.set((signals - 1, edges - self.sources.len()));
    }
}

pub(super) struct Subscription {
    signal: Weak<Signal>,
    id: u64,
    owner_slot: Option<usize>,
}
impl Subscription {
    fn remove(&mut self) -> Option<Rc<Signal>> {
        let signal = std::mem::take(&mut self.signal).upgrade()?;
        signal
            .algorithms
            .borrow_mut()
            .retain(|entry| entry.id != self.id);
        Some(signal)
    }

    pub(super) fn detach(mut self, vm: &mut Vm) -> Result<(), JsError> {
        if let Some(signal) = self.remove()
            && let Some(slot) = self.owner_slot
        {
            set_edge(vm, &signal, ALGORITHM_OWNERS, slot, Value::Undefined)?;
            update_retention(vm, &signal)?;
        }
        Ok(())
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.remove();
    }
}

fn signal(vm: &Vm, value: &Value) -> Option<Rc<Signal>> {
    vm.host_data::<Signal>(value)
}
pub(super) fn require_signal(vm: &Vm, value: &Value) -> Result<(), JsError> {
    signal(vm, value)
        .map(|_| ())
        .ok_or_else(|| JsError::type_error("Expected an AbortSignal"))
}
pub(super) fn reason(vm: &Vm, value: &Value) -> Option<Value> {
    vm.host_slot(value, REASON)
}

pub(super) fn subscribe<F>(vm: &Vm, value: &Value, callback: F) -> Result<Subscription, JsError>
where
    F: Fn(&mut Vm, Value) -> Result<(), JsError> + 'static,
{
    add_algorithm(vm, value, false, callback)
}

/// The owner is retained while this signal can abort the operation. The
/// operation detaches the subscription when it finishes.
pub(super) fn subscribe_traced<F>(
    vm: &mut Vm,
    value: &Value,
    owner: Value,
    callback: F,
) -> Result<Subscription, JsError>
where
    F: Fn(&mut Vm, Value) -> Result<(), JsError> + 'static,
{
    vm.charge_work(MAX_HOOKS)?;
    let subscription = add_algorithm(vm, value, true, callback)?;
    let data = subscription.signal.upgrade().unwrap();
    let slot = subscription.owner_slot.unwrap();
    let result = set_edge(vm, &data, ALGORITHM_OWNERS, slot, owner)
        .and_then(|()| update_retention(vm, &data));
    if let Err(error) = result {
        let _ = subscription.detach(vm);
        return Err(error);
    }
    Ok(subscription)
}

fn add_algorithm<F>(
    vm: &Vm,
    value: &Value,
    traced: bool,
    callback: F,
) -> Result<Subscription, JsError>
where
    F: Fn(&mut Vm, Value) -> Result<(), JsError> + 'static,
{
    let signal = signal(vm, value).ok_or_else(|| JsError::type_error("Expected an AbortSignal"))?;
    let mut algorithms = signal.algorithms.borrow_mut();
    if algorithms.len() == MAX_HOOKS {
        return Err(JsError::new(
            ErrorKind::HeapLimit,
            "AbortSignal subscription limit reached",
        ));
    }
    let id = signal.next_algorithm.get();
    signal.next_algorithm.set(
        id.checked_add(1).ok_or_else(|| {
            JsError::new(ErrorKind::HeapLimit, "AbortSignal identifiers exhausted")
        })?,
    );
    let owner_slot = traced.then(|| {
        let mut used = [false; MAX_HOOKS];
        for slot in algorithms.iter().filter_map(|entry| entry.owner_slot) {
            used[slot] = true;
        }
        used.iter().position(|&occupied| !occupied).unwrap()
    });
    algorithms.push(Algorithm {
        id,
        callback: Rc::new(callback),
        owner_slot,
    });
    Ok(Subscription {
        signal: Rc::downgrade(&signal),
        id,
        owner_slot,
    })
}

fn set_edge(
    vm: &mut Vm,
    owner: &Signal,
    table_slot: u32,
    index: usize,
    value: Value,
) -> Result<(), JsError> {
    let owner = Value::Object(owner.id);
    let table = match vm
        .host_slot(&owner, table_slot)
        .filter(|entry| entry.as_object().is_some())
    {
        Some(table) => table,
        None if value.is_undefined() => return Ok(()),
        None => {
            let table = vm.new_object_with_prototype(Value::Null)?;
            vm.set_host_slot(owner, table_slot, table.clone())?;
            table
        }
    };
    vm.define_data_property(table, &index.to_string(), value, true, false, true)
}

/// While a dependent can emit an abort event, a live source retains it. These
/// are traced edges, so an abandoned source and dependent can collect together.
pub(super) fn keep_for_abort_event(vm: &mut Vm, object: &Value, keep: bool) -> Result<(), JsError> {
    let Some(signal) = signal(vm, object) else {
        return Ok(());
    };
    signal.observed.set(keep);
    update_retention(vm, &signal)
}

fn update_retention(vm: &mut Vm, signal: &Signal) -> Result<(), JsError> {
    let object = Value::Object(signal.id);
    let keep = signal.observed.get()
        || signal
            .algorithms
            .borrow()
            .iter()
            .any(|entry| entry.owner_slot.is_some());
    let value = if keep && reason(vm, &object).is_none() {
        object
    } else {
        Value::Undefined
    };
    for link in &signal.sources {
        if let Some(owner) = link.signal.upgrade() {
            set_edge(vm, &owner, OBSERVED, link.slot, value.clone())?;
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(super) struct AbortHost {
    prototype: Value,
    exception: Value,
    events: event_target::Events,
    budget: Rc<Cell<(usize, usize)>>,
}

impl AbortHost {
    pub(super) fn events(&self) -> &event_target::Events {
        &self.events
    }

    pub(super) fn unaborted_signal(&self, vm: &mut Vm) -> Result<Value, JsError> {
        self.new_signal(vm, false, &[], None)
    }

    fn new_signal(
        &self,
        vm: &mut Vm,
        dependent: bool,
        sources: &[Rc<Signal>],
        aborted: Option<Value>,
    ) -> Result<Value, JsError> {
        let (signals, edges) = self.budget.get();
        if signals >= MAX_SIGNALS || edges + sources.len() > MAX_LINKS {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "AbortSignal graph exceeds its size budget",
            ));
        }
        let mut links = Vec::with_capacity(sources.len());
        for source in sources {
            let mut list = source.dependents.borrow_mut();
            vm.charge_work(list.len() + 1)?;
            list.retain(|link| link.signal.strong_count() != 0);
            let mut used = [false; MAX_DEPENDENTS];
            for link in &*list {
                used[link.slot] = true;
            }
            let slot = used.iter().position(|&used| !used).ok_or_else(|| {
                JsError::new(ErrorKind::HeapLimit, "AbortSignal dependent limit reached")
            })?;
            links.push(Source {
                slot,
                signal: Rc::downgrade(source),
            });
        }
        let object = vm.new_object_with_prototype(self.prototype.clone())?;
        event_target::attach(vm, object.clone())?;
        if !sources.is_empty() {
            let source_values = sources
                .iter()
                .map(|source| Value::Object(source.id))
                .collect::<Vec<_>>();
            let source_array = vm.new_array(&source_values);
            vm.set_host_slot(object.clone(), SOURCES, source_array)?;
        }
        if let Some(reason) = aborted {
            vm.set_host_slot(object.clone(), REASON, reason)?;
        }
        self.budget.set((signals + 1, edges + links.len()));
        let data = Rc::new(Signal {
            id: object.as_object().unwrap(),
            dependent,
            sources: links,
            dependents: RefCell::default(),
            algorithms: RefCell::default(),
            next_algorithm: Cell::new(0),
            observed: Cell::new(false),
            budget: self.budget.clone(),
        });
        vm.set_host_data(object.clone(), data.clone())?;
        for (source, link) in sources.iter().zip(&data.sources) {
            source.dependents.borrow_mut().push(Dependent {
                slot: link.slot,
                signal: Rc::downgrade(&data),
            });
        }
        Ok(object)
    }

    pub(super) fn dependent(&self, vm: &mut Vm, inputs: &[Value]) -> Result<Value, JsError> {
        let mut sources = Vec::new();
        let mut seen = HashSet::new();
        for input in inputs {
            let input_data =
                signal(vm, input).ok_or_else(|| JsError::type_error("Expected an AbortSignal"))?;
            if let Some(reason) = reason(vm, input) {
                return self.new_signal(vm, true, &[], Some(reason));
            }
            let candidates: Vec<_> = if input_data.dependent {
                input_data
                    .sources
                    .iter()
                    .filter_map(|link| link.signal.upgrade())
                    .collect()
            } else {
                vec![input_data]
            };
            for candidate in candidates {
                vm.charge_work(1)?;
                if seen.insert(candidate.id) {
                    if sources.len() == MAX_SOURCES {
                        return Err(JsError::new(
                            ErrorKind::ExecutionLimit,
                            "AbortSignal source limit reached",
                        ));
                    }
                    sources.push(candidate);
                }
            }
        }
        self.new_signal(vm, true, &sources, None)
    }

    pub(super) fn default_reason(&self, vm: &mut Vm, supplied: Value) -> Result<Value, JsError> {
        if supplied.is_undefined() {
            exception(
                vm,
                self.exception.clone(),
                "AbortError",
                "This operation was aborted",
            )
        } else {
            Ok(supplied)
        }
    }

    pub(super) fn abort(&self, vm: &mut Vm, object: Value, supplied: Value) -> Result<(), JsError> {
        let data =
            signal(vm, &object).ok_or_else(|| JsError::type_error("Expected an AbortSignal"))?;
        if reason(vm, &object).is_some() {
            return Ok(());
        }
        let abort_reason = self.default_reason(vm, supplied)?;
        vm.set_host_slot(object.clone(), REASON, abort_reason.clone())?;
        let mut aborted = vec![data.clone()];
        for link in std::mem::take(&mut *data.dependents.borrow_mut()) {
            if let Some(dependent) = link.signal.upgrade() {
                let value = Value::Object(dependent.id);
                if reason(vm, &value).is_none() {
                    vm.set_host_slot(value, REASON, abort_reason.clone())?;
                    aborted.push(dependent);
                }
            }
        }
        // The whole dependent set is marked before the first author listener
        // can observe it. A listener limit cannot keep later I/O alive.
        let mut first_error = None;
        for data in aborted {
            let value = Value::Object(data.id);
            let hooks = std::mem::take(&mut *data.algorithms.borrow_mut());
            for hook in hooks {
                if let Err(error) = (hook.callback)(vm, abort_reason.clone()) {
                    first_error.get_or_insert(error);
                }
            }
            if vm.host_slot(&value, ALGORITHM_OWNERS).is_some()
                && let Err(error) =
                    vm.set_host_slot(value.clone(), ALGORITHM_OWNERS, Value::Undefined)
            {
                first_error.get_or_insert(error);
            }
            if first_error.is_none()
                && data.observed.get()
                && let Err(error) = self.events.fire_abort(vm, value.clone())
            {
                first_error = Some(error);
            }
            if data.dependent {
                if let Err(error) = keep_for_abort_event(vm, &value, false) {
                    first_error.get_or_insert(error);
                }
                if let Err(error) = vm.set_host_slot(value, SOURCES, Value::Undefined) {
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) fn install(vm: &mut Vm, scheduler: Option<super::Shared>) -> Result<Self, JsError> {
        let events = event_target::install(vm, scheduler.clone())?;
        let exception_constructor = vm.get_global("DOMException")?;
        // Capture the intrinsics before page code can replace their globals.
        vm.root(exception_constructor.clone());
        let prototype = vm.new_object_with_prototype(events.target_proto.clone())?;
        vm.root(prototype.clone());
        let host = Self {
            prototype,
            exception: exception_constructor,
            events,
            budget: Rc::default(),
        };
        let constructor = webidl::interface(
            vm,
            host.prototype.clone(),
            "AbortSignal",
            0,
            |_, _, _, _| Err(JsError::type_error("Illegal AbortSignal constructor")),
        )?;
        let object = vm.get_global("Object")?;
        let set_prototype = vm.get_property(object, "setPrototypeOf")?;
        let event_target = vm.get_global("EventTarget")?;
        vm.call(
            set_prototype,
            Value::Undefined,
            &[constructor.clone(), event_target],
        )?;
        let instant = host.clone();
        webidl::operation(vm, constructor.clone(), "abort", 0, move |vm, _, args| {
            let reason = instant.default_reason(vm, arg(args, 0))?;
            instant.new_signal(vm, false, &[], Some(reason))
        })?;
        let compose = host.clone();
        webidl::operation(vm, constructor.clone(), "any", 1, move |vm, _, args| {
            webidl::required(args, 1, "AbortSignal.any")?;
            let input = args[0].clone();
            if input.as_object().is_none() {
                return Err(JsError::type_error("Expected a sequence of AbortSignals"));
            }
            let iterator = webidl::iterator_method(vm, &input)?;
            let sources = webidl::sequence(vm, input, iterator, MAX_SOURCES, |vm, value| {
                require_signal(vm, &value)?;
                Ok(value)
            })?;
            compose.dependent(vm, &sources)
        })?;
        let expire = host.clone();
        let fire = vm.new_native_function("AbortSignal timeout", move |vm, _, args| {
            let reason = exception(
                vm,
                expire.exception.clone(),
                "TimeoutError",
                "The operation timed out",
            )?;
            expire.abort(vm, arg(args, 0), reason)?;
            Ok(Value::Undefined)
        });
        vm.root(fire.clone());
        let delayed = host.clone();
        webidl::operation(vm, constructor, "timeout", 1, move |vm, _, args| {
            webidl::required(args, 1, "AbortSignal.timeout")?;
            let delay = vm.to_number(&args[0])?.trunc();
            if !(0.0..(1u64 << 53) as f64).contains(&delay) {
                return Err(JsError::type_error(
                    "AbortSignal.timeout requires a nonnegative safe integer delay",
                ));
            }
            let scheduler = scheduler
                .as_ref()
                .ok_or_else(|| super::unsupported("This context has no timer scheduler"))?;
            let signal = delayed.new_signal(vm, false, &[], None)?;
            let delay = Duration::from_millis(delay as u64);
            super::schedule_timer(vm, scheduler, fire.clone(), vec![signal.clone()], delay)?;
            Ok(signal)
        })?;
        getter(vm, host.prototype.clone(), "aborted", |vm, this, _| {
            require_signal(vm, &this)?;
            Ok(reason(vm, &this).is_some().into())
        })?;
        getter(vm, host.prototype.clone(), "reason", |vm, this, _| {
            require_signal(vm, &this)?;
            Ok(reason(vm, &this).unwrap_or(Value::Undefined))
        })?;
        webidl::operation(
            vm,
            host.prototype.clone(),
            "throwIfAborted",
            0,
            |vm, this, _| {
                require_signal(vm, &this)?;
                if let Some(value) = reason(vm, &this) {
                    return Err(JsError {
                        kind: ErrorKind::Error,
                        message: "Signal aborted".into(),
                        line: None,
                        column: None,
                        thrown: Some(value),
                    });
                }
                Ok(Value::Undefined)
            },
        )?;
        event_target::install_abort_handler(vm, host.prototype.clone())?;
        let controller = vm.new_object();
        let intrinsic = controller.clone();
        let create = host.clone();
        webidl::interface(
            vm,
            controller.clone(),
            "AbortController",
            0,
            move |vm, _, _, target| {
                let target =
                    target.ok_or_else(|| JsError::type_error("AbortController requires new"))?;
                let controller = webidl::instance(vm, target, &intrinsic)?;
                let signal = create.new_signal(vm, false, &[], None)?;
                vm.set_host_slot(controller.clone(), 0, signal)?;
                vm.set_host_data(controller.clone(), Rc::new(Controller))?;
                Ok(controller)
            },
        )?;
        getter(vm, controller.clone(), "signal", |vm, this, _| {
            controller_signal(vm, &this)
        })?;
        let trigger = host.clone();
        webidl::operation(vm, controller, "abort", 0, move |vm, this, args| {
            let signal = controller_signal(vm, &this)?;
            trigger.abort(vm, signal, arg(args, 0))?;
            Ok(Value::Undefined)
        })?;
        Ok(host)
    }
}
struct Controller;
fn controller_signal(vm: &Vm, receiver: &Value) -> Result<Value, JsError> {
    vm.host_data::<Controller>(receiver)
        .ok_or_else(|| JsError::type_error("Illegal AbortController receiver"))?;
    vm.host_slot(receiver, 0)
        .ok_or_else(|| JsError::new(ErrorKind::InternalError, "Missing AbortController signal"))
}

pub(super) fn exception(
    vm: &mut Vm,
    constructor: Value,
    name: &str,
    message: &str,
) -> Result<Value, JsError> {
    if vm.is_callable(&constructor) {
        vm.construct(constructor, &[message.into(), name.into()])
    } else {
        // A standalone FetchHost used by engine tests may have no DOM realm.
        let value = vm.error_value(&JsError::new(ErrorKind::Error, message))?;
        vm.define_data_property(value.clone(), "name", name.into(), false, false, true)?;
        Ok(value)
    }
}

pub(super) fn dom_exception_error(
    vm: &mut Vm,
    class: Value,
    name: &str,
    message: &str,
) -> Result<JsError, JsError> {
    let value = exception(vm, class, name, message)?;
    Ok(JsError {
        kind: ErrorKind::Error,
        message: message.into(),
        line: None,
        column: None,
        thrown: Some(value),
    })
}
