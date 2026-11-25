//! Listener tables and event state for DOM nodes, Window, and standalone targets.
use super::{Shared, abort, arg, fatal, getter, webidl};
use crate::dom::NodeId;
use rust_browser_javascript::{ErrorKind, JsError, JsString, Value, Vm};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

const LISTENERS_SLOT: u32 = 10;
pub(super) const EVENT_TARGET_SLOT: u32 = 0;
const CURRENT_TARGET_SLOT: u32 = 1;
const CUSTOM_DETAIL_SLOT: u32 = 8;
const MAX_LISTENERS: usize = 128;
const DOM_TARGET_LISTENER_LIMIT: usize = 4096;
const DOM_LISTENER_LIMIT: usize = 8192;
const MAX_TYPE_UNITS: usize = 512;

#[derive(Clone)]
pub(super) enum DispatchPath {
    Direct,
    Dom(Weak<RefCell<super::State>>, Rc<[Option<NodeId>]>),
}

pub(super) struct EventData {
    pub(super) kind: JsString,
    pub(super) bubbles: bool,
    pub(super) cancelable: bool,
    pub(super) composed: bool,
    trusted: Cell<bool>,
    initialized: Cell<bool>,
    pub(super) active: Cell<bool>,
    pub(super) phase: Cell<u32>,
    pub(super) stopped: Cell<bool>,
    pub(super) immediate: Cell<bool>,
    pub(super) prevented: Cell<bool>,
    passive: Cell<bool>,
    path: RefCell<Option<DispatchPath>>,
}

struct Listener {
    serial: u64,
    slot: usize,
    kind: JsString,
    once: bool,
    capture: bool,
    passive: bool,
    handler: bool,
    _abort: Option<abort::Subscription>,
}

struct TargetData {
    serial: Cell<u64>,
    listeners: RefCell<Vec<Listener>>,
    free_slots: RefCell<Vec<usize>>,
    dom_count: Option<Rc<Cell<usize>>>,
}

impl Drop for TargetData {
    fn drop(&mut self) {
        if let Some(count) = &self.dom_count {
            count.set(count.get() - self.listeners.get_mut().len());
        }
    }
}

#[derive(Clone)]
struct DomTargets {
    state: Shared,
    listener_count: Rc<Cell<usize>>,
}

#[derive(Clone)]
pub(super) struct Events {
    pub(super) target_proto: Value,
    pub(super) event_proto: Value,
    report: Value,
    exception: Value,
    dom: Option<DomTargets>,
    implicit_receiver: Option<Value>,
}

pub(super) fn target_table_exists(vm: &Vm, object: &Value) -> bool {
    target_table(vm, object).is_some()
}

fn target_table(vm: &Vm, object: &Value) -> Option<(Value, Rc<TargetData>)> {
    let table = vm.host_slot(object, LISTENERS_SLOT)?;
    let data = vm.host_data::<TargetData>(&table)?;
    Some((table, data))
}

fn target(vm: &Vm, object: &Value) -> Result<(Value, Rc<TargetData>), JsError> {
    target_table(vm, object).ok_or_else(|| JsError::type_error("Illegal EventTarget receiver"))
}

pub(super) fn attach(vm: &mut Vm, object: Value) -> Result<(), JsError> {
    attach_table(vm, object, None)
}

fn attach_table(
    vm: &mut Vm,
    object: Value,
    dom_count: Option<Rc<Cell<usize>>>,
) -> Result<(), JsError> {
    let table = vm.new_object_with_prototype(Value::Null)?;
    vm.set_host_data(
        table.clone(),
        Rc::new(TargetData {
            serial: Cell::new(0),
            listeners: RefCell::default(),
            free_slots: RefCell::default(),
            dom_count,
        }),
    )?;
    vm.set_host_slot(object, LISTENERS_SLOT, table)
}

pub(super) fn event(vm: &Vm, object: &Value) -> Result<Rc<EventData>, JsError> {
    vm.host_data::<EventData>(object)
        .ok_or_else(|| JsError::type_error("Illegal Event receiver"))
}

