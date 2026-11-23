use super::brand;
use crate::script::{arg, event_target, getter, webidl};
use rust_browser_javascript::{JsError, JsString, MAX_STRUCTURED_CLONE_TRANSFERS, Value, Vm};

const DATA: u32 = 12;
const ORIGIN: u32 = 13;
const LAST_EVENT_ID: u32 = 14;
const SOURCE: u32 = 15;
const PORTS: u32 = 16;
const ORIGIN_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct MessageEvents {
    prototype: Value,
    freeze: Value,
}

struct Fields {
    data: Value,
    origin: Value,
    last_event_id: Value,
    source: Value,
    ports: Value,
}

fn null_default(value: Value) -> Value {
    if value.is_undefined() {
        Value::Null
    } else {
        value
    }
}

fn last_event_id(vm: &mut Vm, value: Value) -> Result<Value, JsError> {
    if value.is_undefined() {
        Ok("".into())
    } else {
        Ok(vm.to_string(&value)?.into())
    }
}

fn origin(vm: &mut Vm, value: Value) -> Result<Value, JsError> {
    if value.is_undefined() {
        Ok("".into())
    } else {
        Ok(webidl::usv_string(vm, &value, ORIGIN_BYTES)?.into())
    }
}

fn source(vm: &Vm, value: Value) -> Result<Value, JsError> {
    if value.is_nullish() {
        Ok(Value::Null)
    } else if value == vm.global() || brand(vm, &value).is_ok() {
        Ok(value)
    } else {
        Err(JsError::type_error("Invalid MessageEvent source"))
    }
}

fn require_event(vm: &Vm, value: &Value) -> Result<std::rc::Rc<event_target::EventData>, JsError> {
    if vm.host_slot(value, DATA).is_none() {
        return Err(JsError::type_error("Illegal MessageEvent receiver"));
    }
    event_target::event(vm, value)
}

impl MessageEvents {
    fn freeze_ports(&self, vm: &mut Vm, ports: Value) -> Result<Value, JsError> {
        vm.call(self.freeze.clone(), Value::Undefined, &[ports])
    }

    fn convert_ports(&self, vm: &mut Vm, value: Value) -> Result<Value, JsError> {
        let ports = if value.is_undefined() {
            Vec::new()
        } else {
            if value.as_object().is_none() {
                return Err(JsError::type_error("Expected an iterable of MessagePorts"));
            }
            let method = webidl::iterator_method(vm, &value)?;
            webidl::sequence(
                vm,
                value,
                method,
                MAX_STRUCTURED_CLONE_TRANSFERS,
                |vm, port| {
                    brand(vm, &port)?;
                    Ok(port)
                },
            )?
        };
        let ports = vm.try_new_array(&ports)?;
        self.freeze_ports(vm, ports)
    }

    fn store(vm: &mut Vm, event: &Value, fields: Fields) -> Result<(), JsError> {
        for (index, value) in [
            (DATA, fields.data),
            (ORIGIN, fields.origin),
            (LAST_EVENT_ID, fields.last_event_id),
            (SOURCE, fields.source),
            (PORTS, fields.ports),
        ] {
            vm.set_host_slot(event.clone(), index, value)?;
        }
        Ok(())
    }

    fn read_options(&self, vm: &mut Vm, options: &Value) -> Result<Fields, JsError> {
        let data = null_default(event_target::option(vm, options, "data")?);
        let last = event_target::option(vm, options, "lastEventId")?;
        let last_event_id = last_event_id(vm, last)?;
        let event_origin = event_target::option(vm, options, "origin")?;
        let origin = origin(vm, event_origin)?;
        let ports = event_target::option(vm, options, "ports")?;
        let ports = self.convert_ports(vm, ports)?;
        let event_source = event_target::option(vm, options, "source")?;
        let source = source(vm, event_source)?;
        Ok(Fields {
            data,
            origin,
            last_event_id,
            source,
            ports,
        })
    }

