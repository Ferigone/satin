//! Owned bytecode frames for execution that can suspend at await or yield.
mod controls;
mod delegation;
mod destructuring;

use crate::{
    ErrorKind, JsError, ObjectId, Value, Vm,
    bytecode::{Chunk, Code, Op},
    heap::EnvId,
    patterns::Invocation,
    promise::{PromiseCapability, ReactionPair},
    vm::{Completion, Item, Reference},
};
use controls::Resume;
use std::{mem::size_of, rc::Rc};

type Flow = Result<Completion, JsError>;

struct Frame {
    chunk: Rc<Chunk>,
    pc: usize,
    env: EnvId,
    operands: Vec<Item>,
    result: Option<Value>,
    waiting: Option<Resume>,
    awaiting_control: bool,
    delegate: Option<delegation::Delegation>,
    patterns: Vec<destructuring::ActivePattern>,
}

pub(crate) struct Execution {
    owner: ObjectId,
    frames: Vec<Frame>,
}

pub(crate) struct AsyncExecution {
    execution: Execution,
    capability: PromiseCapability,
}

#[derive(Clone)]
pub(crate) enum ResumeValue {
    Normal(Value),
    Return(Value),
    Throw(Value),
}
impl ResumeValue {
    pub(crate) fn value(&self) -> &Value {
        match self {
            Self::Normal(value) | Self::Return(value) | Self::Throw(value) => value,
        }
    }
}

pub(crate) enum RunResult {
    Completed(Flow),
    Await(ObjectId),
    Yield(Value),
}

enum Advance {
    Keep,
    Finish(Completion),
    Child {
        chunk: Rc<Chunk>,
        env: EnvId,
        resume: Resume,
    },
    Await(ObjectId),
    AwaitControl(ObjectId, Resume),
    Yield(Value),
}
impl Advance {
    fn child(chunk: &Rc<Chunk>, env: EnvId, resume: Resume) -> Self {
        Self::Child {
            chunk: chunk.clone(),
            env,
            resume,
        }
    }
    fn normal(result: Option<Value>) -> Self {
        Self::Finish(Completion::Normal(result))
    }
}

impl Frame {
    fn new(chunk: Rc<Chunk>, env: EnvId) -> Self {
        Self {
            chunk,
            env,
            pc: 0,
            operands: Vec::new(),
            result: None,
            waiting: None,
            awaiting_control: false,
            delegate: None,
            patterns: Vec::new(),
        }
    }
    fn absorb(&mut self, value: Option<Value>) {
        if value.is_some() {
            self.result = value;
        }
    }
    fn finish(mut self, vm: &mut Vm) -> Flow {
        if let Some(operand) = self.operands.pop() {
            self.result = Some(vm.value(operand)?);
        }
        Ok(Completion::Normal(self.result))
    }
}
impl Execution {
    pub(crate) fn new(
        vm: &mut Vm,
        owner: ObjectId,
        chunk: Rc<Chunk>,
        env: EnvId,
    ) -> Result<Self, JsError> {
        let mut frames = Vec::new();
        reserve_one(vm, &mut frames)?;
        frames.push(Frame::new(chunk, env));
        Ok(Self { owner, frames })
    }

    pub(crate) fn owner(&self) -> ObjectId {
        self.owner
    }

    pub(crate) fn visit(&self, mut visit: impl FnMut(&Value)) {
        // The owner retains the compiled code, including all nested chunks.
        visit(&Value::Object(self.owner));
        for frame in &self.frames {
            visit(&Value::Object(frame.env.0));
            if let Some(value) = &frame.result {
                visit(value);
            }
            for item in &frame.operands {
                visit_operand(item, &mut visit);
            }
            for pattern in &frame.patterns {
                pattern.visit(&mut visit);
            }
            if let Some(waiting) = &frame.waiting {
                waiting.visit(&mut visit);
            }
            if let Some(delegate) = &frame.delegate {
                delegate.visit(&mut visit);
            }
        }
    }