pub(super) fn init_event(
    vm: &mut Vm,
    object: Value,
    kind: JsString,
    flags: [bool; 3],
    trusted: bool,
) -> Result<Value, JsError> {
    vm.set_host_data(
        object.clone(),
        Rc::new(EventData {
            kind,
            bubbles: flags[0],
            cancelable: flags[1],
            composed: flags[2],
            trusted: Cell::new(trusted),
            initialized: Cell::new(true),
            active: Cell::new(false),
            phase: Cell::new(0),
            stopped: Cell::new(false),
            immediate: Cell::new(false),
            prevented: Cell::new(false),
            passive: Cell::new(false),
            path: RefCell::new(None),
        }),
    )?;
    Ok(object)
}

fn reinitialize_event(
    vm: &mut Vm,
    object: Value,
    args: &[Value],
    operation: &str,
) -> Result<bool, JsError> {
    event(vm, &object)?;
    webidl::required(args, 1, operation)?;
    let event_type = kind(vm, &args[0])?;
    let data = event(vm, &object)?;
    if data.active.get() {
        return Ok(false);
    }
    if vm.host_slot(&object, EVENT_TARGET_SLOT).is_some() {
        vm.set_host_slot(object.clone(), EVENT_TARGET_SLOT, Value::Null)?;
    }
    let flags = [
        arg(args, 1).is_truthy(),
        arg(args, 2).is_truthy(),
        data.composed,
    ];
    init_event(vm, object, event_type, flags, false)?;
    Ok(true)
}

pub(super) fn kind(vm: &mut Vm, value: &Value) -> Result<JsString, JsError> {
    let kind = vm.to_string(value)?;
    if kind.len() > MAX_TYPE_UNITS {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Event type exceeds its length budget",
        ));
    }
    Ok(kind)
}

pub(super) fn option(vm: &mut Vm, options: &Value, name: &str) -> Result<Value, JsError> {
    if options.as_object().is_some() {
        vm.get_property(options.clone(), name)
    } else {
        Ok(Value::Undefined)
    }
}

pub(super) fn flags(vm: &mut Vm, options: &Value) -> Result<[bool; 3], JsError> {
    if !options.is_nullish() && options.as_object().is_none() {
        return Err(JsError::type_error("Event options must be a dictionary"));
    }
    Ok([
        option(vm, options, "bubbles")?.is_truthy(),
        option(vm, options, "cancelable")?.is_truthy(),
        option(vm, options, "composed")?.is_truthy(),
    ])
}

fn listener_value(vm: &mut Vm, table: &Value, slot: usize) -> Result<Value, JsError> {
    vm.get_property(table.clone(), &slot.to_string())
}

fn store_listener(vm: &mut Vm, table: &Value, slot: usize, callback: Value) -> Result<(), JsError> {
    vm.define_data_property(
        table.clone(),
        &slot.to_string(),
        callback,
        true,
        false,
        true,
    )
}

fn remove(vm: &mut Vm, object: &Value, serial: u64) -> Result<(), JsError> {
    let (table, data) = target(vm, object)?;
    let removed = {
        let mut listeners = data.listeners.borrow_mut();
        listeners
            .iter()
            .position(|listener| listener.serial == serial)
            .map(|index| listeners.remove(index))
    };
    if let Some(removed) = removed {
        if let Some(count) = &data.dom_count {
            count.set(count.get() - 1);
        }
        store_listener(vm, &table, removed.slot, Value::Undefined)?;
        if removed._abort.is_some() {
            store_listener(
                vm,
                &table,
                DOM_TARGET_LISTENER_LIMIT + removed.slot,
                Value::Undefined,
            )?;
        }
        data.free_slots.borrow_mut().push(removed.slot);
        update_listener_liveness(vm, object, &data)?;
    }
    Ok(())
}

pub(super) fn clear_listeners(vm: &mut Vm, object: &Value) -> Result<(), JsError> {
    let Some((table, data)) = target_table(vm, object) else {
        return Ok(());
    };
    let listeners = std::mem::take(&mut *data.listeners.borrow_mut());
    if let Some(count) = &data.dom_count {
        count.set(count.get() - listeners.len());
    }
    for listener in listeners {
        store_listener(vm, &table, listener.slot, Value::Undefined)?;
        if listener._abort.is_some() {
            store_listener(
                vm,
                &table,
                DOM_TARGET_LISTENER_LIMIT + listener.slot,
                Value::Undefined,
            )?;
        }
        data.free_slots.borrow_mut().push(listener.slot);
    }
    update_listener_liveness(vm, object, &data)
}

