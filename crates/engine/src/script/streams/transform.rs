//! A transform's readable and writable endpoints share one backpressure gate.
use super::{
    Shared, Status, StreamHost, Value, Vm, arg, capability, clear_slot, custom, exception, getter,
    promise_record, settle, stream, stream_error, writable,
};
use crate::script::webidl::{instance, interface, operation};
use rust_browser_javascript::{ErrorKind, JsError, ObjectId};
use std::{cell::Cell, rc::Rc};

const READABLE: u32 = 0;
const WRITABLE: u32 = 1;
const CONTROLLER: u32 = 2;
const WRITABLE_CONTROLLER: u32 = 3;
const TRANSFORMER: u32 = 4;
const TRANSFORM: u32 = 5;
const FLUSH: u32 = 6;
const CANCEL: u32 = 7;
const BACKPRESSURE: u32 = 8;
const WAITING_CHUNK: u32 = 9;
const START: u32 = 10;
const FINISH: u32 = 11;
const FINISH_RECORD: u32 = 12;
const FINISH_REASON: u32 = 13;
const TRANSFORM_OWNER: u32 = 0;

struct Transform {
    id: ObjectId,
    backpressure: Cell<bool>,
}

struct Controller;

#[derive(Clone, Copy)]
enum Finish {
    Close,
    Abort,
    Cancel,
}

impl Transform {
    fn object(&self) -> Value {
        Value::Object(self.id)
    }

    fn get(&self, vm: &Vm, slot: u32) -> Value {
        vm.host_slot(&self.object(), slot).unwrap_or_default()
    }

    fn set(&self, vm: &mut Vm, slot: u32, value: Value) -> Result<(), JsError> {
        vm.set_host_slot(self.object(), slot, value)
    }

    fn readable(&self, vm: &Vm) -> Result<Shared, JsError> {
        stream(vm, &self.get(vm, READABLE))
    }

    fn set_backpressure(&self, vm: &mut Vm, backpressure: bool) -> Result<Value, JsError> {
        let old = self.get(vm, BACKPRESSURE);
        if !old.is_undefined() && self.backpressure.get() == backpressure {
            return Ok(promise_record(vm, &old));
        }
        let (promise, record) = capability(vm, &self.object())?;
        self.set(vm, BACKPRESSURE, record)?;
        self.backpressure.set(backpressure);
        if !old.is_undefined() {
            settle(vm, &old, Ok(Value::Undefined))?;
        }
        Ok(promise)
    }

    fn clear_transformer(&self, vm: &mut Vm) -> Result<(), JsError> {
        for key in [TRANSFORMER, TRANSFORM, FLUSH, CANCEL] {
            clear_slot(vm, &self.object(), key)?;
        }
        Ok(())
    }

    fn callback<F>(self: &Rc<Self>, vm: &mut Vm, name: &str, callback: F) -> Result<Value, JsError>
    where
        F: Fn(&mut Vm, &Rc<Transform>, &[Value]) -> Result<Value, JsError> + 'static,
    {
        let owner = Rc::downgrade(self);
        let function = vm.new_native_function(name, move |vm, _, args| {
            if let Some(state) = owner.upgrade() {
                callback(vm, &state, args)
            } else {
                Ok(Value::Undefined)
            }
        });
        vm.set_host_slot(function.clone(), TRANSFORM_OWNER, self.object())?;
        Ok(function)
    }
}

fn transform(vm: &Vm, value: &Value) -> Result<Rc<Transform>, JsError> {
    vm.host_data::<Transform>(value)
        .ok_or_else(|| JsError::type_error("Illegal TransformStream receiver"))
}

pub(super) fn endpoints(vm: &Vm, value: &Value) -> Result<(Value, Value), JsError> {
    let pair = transform(vm, value)?;
    Ok((pair.get(vm, READABLE), pair.get(vm, WRITABLE)))
}

fn controller_transform(vm: &Vm, controller: &Value) -> Result<Rc<Transform>, JsError> {
    vm.host_data::<Controller>(controller)
        .ok_or_else(|| JsError::type_error("Illegal TransformStreamDefaultController receiver"))?;
    transform(
        vm,
        &vm.host_slot(controller, TRANSFORM_OWNER)
            .unwrap_or_default(),
    )
}

