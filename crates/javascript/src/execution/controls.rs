use super::{Advance, Flow};
use crate::{
    ErrorKind, JsError, JsString, Value, Vm,
    bytecode::{Chunk, Op, PatternCode},
    heap::{Binding, EnvId, EnvKind, Environment},
    iterator::IteratorRecord,
    patterns::PatternMode,
    vm::Completion,
};
use std::{mem::size_of, rc::Rc, vec::IntoIter};

pub(super) enum Resume {
    Pass,
    Label,
    Initialize,
    Select,
    BranchBody,
    Return,
    Throw,
    Loop(LoopState),
    EachSource,
    Each(EachState),
    EachStep(EachState),
    EachClose(Flow),
    SwitchValue,
    SwitchTest(SwitchTest),
    SwitchBody(SwitchBody),
    TryBody,
    Catch,
    Finally(Flow),
}

enum Phase {
    Initial,
    Test,
    Body,
    Update,
}
pub(super) struct LoopState {
    scope: EnvId,
    phase: Phase,
    result: Option<Value>,
}
enum Items {
    Keys(IntoIter<JsString>, usize),
    Iterator(IteratorRecord),
    AsyncIterator(IteratorRecord),
}
pub(super) struct EachState {
    items: Items,
    result: Option<Value>,
}
pub(super) struct SwitchTest {
    scope: EnvId,
    value: Value,
    current: usize,
    default: Option<usize>,
}
pub(super) struct SwitchBody {
    scope: EnvId,
    current: usize,
    result: Option<Value>,
}

impl Resume {
    pub(super) fn visit(&self, visit: &mut impl FnMut(&Value)) {
        match self {
            Self::Loop(state) => {
                visit(&Value::Object(state.scope.0));
                state.result.iter().for_each(visit);
            }
            Self::Each(state) | Self::EachStep(state) => {
                state.result.iter().for_each(&mut *visit);
                match &state.items {
                    Items::Keys(keys, _) => {
                        for key in keys.as_slice() {
                            visit(&Value::String(key.clone()));
                        }
                    }
                    Items::Iterator(iterator) | Items::AsyncIterator(iterator) => {
                        visit(&iterator.iterator);
                        visit(&iterator.next);
                    }
                }
            }
            Self::SwitchTest(state) => {
                visit(&Value::Object(state.scope.0));
                visit(&state.value);
            }
            Self::SwitchBody(state) => {
                visit(&Value::Object(state.scope.0));
                state.result.iter().for_each(visit);
            }
            Self::Finally(completion) | Self::EachClose(completion) => match completion {
                Ok(
                    Completion::Normal(Some(value))
                    | Completion::Break(_, Some(value))
                    | Completion::Continue(_, Some(value))
                    | Completion::Return(value),
                ) => visit(value),
                Err(error) => error.thrown.iter().for_each(visit),
                _ => {}
            },
            _ => {}
        }
    }

    pub(super) fn extra_bytes(&self) -> usize {
        match self {
            Self::Each(EachState {
                items: Items::Keys(_, bytes),
                ..
            }) => *bytes,
            Self::Finally(Err(error)) | Self::EachClose(Err(error)) => error.message.capacity(),
            _ => 0,
        }
    }
}

fn expression(completion: Flow) -> Result<Value, JsError> {
    if let Completion::Normal(value) = completion? {
        Ok(value.unwrap_or_default())
    } else {
        Err(JsError::new(
            ErrorKind::InternalError,
            "Abrupt completion in expression bytecode",
        ))
    }
}

fn new_scope(vm: &mut Vm, parent: EnvId) -> Result<EnvId, JsError> {
    vm.ensure_allocation(384)?;
    Ok(vm
        .heap
        .allocate_env(Environment::new(Some(parent), EnvKind::Block, None)))
}