fn update_listener_liveness(vm: &mut Vm, object: &Value, data: &TargetData) -> Result<(), JsError> {
    let has_listener = data.listeners.borrow().iter().any(|listener| {
        listener
            .kind
            .as_utf16()
            .iter()
            .copied()
            .eq("abort".encode_utf16())
    });
    let listens_for_change = data.listeners.borrow().iter().any(|listener| {
        listener
            .kind
            .as_utf16()
            .iter()
            .copied()
            .eq("change".encode_utf16())
    });
    super::media_queries::retain_if_observed(vm, object, listens_for_change);
    abort::keep_for_abort_event(vm, object, has_listener)
}

impl Events {
    pub(super) fn without_document(self) -> Self {
        Self {
            dom: None,
            implicit_receiver: None,
            ..self
        }
    }

    fn receiver(&self, this: Value) -> Value {
        if this.is_nullish()
            && let Some(global) = &self.implicit_receiver
        {
            return global.clone();
        }
        this
    }

    fn dom_receiver(&self, object: &Value) -> Option<(&DomTargets, Option<NodeId>)> {
        let dom = self.dom.as_ref()?;
        super::target(&dom.state, object)
            .ok()
            .map(|node| (dom, node))
    }

    fn make_event(
        &self,
        vm: &mut Vm,
        prototype: Option<Value>,
        kind: JsString,
        flags: [bool; 3],
        trusted: bool,
    ) -> Result<Value, JsError> {
        let object = match prototype {
            Some(target) => webidl::instance(vm, target, &self.event_proto)?,
            None => vm.new_object_with_prototype(self.event_proto.clone())?,
        };
        init_event(vm, object, kind, flags, trusted)
    }

    pub(super) fn fire_abort(&self, vm: &mut Vm, object: Value) -> Result<(), JsError> {
        let event = self.make_event(vm, None, "abort".into(), [false; 3], true)?;
        self.dispatch(vm, object, event).map(|_| ())
    }

    pub(super) fn dispatch(
        &self,
        vm: &mut Vm,
        object: Value,
        event_value: Value,
    ) -> Result<bool, JsError> {
        self.dispatch_with_report(vm, object, event_value, |vm, error| {
            if vm.is_callable(&self.report) {
                vm.call(
                    self.report.clone(),
                    Value::Undefined,
                    &[error.to_string().into()],
                )?;
            }
            Ok(())
        })
    }

    pub(super) fn dispatch_with_report(
        &self,
        vm: &mut Vm,
        object: Value,
        event_value: Value,
        mut report: impl FnMut(&mut Vm, &JsError) -> Result<(), JsError>,
    ) -> Result<bool, JsError> {
        let info = event(vm, &event_value)?;
        info.begin(vm, &event_value, object.clone(), DispatchPath::Direct)?;
        let root = vm.root(event_value.clone());
        let result = vm.with_task(|vm| {
            for capture in [true, false] {
                if info.stopped.get() {
                    break;
                }
                info.current(vm, &event_value, object.clone(), 2)?;
                info.invoke(vm, &object, &event_value, capture, &mut report)?;
            }
            Ok(!info.prevented.get())
        });
        let cleanup = info.end(vm, event_value);
        vm.unroot(root);
        cleanup?;
        result
    }
}

impl EventData {
    pub(super) fn begin(
        &self,
        vm: &mut Vm,
        event: &Value,
        target: Value,
        path: DispatchPath,
    ) -> Result<(), JsError> {
        super::ui_events::capture_dispatch_position(vm, event)?;
        vm.set_host_slot(event.clone(), EVENT_TARGET_SLOT, target)?;
        self.path.replace(Some(path));
        self.active.set(true);
        Ok(())
    }

    pub(super) fn current(
        &self,
        vm: &mut Vm,
        event: &Value,
        target: Value,
        phase: u32,
    ) -> Result<(), JsError> {
        vm.set_host_slot(event.clone(), CURRENT_TARGET_SLOT, target)?;
        self.phase.set(phase);
        Ok(())
    }

    pub(super) fn end(&self, vm: &mut Vm, event: Value) -> Result<(), JsError> {
        self.active.set(false);
        self.phase.set(0);
        self.stopped.set(false);
        self.immediate.set(false);
        self.passive.set(false);
        self.path.take();
        vm.set_host_slot(event, CURRENT_TARGET_SLOT, Value::Null)
    }