    pub(crate) fn extra_bytes(&self) -> usize {
        size_of::<Self>()
            + self.frames.capacity() * size_of::<Frame>()
            + self
                .frames
                .iter()
                .map(|frame| {
                    frame.operands.capacity() * size_of::<Item>()
                        + frame
                            .operands
                            .iter()
                            .map(|item| match item {
                                Item::Arguments(args) => args.capacity() * size_of::<Value>(),
                                _ => 0,
                            })
                            .sum::<usize>()
                        + frame.patterns.capacity() * size_of::<destructuring::ActivePattern>()
                        + frame.patterns.iter().map(|p| p.extra_bytes()).sum::<usize>()
                        + frame.waiting.as_ref().map_or(0, Resume::extra_bytes)
                })
                .sum::<usize>()
    }
}

impl AsyncExecution {
    pub(crate) fn owner(&self) -> ObjectId {
        self.execution.owner()
    }

    pub(crate) fn visit(&self, mut visit: impl FnMut(&Value)) {
        self.capability.visit(&mut visit);
        self.execution.visit(visit);
    }

    pub(crate) fn extra_bytes(&self) -> usize {
        self.execution.extra_bytes() + size_of::<PromiseCapability>()
    }
}

fn visit_operand(item: &Item, visit: &mut impl FnMut(&Value)) {
    match item {
        Item::Value(value) => visit(value),
        Item::Arguments(args) => args.iter().for_each(visit),
        Item::CallTarget { function, this, .. } => {
            visit(function);
            visit(this);
        }
        Item::Reference(reference) => match reference {
            Reference::Binding(env, name) | Reference::Import(env, name) => {
                visit(&Value::Object(env.0));
                visit(&Value::String(name.clone()));
            }
            Reference::With(object, name) | Reference::Global(object, name) => {
                visit(&Value::Object(*object));
                visit(&Value::String(name.clone()));
            }
            Reference::Unresolvable(name) => {
                visit(&Value::String(name.clone()));
            }
            Reference::Property(receiver, key) => {
                visit(receiver);
                visit(key);
            }
            Reference::Private(receiver, _) => visit(receiver),
            Reference::Super(base, receiver, key) => {
                visit(base);
                visit(receiver);
                visit(key);
            }
        },
    }
}

#[cfg(test)]
#[test]
fn resolved_global_operand_traces_its_binding_object() {
    let mut vm = Vm::new();
    let global = vm.new_object();
    let operand = Item::Reference(Reference::Global(
        global.as_object().unwrap(),
        "binding".into(),
    ));
    let mut edges = Vec::new();
    visit_operand(&operand, &mut |value| edges.push(value.clone()));
    assert_eq!(edges, [global, Value::from("binding")]);
}

fn reserve_one<T>(vm: &mut Vm, elements: &mut Vec<T>) -> Result<(), JsError> {
    if elements.len() == elements.capacity() {
        let next = elements.capacity().max(2).saturating_mul(2);
        vm.reserve_allocation(next.saturating_mul(size_of::<T>()))?;
        elements.reserve_exact(next - elements.len());
    }
    Ok(())
}

impl Vm {
    #[inline(never)]
    pub(crate) fn invoke_async(
        &mut self,
        code: Rc<Code>,
        parent: EnvId,
        invocation: Invocation<'_>,
    ) -> Result<Value, JsError> {
        let capability = self.new_promise_capability()?;
        let promise = capability.promise.clone();
        let owner = invocation.function.as_object().unwrap();
        match self.function_environment(&code, parent, invocation) {
            Ok((env, body)) => self.start_async_body(owner, body, env, capability, false)?,
            Err(error) => self.finish_async(capability, Err(error))?,
        }
        Ok(promise)
    }

