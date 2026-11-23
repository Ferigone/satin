//! Web IDL entry points for the byte stream controller, reader, and request.
use super::super::{OWNER, arg, clear_slot, custom, getter, reader_stream_kind, stream};
use super::{
    ArrayBufferViewKind, JsError, REQUEST, Shared, Source, Status, StreamHost, Value, Vm, acquire,
    args_read, enqueue_view, head, incomplete_element, range_error, respond, unsigned_long_long,
};
use crate::script::webidl::{interface, operation};
use std::rc::Rc;

const VIEW: u32 = 0;

struct Controller;
struct Request;

pub(in crate::script::streams) fn controller(
    vm: &mut Vm,
    host: &StreamHost,
) -> Result<Value, JsError> {
    let prototype = vm.root_value(host.0.bytes.controller).unwrap();
    let controller = vm.new_object_with_prototype(prototype)?;
    vm.set_host_data(controller.clone(), Rc::new(Controller))?;
    Ok(controller)
}

fn controller_owner(vm: &Vm, controller: &Value) -> Result<Shared, JsError> {
    vm.host_data::<Controller>(controller)
        .ok_or_else(|| JsError::type_error("Illegal ReadableByteStreamController receiver"))?;
    stream(vm, &vm.host_slot(controller, OWNER).unwrap_or_default())
}

fn require_request(vm: &Vm, request: &Value) -> Result<(), JsError> {
    vm.host_data::<Request>(request)
        .ok_or_else(|| JsError::type_error("Illegal ReadableStreamBYOBRequest receiver"))?;
    Ok(())
}

fn request_owner(vm: &Vm, request: &Value) -> Result<Shared, JsError> {
    let owner = vm
        .host_slot(request, OWNER)
        .filter(|value| !value.is_undefined())
        .ok_or_else(|| JsError::type_error("The BYOB request is no longer active"))?;
    stream(vm, &owner)
}

pub(super) fn invalidate(vm: &mut Vm, request: &Value) -> Result<(), JsError> {
    vm.set_host_slot(request.clone(), VIEW, Value::Null)?;
    clear_slot(vm, request, OWNER)
}

fn get_request(vm: &mut Vm, host: &StreamHost, source: &Shared) -> Result<Value, JsError> {
    let object = source.borrow().value();
    if let Some(request) = vm
        .host_slot(&object, REQUEST)
        .filter(|value| !value.is_undefined())
    {
        return Ok(request);
    }
    let Some(pull) = head(source) else {
        return Ok(Value::Null);
    };
    let buffer = pull.buffer(vm, &object);
    let view = vm.new_array_buffer_view(
        &buffer,
        pull.offset()..pull.range.end,
        ArrayBufferViewKind::UINT8,
    )?;
    let prototype = vm.root_value(host.0.bytes.request).unwrap();
    let request = vm.new_object_with_prototype(prototype)?;
    vm.set_host_data(request.clone(), Rc::new(Request))?;
    vm.set_host_slot(request.clone(), VIEW, view)?;
    vm.set_host_slot(request.clone(), OWNER, object.clone())?;
    vm.set_host_slot(object, REQUEST, request.clone())?;
    Ok(request)
}

fn check_response(source: &Shared, count: u64) -> Result<super::PullInto, JsError> {
    let closed = source.borrow().status == Status::Closed;
    if (count == 0) != closed {
        return Err(JsError::type_error(
            "BYOB response size does not match the stream state",
        ));
    }
    let pull = head(source).expect("active BYOB request has a descriptor");
    if count > (pull.range.end - pull.offset()) as u64 {
        return Err(range_error("BYOB response exceeds the remaining view"));
    }
    Ok(pull)
}

pub(in crate::script::streams) fn install(vm: &mut Vm, host: &StreamHost) -> Result<(), JsError> {
    install_reader(vm, host)?;
    install_controller(vm, host)?;
    install_request(vm, host)
}

fn install_reader(vm: &mut Vm, host: &StreamHost) -> Result<(), JsError> {
    let prototype = vm.root_value(host.0.bytes.reader).unwrap();
    let shared = host.clone();
    interface(
        vm,
        prototype.clone(),
        "ReadableStreamBYOBReader",
        1,
        move |vm, _, args, target| {
            if target.is_none() {
                return Err(JsError::type_error("ReadableStreamBYOBReader requires new"));
            }
            let source = stream(vm, &arg(args, 0))?;
            acquire(&shared, vm, &source)
        },
    )?;
    getter(vm, prototype.clone(), "closed", |vm, this, _| {
        StreamHost::reader_closed(vm, &this, true)
    })?;
    let shared = host.clone();
    operation(
        vm,
        prototype.clone(),
        "read",
        1,
        move |vm, this, args| match args_read(&shared, vm, this, args) {
            Ok(promise) => Ok(promise),
            Err(error) => StreamHost::reject(vm, error),
        },
    )?;
    let shared = host.clone();
    operation(vm, prototype.clone(), "cancel", 0, move |vm, this, args| {
        let source = match reader_stream_kind(vm, &this, true) {
            Ok(Some(source)) => source,
            Ok(None) => {
                return StreamHost::reject(
                    vm,
                    JsError::type_error("ReadableStream reader was released"),
                );
            }
            Err(error) => return StreamHost::reject(vm, error),
        };
        shared.cancel(vm, &source, arg(args, 0))
    })?;
    let shared = host.clone();
    operation(vm, prototype, "releaseLock", 0, move |vm, this, _| {
        if let Some(source) = reader_stream_kind(vm, &this, true)? {
            shared.release(vm, &source, &this)?;
        }
        Ok(Value::Undefined)
    })?;
    Ok(())
}