pub(super) fn start(vm: &mut Vm, op: &Op, env: EnvId) -> Result<Advance, JsError> {
    Ok(match op {
        Op::Evaluate(expression) => Advance::child(expression, env, Resume::Pass),
        Op::Initialize(_, _, Some(value)) => Advance::child(value, env, Resume::Initialize),
        Op::Block(body) => Advance::child(body, new_scope(vm, env)?, Resume::Pass),
        Op::Label(_, body) => Advance::child(body, env, Resume::Label),
        Op::Branch(test, ..) => Advance::child(test, env, Resume::Select),
        Op::While(..) => LoopState::new(env).test(vm, op)?,
        Op::DoWhile(..) => LoopState::new(env).body(vm, op)?,
        Op::For {
            initial, lexical, ..
        } => {
            let scope = if lexical.is_empty() {
                env
            } else {
                let scope = new_scope(vm, env)?;
                for (name, mutable) in lexical {
                    vm.reserve_binding(name)?;
                    vm.heap.environment_mut(scope).bindings.insert(
                        name.clone(),
                        Binding {
                            value: Value::Undefined,
                            mutable: *mutable,
                            initialized: false,
                            lexical: true,
                            strict_immutable: true,
                        },
                    );
                }
                scope
            };
            let state = LoopState::new(scope);
            if let Some(initial) = initial {
                Advance::child(initial, scope, Resume::Loop(state))
            } else {
                state.test(vm, op)?
            }
        }
        Op::ForIn(kind, pattern, source, _, _) | Op::ForOf(kind, pattern, source, _, _, _) => {
            let head = vm.iteration_scope(env, *kind, pattern)?;
            Advance::child(source, head, Resume::EachSource)
        }
        Op::Return(Some(value)) => Advance::child(value, env, Resume::Return),
        Op::Throw(value) => Advance::child(value, env, Resume::Throw),
        Op::Switch(value, ..) => Advance::child(value, env, Resume::SwitchValue),
        Op::Try(body, ..) => Advance::child(body, env, Resume::TryBody),
        _ => unreachable!("suspending control opcode expected"),
    })
}