fn callable(vm: &Vm, function: Value, name: &str) -> Result<Value, JsError> {
    if function.is_undefined() || vm.is_callable(&function) {
        Ok(function)
    } else {
        Err(JsError::type_error(format!("{name} must be callable")))
    }
}

struct Strategy {
    high_water_mark: f64,
    size: Value,
}

impl Strategy {
    fn convert(vm: &mut Vm, input: Value, high_water_mark: f64) -> Result<Self, JsError> {
        if input.is_nullish() {
            return Ok(Self {
                high_water_mark,
                size: Value::Undefined,
            });
        }
        if input.as_object().is_none() {
            return Err(JsError::type_error("QueuingStrategy must be an object"));
        }
        let high_water = vm.get_property(input.clone(), "highWaterMark")?;
        let high_water_mark = if high_water.is_undefined() {
            high_water_mark
        } else {
            vm.to_number(&high_water)?
        };
        let size = vm.get_property(input, "size")?;
        let size = callable(vm, size, "QueuingStrategy size")?;
        Ok(Self {
            high_water_mark,
            size,
        })
    }

    fn into_options(self, vm: &mut Vm) -> Result<Value, JsError> {
        if self.high_water_mark.is_nan() || self.high_water_mark < 0. {
            return Err(JsError::new(ErrorKind::RangeError, "Invalid highWaterMark"));
        }
        let object = vm.new_object_with_prototype(Value::Null)?;
        vm.set_property(object.clone(), "highWaterMark", self.high_water_mark.into())?;
        vm.set_property(object.clone(), "size", self.size)?;
        Ok(object)
    }
}

#[derive(Default)]
struct Transformer {
    start: Value,
    transform: Value,
    flush: Value,
    cancel: Value,
}

impl Transformer {
    fn convert(vm: &mut Vm, input: &Value) -> Result<Self, JsError> {
        if input.is_undefined() {
            return Ok(Self::default());
        }
        let cancel = vm.get_property(input.clone(), "cancel")?;
        let cancel = callable(vm, cancel, "Transformer cancel")?;
        let flush = vm.get_property(input.clone(), "flush")?;
        let flush = callable(vm, flush, "Transformer flush")?;
        let readable_type = vm.get_property(input.clone(), "readableType")?;
        let start = vm.get_property(input.clone(), "start")?;
        let start = callable(vm, start, "Transformer start")?;
        let transform = vm.get_property(input.clone(), "transform")?;
        let transform = callable(vm, transform, "Transformer transform")?;
        let writable_type = vm.get_property(input.clone(), "writableType")?;
        if !readable_type.is_undefined() || !writable_type.is_undefined() {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "Unsupported TransformStream type",
            ));
        }
        Ok(Self {
            start,
            transform,
            flush,
            cancel,
        })
    }
}

impl StreamHost {
    fn create_transform(
        &self,
        vm: &mut Vm,
        object: Value,
        controller_proto: Value,
        args: &[Value],
    ) -> Result<Value, JsError> {
        let receiver = arg(args, 0);
        if !receiver.is_undefined() && receiver.as_object().is_none() {
            return Err(JsError::type_error("Transformer must be an object"));
        }
        let writable_strategy = Strategy::convert(vm, arg(args, 1), 1.)?;
        let readable_strategy = Strategy::convert(vm, arg(args, 2), 0.)?;
        let transformer = Transformer::convert(vm, &receiver)?;
        let readable_strategy = readable_strategy.into_options(vm)?;
        let writable_strategy = writable_strategy.into_options(vm)?;

        let state = Rc::new(Transform {
            id: object.as_object().unwrap(),
            backpressure: Cell::new(true),
        });
        let controller = vm.new_object_with_prototype(controller_proto)?;
        vm.set_host_data(object.clone(), state.clone())?;
        vm.set_host_data(controller.clone(), Rc::new(Controller))?;
        vm.set_host_slot(controller.clone(), TRANSFORM_OWNER, object.clone())?;
        let (start, start_record) = capability(vm, &object)?;
        for (key, value) in [
            (CONTROLLER, controller.clone()),
            (TRANSFORMER, receiver.clone()),
            (TRANSFORM, transformer.transform),
            (FLUSH, transformer.flush),
            (CANCEL, transformer.cancel),
            (START, start),
        ] {
            state.set(vm, key, value)?;
        }
        state.set_backpressure(vm, true)?;

        let sink = self.transform_sink(vm, &state)?;
        let writable = self.0.writable.construct(vm, sink, writable_strategy)?;
        state.set(vm, WRITABLE, writable)?;
        let source = self.transform_source(vm, &state)?;
        let readable = self.create_custom(vm, source, readable_strategy, None)?;
        state.set(vm, READABLE, readable)?;

        let result = if transformer.start.is_undefined() {
            Value::Undefined
        } else {
            vm.call(transformer.start, receiver, &[controller])?
        };
        settle(vm, &start_record, Ok(result))?;
        clear_slot(vm, &object, START)?;
        Ok(object)
    }