    pub(super) fn invoke(
        &self,
        vm: &mut Vm,
        object: &Value,
        event: &Value,
        capture: bool,
        report: impl FnMut(&mut Vm, &JsError) -> Result<(), JsError>,
    ) -> Result<(), JsError> {
        self.invoke_with_checkpoint(vm, object, event, capture, report, |_| Ok(()))
    }

    pub(super) fn invoke_with_checkpoint(
        &self,
        vm: &mut Vm,
        object: &Value,
        event: &Value,
        capture: bool,
        mut report: impl FnMut(&mut Vm, &JsError) -> Result<(), JsError>,
        mut checkpoint: impl FnMut(&mut Vm) -> Result<(), JsError>,
    ) -> Result<(), JsError> {
        let Some((table, data)) = target_table(vm, object) else {
            return Ok(());
        };
        let calls = data
            .listeners
            .borrow()
            .iter()
            .filter(|listener| listener.capture == capture && listener.kind == self.kind)
            .map(|listener| listener.serial)
            .collect::<Vec<_>>();
        for serial in calls {
            if self.immediate.get() {
                break;
            }
            let entry = data
                .listeners
                .borrow()
                .iter()
                .find(|listener| listener.serial == serial)
                .map(|listener| (listener.slot, listener.once, listener.passive));
            let Some((slot, once, passive)) = entry else {
                continue;
            };
            let callback = listener_value(vm, &table, slot)?;
            if once {
                remove(vm, object, serial)?;
            }
            self.passive.set(passive);
            let invoked = if vm.is_callable(&callback) {
                vm.call(callback, object.clone(), std::slice::from_ref(event))
            } else {
                vm.get_property(callback.clone(), "handleEvent")
                    .and_then(|function| vm.call(function, callback, std::slice::from_ref(event)))
            };
            let completed = match invoked {
                Err(error) if fatal(&error) => Err(error),
                Err(error) => checkpoint(vm).and_then(|()| report(vm, &error)),
                Ok(_) => checkpoint(vm),
            };
            self.passive.set(false);
            completed?;
        }
        Ok(())
    }
}

fn add(
    vm: &mut Vm,
    object: Value,
    (kind, callback): (JsString, Value),
    options: Value,
    handler: bool,
    dom_count: Option<Rc<Cell<usize>>>,
) -> Result<(), JsError> {
    let (capture, once, passive, signal) = if options.as_object().is_some() {
        (
            option(vm, &options, "capture")?.is_truthy(),
            option(vm, &options, "once")?.is_truthy(),
            option(vm, &options, "passive")?.is_truthy(),
            option(vm, &options, "signal")?,
        )
    } else {
        (options.is_truthy(), false, false, Value::Undefined)
    };
    if callback.is_nullish() {
        return Ok(());
    }
    if callback.as_object().is_none() {
        return Err(JsError::type_error("Event listener must be an object"));
    }
    if !signal.is_undefined() {
        abort::require_signal(vm, &signal)?;
        if abort::reason(vm, &signal).is_some() {
            return Ok(());
        }
    }
    if target_table(vm, &object).is_none() {
        attach_table(vm, object.clone(), dom_count)?;
    }
    let (table, data) = target(vm, &object)?;
    let existing = data
        .listeners
        .borrow()
        .iter()
        .filter(|listener| {
            !listener.handler && listener.kind == kind && listener.capture == capture
        })
        .map(|listener| listener.slot)
        .collect::<Vec<_>>();
    if !handler {
        for slot in existing {
            if listener_value(vm, &table, slot)? == callback {
                return Ok(());
            }
        }
    }
    let full = if let Some(count) = &data.dom_count {
        count.get() >= DOM_LISTENER_LIMIT
            || data.listeners.borrow().len() >= DOM_TARGET_LISTENER_LIMIT
    } else {
        data.listeners.borrow().len() >= MAX_LISTENERS
    };
    if full {
        return Err(JsError::new(
            ErrorKind::HeapLimit,
            "EventTarget listener limit reached",
        ));
    }
    let slot = data
        .free_slots
        .borrow()
        .last()
        .copied()
        .unwrap_or_else(|| data.listeners.borrow().len());
    let serial = data.serial.get();
    data.serial.set(serial.checked_add(1).ok_or_else(|| {
        JsError::new(ErrorKind::HeapLimit, "Event listener identifiers exhausted")
    })?);
    let abort = if signal.is_undefined() {
        None
    } else {
        let id = object.as_object().unwrap();
        Some(abort::subscribe(vm, &signal, move |vm, _| {
            let target_value = Value::Object(id);
            if target(vm, &target_value).is_ok() {
                remove(vm, &target_value, serial)?;
            }
            Ok(())
        })?)
    };
    store_listener(vm, &table, slot, callback)?;
    if abort.is_some() {
        store_listener(vm, &table, DOM_TARGET_LISTENER_LIMIT + slot, signal)?;
    }
    data.free_slots.borrow_mut().pop();
    data.listeners.borrow_mut().push(Listener {
        serial,
        slot,
        kind,
        once,
        capture,
        passive,
        handler,
        _abort: abort,
    });
    if let Some(count) = &data.dom_count {
        count.set(count.get() + 1);
    }
    update_listener_liveness(vm, &object, &data)
}

