use crate::{
    ErrorKind, JsError, ObjectId, Value, Vm,
    execution::{Execution, ResumeValue, RunResult},
    heap::{ObjectKind, TraceEdges},
    promise::{PromiseCapability, ReactionPair},
    vm::Completion,
};
use std::{collections::VecDeque, mem};

pub(crate) struct AsyncGenerator {
    state: State,
    queue: VecDeque<Request>,
}

enum State {
    SuspendedStart(Box<Execution>),
    SuspendedYield(Box<Execution>),
    Executing,
    Draining,
    Completed,
}

struct Request {
    input: ResumeValue,
    capability: PromiseCapability,
}

/// A pending reaction owns the suspended frames until its promise settles.
pub(crate) enum GeneratorAwait {
    Body(Box<Execution>),
    Return(Box<Execution>),
    Closed,
}

impl GeneratorAwait {
    pub(crate) fn visit(&self, visit: impl FnMut(&Value)) {
        if let Self::Body(execution) | Self::Return(execution) = self {
            execution.visit(visit);
        }
    }

    pub(crate) fn extra_bytes(&self) -> usize {
        match self {
            Self::Body(execution) | Self::Return(execution) => execution.extra_bytes(),
            Self::Closed => 0,
        }
    }
}

impl AsyncGenerator {
    pub(super) fn new(execution: Box<Execution>) -> Self {
        Self {
            state: State::SuspendedStart(execution),
            queue: VecDeque::new(),
        }
    }

    pub(crate) fn visit(&self, mut visit: impl FnMut(&Value)) {
        if let State::SuspendedStart(execution) | State::SuspendedYield(execution) = &self.state {
            execution.visit(&mut visit);
        }
        for request in &self.queue {
            visit(request.input.value());
            request.capability.visit(&mut visit);
        }
    }

    pub(crate) fn trace(&self, pending: &mut TraceEdges) {
        self.visit(|value| pending.value(value));
    }

    pub(crate) fn extra_bytes(&self) -> usize {
        let execution = match &self.state {
            State::SuspendedStart(execution) | State::SuspendedYield(execution) => {
                execution.extra_bytes()
            }
            _ => 0,
        };
        execution + self.queue.capacity() * mem::size_of::<Request>()
    }
}