    fn transform_sink(&self, vm: &mut Vm, state: &Rc<Transform>) -> Result<Value, JsError> {
        let sink = vm.new_object_with_prototype(Value::Null)?;
        let start = state.callback(vm, "start", |vm, state, args| {
            state.set(vm, WRITABLE_CONTROLLER, arg(args, 0))?;
            Ok(state.get(vm, START))
        })?;
        let host = self.clone();
        let write = state.callback(vm, "write", move |vm, state, args| {
            host.transform_write(vm, state, arg(args, 0))
        })?;
        let host = self.clone();
        let close = state.callback(vm, "close", move |vm, state, _| {
            host.finish_transform(vm, state, Finish::Close, Value::Undefined)
        })?;
        let host = self.clone();
        let abort = state.callback(vm, "abort", move |vm, state, args| {
            host.finish_transform(vm, state, Finish::Abort, arg(args, 0))
        })?;
        for (key, value) in [
            ("start", start),
            ("write", write),
            ("close", close),
            ("abort", abort),
        ] {
            vm.set_property(sink.clone(), key, value)?;
        }
        Ok(sink)
    }

    fn transform_source(&self, vm: &mut Vm, state: &Rc<Transform>) -> Result<Value, JsError> {
        let source = vm.new_object_with_prototype(Value::Null)?;
        vm.set_host_data(source.clone(), Rc::new(custom::NativeSource))?;
        let start = state.callback(vm, "start", |vm, state, _| Ok(state.get(vm, START)))?;
        let pull = state.callback(vm, "pull", |vm, state, _| state.set_backpressure(vm, false))?;
        let host = self.clone();
        let cancel = state.callback(vm, "cancel", move |vm, state, args| {
            host.finish_transform(vm, state, Finish::Cancel, arg(args, 0))
        })?;
        for (key, value) in [("start", start), ("pull", pull), ("cancel", cancel)] {
            vm.set_property(source.clone(), key, value)?;
        }
        Ok(source)
    }

    fn transform_enqueue(
        &self,
        vm: &mut Vm,
        state: &Transform,
        chunk: Value,
    ) -> Result<(), JsError> {
        let readable = state.readable(vm)?;
        if !custom::open(&readable) {
            return Err(JsError::type_error(
                "TransformStream readable cannot accept chunks",
            ));
        }
        if let Err(error) = self.enqueue_custom(vm, &readable, chunk) {
            let reason = exception(vm, error.clone())?;
            self.error_transform_writable(vm, state, reason)?;
            return Err(error);
        }
        if !state.backpressure.get() && custom::has_backpressure(&readable) {
            state.set_backpressure(vm, true)?;
        }
        Ok(())
    }

    fn error_transform_writable(
        &self,
        vm: &mut Vm,
        state: &Transform,
        reason: Value,
    ) -> Result<(), JsError> {
        state.clear_transformer(vm)?;
        writable::controller_error(vm, &state.get(vm, WRITABLE_CONTROLLER), reason)?;
        if state.backpressure.get() {
            state.set_backpressure(vm, false)?;
        }
        Ok(())
    }

