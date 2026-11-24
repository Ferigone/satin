//! Async iterator lookup and the specified sync-to-async adapter.
use crate::{
    JsError, ObjectId, Value, Vm,
    builtins::method,
    heap::{ObjectKind, TraceEdges},
    iterator::IteratorRecord,
    promise::ReactionPair,
};
use std::rc::Rc;

pub(crate) struct AsyncFromSyncIterator {
    iterator: Value,
    next: Value,
}

impl AsyncFromSyncIterator {
    pub(crate) fn visit(&self, mut visit: impl FnMut(&Value)) {
        visit(&self.iterator);
        visit(&self.next);
    }

    pub(crate) fn trace(&self, pending: &mut TraceEdges) {
        self.visit(|value| pending.value(value));
    }
}

pub(crate) struct Continuation {
    promise: ObjectId,
    close: Option<Value>,
    done: bool,
}

impl Continuation {
    pub(crate) fn visit(&self, mut visit: impl FnMut(&Value)) {
        visit(&Value::Object(self.promise));
        self.close.iter().for_each(visit);
    }

    pub(crate) fn settle(self, vm: &mut Vm, value: Value, rejected: bool) -> Result<(), JsError> {
        if rejected {
            if let Some(iterator) = self.close {
                let error = vm.thrown_error(value)?;
                let error = close_with_error(vm, iterator, error);
                let value = vm.error_reason(error)?;
                vm.settle_promise(self.promise, value, true)
            } else {
                vm.settle_promise(self.promise, value, true)
            }
        } else {
            let result = vm.iterator_result(value, self.done)?;
            vm.resolve_promise(self.promise, result)
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Method {
    Next,
    Return,
    Throw,
}

pub(crate) fn install(vm: &mut Vm) -> ObjectId {
    let async_iterator = vm.new_object();
    vm.realm.async_iterator_prototype = async_iterator.as_object();
    vm.root(async_iterator.clone());
    let identity = vm.native(
        "[Symbol.asyncIterator]",
        0,
        false,
        Rc::new(|_, this, _| Ok(this)),
    );
    vm.define_data_property_key(
        async_iterator.clone(),
        vm.well_known_key("asyncIterator"),
        identity,
        true,
        false,
        true,
    )
    .unwrap();
    let prototype = vm
        .new_object_with_prototype(async_iterator.clone())
        .unwrap();
    vm.realm.async_from_sync_prototype = prototype.as_object();
    vm.root(prototype.clone());
    let tag = vm.well_known_key("toStringTag");
    vm.define_data_property_key(
        prototype.clone(),
        tag,
        "Async-from-Sync Iterator".into(),
        false,
        false,
        true,
    )
    .unwrap();
    for (name, operation) in [
        ("next", Method::Next),
        ("return", Method::Return),
        ("throw", Method::Throw),
    ] {
        method(vm, prototype.clone(), name, 1, move |vm, this, args| {
            adapter_call(vm, this, operation, &args[..args.len().min(1)])
        });
    }
    async_iterator.as_object().unwrap()
}

impl Vm {
    /// %AsyncIteratorPrototype% shared by generators and host asynchronous iterables.
    pub fn async_iterator_prototype(&self) -> Value {
        Value::Object(self.realm.async_iterator_prototype.unwrap())
    }

    pub(crate) fn get_async_iterator(
        &mut self,
        iterable: Value,
    ) -> Result<IteratorRecord, JsError> {
        let method =
            self.get_property_key(iterable.clone(), self.well_known_key("asyncIterator"))?;
        if !method.is_nullish() {
            return self.iterator_from_method(iterable, method);
        }
        let sync = self.get_iterator(iterable)?;
        let wrapped = self.allocate_constructed(
            ObjectKind::AsyncFromSyncIterator(AsyncFromSyncIterator {
                iterator: sync.iterator,
                next: sync.next,
            }),
            self.realm.async_from_sync_prototype.unwrap(),
        )?;
        let next = self.get_property(wrapped.clone(), "next")?;
        Ok(IteratorRecord {
            iterator: wrapped,
            next,
            done: false,
        })
    }

    pub(crate) fn start_async_iterator_close(
        &mut self,
        record: &mut IteratorRecord,
    ) -> Result<Option<ObjectId>, JsError> {
        if record.done {
            return Ok(None);
        }
        record.done = true;
        let method = self.get_property(record.iterator.clone(), "return")?;
        if method.is_nullish() {
            return Ok(None);
        }
        let result = self.invoke(method, record.iterator.clone(), &[])?;
        self.promise_for_await(result).map(Some)
    }
}

#[inline(never)]
fn adapter_call(
    vm: &mut Vm,
    this: Value,
    operation: Method,
    args: &[Value],
) -> Result<Value, JsError> {
    let capability = vm.new_promise_capability()?;
    let promise = capability.promise.as_object().unwrap();
    if let Err(error) = forward(vm, this, operation, args, promise) {
        let reason = vm.error_reason(error)?;
        vm.settle_promise(promise, reason, true)?;
    }
    Ok(capability.promise)
}

fn forward(
    vm: &mut Vm,
    this: Value,
    operation: Method,
    args: &[Value],
    promise: ObjectId,
) -> Result<(), JsError> {
    let id = this
        .as_object()
        .ok_or_else(|| JsError::type_error("Async iterator adapter receiver is not an object"))?;
    let ObjectKind::AsyncFromSyncIterator(record) = &vm.heap.object(id)?.kind else {
        return Err(JsError::type_error(
            "Async iterator adapter receiver has no sync iterator",
        ));
    };
    let iterator = record.iterator.clone();
    let method = match operation {
        Method::Next => record.next.clone(),
        Method::Return => vm.get_property(iterator.clone(), "return")?,
        Method::Throw => vm.get_property(iterator.clone(), "throw")?,
    };
    if method.is_nullish() {
        match operation {
            Method::Return => {
                let value = args.first().cloned().unwrap_or_default();
                let result = vm.iterator_result(value, true)?;
                return vm.resolve_promise(promise, result);
            }
            Method::Throw => {
                let mut record = close_record(iterator);
                vm.iterator_close(&mut record, Ok(Value::Undefined))?;
                return Err(JsError::type_error("The sync iterator has no throw method"));
            }
            Method::Next => {} // Call validates the cached next method.
        }
    }
    let result = vm.invoke(method, iterator.clone(), args)?;
    if result.as_object().is_none() {
        return Err(JsError::type_error(
            "Sync iterator method returned a non-object result",
        ));
    }
    let done = vm.get_property(result.clone(), "done")?.is_truthy();
    let value = vm.get_property(result, "value")?;
    let close = (!done && operation != Method::Return).then_some(iterator);
    let promise_for_value = match vm.promise_for_await(value) {
        Ok(wait) => wait,
        Err(error) => {
            return Err(if let Some(iterator) = close {
                close_with_error(vm, iterator, error)
            } else {
                error
            });
        }
    };
    vm.register_reaction(
        promise_for_value,
        ReactionPair::AsyncFromSync(Continuation {
            promise,
            close,
            done,
        }),
    )
}

fn close_record(iterator: Value) -> IteratorRecord {
    IteratorRecord {
        iterator,
        next: Value::Undefined,
        done: false,
    }
}

fn close_with_error(vm: &mut Vm, iterator: Value, error: JsError) -> JsError {
    vm.iterator_close(&mut close_record(iterator), Err(error))
        .unwrap_err()
}