fn install_controller(vm: &mut Vm, host: &StreamHost) -> Result<(), JsError> {
    let prototype = vm.root_value(host.0.bytes.controller).unwrap();
    interface(
        vm,
        prototype.clone(),
        "ReadableByteStreamController",
        0,
        |_, _, _, _| {
            Err(JsError::type_error(
                "Illegal ReadableByteStreamController constructor",
            ))
        },
    )?;
    let shared = host.clone();
    getter(vm, prototype.clone(), "byobRequest", move |vm, this, _| {
        let source = controller_owner(vm, &this)?;
        get_request(vm, &shared, &source)
    })?;
    getter(vm, prototype.clone(), "desiredSize", |vm, this, _| {
        let source = controller_owner(vm, &this)?;
        let state = source.borrow();
        Ok(match state.status {
            Status::Errored => Value::Null,
            Status::Closed => 0.into(),
            Status::Readable => match &state.source {
                Source::Custom(custom) => (custom.high_water - state.queue_size).into(),
                _ => 0.into(),
            },
        })
    })?;
    let shared = host.clone();
    operation(vm, prototype.clone(), "close", 0, move |vm, this, _| {
        let source = controller_owner(vm, &this)?;
        if !custom::open(&source) {
            return Err(JsError::type_error("ReadableStream cannot be closed"));
        }
        if source.borrow().queue.is_empty() && incomplete_element(&source) {
            let mut error = JsError::type_error("Byte stream closed with an incomplete element");
            let reason = vm.error_value(&error)?;
            shared.fail(vm, &source, reason.clone())?;
            error.thrown = Some(reason);
            return Err(error);
        }
        shared.close(vm, &source)?;
        Ok(Value::Undefined)
    })?;
    let shared = host.clone();
    operation(
        vm,
        prototype.clone(),
        "enqueue",
        1,
        move |vm, this, args| {
            let source = controller_owner(vm, &this)?;
            enqueue_view(&shared, vm, &source, arg(args, 0))?;
            Ok(Value::Undefined)
        },
    )?;
    let shared = host.clone();
    operation(vm, prototype, "error", 0, move |vm, this, args| {
        let source = controller_owner(vm, &this)?;
        shared.fail(vm, &source, arg(args, 0))?;
        Ok(Value::Undefined)
    })?;
    Ok(())
}

fn install_request(vm: &mut Vm, host: &StreamHost) -> Result<(), JsError> {
    let prototype = vm.root_value(host.0.bytes.request).unwrap();
    interface(
        vm,
        prototype.clone(),
        "ReadableStreamBYOBRequest",
        0,
        |_, _, _, _| {
            Err(JsError::type_error(
                "Illegal ReadableStreamBYOBRequest constructor",
            ))
        },
    )?;
    getter(vm, prototype.clone(), "view", |vm, this, _| {
        require_request(vm, &this)?;
        Ok(vm.host_slot(&this, VIEW).unwrap_or(Value::Null))
    })?;
    let shared = host.clone();
    operation(
        vm,
        prototype.clone(),
        "respond",
        1,
        move |vm, this, args| {
            require_request(vm, &this)?;
            let count = unsigned_long_long(vm, &arg(args, 0))?;
            let source = request_owner(vm, &this)?;
            let view =
                vm.fixed_array_buffer_view(&vm.host_slot(&this, VIEW).unwrap_or_default())?;
            check_response(&source, count)?;
            respond(&shared, vm, &source, view.buffer, count as usize)?;
            Ok(Value::Undefined)
        },
    )?;
    let shared = host.clone();
    operation(
        vm,
        prototype,
        "respondWithNewView",
        1,
        move |vm, this, args| {
            require_request(vm, &this)?;
            let view = vm.fixed_array_buffer_view(&arg(args, 0))?;
            let source = request_owner(vm, &this)?;
            let count = view.byte_range.len();
            let pull = check_response(&source, count as u64)?;
            if view.byte_range.start != pull.offset()
                || vm.array_buffer_byte_length(&view.buffer)? != pull.buffer_length
            {
                return Err(range_error(
                    "BYOB replacement view has the wrong buffer length or byte offset",
                ));
            }
            respond(&shared, vm, &source, view.buffer, count)?;
            Ok(Value::Undefined)
        },
    )?;
    Ok(())
}
