//! Default controllers for author-defined readable streams.
use super::{
    Controller, Item, Shared, Source as StreamSource, Status, StreamHost, Value, Vm, arg, bytes,
    clear_slot, exception, getter, method, stream, wants,
};
use crate::script::webidl::{instance, interface};
use rust_browser_javascript::{ErrorKind, JsError};
use std::rc::Rc;

const SOURCE: u32 = 12;
const PULL: u32 = 13;
const CANCEL: u32 = 14;
const SIZE: u32 = 15;
const CONTROLLER: u32 = 16;

pub(super) struct Source {
    pub(super) high_water: f64,
    started: bool,
    pulling: bool,
    pull_again: bool,
}

fn owner(vm: &Vm, controller: &Value) -> Result<Shared, JsError> {
    vm.host_data::<Controller>(controller)
        .ok_or_else(|| JsError::type_error("Illegal ReadableStreamDefaultController receiver"))?;
    let owner = vm.host_slot(controller, super::OWNER).unwrap_or_default();
    stream(vm, &owner)
}

fn dictionary(value: &Value) -> Result<(), JsError> {
    if value.is_nullish() || value.as_object().is_some() {
        return Ok(());
    }
    Err(JsError::type_error(
        "ReadableStream options must be an object",
    ))
}

fn field(vm: &mut Vm, input: &Value, name: &str) -> Result<Value, JsError> {
    if input.is_nullish() {
        return Ok(Value::Undefined);
    }
    vm.get_property(input.clone(), name)
}

fn function(vm: &mut Vm, input: &Value, name: &str) -> Result<Value, JsError> {
    let value = field(vm, input, name)?;
    if !value.is_undefined() && !vm.is_callable(&value) {
        return Err(JsError::type_error(format!(
            "ReadableStream {name} must be callable"
        )));
    }
    Ok(value)
}

pub(super) fn open(source: &Shared) -> bool {
    let state = source.borrow();
    state.status == Status::Readable && !state.closing
}

pub(super) fn clear(vm: &mut Vm, object: &Value) -> Result<(), JsError> {
    for slot in [SOURCE, PULL, CANCEL, SIZE, CONTROLLER] {
        clear_slot(vm, object, slot)?;
    }
    Ok(())
}

pub(super) fn cancel_callback(vm: &Vm, object: &Value) -> Option<(Value, Value)> {
    let receiver = vm.host_slot(object, SOURCE)?;
    let callback = vm.host_slot(object, CANCEL)?;
    vm.is_callable(&callback).then_some((receiver, callback))
}