    fn construct(&self, vm: &mut Vm, new_target: Value, args: &[Value]) -> Result<Value, JsError> {
        webidl::required(args, 1, "MessageEvent")?;
        let kind = event_target::kind(vm, &args[0])?;
        let options = arg(args, 1);
        let flags = event_target::flags(vm, &options)?;
        let fields = self.read_options(vm, &options)?;
        let object = webidl::instance(vm, new_target, &self.prototype)?;
        let event = event_target::init_event(vm, object, kind, flags, false)?;
        Self::store(vm, &event, fields)?;
        Ok(event)
    }

    fn initialize(&self, vm: &mut Vm, event: Value, args: &[Value]) -> Result<Value, JsError> {
        require_event(vm, &event)?;
        webidl::required(args, 1, "MessageEvent.initMessageEvent")?;
        let kind = event_target::kind(vm, &args[0])?;
        let bubbles = arg(args, 1).is_truthy();
        let cancelable = arg(args, 2).is_truthy();
        let data = null_default(arg(args, 3));
        let origin = origin(vm, arg(args, 4))?;
        let last_event_id = last_event_id(vm, arg(args, 5))?;
        let source = source(vm, arg(args, 6))?;
        let ports = self.convert_ports(vm, arg(args, 7))?;
        let old = require_event(vm, &event)?;
        if !old.active.get() {
            vm.set_host_slot(event.clone(), event_target::EVENT_TARGET_SLOT, Value::Null)?;
            Self::store(
                vm,
                &event,
                Fields {
                    data,
                    origin,
                    last_event_id,
                    source,
                    ports,
                },
            )?;
            event_target::init_event(vm, event, kind, [bubbles, cancelable, old.composed], false)?;
        }
        Ok(Value::Undefined)
    }

    pub(super) fn received(&self, vm: &mut Vm, payload: Value) -> Result<Value, JsError> {
        let data = vm.get_property(payload.clone(), "0")?;
        let ports = vm.get_property(payload, "1")?;
        let fields = Fields {
            data,
            origin: "".into(),
            last_event_id: "".into(),
            source: Value::Null,
            ports: self.freeze_ports(vm, ports)?,
        };
        let object = vm.new_object_with_prototype(self.prototype.clone())?;
        vm.mark_platform_object(&object)?;
        let event =
            event_target::init_event(vm, object, JsString::from("message"), [false; 3], true)?;
        Self::store(vm, &event, fields)?;
        Ok(event)
    }
}

pub(super) fn install(
    vm: &mut Vm,
    events: &event_target::Events,
    event_constructor: &Value,
    set_prototype: &Value,
    freeze: Value,
) -> Result<MessageEvents, JsError> {
    let prototype = vm.new_object_with_prototype(events.event_proto.clone())?;
    vm.root(prototype.clone());
    vm.root(freeze.clone());
    let events = MessageEvents {
        prototype: prototype.clone(),
        freeze,
    };
    let construction = events.clone();
    let constructor = webidl::interface(
        vm,
        prototype.clone(),
        "MessageEvent",
        1,
        move |vm, _, args, target| {
            let target = target.ok_or_else(|| JsError::type_error("MessageEvent requires new"))?;
            construction.construct(vm, target, args)
        },
    )?;
    vm.call(
        set_prototype.clone(),
        Value::Undefined,
        &[constructor, event_constructor.clone()],
    )?;
    for (name, index) in [
        ("data", DATA),
        ("origin", ORIGIN),
        ("lastEventId", LAST_EVENT_ID),
        ("source", SOURCE),
        ("ports", PORTS),
    ] {
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            require_event(vm, &this)?;
            Ok(vm
                .host_slot(&this, index)
                .expect("initialized MessageEvent slot"))
        })?;
    }
    let initialize = events.clone();
    webidl::operation(
        vm,
        prototype,
        "initMessageEvent",
        1,
        move |vm, this, args| initialize.initialize(vm, this, args),
    )?;
    Ok(events)
}
