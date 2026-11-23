//! Writable streams for author-defined sinks and native stream composition.
mod algorithms;
mod strategies;
#[cfg(test)]
mod tests;

use super::{
    OWNER, StreamHost, capability, clear_slot, exception, promise_record, put, settle, take,
};
use crate::script::{abort, arg, webidl};
use algorithms::{Operation, advance, clear_algorithms, observe, start_erroring};
use rust_browser_javascript::{ErrorKind, JsError, ObjectId, RootId, Value, Vm};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

const CONTROLLER: u32 = 1;
const WRITER: u32 = 2;
const ERROR: u32 = 4;
const SINK: u32 = 5;
const WRITE: u32 = 6;
const CLOSE: u32 = 7;
const ABORT: u32 = 8;
const SIZE: u32 = 9;
const CLOSE_REQUEST: u32 = 10;
const ABORT_REQUEST: u32 = 11;
const SINK_PROMISE: u32 = 12;
const CHUNK: u32 = 13;
const SIGNAL: u32 = 14;
const READY: u32 = 15;
const CLOSED: u32 = 16;
const MAX_WRITES: usize = 1024;

type Shared = Rc<RefCell<State>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Writable,
    Erroring,
    Errored,
    Closed,
}

#[derive(Clone, Copy)]
enum PendingAbort {
    StartedErroring,
    AlreadyErroring,
}

struct State {
    id: ObjectId,
    abort: abort::AbortHost,
    native_sink: bool,
    status: Status,
    started: bool,
    active: bool,
    closing: bool,
    abort_pending: Option<PendingAbort>,
    backpressure: bool,
    high_water: f64,
    queue: VecDeque<f64>,
    queue_head: usize,
    queue_size: f64,
}

impl State {
    fn value(&self) -> Value {
        Value::Object(self.id)
    }
}

struct Controller;
struct Writer;

struct Intrinsics {
    stream: RootId,
    writer: RootId,
    controller: RootId,
    abort: abort::AbortHost,
}

#[derive(Clone)]
pub(super) struct Host(Rc<Intrinsics>);

fn stream(vm: &Vm, value: &Value) -> Result<Shared, JsError> {
    vm.host_data::<RefCell<State>>(value)
        .ok_or_else(|| JsError::type_error("Expected a WritableStream"))
}

pub(super) fn is_stream(vm: &Vm, value: &Value) -> bool {
    stream(vm, value).is_ok()
}

fn slot(vm: &Vm, value: &Value, index: u32) -> Value {
    vm.host_slot(value, index).unwrap_or_default()
}

pub(super) fn locked(vm: &Vm, value: &Value) -> Result<bool, JsError> {
    stream(vm, value)?;
    Ok(!slot(vm, value, WRITER).is_undefined())
}

pub(super) fn closing_or_closed(vm: &Vm, value: &Value) -> Result<bool, JsError> {
    let stream = stream(vm, value)?;
    let state = stream.borrow();
    Ok(state.closing || state.status == Status::Closed)
}

pub(super) fn can_write(vm: &Vm, value: &Value) -> Result<bool, JsError> {
    let stream = stream(vm, value)?;
    let state = stream.borrow();
    Ok(!state.closing && state.status == Status::Writable)
}

pub(super) fn is_writable(vm: &Vm, value: &Value) -> Result<bool, JsError> {
    Ok(stream(vm, value)?.borrow().status == Status::Writable)
}

pub(super) fn stream_error(vm: &Vm, value: &Value) -> Result<Option<Value>, JsError> {
    let status = stream(vm, value)?.borrow().status;
    Ok(matches!(status, Status::Erroring | Status::Errored).then(|| slot(vm, value, ERROR)))
}

pub(super) fn errored(vm: &Vm, value: &Value) -> Result<Option<Value>, JsError> {
    let status = stream(vm, value)?.borrow().status;
    Ok((status == Status::Errored).then(|| slot(vm, value, ERROR)))
}