impl StreamHost {
    pub(super) fn create_custom(
        &self,
        vm: &mut Vm,
        underlying: Value,
        strategy: Value,
        new_target: Option<Value>,
    ) -> Result<Value, JsError> {
        if !underlying.is_undefined() && underlying.as_object().is_none() {
            return Err(JsError::type_error(
                "ReadableStream source must be an object",
            ));
        }
        dictionary(&strategy)?;
        let high_water = field(vm, &strategy, "highWaterMark")?;
        let size = function(vm, &strategy, "size")?;
        let auto_allocate = field(vm, &underlying, "autoAllocateChunkSize")?;
        let auto_allocate = if auto_allocate.is_undefined() {
            None
        } else {
            Some(bytes::unsigned_long_long(vm, &auto_allocate)?)
        };
        let cancel = function(vm, &underlying, "cancel")?;
        let pull = function(vm, &underlying, "pull")?;
        let start = function(vm, &underlying, "start")?;
        let kind = field(vm, &underlying, "type")?;
        let is_bytes = !kind.is_undefined();
        if is_bytes && super::super::text(vm, kind)? != "bytes" {
            return Err(JsError::type_error(
                "Invalid ReadableStream underlying source type",
            ));
        }
        if is_bytes && !size.is_undefined() {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "A byte stream cannot have a chunk size algorithm",
            ));
        }
        let high_water = if high_water.is_undefined() {
            if is_bytes { 0. } else { 1. }
        } else {
            vm.to_number(&high_water)?
        };
        if high_water.is_nan() || high_water < 0. {
            return Err(JsError::new(ErrorKind::RangeError, "Invalid highWaterMark"));
        }
        if is_bytes && auto_allocate == Some(0) {
            return Err(JsError::type_error(
                "autoAllocateChunkSize must be positive",
            ));
        }
        let source = StreamSource::Custom(Source {
            high_water,
            started: false,
            pulling: false,
            pull_again: false,
        });
        let object = if let Some(target) = new_target {
            let intrinsic = vm.root_value(self.0.stream).unwrap();
            let object = instance(vm, target, &intrinsic)?;
            Self::initialize_object(vm, object, source, is_bytes, 0)?
        } else {
            self.object(vm, source, is_bytes, 0)?
        };
        let source = stream(vm, &object)?;
        let controller = if is_bytes {
            source.borrow_mut().auto_allocate = auto_allocate;
            bytes::controller(vm, self)?
        } else {
            let controller =
                vm.new_object_with_prototype(vm.root_value(self.0.controller).unwrap())?;
            vm.set_host_data(controller.clone(), Rc::new(Controller))?;
            controller
        };
        vm.set_host_slot(controller.clone(), super::OWNER, object.clone())?;
        for (slot, value) in [
            (SOURCE, underlying.clone()),
            (PULL, pull),
            (CANCEL, cancel),
            (SIZE, size),
            (CONTROLLER, controller.clone()),
        ] {
            vm.set_host_slot(object.clone(), slot, value)?;
        }
        let result = if start.is_undefined() {
            Value::Undefined
        } else {
            vm.call(start, underlying, &[controller])?
        };
        self.observe_custom(vm, &source, result, true)?;
        Ok(object)
    }

    pub(super) fn maybe_pull(&self, vm: &mut Vm, source: &Shared) -> Result<(), JsError> {
        let (object, callback) = {
            let mut state = source.borrow_mut();
            if state.status != Status::Readable || state.closing {
                return Ok(());
            }
            let object = state.value();
            let demand = wants(&state);
            let pending = state.queue_size;
            let StreamSource::Custom(custom) = &mut state.source else {
                return Ok(());
            };
            if !custom.started || !(demand || pending < custom.high_water) {
                return Ok(());
            }
            let Some(callback) = vm.host_slot(&object, PULL).filter(|v| vm.is_callable(v)) else {
                return Ok(());
            };
            if custom.pulling {
                custom.pull_again = true;
                return Ok(());
            }
            custom.pulling = true;
            // The callback can reenter through its controller. Its pull flag
            // prevents overlapping calls while allowing synchronous enqueue.
            state.driving = false;
            (object, callback)
        };
        let receiver = vm.host_slot(&object, SOURCE).unwrap_or_default();
        let controller = vm.host_slot(&object, CONTROLLER).unwrap_or_default();
        match vm.call(callback, receiver, &[controller]) {
            Ok(result) => self.observe_custom(vm, source, result, false),
            Err(error) => {
                let reason = exception(vm, error)?;
                self.fail(vm, source, reason)
            }
        }
    }

    fn observe_custom(
        &self,
        vm: &mut Vm,
        source: &Shared,
        result: Value,
        starting: bool,
    ) -> Result<(), JsError> {
        let object = source.borrow().value();
        let internal = vm
            .host_slot(&object, SOURCE)
            .is_some_and(|receiver| vm.host_data::<NativeSource>(&receiver).is_some());
        let promise = if internal {
            result
        } else {
            let capability = vm.new_promise_capability()?;
            vm.call(capability.resolve, Value::Undefined, &[result])?;
            capability.promise
        };
        let owner = Rc::downgrade(source);
        let on_fulfilled = owner.clone();
        let host = self.clone();
        let done = vm.new_native_function("", move |vm, _, _| {
            if let Some(source) = on_fulfilled.upgrade() {
                let reschedule = {
                    let mut state = source.borrow_mut();
                    let StreamSource::Custom(custom) = &mut state.source else {
                        return Ok(Value::Undefined);
                    };
                    if starting {
                        custom.started = true;
                        true
                    } else {
                        custom.pulling = false;
                        std::mem::take(&mut custom.pull_again)
                    }
                };
                if reschedule {
                    host.advance(vm, &source)?;
                }
            }
            Ok(Value::Undefined)
        });
        let host = self.clone();
        let failed = vm.new_native_function("", move |vm, _, args| {
            if let Some(source) = owner.upgrade() {
                host.fail(vm, &source, arg(args, 0))?;
            }
            Ok(Value::Undefined)
        });
        vm.set_host_slot(done.clone(), super::OWNER, object.clone())?;
        vm.set_host_slot(failed.clone(), super::OWNER, object)?;
        self.then(vm, promise, done, failed)?;
        Ok(())
    }
}