impl Vm {
    #[inline(never)]
    pub(super) fn request_async_generator(
        &mut self,
        this: Value,
        input: ResumeValue,
    ) -> Result<Value, JsError> {
        let capability = self.new_promise_capability()?;
        let promise = capability.promise.as_object().unwrap();
        let id = match this.as_object().filter(|&id| {
            self.heap
                .object(id)
                .is_ok_and(|object| matches!(object.kind, ObjectKind::AsyncGenerator(_)))
        }) {
            Some(id) => id,
            None => {
                let error = self.error_reason(JsError::type_error(
                    "Async generator method requires an AsyncGenerator receiver",
                ))?;
                self.settle_promise(promise, error, true)?;
                return Ok(capability.promise);
            }
        };
        let limits = self.limits.max_pending_jobs;
        let queue = &self.async_generator(id)?.queue;
        if queue.len() >= limits {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Async generator request budget exhausted",
            ));
        }
        if queue.len() == queue.capacity() {
            let capacity = (queue.capacity() * 2).max(4);
            self.reserve_allocation(capacity * mem::size_of::<Request>())?;
        }
        let response = capability.promise.clone();
        self.async_generator(id)?
            .queue
            .push_back(Request { input, capability });
        let result = self.advance_async_generator(id);
        self.check_async_generator_result(id, result)?;
        Ok(response)
    }

    fn async_generator(&mut self, id: ObjectId) -> Result<&mut AsyncGenerator, JsError> {
        if let ObjectKind::AsyncGenerator(data) = &mut self.heap.object_mut(id)?.kind {
            Ok(data)
        } else {
            Err(JsError::new(
                ErrorKind::InternalError,
                "Async generator state is missing",
            ))
        }
    }

    fn check_async_generator_result(
        &mut self,
        id: ObjectId,
        result: Result<(), JsError>,
    ) -> Result<(), JsError> {
        if result.is_err() {
            let generator = self.async_generator(id)?;
            generator.state = State::Completed;
            generator.queue.clear();
        }
        result
    }

    #[inline(never)]
    fn advance_async_generator(&mut self, id: ObjectId) -> Result<(), JsError> {
        loop {
            let data = self.async_generator(id)?;
            if data.queue.is_empty() || matches!(data.state, State::Executing | State::Draining) {
                return Ok(());
            }
            let input = data.queue.front().unwrap().input.clone();
            let state = mem::replace(&mut data.state, State::Executing);
            self.tick()?;
            match state {
                State::SuspendedStart(execution) if matches!(input, ResumeValue::Normal(_)) => {
                    self.drive_async_generator(id, execution, None)?;
                }
                State::SuspendedYield(execution) => match input {
                    ResumeValue::Return(value) => match self.promise_for_await(value) {
                        Ok(promise) => self.wait_for_async_generator(
                            promise,
                            id,
                            GeneratorAwait::Return(execution),
                        )?,
                        Err(error) => {
                            let value = self.error_reason(error)?;
                            self.drive_async_generator(
                                id,
                                execution,
                                Some(ResumeValue::Throw(value)),
                            )?;
                        }
                    },
                    other => self.drive_async_generator(id, execution, Some(other))?,
                },
                State::SuspendedStart(_) | State::Completed => {
                    self.async_generator(id)?.state = State::Draining;
                    match input {
                        ResumeValue::Normal(_) => {
                            self.complete_async_generator(id, Ok(Value::Undefined), true)?
                        }
                        ResumeValue::Throw(value) => {
                            self.complete_async_generator(id, Err(value), true)?
                        }
                        ResumeValue::Return(value) => match self.promise_for_await(value) {
                            Ok(promise) => {
                                self.wait_for_async_generator(promise, id, GeneratorAwait::Closed)?;
                                return Ok(());
                            }
                            Err(error) => {
                                let value = self.error_reason(error)?;
                                self.complete_async_generator(id, Err(value), true)?;
                            }
                        },
                    }
                    self.async_generator(id)?.state = State::Completed;
                }
                State::Executing | State::Draining => unreachable!(),
            }
        }
    }

    #[inline(never)]
    fn drive_async_generator(
        &mut self,
        id: ObjectId,
        mut execution: Box<Execution>,
        input: Option<ResumeValue>,
    ) -> Result<(), JsError> {
        let owner = execution.owner();
        let result = self.with_function_realm(owner, |vm| {
            let referrer = vm.referrer_for_owner(owner)?;
            vm.with_referrer(referrer, |vm| Ok(vm.run_execution(&mut execution, input)))
        })?;
        match result {
            RunResult::Await(promise) => {
                self.wait_for_async_generator(promise, id, GeneratorAwait::Body(execution))
            }
            RunResult::Yield(result) => {
                let promise = self.pop_async_request(id)?;
                self.resolve_promise(promise, result)?;
                self.async_generator(id)?.state = State::SuspendedYield(execution);
                Ok(())
            }
            RunResult::Completed(flow) => {
                self.async_generator(id)?.state = State::Draining;
                let settlement = match flow {
                    Ok(Completion::Normal(_)) => Ok(Value::Undefined),
                    Ok(Completion::Return(value)) => Ok(value),
                    Err(error) => Err(self.error_reason(error)?),
                    _ => {
                        return Err(JsError::new(
                            ErrorKind::InternalError,
                            "Invalid async generator completion",
                        ));
                    }
                };
                self.complete_async_generator(id, settlement, true)?;
                self.async_generator(id)?.state = State::Completed;
                Ok(())
            }
        }
    }

    fn wait_for_async_generator(
        &mut self,
        promise: ObjectId,
        generator: ObjectId,
        waiting: GeneratorAwait,
    ) -> Result<(), JsError> {
        self.register_reaction(promise, ReactionPair::AsyncGenerator { generator, waiting })
    }

    #[inline(never)]
    pub(crate) fn resume_async_generator(
        &mut self,
        id: ObjectId,
        waiting: GeneratorAwait,
        argument: Value,
        rejected: bool,
    ) -> Result<(), JsError> {
        if self.call_depth >= self.limits.max_call_depth {
            return self.check_async_generator_result(
                id,
                Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Call depth budget exhausted",
                )),
            );
        }
        self.call_depth += 1;
        let result = self
            .finish_async_generator_await(id, waiting, argument, rejected)
            .and_then(|_| self.advance_async_generator(id));
        self.call_depth -= 1;
        self.check_async_generator_result(id, result)
    }

    fn finish_async_generator_await(
        &mut self,
        id: ObjectId,
        waiting: GeneratorAwait,
        value: Value,
        rejected: bool,
    ) -> Result<(), JsError> {
        match waiting {
            GeneratorAwait::Body(execution) => {
                let input = if rejected {
                    ResumeValue::Throw(value)
                } else {
                    ResumeValue::Normal(value)
                };
                self.drive_async_generator(id, execution, Some(input))
            }
            GeneratorAwait::Return(execution) => {
                let input = if rejected {
                    ResumeValue::Throw(value)
                } else {
                    ResumeValue::Return(value)
                };
                self.drive_async_generator(id, execution, Some(input))
            }
            GeneratorAwait::Closed => {
                let result = if rejected { Err(value) } else { Ok(value) };
                self.complete_async_generator(id, result, true)?;
                self.async_generator(id)?.state = State::Completed;
                Ok(())
            }
        }
    }

    fn pop_async_request(&mut self, id: ObjectId) -> Result<ObjectId, JsError> {
        let request = self.async_generator(id)?.queue.pop_front().ok_or_else(|| {
            JsError::new(ErrorKind::InternalError, "Missing async generator request")
        })?;
        Ok(request.capability.promise.as_object().unwrap())
    }

    fn complete_async_generator(
        &mut self,
        id: ObjectId,
        result: Result<Value, Value>,
        done: bool,
    ) -> Result<(), JsError> {
        let promise = self.pop_async_request(id)?;
        match result {
            Ok(value) => {
                let result = self.iterator_result(value, done)?;
                self.resolve_promise(promise, result)
            }
            Err(value) => self.settle_promise(promise, value, true),
        }
    }
}