fn writer_brand(vm: &Vm, writer: &Value) -> Result<(), JsError> {
    vm.host_data::<Writer>(writer)
        .map(|_| ())
        .ok_or_else(|| JsError::type_error("Illegal WritableStreamDefaultWriter receiver"))
}

fn writer_stream(vm: &Vm, writer: &Value) -> Result<Shared, JsError> {
    writer_brand(vm, writer)?;
    let owner = slot(vm, writer, OWNER);
    if owner.is_undefined() {
        return Err(released_writer());
    }
    stream(vm, &owner)
}

fn released_writer() -> JsError {
    JsError::type_error("WritableStream writer was released")
}

fn controller_stream(vm: &Vm, controller: &Value) -> Result<Shared, JsError> {
    vm.host_data::<Controller>(controller)
        .ok_or_else(|| JsError::type_error("Illegal WritableStreamDefaultController receiver"))?;
    stream(vm, &slot(vm, controller, OWNER))
}

pub(super) fn controller_signal(vm: &Vm, controller: &Value) -> Result<Value, JsError> {
    controller_stream(vm, controller)?;
    Ok(slot(vm, controller, SIGNAL))
}

pub(super) fn controller_error(
    vm: &mut Vm,
    controller: &Value,
    reason: Value,
) -> Result<(), JsError> {
    let stream = controller_stream(vm, controller)?;
    error_if_writable(vm, &stream, reason)
}

fn error_if_writable(vm: &mut Vm, stream: &Shared, reason: Value) -> Result<(), JsError> {
    if stream.borrow().status == Status::Writable {
        clear_algorithms(vm, &stream.borrow().value())?;
        start_erroring(vm, stream, reason)?;
    }
    Ok(())
}

fn settle_with(vm: &mut Vm, result: Result<Value, Value>) -> Result<Value, JsError> {
    let cap = vm.new_promise_capability()?;
    let (callback, argument) = match result {
        Ok(value) => (cap.resolve, value),
        Err(reason) => (cap.reject, reason),
    };
    vm.call(callback, Value::Undefined, &[argument])?;
    Ok(cap.promise)
}

fn writer_promise(vm: &mut Vm, writer: &Value, index: u32) -> Result<Value, JsError> {
    let (promise, record) = capability(vm, writer)?;
    vm.set_host_slot(writer.clone(), index, record)?;
    Ok(promise)
}

fn settle_writer(
    vm: &mut Vm,
    writer: &Value,
    index: u32,
    result: Result<Value, Value>,
) -> Result<(), JsError> {
    let record = slot(vm, writer, index);
    let promise = promise_record(vm, &record);
    if result.is_err() {
        vm.mark_promise_handled(&promise)?;
    }
    settle(vm, &record, result)
}

fn reject_writer(vm: &mut Vm, writer: &Value, index: u32, reason: Value) -> Result<(), JsError> {
    let promise = promise_record(vm, &slot(vm, writer, index));
    if !vm.is_promise_pending(&promise) {
        writer_promise(vm, writer, index)?;
    }
    settle_writer(vm, writer, index, Err(reason))
}

pub(super) fn writer_ready(vm: &Vm, writer: &Value) -> Result<Value, JsError> {
    writer_brand(vm, writer)?;
    Ok(promise_record(vm, &slot(vm, writer, READY)))
}

pub(super) fn writer_closed(vm: &Vm, writer: &Value) -> Result<Value, JsError> {
    writer_brand(vm, writer)?;
    Ok(promise_record(vm, &slot(vm, writer, CLOSED)))
}