/// Marks an internal underlying source with promise-returning native algorithms.
pub(super) struct NativeSource;

pub(super) fn desired_size(source: &Shared) -> Value {
    let state = source.borrow();
    match state.status {
        Status::Errored => Value::Null,
        Status::Closed => 0.into(),
        Status::Readable => match &state.source {
            StreamSource::Custom(custom) => (custom.high_water - state.queue_size).into(),
            _ => 0.into(),
        },
    }
}

pub(super) fn has_backpressure(source: &Shared) -> bool {
    let state = source.borrow();
    !(state.status == Status::Readable
        && !state.closing
        && matches!(&state.source, StreamSource::Custom(custom)
            if custom.started && (wants(&state) || state.queue_size < custom.high_water)))
}

impl StreamHost {
    pub(super) fn enqueue_custom(
        &self,
        vm: &mut Vm,
        source: &Shared,
        item: Value,
    ) -> Result<(), JsError> {
        if !open(source) {
            return Err(JsError::type_error("ReadableStream cannot accept chunks"));
        }
        let (object, demand) = {
            let state = source.borrow();
            (state.value(), wants(&state))
        };
        if demand {
            self.deliver(vm, source, Item::Js(item))?;
        } else {
            let size = vm.host_slot(&object, SIZE).unwrap_or_default();
            let result = (|| {
                let weight = if size.is_undefined() {
                    1.
                } else {
                    let weight = vm.call(size, Value::Undefined, std::slice::from_ref(&item))?;
                    vm.to_number(&weight)?
                };
                if !weight.is_finite() || weight < 0. {
                    return Err(JsError::new(
                        ErrorKind::RangeError,
                        "Invalid ReadableStream chunk size",
                    ));
                }
                // A size callback can close or error the stream. A chunk
                // added to a terminal queue can never be delivered.
                if source.borrow().status == Status::Readable {
                    self.enqueue(vm, source, Item::Js(item), weight)?;
                }
                Ok(())
            })();
            if let Err(mut error) = result {
                let reason = exception(vm, error.clone())?;
                self.fail(vm, source, reason.clone())?;
                error.thrown = Some(reason);
                return Err(error);
            }
        }
        self.maybe_pull(vm, source)
    }
}

pub(super) fn install(vm: &mut Vm, host: &StreamHost, prototype: Value) -> Result<(), JsError> {
    interface(
        vm,
        prototype.clone(),
        "ReadableStreamDefaultController",
        0,
        |_, _, _, _| {
            Err(JsError::type_error(
                "Illegal ReadableStreamDefaultController constructor",
            ))
        },
    )?;
    getter(vm, prototype.clone(), "desiredSize", |vm, receiver, _| {
        Ok(desired_size(&owner(vm, &receiver)?))
    })?;
    let shared = host.clone();
    method(vm, prototype.clone(), "close", move |vm, receiver, _| {
        let source = owner(vm, &receiver)?;
        if !open(&source) {
            return Err(JsError::type_error("ReadableStream cannot be closed"));
        }
        shared.close(vm, &source)?;
        Ok(Value::Undefined)
    })?;
    let shared = host.clone();
    method(vm, prototype.clone(), "error", move |vm, receiver, args| {
        let source = owner(vm, &receiver)?;
        shared.fail(vm, &source, arg(args, 0))?;
        Ok(Value::Undefined)
    })?;
    let shared = host.clone();
    method(vm, prototype, "enqueue", move |vm, receiver, args| {
        let source = owner(vm, &receiver)?;
        shared.enqueue_custom(vm, &source, arg(args, 0))?;
        Ok(Value::Undefined)
    })?;
    Ok(())
}