pub(super) fn after_child(
    vm: &mut Vm,
    op: &Op,
    env: EnvId,
    strict: bool,
    resume: Resume,
    completed: Flow,
) -> Result<Advance, JsError> {
    // A generator return can interrupt evaluation of an expression as well
    // as a statement. Body continuations still run cleanup where needed.
    if matches!(completed, Ok(Completion::Return(_)))
        && matches!(
            resume,
            Resume::Initialize
                | Resume::Select
                | Resume::Return
                | Resume::Throw
                | Resume::EachSource
                | Resume::SwitchValue
                | Resume::SwitchTest(_)
        )
    {
        return completed.map(Advance::Finish);
    }
    match resume {
        Resume::Pass => completed.map(Advance::Finish),
        Resume::Label => {
            let Op::Label(label, _) = op else {
                unreachable!()
            };
            Ok(Advance::Finish(completed?.leave_label(*label)))
        }
        Resume::Initialize => {
            let Op::Initialize(kind, pattern, _) = op else {
                unreachable!()
            };
            vm.bind_pattern(
                pattern,
                expression(completed)?,
                env,
                PatternMode::Binding(*kind),
                strict,
                None,
            )?;
            Ok(Advance::normal(None))
        }
        Resume::Select => {
            let Op::Branch(_, yes, no) = op else {
                unreachable!()
            };
            let chosen = if expression(completed)?.is_truthy() {
                Some(yes)
            } else {
                no.as_ref()
            };
            Ok(
                chosen.map_or(Advance::normal(Some(Value::Undefined)), |chunk| {
                    Advance::child(chunk, env, Resume::BranchBody)
                }),
            )
        }
        Resume::BranchBody => Ok(Advance::Finish(
            completed?.update_empty(Some(Value::Undefined)),
        )),
        Resume::Return => Ok(Advance::Finish(Completion::Return(expression(completed)?))),
        Resume::Throw => Err(vm.thrown_error(expression(completed)?)?),
        Resume::Loop(state) => state.advance(vm, op, env, completed),
        Resume::EachSource => {
            let source = expression(completed)?;
            let items = match op {
                Op::ForOf(_, _, _, _, true, _) => {
                    Items::AsyncIterator(vm.get_async_iterator(source)?)
                }
                Op::ForOf(..) => Items::Iterator(vm.get_iterator(source)?),
                _ => {
                    let keys = vm.enumerable_keys(source)?;
                    let bytes = keys.capacity() * size_of::<JsString>();
                    Items::Keys(keys.into_iter(), bytes)
                }
            };
            EachState {
                items,
                result: Some(Value::Undefined),
            }
            .next(vm, op, env, strict)
        }
        Resume::Each(state) => state.advance(vm, op, env, strict, completed),
        Resume::EachStep(state) => {
            state.after_async_next(vm, op, env, strict, expression(completed)?)
        }
        Resume::EachClose(previous) => finish_async_close(
            previous,
            expression(completed).and_then(|result| check_async_result(&result)),
        ),
        Resume::SwitchValue => {
            let Op::Switch(_, cases, plan) = op else {
                unreachable!()
            };
            let value = expression(completed)?;
            let scope = new_scope(vm, env)?;
            vm.prepare(plan, scope)?;
            SwitchTest {
                scope,
                value,
                current: 0,
                default: None,
            }
            .search(vm, cases)
        }
        Resume::SwitchTest(mut state) => {
            let Op::Switch(_, cases, _) = op else {
                unreachable!()
            };
            let test = expression(completed)?;
            vm.charge_bigint_comparison(&test, &state.value)?;
            if test == state.value {
                Ok(SwitchBody::new(state.scope, state.current).enter(cases))
            } else {
                state.current += 1;
                state.search(vm, cases)
            }
        }
        Resume::SwitchBody(mut state) => {
            let Op::Switch(_, cases, _) = op else {
                unreachable!()
            };
            match completed?.update_empty(state.result.take()) {
                Completion::Normal(value) => {
                    state.result = value;
                    state.current += 1;
                    Ok(state.enter(cases))
                }
                Completion::Break(None, value) => Ok(Advance::normal(value)),
                abrupt => Ok(Advance::Finish(abrupt)),
            }
        }
        Resume::TryBody => {
            let Op::Try(_, catch, finally) = op else {
                unreachable!()
            };
            if let (Err(error), Some((pattern, body))) = (&completed, catch) {
                let binding = catch_scope(vm, env, strict, pattern.as_deref(), error);
                if let Ok(scope) = binding {
                    return Ok(Advance::child(body, scope, Resume::Catch));
                }
                return enter_finally(vm, env, finally.as_ref(), binding.map(|_| unreachable!()));
            }
            enter_finally(vm, env, finally.as_ref(), completed)
        }
        Resume::Catch => {
            let Op::Try(_, _, finally) = op else {
                unreachable!()
            };
            enter_finally(vm, env, finally.as_ref(), completed)
        }
        Resume::Finally(previous) => {
            let result = if matches!(completed, Ok(Completion::Normal(_))) {
                previous
            } else {
                completed
            };
            Ok(Advance::Finish(
                result?.update_empty(Some(Value::Undefined)),
            ))
        }
    }
}

impl LoopState {
    fn new(scope: EnvId) -> Self {
        Self {
            scope,
            phase: Phase::Initial,
            result: Some(Value::Undefined),
        }
    }

    fn body(mut self, vm: &mut Vm, op: &Op) -> Result<Advance, JsError> {
        let body = match op {
            Op::While(_, body, _) | Op::DoWhile(body, _, _) => {
                vm.tick()?;
                body
            }
            Op::For { body, .. } => body,
            _ => unreachable!(),
        };
        self.phase = Phase::Body;
        Ok(Advance::child(body, self.scope, Resume::Loop(self)))
    }

    fn test(mut self, vm: &mut Vm, op: &Op) -> Result<Advance, JsError> {
        let test = match op {
            Op::While(test, _, _) | Op::DoWhile(_, test, _) => Some(test),
            Op::For { test, .. } => {
                vm.tick()?;
                test.as_ref()
            }
            _ => unreachable!(),
        };
        self.phase = Phase::Test;
        if let Some(test) = test {
            Ok(Advance::child(test, self.scope, Resume::Loop(self)))
        } else {
            self.body(vm, op)
        }
    }