pub(super) fn writer_write(vm: &mut Vm, writer: Value, chunk: Value) -> Result<Value, JsError> {
    let source = match writer_stream(vm, &writer) {
        Ok(source) => source,
        Err(error) => return StreamHost::reject(vm, error),
    };
    let object = source.borrow().value();
    let size = slot(vm, &object, SIZE);
    let size = if size.is_undefined() {
        1.0
    } else {
        let size = vm.call(size, Value::Undefined, std::slice::from_ref(&chunk));
        match size.and_then(|size| vm.to_number(&size)) {
            Ok(size) => size,
            Err(error) => {
                let reason = exception(vm, error)?;
                error_if_writable(vm, &source, reason)?;
                1.0
            }
        }
    };
    // Size conversion can invoke author code that closes or unlocks this stream.
    if slot(vm, &writer, OWNER) != object {
        return StreamHost::reject(vm, released_writer());
    }
    let (status, closing, len, head) = {
        let source = source.borrow();
        (
            source.status,
            source.closing,
            source.queue.len(),
            source.queue_head,
        )
    };
    if status == Status::Errored {
        return settle_with(vm, Err(slot(vm, &object, ERROR)));
    }
    if closing || status == Status::Closed {
        return StreamHost::reject(
            vm,
            JsError::type_error("WritableStream is closing or closed"),
        );
    }
    if status == Status::Erroring {
        return settle_with(vm, Err(slot(vm, &object, ERROR)));
    }
    if len == MAX_WRITES {
        return StreamHost::reject(
            vm,
            JsError::type_error("WritableStream write queue limit exceeded"),
        );
    }
    let valid_size = size.is_finite() && size >= 0.0;
    let (promise, record) = capability(vm, &object)?;
    vm.set_host_slot(record.clone(), CHUNK, chunk)?;
    put(vm, &object, &write_key((head + len) % MAX_WRITES), record)?;
    {
        let mut state = source.borrow_mut();
        let size = if valid_size { size } else { 0.0 };
        state.queue.push_back(size);
        state.queue_size += size;
    }
    if valid_size {
        algorithms::update_backpressure(vm, &source)?;
        advance(vm, &source)?;
    } else {
        let reason = vm.error_value(&JsError::new(
            ErrorKind::RangeError,
            "Invalid WritableStream chunk size",
        ))?;
        error_if_writable(vm, &source, reason)?;
    }
    Ok(promise)
}

fn write_key(index: usize) -> String {
    index.to_string()
}

fn close_stream(vm: &mut Vm, source: &Shared) -> Result<Value, JsError> {
    let (object, status, closing, backpressure) = {
        let state = source.borrow();
        (
            state.value(),
            state.status,
            state.closing,
            state.backpressure,
        )
    };
    if closing || matches!(status, Status::Closed | Status::Errored) {
        return StreamHost::reject(vm, JsError::type_error("WritableStream cannot be closed"));
    }
    let (promise, record) = capability(vm, &object)?;
    vm.set_host_slot(object.clone(), CLOSE_REQUEST, record)?;
    source.borrow_mut().closing = true;
    let writer = slot(vm, &object, WRITER);
    if status == Status::Writable && backpressure && !writer.is_undefined() {
        settle_writer(vm, &writer, READY, Ok(Value::Undefined))?;
    }
    advance(vm, source)?;
    Ok(promise)
}

pub(super) fn abort_stream(vm: &mut Vm, object: Value, reason: Value) -> Result<Value, JsError> {
    let source = stream(vm, &object)?;
    let (status, abort) = {
        let state = source.borrow();
        (state.status, state.abort.clone())
    };
    if matches!(status, Status::Closed | Status::Errored) {
        return settle_with(vm, Ok(Value::Undefined));
    }
    let control = slot(vm, &object, CONTROLLER);
    abort.abort(vm, slot(vm, &control, SIGNAL), reason.clone())?;
    // Dispatching the signal event can synchronously change state.
    let (status, pending) = {
        let state = source.borrow();
        (state.status, state.abort_pending.is_some())
    };
    if matches!(status, Status::Closed | Status::Errored) {
        return settle_with(vm, Ok(Value::Undefined));
    }
    if pending {
        return Ok(promise_record(vm, &slot(vm, &object, ABORT_REQUEST)));
    }
    let (promise, record) = capability(vm, &object)?;
    vm.set_host_slot(object, ABORT_REQUEST, record)?;
    match status {
        Status::Erroring => source.borrow_mut().abort_pending = Some(PendingAbort::AlreadyErroring),
        Status::Writable => {
            source.borrow_mut().abort_pending = Some(PendingAbort::StartedErroring);
            start_erroring(vm, &source, reason)?;
        }
        Status::Closed | Status::Errored => unreachable!(),
    }
    Ok(promise)
}

