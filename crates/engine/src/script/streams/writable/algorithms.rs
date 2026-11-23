//! Serialized execution of sink algorithms and completion of stream requests.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum Operation {
    Start,
    Write,
    Close,
    Abort,
}

pub(super) fn clear_algorithms(vm: &mut Vm, object: &Value) -> Result<(), JsError> {
    for index in [SINK, WRITE, CLOSE, ABORT, SIZE] {
        clear_slot(vm, object, index)?;
    }
    Ok(())
}

pub(super) fn observe(
    vm: &mut Vm,
    source: &Shared,
    op: Operation,
    result: Result<Value, JsError>,
) -> Result<(), JsError> {
    let (object, native_sink) = {
        let state = source.borrow();
        (state.value(), state.native_sink)
    };
    let promise = match result {
        Ok(value) if native_sink && vm.is_promise(&value) => value,
        Ok(value) => settle_with(vm, Ok(value))?,
        Err(error) => {
            let reason = exception(vm, error)?;
            settle_with(vm, Err(reason))?
        }
    };
    vm.set_host_slot(object.clone(), SINK_PROMISE, promise.clone())?;
    let callback = |vm: &mut Vm, fulfilled| -> Result<Value, JsError> {
        let source = Rc::downgrade(source);
        let callback = vm.new_native_function("", move |vm, _, args| {
            if let Some(source) = source.upgrade() {
                let object = source.borrow().value();
                clear_slot(vm, &object, SINK_PROMISE)?;
                let result = if fulfilled {
                    Ok(Value::Undefined)
                } else {
                    Err(arg(args, 0))
                };
                match op {
                    Operation::Start => {
                        source.borrow_mut().started = true;
                        match result {
                            Ok(_) => advance(vm, &source)?,
                            Err(reason) => start_erroring(vm, &source, reason)?,
                        }
                    }
                    Operation::Write => finish_write(vm, &source, result)?,
                    Operation::Close => finish_close(vm, &source, result)?,
                    Operation::Abort => {
                        settle_request(vm, &object, ABORT_REQUEST, result)?;
                        reject_close_and_closed(vm, &source)?;
                    }
                }
            }
            Ok(Value::Undefined)
        });
        vm.set_host_slot(callback.clone(), OWNER, object.clone())?;
        Ok(callback)
    };
    let fulfilled = callback(vm, true)?;
    let rejected = callback(vm, false)?;
    vm.then_intrinsic(promise, fulfilled, rejected)?;
    Ok(())
}

fn invoke_sink(vm: &mut Vm, object: &Value, index: u32, args: &[Value]) -> Result<Value, JsError> {
    let method = slot(vm, object, index);
    if method.is_undefined() {
        Ok(Value::Undefined)
    } else {
        vm.call(method, slot(vm, object, SINK), args)
    }
}

pub(super) fn advance(vm: &mut Vm, source: &Shared) -> Result<(), JsError> {
    let (object, head, op) = {
        let mut state = source.borrow_mut();
        if !state.started || state.active {
            return Ok(());
        }
        match state.status {
            Status::Erroring => {
                drop(state);
                return finish_erroring(vm, source);
            }
            Status::Writable => {}
            _ => return Ok(()),
        }
        let op = if !state.queue.is_empty() {
            Operation::Write
        } else if state.closing {
            Operation::Close
        } else {
            return Ok(());
        };
        state.active = true;
        (state.value(), state.queue_head, op)
    };
    // Sink code can reenter the stream. Keep the in-flight marker set and
    // release the Rust borrow before invoking any callback.
    match op {
        Operation::Write => {
            let items = super::super::table(vm, &object)?;
            let record = vm.get_property(items, &write_key(head))?;
            let chunk = slot(vm, &record, CHUNK);
            let control = slot(vm, &object, CONTROLLER);
            let result = invoke_sink(vm, &object, WRITE, &[chunk, control]);
            observe(vm, source, op, result)
        }
        Operation::Close => {
            let result = invoke_sink(vm, &object, CLOSE, &[]);
            observe(vm, source, op, result)?;
            clear_algorithms(vm, &object)
        }
        Operation::Start | Operation::Abort => unreachable!(),
    }
}

pub(super) fn update_backpressure(vm: &mut Vm, source: &Shared) -> Result<(), JsError> {
    let (object, backpressure) = {
        let mut state = source.borrow_mut();
        if state.status != Status::Writable || state.closing {
            return Ok(());
        }
        let next = state.high_water - state.queue_size <= 0.0;
        if next == state.backpressure {
            return Ok(());
        }
        state.backpressure = next;
        (state.value(), next)
    };
    let writer = slot(vm, &object, WRITER);
    if !writer.is_undefined() {
        if backpressure {
            writer_promise(vm, &writer, READY)?;
        } else {
            settle_writer(vm, &writer, READY, Ok(Value::Undefined))?;
        }
    }
    Ok(())
}