    pub(crate) fn start_async_body(
        &mut self,
        owner: ObjectId,
        chunk: Rc<Chunk>,
        env: EnvId,
        capability: PromiseCapability,
        prepared: bool,
    ) -> Result<(), JsError> {
        if !chunk.can_suspend {
            let result = if prepared {
                self.run_prepared_chunk(&chunk, env)
            } else {
                self.run_chunk(&chunk, env)
            };
            return self.finish_async(capability, result);
        }
        if !prepared && let Err(error) = self.prepare(&chunk.scope, env) {
            return self.finish_async(capability, Err(error));
        }
        self.reserve_allocation(size_of::<AsyncExecution>())?;
        let execution = Execution::new(self, owner, chunk, env)?;
        self.drive_async(
            Box::new(AsyncExecution {
                execution,
                capability,
            }),
            None,
        )
    }

    pub(crate) fn resume_async(
        &mut self,
        execution: Box<AsyncExecution>,
        argument: Value,
        rejected: bool,
    ) -> Result<(), JsError> {
        let owner = execution.owner();
        let result = self.with_function_realm(owner, |vm| {
            if !vm.can_enter_call() {
                return Err(vm.call_depth_error());
            }
            let input = if rejected {
                ResumeValue::Throw(argument)
            } else {
                ResumeValue::Normal(argument)
            };
            let referrer = vm.referrer_for_owner(owner)?;
            vm.call_depth += 1;
            let result = vm.with_referrer(referrer, |vm| vm.drive_async(execution, Some(input)));
            vm.call_depth -= 1;
            result
        });
        if let Err(error) = &result {
            self.record_async_execution_limit(owner, error);
        }
        result
    }

    fn drive_async(
        &mut self,
        mut execution: Box<AsyncExecution>,
        input: Option<ResumeValue>,
    ) -> Result<(), JsError> {
        match self.run_execution(&mut execution.execution, input) {
            RunResult::Completed(result) => self.finish_async(execution.capability, result),
            RunResult::Await(promise) => {
                self.register_reaction(promise, ReactionPair::Resume(execution))
            }
            RunResult::Yield(_) => Err(JsError::new(
                ErrorKind::InternalError,
                "Yield in an async function",
            )),
        }
    }

    pub(crate) fn run_execution(
        &mut self,
        execution: &mut Execution,
        input: Option<ResumeValue>,
    ) -> RunResult {
        let depth = self.execution_depth;
        let result = self.run_frames(&mut execution.frames, input, depth);
        self.execution_depth = depth;
        result
    }

    fn finish_async(&mut self, capability: PromiseCapability, result: Flow) -> Result<(), JsError> {
        let promise = capability.promise.as_object().unwrap();
        match result {
            Ok(Completion::Normal(_)) => self.resolve_promise(promise, Value::Undefined),
            Ok(Completion::Return(value)) => self.resolve_promise(promise, value),
            Err(error) => {
                let reason = self.error_reason(error)?;
                self.settle_promise(promise, reason, true)
            }
            Ok(Completion::Break(..) | Completion::Continue(..)) => Err(JsError::new(
                ErrorKind::InternalError,
                "Unexpected async body abrupt completion",
            )),
        }
    }