    fn advance(
        mut self,
        vm: &mut Vm,
        op: &Op,
        env: EnvId,
        completed: Flow,
    ) -> Result<Advance, JsError> {
        if matches!(completed, Ok(Completion::Return(_))) {
            return completed.map(Advance::Finish);
        }
        match self.phase {
            Phase::Initial | Phase::Update => {
                completed?;
                self.test(vm, op)
            }
            Phase::Test => {
                if expression(completed)?.is_truthy() {
                    self.body(vm, op)
                } else {
                    Ok(Advance::normal(self.result))
                }
            }
            Phase::Body => {
                if let Some(completion) =
                    completed?.after_loop_body(op.loop_label(), &mut self.result)
                {
                    return Ok(Advance::Finish(completion));
                }
                if let Op::For {
                    lexical, update, ..
                } = op
                {
                    if !lexical.is_empty() {
                        let scope = new_scope(vm, env)?;
                        let bindings = &vm.heap.environment(self.scope).bindings;
                        let bytes: usize = bindings.keys().map(|key| 192 + key.len() * 2).sum();
                        vm.ensure_allocation(bytes)?;
                        let bindings = vm.heap.environment(self.scope).bindings.clone();
                        for name in bindings.keys() {
                            vm.reserve_binding(name)?;
                        }
                        vm.heap.environment_mut(scope).bindings = bindings;
                        self.scope = scope;
                    }
                    if let Some(update) = update {
                        self.phase = Phase::Update;
                        return Ok(Advance::child(update, self.scope, Resume::Loop(self)));
                    }
                }
                self.test(vm, op)
            }
        }
    }
}

impl EachState {
    fn close(mut self, vm: &mut Vm, completion: Flow) -> Result<Advance, JsError> {
        if completion.as_ref().is_err_and(JsError::uncatchable) {
            return completion.map(Advance::Finish);
        }
        match &mut self.items {
            Items::Iterator(iterator) => {
                if let Err(error) = completion {
                    return vm
                        .iterator_close(iterator, Err(error))
                        .map(|_| Advance::normal(None));
                }
                vm.iterator_close(iterator, Ok(Value::Undefined))?;
            }
            Items::AsyncIterator(iterator) => {
                return match vm.start_async_iterator_close(iterator) {
                    Ok(Some(promise)) => Ok(Advance::AwaitControl(
                        promise,
                        Resume::EachClose(completion),
                    )),
                    Ok(None) => completion.map(Advance::Finish),
                    Err(error) => finish_async_close(completion, Err(error)),
                };
            }
            Items::Keys(..) => {}
        }
        completion.map(Advance::Finish)
    }

    fn advance(
        mut self,
        vm: &mut Vm,
        op: &Op,
        env: EnvId,
        strict: bool,
        completed: Flow,
    ) -> Result<Advance, JsError> {
        match completed {
            Ok(completion) => {
                if let Some(completion) =
                    completion.after_loop_body(op.loop_label(), &mut self.result)
                {
                    return self.close(vm, Ok(completion));
                }
            }
            abrupt => return self.close(vm, abrupt),
        }
        self.next(vm, op, env, strict)
    }

    fn next(mut self, vm: &mut Vm, op: &Op, env: EnvId, strict: bool) -> Result<Advance, JsError> {
        let value = match &mut self.items {
            Items::Keys(keys, _) => keys.next().map(Value::String),
            Items::Iterator(iterator) => vm.iterator_step(iterator)?,
            Items::AsyncIterator(iterator) => {
                vm.tick()?;
                let result = vm.invoke(iterator.next.clone(), iterator.iterator.clone(), &[])?;
                let promise = vm.promise_for_await(result)?;
                return Ok(Advance::AwaitControl(promise, Resume::EachStep(self)));
            }
        };
        if let Some(value) = value {
            self.enter(vm, op, env, strict, value)
        } else {
            Ok(Advance::normal(self.result))
        }
    }

