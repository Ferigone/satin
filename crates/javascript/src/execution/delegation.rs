use super::{Advance, Frame, ResumeValue};
use crate::{
    ErrorKind, JsError, Value, Vm,
    iterator::IteratorRecord,
    vm::{Completion, Item},
};

pub(super) struct Delegation {
    record: IteratorRecord,
    asynchronous: bool,
    awaiting: Option<Awaiting>,
}

#[derive(Clone, Copy)]
enum Awaiting {
    Result { returning: bool },
    CloseMissingThrow,
    ReturnWithoutMethod,
}

impl Delegation {
    pub(super) fn new(vm: &mut Vm, iterable: Value, asynchronous: bool) -> Result<Self, JsError> {
        let record = if asynchronous {
            vm.get_async_iterator(iterable)?
        } else {
            vm.get_iterator(iterable)?
        };
        Ok(Self {
            record,
            asynchronous,
            awaiting: None,
        })
    }

    pub(super) fn visit(&self, visit: &mut impl FnMut(&Value)) {
        visit(&self.record.iterator);
        visit(&self.record.next);
    }

    fn suspend(
        mut self,
        vm: &mut Vm,
        frame: &mut Frame,
        result: Value,
        awaiting: Awaiting,
    ) -> Result<Advance, JsError> {
        let promise = vm.promise_for_await(result)?;
        self.awaiting = Some(awaiting);
        frame.delegate = Some(self);
        Ok(Advance::Await(promise))
    }

    fn result(
        self,
        vm: &mut Vm,
        frame: &mut Frame,
        result: Value,
        returning: bool,
    ) -> Result<Advance, JsError> {
        require_object(&result)?;
        if !vm.get_property(result.clone(), "done")?.is_truthy() {
            let result = if self.asynchronous {
                let value = vm.get_property(result, "value")?;
                vm.iterator_result(value, false)?
            } else {
                // A synchronous yield* returns the actual IteratorResult.
                result
            };
            frame.delegate = Some(self);
            return Ok(Advance::Yield(result));
        }
        let value = vm.get_property(result, "value")?;
        if returning {
            Ok(Advance::Finish(Completion::Return(value)))
        } else {
            frame.operands.push(Item::Value(value));
            Ok(Advance::Keep)
        }
    }

    fn close_missing_throw(mut self, vm: &mut Vm, frame: &mut Frame) -> Result<Advance, JsError> {
        if self.asynchronous {
            if let Some(promise) = vm.start_async_iterator_close(&mut self.record)? {
                self.awaiting = Some(Awaiting::CloseMissingThrow);
                frame.delegate = Some(self);
                return Ok(Advance::Await(promise));
            }
        } else {
            vm.iterator_close(&mut self.record, Ok(Value::Undefined))?;
        }
        Err(missing_throw())
    }
}

#[inline(never)]
pub(super) fn resume(
    vm: &mut Vm,
    frame: &mut Frame,
    sent: ResumeValue,
) -> Result<Advance, JsError> {
    let Some(mut delegate) = frame.delegate.take() else {
        return match sent {
            ResumeValue::Normal(value) => {
                frame.operands.push(Item::Value(value));
                Ok(Advance::Keep)
            }
            ResumeValue::Return(value) => Ok(Advance::Finish(Completion::Return(value))),
            ResumeValue::Throw(value) => Err(vm.thrown_error(value)?),
        };
    };
    if let Some(waiting) = delegate.awaiting.take() {
        let value = match sent {
            ResumeValue::Normal(value) => value,
            ResumeValue::Throw(value) => return Err(vm.thrown_error(value)?),
            ResumeValue::Return(_) => {
                return Err(JsError::new(
                    ErrorKind::InternalError,
                    "Invalid continuation for an awaited iterator method",
                ));
            }
        };
        return match waiting {
            Awaiting::Result { returning } => delegate.result(vm, frame, value, returning),
            Awaiting::CloseMissingThrow => {
                require_object(&value)?;
                Err(missing_throw())
            }
            Awaiting::ReturnWithoutMethod => Ok(Advance::Finish(Completion::Return(value))),
        };
    }
    vm.tick()?;
    let iterator = &delegate.record.iterator;
    let (method, argument, returning) = match sent {
        ResumeValue::Normal(value) => (delegate.record.next.clone(), value, false),
        ResumeValue::Throw(value) => {
            let method = vm.get_property(iterator.clone(), "throw")?;
            if method.is_nullish() {
                return delegate.close_missing_throw(vm, frame);
            }
            (method, value, false)
        }
        ResumeValue::Return(value) => {
            let method = vm.get_property(iterator.clone(), "return")?;
            if method.is_nullish() {
                if delegate.asynchronous {
                    return delegate.suspend(vm, frame, value, Awaiting::ReturnWithoutMethod);
                }
                return Ok(Advance::Finish(Completion::Return(value)));
            }
            (method, value, true)
        }
    };
    let result = vm.invoke(method, iterator.clone(), &[argument])?;
    if delegate.asynchronous {
        delegate.suspend(vm, frame, result, Awaiting::Result { returning })
    } else {
        delegate.result(vm, frame, result, returning)
    }
}

fn missing_throw() -> JsError {
    JsError::type_error("The delegated iterator has no throw method")
}

fn require_object(result: &Value) -> Result<(), JsError> {
    if result.as_object().is_some() {
        Ok(())
    } else {
        Err(JsError::type_error(
            "Delegated iterator method returned a non-object result",
        ))
    }
}