    #[inline(never)]
    fn run_frames(
        &mut self,
        frames: &mut Vec<Frame>,
        mut input: Option<ResumeValue>,
        base_depth: usize,
    ) -> RunResult {
        let mut completed: Option<Flow> = None;
        // Owned frames share one driver stack frame and its opcode dispatch.
        if !frames.is_empty() && !self.permits_bytecode_depth(base_depth + 2) {
            return RunResult::Completed(Err(self.bytecode_depth_error(base_depth + 2)));
        }
        while !frames.is_empty() {
            if completed
                .as_ref()
                .is_some_and(|result| result.as_ref().is_err_and(JsError::uncatchable))
            {
                break;
            }
            if frames.len() > self.limits().max_bytecode_depth {
                return RunResult::Completed(Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Owned bytecode frame depth budget exhausted",
                )));
            }
            self.execution_depth = base_depth + 2;
            let frame = frames.last_mut().unwrap();
            let advance = if let Some(input) = input.take() {
                self.resume_frame(frame, input)
            } else if let Some(child) = completed.take() {
                let op = &frame.chunk.ops[frame.pc - 1];
                controls::after_child(
                    self,
                    op,
                    frame.env,
                    frame.chunk.strict,
                    frame.waiting.take().unwrap(),
                    child,
                )
            } else if frame.pc == frame.chunk.ops.len() {
                completed = Some(frames.pop().unwrap().finish(self));
                continue;
            } else {
                self.advance_frame(frame)
            };
            match advance {
                Ok(Advance::Keep) => {}
                Ok(Advance::Finish(Completion::Normal(result))) => frame.absorb(result),
                Ok(Advance::Await(promise)) => return RunResult::Await(promise),
                Ok(Advance::AwaitControl(promise, resume)) => {
                    frame.awaiting_control = true;
                    frame.waiting = Some(resume);
                    return RunResult::Await(promise);
                }
                Ok(Advance::Yield(result)) => return RunResult::Yield(result),
                Ok(Advance::Child { chunk, env, resume }) => {
                    frame.waiting = Some(resume);
                    if chunk.can_suspend {
                        if let Err(error) =
                            reserve_one(self, frames).and_then(|_| self.prepare(&chunk.scope, env))
                        {
                            completed = Some(Err(error));
                        } else {
                            frames.push(Frame::new(chunk, env));
                        }
                    } else {
                        completed = Some(self.run_chunk(&chunk, env));
                    }
                }
                result => {
                    let flow = result.map(|advance| match advance {
                        Advance::Finish(completion) => completion.update_empty(frame.result.take()),
                        _ => unreachable!(),
                    });
                    let flow = destructuring::close_patterns(self, &mut frame.patterns, flow);
                    frames.pop();
                    completed = Some(flow);
                }
            }
        }
        RunResult::Completed(completed.unwrap())
    }

    #[inline(never)]
    fn resume_frame(&mut self, frame: &mut Frame, input: ResumeValue) -> Result<Advance, JsError> {
        if !frame.awaiting_control {
            return delegation::resume(self, frame, input);
        }
        frame.awaiting_control = false;
        let completed = match input {
            ResumeValue::Normal(value) => Ok(Completion::Normal(Some(value))),
            ResumeValue::Return(value) => Ok(Completion::Return(value)),
            ResumeValue::Throw(value) => Err(self.thrown_error(value)?),
        };
        controls::after_child(
            self,
            &frame.chunk.ops[frame.pc - 1],
            frame.env,
            frame.chunk.strict,
            frame.waiting.take().unwrap(),
            completed,
        )
    }

    #[inline(never)]
    fn advance_frame(&mut self, frame: &mut Frame) -> Result<Advance, JsError> {
        self.tick()?;
        let op = &frame.chunk.ops[frame.pc];
        frame.pc += 1;
        if matches!(op, Op::StatementValue) {
            frame.result = Some(self.pop_value(&mut frame.operands)?);
            return Ok(Advance::Keep);
        }
        if let Op::Pattern(step) = op {
            let step = *step;
            destructuring::step(self, frame, step)?;
            return Ok(Advance::Keep);
        }
        if matches!(op, Op::Await) {
            let value = self.pop_value(&mut frame.operands)?;
            return Ok(Advance::Await(self.promise_for_await(value)?));
        }
        if let Op::Yield {
            delegate,
            asynchronous,
        } = op
        {
            let value = self.pop_value(&mut frame.operands)?;
            if *delegate {
                frame.delegate = Some(delegation::Delegation::new(self, value, *asynchronous)?);
                return delegation::resume(self, frame, ResumeValue::Normal(Value::Undefined));
            }
            return self.iterator_result(value, false).map(Advance::Yield);
        }
        reserve_one(self, &mut frame.operands)?;
        if self.run_stack_op(
            op,
            &mut frame.operands,
            frame.env,
            frame.chunk.strict,
            &mut frame.pc,
        )? {
            return Ok(Advance::Keep);
        }
        if op.can_suspend() {
            controls::start(self, op, frame.env)
        } else {
            self.run_control_op(op, frame.env, frame.chunk.strict)
                .map(Advance::Finish)
        }
    }
}