pub(super) fn start_erroring(vm: &mut Vm, source: &Shared, reason: Value) -> Result<(), JsError> {
    if source.borrow().status == Status::Writable {
        let object = source.borrow().value();
        vm.set_host_slot(object.clone(), ERROR, reason.clone())?;
        source.borrow_mut().status = Status::Erroring;
        let writer = slot(vm, &object, WRITER);
        if !writer.is_undefined() {
            reject_writer(vm, &writer, READY, reason)?;
        }
    }
    advance(vm, source)
}

fn finish_erroring(vm: &mut Vm, source: &Shared) -> Result<(), JsError> {
    let (object, abort) = {
        let mut state = source.borrow_mut();
        state.status = Status::Errored;
        (state.value(), state.abort_pending.take())
    };
    let reason = slot(vm, &object, ERROR);
    while let Some(record) = pop_write(vm, source)? {
        settle(vm, &record, Err(reason.clone()))?;
    }
    match abort {
        // The first abort of a writable stream runs the underlying sink.
        Some(PendingAbort::StartedErroring) => {
            let result = invoke_sink(vm, &object, ABORT, &[reason]);
            observe(vm, source, Operation::Abort, result)?;
            clear_algorithms(vm, &object)
        }
        Some(PendingAbort::AlreadyErroring) => {
            settle_request(vm, &object, ABORT_REQUEST, Err(reason))?;
            clear_algorithms(vm, &object)?;
            reject_close_and_closed(vm, source)
        }
        None => {
            clear_algorithms(vm, &object)?;
            reject_close_and_closed(vm, source)
        }
    }
}

fn reject_close_and_closed(vm: &mut Vm, source: &Shared) -> Result<(), JsError> {
    let object = source.borrow().value();
    let reason = slot(vm, &object, ERROR);
    source.borrow_mut().closing = false;
    settle_request(vm, &object, CLOSE_REQUEST, Err(reason.clone()))?;
    let writer = slot(vm, &object, WRITER);
    if !writer.is_undefined() {
        settle_writer(vm, &writer, CLOSED, Err(reason))?;
    }
    Ok(())
}

fn settle_request(
    vm: &mut Vm,
    object: &Value,
    index: u32,
    result: Result<Value, Value>,
) -> Result<(), JsError> {
    let record = slot(vm, object, index);
    if !record.is_undefined() {
        clear_slot(vm, object, index)?;
        settle(vm, &record, result)?;
    }
    Ok(())
}

fn pop_write(vm: &mut Vm, source: &Shared) -> Result<Option<Value>, JsError> {
    let (object, head) = {
        let mut state = source.borrow_mut();
        let Some(size) = state.queue.pop_front() else {
            return Ok(None);
        };
        let head = state.queue_head;
        state.queue_head = if state.queue.is_empty() {
            0
        } else {
            (head + 1) % MAX_WRITES
        };
        state.queue_size -= size;
        if state.queue_size < 0.0 {
            state.queue_size = 0.0;
        }
        (state.value(), head)
    };
    let record = take(vm, &object, &write_key(head))?;
    Ok(Some(record))
}

fn finish_write(vm: &mut Vm, source: &Shared, result: Result<Value, Value>) -> Result<(), JsError> {
    source.borrow_mut().active = false;
    let request = pop_write(vm, source)?.expect("in-flight write has a request");
    settle(vm, &request, result.clone())?;
    match result {
        Ok(_) => {
            update_backpressure(vm, source)?;
            advance(vm, source)
        }
        Err(reason) => {
            if source.borrow().status == Status::Writable {
                clear_algorithms(vm, &source.borrow().value())?;
            }
            start_erroring(vm, source, reason)
        }
    }
}

fn finish_close(vm: &mut Vm, source: &Shared, result: Result<Value, Value>) -> Result<(), JsError> {
    let object = {
        let mut state = source.borrow_mut();
        state.active = false;
        state.closing = false;
        state.abort_pending = None;
        state.value()
    };
    settle_request(vm, &object, CLOSE_REQUEST, result.clone())?;
    settle_request(vm, &object, ABORT_REQUEST, result.clone())?;
    if let Err(reason) = result {
        return start_erroring(vm, source, reason);
    }
    source.borrow_mut().status = Status::Closed;
    clear_slot(vm, &object, ERROR)?;
    let writer = slot(vm, &object, WRITER);
    if !writer.is_undefined() {
        settle_writer(vm, &writer, CLOSED, Ok(Value::Undefined))?;
    }
    Ok(())
}