    fn error_transform(
        &self,
        vm: &mut Vm,
        state: &Transform,
        reason: Value,
    ) -> Result<(), JsError> {
        self.fail(vm, &state.readable(vm)?, reason.clone())?;
        self.error_transform_writable(vm, state, reason)
    }

    fn transform_write(
        &self,
        vm: &mut Vm,
        state: &Rc<Transform>,
        chunk: Value,
    ) -> Result<Value, JsError> {
        if !state.backpressure.get() {
            return self.perform_transform(vm, state, chunk);
        }
        state.set(vm, WAITING_CHUNK, chunk)?;
        let host = self.clone();
        let resume = state.callback(vm, "", move |vm, state, _| {
            let chunk = state.get(vm, WAITING_CHUNK);
            clear_slot(vm, &state.object(), WAITING_CHUNK)?;
            if let Some(reason) = writable::stream_error(vm, &state.get(vm, WRITABLE))? {
                return Err(thrown(reason));
            }
            host.perform_transform(vm, state, chunk)
        })?;
        let pressure = state.get(vm, BACKPRESSURE);
        self.then(vm, promise_record(vm, &pressure), resume, Value::Undefined)
    }

    fn perform_transform(
        &self,
        vm: &mut Vm,
        state: &Rc<Transform>,
        chunk: Value,
    ) -> Result<Value, JsError> {
        let operation = state.get(vm, TRANSFORM);
        let result = if operation.is_undefined() {
            self.transform_enqueue(vm, state, chunk)
                .map(|_| Value::Undefined)
        } else {
            let receiver = state.get(vm, TRANSFORMER);
            let controller = state.get(vm, CONTROLLER);
            vm.call(operation, receiver, &[chunk, controller])
        };
        let promise = algorithm_promise(vm, result)?;
        let host = self.clone();
        let rejected = state.callback(vm, "", move |vm, state, args| {
            let reason = arg(args, 0);
            host.error_transform(vm, state, reason.clone())?;
            Err(thrown(reason))
        })?;
        let fulfilled = vm.new_native_function("", |_, _, _| Ok(Value::Undefined));
        self.then(vm, promise, fulfilled, rejected)
    }

    fn finish_transform(
        &self,
        vm: &mut Vm,
        state: &Rc<Transform>,
        finish: Finish,
        reason: Value,
    ) -> Result<Value, JsError> {
        let existing = state.get(vm, FINISH);
        if !existing.is_undefined() {
            return Ok(existing);
        }
        let (promise, record) = capability(vm, &state.object())?;
        state.set(vm, FINISH, promise.clone())?;
        state.set(vm, FINISH_RECORD, record)?;
        state.set(vm, FINISH_REASON, reason.clone())?;
        let (operation, argument) = match finish {
            Finish::Close => (state.get(vm, FLUSH), state.get(vm, CONTROLLER)),
            Finish::Abort | Finish::Cancel => (state.get(vm, CANCEL), reason),
        };
        let result = if operation.is_undefined() {
            Ok(Value::Undefined)
        } else {
            vm.call(operation, state.get(vm, TRANSFORMER), &[argument])
        };
        state.clear_transformer(vm)?;
        let done = algorithm_promise(vm, result)?;
        let host = self.clone();
        let fulfilled = state.callback(vm, "", move |vm, state, _| {
            host.finish_transform_result(vm, state, finish, None)?;
            Ok(Value::Undefined)
        })?;
        let host = self.clone();
        let rejected = state.callback(vm, "", move |vm, state, args| {
            host.finish_transform_result(vm, state, finish, Some(arg(args, 0)))?;
            Ok(Value::Undefined)
        })?;
        self.then(vm, done, fulfilled, rejected)?;
        Ok(promise)
    }