pub(super) fn writer_close(vm: &mut Vm, writer: Value) -> Result<Value, JsError> {
    match writer_stream(vm, &writer) {
        Ok(source) => close_stream(vm, &source),
        Err(error) => StreamHost::reject(vm, error),
    }
}

pub(super) fn writer_abort(vm: &mut Vm, writer: Value, reason: Value) -> Result<Value, JsError> {
    let source = match writer_stream(vm, &writer) {
        Ok(source) => source,
        Err(error) => return StreamHost::reject(vm, error),
    };
    let object = source.borrow().value();
    abort_stream(vm, object, reason)
}

pub(super) fn writer_release(vm: &mut Vm, writer: &Value) -> Result<(), JsError> {
    writer_brand(vm, writer)?;
    let owner = slot(vm, writer, OWNER);
    if owner.is_undefined() {
        return Ok(());
    }
    let reason = vm.error_value(&released_writer())?;
    reject_writer(vm, writer, READY, reason.clone())?;
    reject_writer(vm, writer, CLOSED, reason)?;
    clear_slot(vm, &owner, WRITER)?;
    clear_slot(vm, writer, OWNER)
}

fn writer_desired_size(vm: &Vm, writer: &Value) -> Result<Value, JsError> {
    let source = writer_stream(vm, writer)?;
    let state = source.borrow();
    Ok(match state.status {
        Status::Writable => (state.high_water - state.queue_size).into(),
        Status::Closed => 0.into(),
        Status::Erroring | Status::Errored => Value::Null,
    })
}

fn callback(vm: &mut Vm, input: &Value, name: &str) -> Result<Value, JsError> {
    let value = field(vm, input, name)?;
    if !value.is_undefined() && !vm.is_callable(&value) {
        return Err(JsError::type_error(format!(
            "WritableStream {name} must be callable"
        )));
    }
    Ok(value)
}

fn field(vm: &mut Vm, input: &Value, name: &str) -> Result<Value, JsError> {
    if input.is_nullish() {
        Ok(Value::Undefined)
    } else {
        vm.get_property(input.clone(), name)
    }
}

fn accessor(
    vm: &mut Vm,
    prototype: Value,
    name: &str,
    f: impl Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
) -> Result<(), JsError> {
    let get = vm.new_native_function(&format!("get {name}"), f);
    vm.define_accessor(prototype, name, Some(get), None)
}

