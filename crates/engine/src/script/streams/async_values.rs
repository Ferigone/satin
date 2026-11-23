//! The Web IDL async iterator for a ReadableStream and its default reader.
use super::{
    ReadKind, StreamHost, capability, clear_slot, promise_record, read_result, reader_stream,
    settle, stream,
};
use crate::script::{arg, webidl};
use rust_browser_javascript::{JsError, Value, Vm};
use std::{cell::Cell, rc::Rc};

const READER: u32 = 0;
const ONGOING: u32 = 1;
const ITERATOR: u32 = 3;
const RETURN_VALUE: u32 = 4;

struct IteratorState {
    prevent_cancel: bool,
    finished: Cell<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Next,
    Return,
}

pub(super) fn install(vm: &mut Vm, host: &StreamHost, prototype: Value) -> Result<(), JsError> {
    let iterator = vm.new_object_with_prototype(vm.async_iterator_prototype())?;
    webidl::tag(vm, iterator.clone(), "ReadableStream AsyncIterator")?;
    for (name, length, kind) in [
        ("next", 0, Operation::Next),
        ("return", 1, Operation::Return),
    ] {
        let host = host.clone();
        webidl::operation(vm, iterator.clone(), name, length, move |vm, this, args| {
            schedule(vm, &host, this, kind, arg(args, 0))
        })?;
    }
    let iterator = vm.root(iterator);
    let host = host.clone();
    let values = webidl::operation(vm, prototype.clone(), "values", 0, move |vm, this, args| {
        let source = stream(vm, &this)?;
        let options = arg(args, 0);
        let prevent_cancel = if options.is_nullish() {
            false
        } else {
            if options.as_object().is_none() {
                return Err(JsError::type_error(
                    "ReadableStream iterator options must be an object",
                ));
            }
            vm.get_property(options, "preventCancel")?.is_truthy()
        };
        let iterator = vm.new_object_with_prototype(vm.root_value(iterator).unwrap())?;
        let reader = host.acquire(vm, &source)?;
        vm.set_host_data(
            iterator.clone(),
            Rc::new(IteratorState {
                prevent_cancel,
                finished: Cell::new(false),
            }),
        )?;
        vm.set_host_slot(iterator.clone(), READER, reader)?;
        Ok(iterator)
    })?;
    vm.define_data_property_key(
        prototype,
        webidl::well_known_key(vm, "asyncIterator"),
        values,
        true,
        false,
        true,
    )
}

fn schedule(
    vm: &mut Vm,
    host: &StreamHost,
    iterator: Value,
    operation: Operation,
    value: Value,
) -> Result<Value, JsError> {
    if vm.host_data::<IteratorState>(&iterator).is_none() {
        return StreamHost::reject(
            vm,
            JsError::type_error("Illegal ReadableStream async iterator receiver"),
        );
    }
    let previous = vm
        .host_slot(&iterator, ONGOING)
        .filter(|value| !value.is_undefined());
    let (promise, record) = capability(vm, &iterator)?;
    vm.set_host_slot(iterator.clone(), ONGOING, promise.clone())?;
    vm.set_host_slot(record.clone(), ITERATOR, iterator)?;
    if operation == Operation::Return {
        vm.set_host_slot(record.clone(), RETURN_VALUE, value)?;
    }

    if let Some(previous) = previous {
        let current = record.clone();
        let host_step = host.clone();
        let start = vm.new_native_function("", move |vm, _, _| {
            run(vm, &host_step, &current, operation)?;
            Ok(Value::Undefined)
        });
        vm.set_host_slot(start.clone(), ITERATOR, record)?;
        host.then(vm, previous, start.clone(), start)?;
    } else {
        run(vm, host, &record, operation)?;
    }
    Ok(promise)
}

fn run(
    vm: &mut Vm,
    host: &StreamHost,
    record: &Value,
    operation: Operation,
) -> Result<(), JsError> {
    let iterator = vm.host_slot(record, ITERATOR).unwrap();
    let state = vm.host_data::<IteratorState>(&iterator).unwrap();
    let value = vm.host_slot(record, RETURN_VALUE).unwrap_or_default();
    if state.finished.get() {
        let result = read_result(vm, value, true)?;
        return finish(vm, record, Ok(result));
    }
    let reader = vm.host_slot(&iterator, READER).unwrap();
    let source = reader_stream(vm, &reader)?.unwrap();
    let wait = match operation {
        Operation::Next => host.read_with_kind(vm, &source, ReadKind::Iterator(reader))?,
        Operation::Return => {
            state.finished.set(true);
            let cancel = if state.prevent_cancel {
                None
            } else {
                Some(host.cancel(vm, &source, value.clone()))
            };
            host.release(vm, &source, &reader)?;
            clear_slot(vm, &iterator, READER)?;
            let Some(cancel) = cancel else {
                let result = read_result(vm, value, true)?;
                return finish(vm, record, Ok(result));
            };
            cancel?
        }
    };

    let success = record.clone();
    let fulfilled = vm.new_native_function("", move |vm, _, args| {
        let result = match operation {
            Operation::Next => arg(args, 0),
            Operation::Return => read_result(
                vm,
                vm.host_slot(&success, RETURN_VALUE).unwrap_or_default(),
                true,
            )?,
        };
        finish(vm, &success, Ok(result))?;
        Ok(Value::Undefined)
    });
    let failure = record.clone();
    let rejected = vm.new_native_function("", move |vm, _, args| {
        finish(vm, &failure, Err(arg(args, 0)))?;
        Ok(Value::Undefined)
    });
    vm.set_host_slot(fulfilled.clone(), ITERATOR, record.clone())?;
    vm.set_host_slot(rejected.clone(), ITERATOR, record.clone())?;
    host.then(vm, wait, fulfilled, rejected)?;
    Ok(())
}

fn finish(vm: &mut Vm, record: &Value, result: Result<Value, Value>) -> Result<(), JsError> {
    let iterator = vm.host_slot(record, ITERATOR).unwrap();
    let done = match &result {
        Ok(value) => vm.get_property(value.clone(), "done")?.is_truthy(),
        Err(_) => true,
    };
    if done {
        vm.host_data::<IteratorState>(&iterator)
            .unwrap()
            .finished
            .set(true);
        clear_slot(vm, &iterator, READER)?;
    }
    if vm.host_slot(&iterator, ONGOING) == Some(promise_record(vm, record)) {
        clear_slot(vm, &iterator, ONGOING)?;
    }
    settle(vm, record, result)
}