    fn finish_transform_result(
        &self,
        vm: &mut Vm,
        state: &Transform,
        finish: Finish,
        error: Option<Value>,
    ) -> Result<(), JsError> {
        let reason = state.get(vm, FINISH_REASON);
        let outcome = match finish {
            Finish::Cancel => {
                let existing = writable::errored(vm, &state.get(vm, WRITABLE))?;
                if error.is_some() || existing.is_none() {
                    self.error_transform_writable(vm, state, error.clone().unwrap_or(reason))?;
                }
                error.or(existing)
            }
            Finish::Close | Finish::Abort => {
                let readable = state.readable(vm)?;
                if let Some(error) = error {
                    self.fail(vm, &readable, error.clone())?;
                    Some(error)
                } else if readable.borrow().status == Status::Errored {
                    Some(stream_error(vm, &state.get(vm, READABLE)))
                } else {
                    match finish {
                        Finish::Close if custom::open(&readable) => self.close(vm, &readable)?,
                        Finish::Abort => self.fail(vm, &readable, reason)?,
                        _ => {}
                    }
                    None
                }
            }
        };
        let record = state.get(vm, FINISH_RECORD);
        clear_slot(vm, &state.object(), FINISH_RECORD)?;
        clear_slot(vm, &state.object(), FINISH_REASON)?;
        settle(vm, &record, outcome.map_or(Ok(Value::Undefined), Err))
    }
}

fn thrown(reason: Value) -> JsError {
    let mut error = JsError::new(ErrorKind::Thrown, "TransformStream algorithm rejected");
    error.thrown = Some(reason);
    error
}

fn algorithm_promise(vm: &mut Vm, result: Result<Value, JsError>) -> Result<Value, JsError> {
    match result {
        Ok(result) => {
            let promise = vm.new_promise_capability()?;
            vm.call(promise.resolve, Value::Undefined, &[result])?;
            Ok(promise.promise)
        }
        Err(error) => StreamHost::reject(vm, error),
    }
}

pub(super) fn install(vm: &mut Vm, host: &StreamHost) -> Result<(), JsError> {
    let prototype = vm.new_object();
    let controller = vm.new_object();
    let prototype_root = vm.root(prototype.clone());
    let controller_root = vm.root(controller.clone());
    let shared = host.clone();
    let constructor = interface(
        vm,
        prototype.clone(),
        "TransformStream",
        0,
        move |vm, _, args, new| {
            let new = new.ok_or_else(|| JsError::type_error("TransformStream requires new"))?;
            let prototype = vm.root_value(prototype_root).unwrap();
            let controller = vm.root_value(controller_root).unwrap();
            let object = instance(vm, new, &prototype)?;
            shared.create_transform(vm, object, controller, args)
        },
    )?;
    host.0.transform.set(Some(vm.root(constructor)));
    getter(vm, prototype.clone(), "readable", |vm, this, _| {
        Ok(transform(vm, &this)?.get(vm, READABLE))
    })?;
    getter(vm, prototype, "writable", |vm, this, _| {
        Ok(transform(vm, &this)?.get(vm, WRITABLE))
    })?;

    interface(
        vm,
        controller.clone(),
        "TransformStreamDefaultController",
        0,
        |_, _, _, _| {
            Err(JsError::type_error(
                "Illegal TransformStreamDefaultController constructor",
            ))
        },
    )?;
    getter(vm, controller.clone(), "desiredSize", |vm, this, _| {
        let readable = controller_transform(vm, &this)?.readable(vm)?;
        Ok(custom::desired_size(&readable))
    })?;
    let shared = host.clone();
    operation(
        vm,
        controller.clone(),
        "enqueue",
        0,
        move |vm, this, args| {
            let state = controller_transform(vm, &this)?;
            shared.transform_enqueue(vm, &state, arg(args, 0))?;
            Ok(Value::Undefined)
        },
    )?;
    let shared = host.clone();
    operation(vm, controller.clone(), "error", 0, move |vm, this, args| {
        let state = controller_transform(vm, &this)?;
        shared.error_transform(vm, &state, arg(args, 0))?;
        Ok(Value::Undefined)
    })?;
    let shared = host.clone();
    operation(vm, controller, "terminate", 0, move |vm, this, _| {
        let state = controller_transform(vm, &this)?;
        let readable = state.readable(vm)?;
        if custom::open(&readable) {
            shared.close(vm, &readable)?;
        }
        let reason = vm.error_value(&JsError::type_error("TransformStream was terminated"))?;
        shared.error_transform_writable(vm, &state, reason)?;
        Ok(Value::Undefined)
    })?;
    Ok(())
}