impl Host {
    pub(super) fn install(vm: &mut Vm, abort: abort::AbortHost) -> Result<Self, JsError> {
        let stream_proto = vm.new_object();
        let writer_proto = vm.new_object();
        let controller_proto = vm.new_object();
        let host = Self(Rc::new(Intrinsics {
            stream: vm.root(stream_proto.clone()),
            writer: vm.root(writer_proto.clone()),
            controller: vm.root(controller_proto.clone()),
            abort,
        }));
        let shared = host.clone();
        webidl::interface(
            vm,
            stream_proto.clone(),
            "WritableStream",
            0,
            move |vm, _, args, new| {
                let new = new.ok_or_else(|| JsError::type_error("WritableStream requires new"))?;
                let intrinsic = vm.root_value(shared.0.stream).unwrap();
                let instance = webidl::instance(vm, new, &intrinsic)?;
                shared.create(vm, instance, arg(args, 0), arg(args, 1), false)
            },
        )?;
        accessor(vm, stream_proto.clone(), "locked", |vm, this, _| {
            Ok(locked(vm, &this)?.into())
        })?;
        webidl::operation(
            vm,
            stream_proto.clone(),
            "abort",
            0,
            |vm, this, args| match locked(vm, &this) {
                Ok(false) => abort_stream(vm, this, arg(args, 0)),
                Ok(true) => StreamHost::reject(vm, JsError::type_error("WritableStream is locked")),
                Err(error) => StreamHost::reject(vm, error),
            },
        )?;
        webidl::operation(
            vm,
            stream_proto.clone(),
            "close",
            0,
            |vm, this, _| match locked(vm, &this) {
                Ok(false) => close_stream(vm, &stream(vm, &this)?),
                Ok(true) => StreamHost::reject(vm, JsError::type_error("WritableStream is locked")),
                Err(error) => StreamHost::reject(vm, error),
            },
        )?;
        let shared = host.clone();
        webidl::operation(vm, stream_proto, "getWriter", 0, move |vm, this, _| {
            shared.get_writer(vm, this)
        })?;

        let shared = host.clone();
        webidl::interface(
            vm,
            writer_proto.clone(),
            "WritableStreamDefaultWriter",
            1,
            move |vm, _, args, new| {
                let new = new.ok_or_else(|| {
                    JsError::type_error("WritableStreamDefaultWriter requires new")
                })?;
                let intrinsic = vm.root_value(shared.0.writer).unwrap();
                let writer = webidl::instance(vm, new, &intrinsic)?;
                acquire_writer(vm, writer, arg(args, 0))
            },
        )?;
        accessor(
            vm,
            writer_proto.clone(),
            "closed",
            |vm, this, _| match writer_closed(vm, &this) {
                Ok(promise) => Ok(promise),
                Err(error) => StreamHost::reject(vm, error),
            },
        )?;
        accessor(vm, writer_proto.clone(), "desiredSize", |vm, this, _| {
            writer_desired_size(vm, &this)
        })?;
        accessor(
            vm,
            writer_proto.clone(),
            "ready",
            |vm, this, _| match writer_ready(vm, &this) {
                Ok(promise) => Ok(promise),
                Err(error) => StreamHost::reject(vm, error),
            },
        )?;
        webidl::operation(vm, writer_proto.clone(), "abort", 0, |vm, this, args| {
            writer_abort(vm, this, arg(args, 0))
        })?;
        webidl::operation(vm, writer_proto.clone(), "close", 0, |vm, this, _| {
            writer_close(vm, this)
        })?;
        webidl::operation(vm, writer_proto.clone(), "releaseLock", 0, |vm, this, _| {
            writer_release(vm, &this)?;
            Ok(Value::Undefined)
        })?;
        webidl::operation(vm, writer_proto, "write", 0, |vm, this, args| {
            writer_write(vm, this, arg(args, 0))
        })?;

        webidl::interface(
            vm,
            controller_proto.clone(),
            "WritableStreamDefaultController",
            0,
            |_, _, _, _| {
                Err(JsError::type_error(
                    "Illegal WritableStreamDefaultController constructor",
                ))
            },
        )?;
        accessor(vm, controller_proto.clone(), "signal", |vm, this, _| {
            controller_signal(vm, &this)
        })?;
        webidl::operation(vm, controller_proto, "error", 0, |vm, this, args| {
            controller_error(vm, &this, arg(args, 0))?;
            Ok(Value::Undefined)
        })?;
        strategies::install(vm)?;
        Ok(host)
    }

    pub(super) fn construct(
        &self,
        vm: &mut Vm,
        sink: Value,
        strategy: Value,
    ) -> Result<Value, JsError> {
        let instance = vm.new_object_with_prototype(vm.root_value(self.0.stream).unwrap())?;
        self.create(vm, instance, sink, strategy, true)
    }