    fn after_async_next(
        self,
        vm: &mut Vm,
        op: &Op,
        env: EnvId,
        strict: bool,
        result: Value,
    ) -> Result<Advance, JsError> {
        check_async_result(&result)?;
        if vm.get_property(result.clone(), "done")?.is_truthy() {
            return Ok(Advance::normal(self.result));
        }
        let value = vm.get_property(result, "value")?;
        self.enter(vm, op, env, strict, value)
    }

    fn enter(
        self,
        vm: &mut Vm,
        op: &Op,
        env: EnvId,
        strict: bool,
        value: Value,
    ) -> Result<Advance, JsError> {
        let (kind, pattern, body) = match op {
            Op::ForIn(kind, pattern, _, body, _) | Op::ForOf(kind, pattern, _, body, _, _) => {
                (kind, pattern, body)
            }
            _ => unreachable!(),
        };
        match vm.bind_iteration_value(env, *kind, pattern, value, strict) {
            Ok(scope) => Ok(Advance::child(body, scope, Resume::Each(self))),
            Err(error) => self.close(vm, Err(error)),
        }
    }
}

fn check_async_result(result: &Value) -> Result<(), JsError> {
    if result.as_object().is_some() {
        Ok(())
    } else {
        Err(JsError::type_error(
            "Async iterator method returned a non-object result",
        ))
    }
}

fn finish_async_close(completion: Flow, close: Result<(), JsError>) -> Result<Advance, JsError> {
    if let Err(error) = close
        && (error.uncatchable() || completion.is_ok())
    {
        return Err(error);
    }
    completion.map(Advance::Finish)
}

type Cases = [(Option<Rc<Chunk>>, Rc<Chunk>)];

impl SwitchTest {
    fn search(mut self, vm: &mut Vm, cases: &Cases) -> Result<Advance, JsError> {
        for (test, _) in &cases[self.current..] {
            vm.tick()?;
            if let Some(test) = test {
                return Ok(Advance::child(test, self.scope, Resume::SwitchTest(self)));
            }
            self.default = Some(self.current);
            self.current += 1;
        }
        Ok(self
            .default
            .map_or(Advance::normal(Some(Value::Undefined)), |index| {
                SwitchBody::new(self.scope, index).enter(cases)
            }))
    }
}
impl SwitchBody {
    fn new(scope: EnvId, current: usize) -> Self {
        Self {
            scope,
            current,
            result: Some(Value::Undefined),
        }
    }
    fn enter(self, cases: &Cases) -> Advance {
        if let Some((_, body)) = cases.get(self.current) {
            Advance::child(body, self.scope, Resume::SwitchBody(self))
        } else {
            Advance::normal(self.result)
        }
    }
}

fn catch_scope(
    vm: &mut Vm,
    env: EnvId,
    strict: bool,
    pattern: Option<&PatternCode>,
    error: &JsError,
) -> Result<EnvId, JsError> {
    let scope = new_scope(vm, env)?;
    if let Some(pattern) = pattern {
        let value = if let Some(value) = &error.thrown {
            value.clone()
        } else {
            vm.error_value(error)?
        };
        vm.declare_pattern(pattern, scope, true, true)?;
        vm.bind_pattern(pattern, value, scope, PatternMode::Parameter, strict, None)?;
    }
    Ok(scope)
}

fn enter_finally(
    vm: &mut Vm,
    env: EnvId,
    finally: Option<&Rc<Chunk>>,
    completion: Flow,
) -> Result<Advance, JsError> {
    if let Some(finally) = finally
        && !completion.as_ref().is_err_and(JsError::uncatchable)
    {
        if let Err(error) = &completion {
            vm.reserve_allocation(error.message.capacity())?;
        }
        Ok(Advance::child(finally, env, Resume::Finally(completion)))
    } else {
        Ok(Advance::Finish(
            completion?.update_empty(Some(Value::Undefined)),
        ))
    }
}