pub(super) fn install_abort_handler(vm: &mut Vm, proto: Value) -> Result<(), JsError> {
    install_handler(vm, proto, "abort", abort::require_signal, |_, _| Ok(()))
}

pub(super) fn install_handler(
    vm: &mut Vm,
    proto: Value,
    event_type: &str,
    brand: impl Fn(&Vm, &Value) -> Result<(), JsError> + Clone + 'static,
    on_assign: impl Fn(&mut Vm, &Value) -> Result<(), JsError> + 'static,
) -> Result<(), JsError> {
    let name = format!("on{event_type}");
    let kind = JsString::from(event_type);
    let get_kind = kind.clone();
    let get_brand = brand.clone();
    let get = vm.new_native_function(&format!("get {name}"), move |vm, this, _| {
        get_brand(vm, &this)?;
        let (table, data) = target(vm, &this)?;
        let slot = data
            .listeners
            .borrow()
            .iter()
            .find(|listener| listener.handler && listener.kind == get_kind)
            .map(|listener| listener.slot);
        match slot {
            Some(slot) => listener_value(vm, &table, slot),
            None => Ok(Value::Null),
        }
    });
    let set = vm.new_native_function(&format!("set {name}"), move |vm, this, args| {
        brand(vm, &this)?;
        let value = arg(args, 0);
        let (table, data) = target(vm, &this)?;
        let existing = data
            .listeners
            .borrow()
            .iter()
            .find(|listener| listener.handler && listener.kind == kind)
            .map(|listener| (listener.slot, listener.serial));
        match (existing, vm.is_callable(&value)) {
            (Some((slot, _)), true) => store_listener(vm, &table, slot, value)?,
            (Some((_, serial)), false) => remove(vm, &this, serial)?,
            (None, true) => add(
                vm,
                this.clone(),
                (kind.clone(), value),
                Value::Undefined,
                true,
                None,
            )?,
            (None, false) => {}
        }
        on_assign(vm, &this)?;
        Ok(Value::Undefined)
    });
    vm.define_data_property(set.clone(), "length", 1.into(), false, false, true)?;
    vm.define_accessor(proto, &name, Some(get), Some(set))
}

pub(super) fn install(vm: &mut Vm, dom: Option<Shared>) -> Result<Events, JsError> {
    let implicit_receiver = dom.as_ref().map(|state| state.borrow().window.clone());
    install_with_receiver(vm, dom, implicit_receiver)
}

pub(super) fn install_worker(vm: &mut Vm) -> Result<Events, JsError> {
    let global = vm.global();
    install_with_receiver(vm, None, Some(global))
}