    fn create(
        &self,
        vm: &mut Vm,
        object: Value,
        sink: Value,
        strategy: Value,
        native_sink: bool,
    ) -> Result<Value, JsError> {
        if !sink.is_undefined() && sink.as_object().is_none() {
            return Err(JsError::type_error("WritableStream sink must be an object"));
        }
        if !strategy.is_nullish() && strategy.as_object().is_none() {
            return Err(JsError::type_error(
                "WritableStream strategy must be an object",
            ));
        }
        let high_water = field(vm, &strategy, "highWaterMark")?;
        let high_water = if high_water.is_undefined() {
            1.0
        } else {
            vm.to_number(&high_water)?
        };
        let size = callback(vm, &strategy, "size")?;
        let abort = callback(vm, &sink, "abort")?;
        let close = callback(vm, &sink, "close")?;
        let start = callback(vm, &sink, "start")?;
        let kind = field(vm, &sink, "type")?;
        let write = callback(vm, &sink, "write")?;
        if !kind.is_undefined() {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "Invalid WritableStream sink type",
            ));
        }
        if high_water.is_nan() || high_water < 0.0 {
            return Err(JsError::new(ErrorKind::RangeError, "Invalid highWaterMark"));
        }

        let signal = self.0.abort.unaborted_signal(vm)?;
        let state = Rc::new(RefCell::new(State {
            id: object.as_object().unwrap(),
            abort: self.0.abort.clone(),
            native_sink,
            status: Status::Writable,
            started: false,
            active: false,
            closing: false,
            abort_pending: None,
            backpressure: high_water <= 0.0,
            high_water,
            queue: VecDeque::new(),
            queue_head: 0,
            queue_size: 0.0,
        }));
        vm.set_host_data(object.clone(), state.clone())?;
        let control = vm.new_object_with_prototype(vm.root_value(self.0.controller).unwrap())?;
        vm.set_host_data(control.clone(), Rc::new(Controller))?;
        vm.set_host_slot(control.clone(), OWNER, object.clone())?;
        vm.set_host_slot(control.clone(), SIGNAL, signal)?;
        for (index, value) in [
            (CONTROLLER, control.clone()),
            (SINK, sink.clone()),
            (WRITE, write),
            (CLOSE, close),
            (ABORT, abort),
            (SIZE, size),
        ] {
            vm.set_host_slot(object.clone(), index, value)?;
        }
        let result = if start.is_undefined() {
            Value::Undefined
        } else {
            vm.call(start, sink, &[control])?
        };
        observe(vm, &state, Operation::Start, Ok(result))?;
        Ok(object)
    }

    pub(super) fn get_writer(&self, vm: &mut Vm, stream: Value) -> Result<Value, JsError> {
        let writer = vm.new_object_with_prototype(vm.root_value(self.0.writer).unwrap())?;
        acquire_writer(vm, writer, stream)
    }
}

fn acquire_writer(vm: &mut Vm, writer: Value, object: Value) -> Result<Value, JsError> {
    if locked(vm, &object)? {
        return Err(JsError::type_error("WritableStream is locked"));
    }
    let state = stream(vm, &object)?;
    let (status, pressured) = {
        let state = state.borrow();
        (state.status, state.backpressure && !state.closing)
    };
    vm.set_host_data(writer.clone(), Rc::new(Writer))?;
    vm.set_host_slot(writer.clone(), OWNER, object.clone())?;
    writer_promise(vm, &writer, READY)?;
    writer_promise(vm, &writer, CLOSED)?;
    vm.set_host_slot(object.clone(), WRITER, writer.clone())?;
    match status {
        Status::Writable if pressured => {}
        Status::Writable | Status::Closed => {
            settle_writer(vm, &writer, READY, Ok(Value::Undefined))?;
        }
        Status::Erroring | Status::Errored => {
            settle_writer(vm, &writer, READY, Err(slot(vm, &object, ERROR)))?;
        }
    }
    match status {
        Status::Closed => settle_writer(vm, &writer, CLOSED, Ok(Value::Undefined))?,
        Status::Errored => settle_writer(vm, &writer, CLOSED, Err(slot(vm, &object, ERROR)))?,
        Status::Writable | Status::Erroring => {}
    }
    Ok(writer)
}