fn install_with_receiver(
    vm: &mut Vm,
    dom: Option<Shared>,
    implicit_receiver: Option<Value>,
) -> Result<Events, JsError> {
    let console = vm.get_global("console")?;
    let report = if console.as_object().is_some() {
        vm.get_property(console, "error")?
    } else {
        Value::Undefined
    };
    vm.root(report.clone());
    let exception = vm.get_global("DOMException")?;
    vm.root(exception.clone());
    let events = Events {
        target_proto: vm.new_object(),
        event_proto: vm.new_object(),
        report,
        exception,
        dom: dom.map(|state| DomTargets {
            state,
            listener_count: Rc::default(),
        }),
        implicit_receiver,
    };
    vm.root(events.target_proto.clone());
    vm.root(events.event_proto.clone());
    let proto = events.target_proto.clone();
    let target_constructor = webidl::interface(
        vm,
        proto.clone(),
        "EventTarget",
        0,
        move |vm, _, _, target| {
            let target = target.ok_or_else(|| JsError::type_error("EventTarget requires new"))?;
            let this = webidl::instance(vm, target, &proto)?;
            attach(vm, this.clone())?;
            Ok(this)
        },
    )?;
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    if let Some(dom) = &events.dom {
        let (node, window) = {
            let state = dom.state.borrow();
            (state.prototype.clone(), state.window.clone())
        };
        let window_proto = vm.new_object_with_prototype(events.target_proto.clone())?;
        let window_constructor =
            webidl::interface(vm, window_proto.clone(), "Window", 0, |_, _, _, _| {
                Err(JsError::type_error("Illegal Window constructor"))
            })?;
        vm.define_data_property(
            window.clone(),
            "Window",
            window_constructor.clone(),
            true,
            false,
            true,
        )?;
        vm.set_prototype(node.as_object().unwrap(), events.target_proto.as_object())?;
        vm.set_prototype(
            window_constructor.as_object().unwrap(),
            target_constructor.as_object(),
        )?;
        vm.set_prototype(window.as_object().unwrap(), window_proto.as_object())?;
        for object in [&window, &window_proto, &events.target_proto] {
            vm.make_prototype_immutable(object)?;
        }
    }
    let proto = events.target_proto.clone();
    let add_target = events.clone();
    webidl::operation(
        vm,
        proto.clone(),
        "addEventListener",
        2,
        move |vm, this, args| {
            let this = add_target.receiver(this);
            let dom_count = add_target
                .dom_receiver(&this)
                .map(|(dom, _)| dom.listener_count.clone());
            if dom_count.is_none() {
                target(vm, &this)?;
            }
            webidl::required(args, 2, "addEventListener")?;
            let event_type = kind(vm, &args[0])?;
            add(
                vm,
                this,
                (event_type, args[1].clone()),
                arg(args, 2),
                false,
                dom_count,
            )?;
            Ok(Value::Undefined)
        },
    )?;
    let remove_target = events.clone();
    webidl::operation(
        vm,
        proto.clone(),
        "removeEventListener",
        2,
        move |vm, this, args| {
            let this = remove_target.receiver(this);
            if remove_target.dom_receiver(&this).is_none() {
                target(vm, &this)?;
            }
            webidl::required(args, 2, "removeEventListener")?;
            let event_type = kind(vm, &args[0])?;
            let options = arg(args, 2);
            let capture = if options.as_object().is_some() {
                option(vm, &options, "capture")?.is_truthy()
            } else {
                options.is_truthy()
            };
            let Some((table, data)) = target_table(vm, &this) else {
                return Ok(Value::Undefined);
            };
            let matching = data
                .listeners
                .borrow()
                .iter()
                .filter(|listener| {
                    !listener.handler && listener.kind == event_type && listener.capture == capture
                })
                .map(|listener| (listener.slot, listener.serial))
                .collect::<Vec<_>>();
            for (slot, serial) in matching {
                if listener_value(vm, &table, slot)? == args[1] {
                    remove(vm, &this, serial)?;
                    break;
                }
            }
            Ok(Value::Undefined)
        },
    )?;
    let dispatch = events.clone();
    webidl::operation(vm, proto, "dispatchEvent", 1, move |vm, this, args| {
        let this = dispatch.receiver(this);
        let dom = dispatch.dom_receiver(&this);
        if dom.is_none() {
            target(vm, &this)?;
        }
        webidl::required(args, 1, "dispatchEvent")?;
        let info = event(vm, &args[0])?;
        if info.active.get() || !info.initialized.get() {
            return Err(abort::dom_exception_error(
                vm,
                dispatch.exception.clone(),
                "InvalidStateError",
                "The event is uninitialized or already being dispatched",
            )?);
        }
        info.trusted.set(false);
        let event = args[0].clone();
        if let Some((dom, node)) = dom {
            Ok(super::dispatch_prepared(vm, &dom.state, node, event, &[])?.into())
        } else {
            Ok(dispatch.dispatch(vm, this, event)?.into())
        }
    })?;
    let proto = events.event_proto.clone();
    let create = events.clone();
    let constructor =
        webidl::interface(vm, proto.clone(), "Event", 1, move |vm, _, args, target| {
            let target = target.ok_or_else(|| JsError::type_error("Event requires new"))?;
            webidl::required(args, 1, "Event")?;
            let event_type = kind(vm, &args[0])?;
            let flags = flags(vm, &arg(args, 1))?;
            create.make_event(vm, Some(target), event_type, flags, false)
        })?;
    for (name, phase) in [
        ("NONE", 0u32),
        ("CAPTURING_PHASE", 1),
        ("AT_TARGET", 2),
        ("BUBBLING_PHASE", 3),
    ] {
        for receiver in [proto.clone(), constructor.clone()] {
            vm.define_data_property(receiver, name, phase.into(), false, true, false)?;
        }
    }
    for name in [
        "type",
        "bubbles",
        "cancelable",
        "composed",
        "isTrusted",
        "defaultPrevented",
        "target",
        "srcElement",
        "currentTarget",
        "eventPhase",
    ] {
        getter(vm, proto.clone(), name, move |vm, this, _| {
            let info = event(vm, &this)?;
            Ok(match name {
                "type" => Value::String(info.kind.clone()),
                "bubbles" => info.bubbles.into(),
                "cancelable" => info.cancelable.into(),
                "composed" => info.composed.into(),
                "isTrusted" => info.trusted.get().into(),
                "defaultPrevented" => info.prevented.get().into(),
                "target" | "srcElement" => vm
                    .host_slot(&this, EVENT_TARGET_SLOT)
                    .unwrap_or(Value::Null),
                "currentTarget" => vm
                    .host_slot(&this, CURRENT_TARGET_SLOT)
                    .unwrap_or(Value::Null),
                _ => info.phase.get().into(),
            })
        })?;
    }
    webidl::operation(vm, proto.clone(), "composedPath", 0, |vm, this, _| {
        let path = event(vm, &this)?.path.borrow().clone();
        let targets = match path {
            Some(DispatchPath::Direct) => vec![vm.host_slot(&this, EVENT_TARGET_SLOT).unwrap()],
            Some(DispatchPath::Dom(dom, path)) => {
                let state = dom.upgrade().ok_or_else(|| {
                    JsError::new(ErrorKind::InternalError, "Event document is unavailable")
                })?;
                path.iter()
                    .map(|node| match node {
                        Some(id) => super::wrap(vm, &state, Some(*id)),
                        None => Ok(state.borrow().window.clone()),
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
            None => vec![],
        };
        vm.try_new_array(&targets)
    })?;
    webidl::operation(vm, proto.clone(), "initEvent", 1, |vm, this, args| {
        reinitialize_event(vm, this, args, "initEvent")?;
        Ok(Value::Undefined)
    })?;
    for name in ["cancelBubble", "returnValue"] {
        let get = vm.new_native_function(&format!("get {name}"), move |vm, this, _| {
            let data = event(vm, &this)?;
            Ok(if name == "cancelBubble" {
                data.stopped.get().into()
            } else {
                (!data.prevented.get()).into()
            })
        });
        let set = vm.new_native_function(&format!("set {name}"), move |vm, this, args| {
            let data = event(vm, &this)?;
            let value = arg(args, 0).is_truthy();
            if name == "cancelBubble" {
                if value {
                    data.stopped.set(true);
                }
            } else if !value && data.cancelable && !data.passive.get() {
                data.prevented.set(true);
            }
            Ok(Value::Undefined)
        });
        vm.define_accessor(proto.clone(), name, Some(get), Some(set))?;
    }
    for name in [
        "stopPropagation",
        "stopImmediatePropagation",
        "preventDefault",
    ] {
        webidl::operation(vm, proto.clone(), name, 0, move |vm, this, _| {
            let info = event(vm, &this)?;
            match name {
                "stopPropagation" => info.stopped.set(true),
                "stopImmediatePropagation" => {
                    info.stopped.set(true);
                    info.immediate.set(true);
                }
                _ if info.cancelable && !info.passive.get() => info.prevented.set(true),
                _ => {}
            }
            Ok(Value::Undefined)
        })?;
    }

    let custom_proto = vm.new_object_with_prototype(proto.clone())?;
    let intrinsic = custom_proto.clone();
    let custom = webidl::interface(
        vm,
        custom_proto.clone(),
        "CustomEvent",
        1,
        move |vm, _, args, target| {
            let target = target.ok_or_else(|| JsError::type_error("CustomEvent requires new"))?;
            webidl::required(args, 1, "CustomEvent")?;
            let kind = kind(vm, &args[0])?;
            let options = arg(args, 1);
            let flags = flags(vm, &options)?;
            let detail = option(vm, &options, "detail")?;
            let detail = if detail.is_undefined() {
                Value::Null
            } else {
                detail
            };
            let object = webidl::instance(vm, target, &intrinsic)?;
            let object = init_event(vm, object, kind, flags, false)?;
            vm.set_host_slot(object.clone(), CUSTOM_DETAIL_SLOT, detail)?;
            Ok(object)
        },
    )?;
    vm.call(
        set_prototype.clone(),
        Value::Undefined,
        &[custom, constructor.clone()],
    )?;
    getter(vm, custom_proto.clone(), "detail", |vm, this, _| {
        event(vm, &this)?;
        vm.host_slot(&this, CUSTOM_DETAIL_SLOT)
            .ok_or_else(|| JsError::type_error("Illegal CustomEvent receiver"))
    })?;
    webidl::operation(
        vm,
        custom_proto.clone(),
        "initCustomEvent",
        1,
        |vm, this, args| {
            if vm.host_slot(&this, CUSTOM_DETAIL_SLOT).is_none() {
                return Err(JsError::type_error("Illegal CustomEvent receiver"));
            }
            if reinitialize_event(vm, this.clone(), args, "initCustomEvent")? {
                let detail = match arg(args, 3) {
                    Value::Undefined => Value::Null,
                    value => value,
                };
                vm.set_host_slot(this, CUSTOM_DETAIL_SLOT, detail)?;
            }
            Ok(Value::Undefined)
        },
    )?;
    if let Some(dom) = &events.dom {
        let (ui_proto, mouse_proto) =
            super::ui_events::install(vm, &dom.state, proto, constructor, set_prototype)?;
        vm.root(ui_proto.clone());
        vm.root(mouse_proto.clone());
        dom.state.borrow_mut().mouse_event_prototype = mouse_proto.clone();
        let root = dom.state.borrow().document.root;
        let document = super::wrap(vm, &dom.state, Some(root))?;
        let state = dom.state.clone();
        let create = events.clone();
        vm.root(custom_proto.clone());
        webidl::operation(vm, document, "createEvent", 1, move |vm, this, args| {
            if super::node(&state, &this)? != root {
                return Err(JsError::type_error("Illegal Document receiver"));
            }
            webidl::required(args, 1, "createEvent")?;
            let interface = vm.to_string(&args[0])?;
            // The supported names are all ASCII and at most eleven code units.
            let name = if interface.len() <= 11 {
                interface.to_string_lossy().to_ascii_lowercase()
            } else {
                String::new()
            };
            let object = match name.as_str() {
                "event" | "events" | "htmlevents" => {
                    vm.new_object_with_prototype(create.event_proto.clone())?
                }
                "uievent" | "uievents" | "mouseevent" | "mouseevents" => {
                    let mouse = name.starts_with("mouse");
                    let proto = if mouse { &mouse_proto } else { &ui_proto };
                    let object = vm.new_object_with_prototype(proto.clone())?;
                    super::ui_events::initialize_empty(vm, &object, mouse)?;
                    object
                }
                "customevent" => {
                    let object = vm.new_object_with_prototype(custom_proto.clone())?;
                    vm.set_host_slot(object.clone(), CUSTOM_DETAIL_SLOT, Value::Null)?;
                    object
                }
                _ => {
                    return Err(abort::dom_exception_error(
                        vm,
                        create.exception.clone(),
                        "NotSupportedError",
                        "Unknown legacy event interface",
                    )?);
                }
            };
            let object = init_event(vm, object, JsString::default(), [false; 3], false)?;
            event(vm, &object)?.initialized.set(false);
            Ok(object)
        })?;
    }
    Ok(events)
}
