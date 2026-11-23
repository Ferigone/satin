//! Generator intrinsics and their suspended execution contexts.
mod async_generator;
use crate::{
    ErrorKind, JsError, ObjectId, Value, Vm,
    builtins::{create_dynamic_function, method, replace_construct},
    bytecode::Code,
    execution::{Execution, ResumeValue, RunResult},
    heap::{EnvId, ObjectKind, TraceEdges},
    parser::FunctionKind,
    patterns::Invocation,
    vm::Completion,
};
pub(crate) use async_generator::{AsyncGenerator, GeneratorAwait};
use std::{mem, rc::Rc};

pub(crate) enum GeneratorState {
    SuspendedStart(Box<Execution>),
    SuspendedYield(Box<Execution>),
    Executing,
    Completed,
}

impl GeneratorState {
    pub(crate) fn visit(&self, visit: impl FnMut(&Value)) {
        if let Self::SuspendedStart(code) | Self::SuspendedYield(code) = self {
            code.visit(visit);
        }
    }

    pub(crate) fn trace(&self, pending: &mut TraceEdges) {
        self.visit(|value| pending.value(value));
    }

    pub(crate) fn extra_bytes(&self) -> usize {
        match self {
            Self::SuspendedStart(code) | Self::SuspendedYield(code) => code.extra_bytes(),
            _ => 0,
        }
    }
}

pub(crate) fn install(vm: &mut Vm) {
    install_kind(vm, vm.iterator_prototype.unwrap(), FunctionKind::Generator);
    let async_iterator = crate::async_iterator::install(vm);
    install_kind(vm, async_iterator, FunctionKind::AsyncGenerator);
}

fn install_kind(vm: &mut Vm, iterator: ObjectId, kind: FunctionKind) {
    let function = vm
        .new_object_with_prototype(Value::Object(vm.function_prototype))
        .unwrap();
    let generator = vm
        .new_object_with_prototype(Value::Object(iterator))
        .unwrap();
    let asynchronous = kind == FunctionKind::AsyncGenerator;
    let (constructor_name, tag_name) = if asynchronous {
        vm.async_generator_prototype = generator.as_object();
        vm.async_generator_function_prototype = function.as_object();
        ("AsyncGeneratorFunction", "AsyncGenerator")
    } else {
        vm.generator_prototype = generator.as_object();
        vm.generator_function_prototype = function.as_object();
        ("GeneratorFunction", "Generator")
    };
    vm.root(function.clone());
    vm.root(generator.clone());

    let constructor = vm.native(
        constructor_name,
        1,
        false,
        Rc::new(move |vm, _, args| create_dynamic_function(vm, args, None, kind)),
    );
    replace_construct(vm, constructor.clone(), move |vm, _, args, target| {
        create_dynamic_function(vm, args, Some(target), kind)
    });
    let function_constructor = vm.get_global("Function").unwrap();
    vm.set_prototype(
        constructor.as_object().unwrap(),
        function_constructor.as_object(),
    )
    .unwrap();
    vm.define_data_property(
        constructor.clone(),
        "prototype",
        function.clone(),
        false,
        false,
        false,
    )
    .unwrap();
    vm.define_data_property(
        function.clone(),
        "constructor",
        constructor,
        false,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property(
        function.clone(),
        "prototype",
        generator.clone(),
        false,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property(
        generator.clone(),
        "constructor",
        function.clone(),
        false,
        false,
        true,
    )
    .unwrap();

    let tag = vm.well_known_key("toStringTag");
    vm.define_data_property_key(
        function,
        tag.clone(),
        constructor_name.into(),
        false,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property_key(generator.clone(), tag, tag_name.into(), false, false, true)
        .unwrap();
    let methods = [
        ("next", ResumeValue::Normal as fn(Value) -> ResumeValue),
        ("return", ResumeValue::Return),
        ("throw", ResumeValue::Throw),
    ];
    for (name, input) in methods {
        method(vm, generator.clone(), name, 1, move |vm, this, args| {
            let input = input(args.first().cloned().unwrap_or_default());
            if asynchronous {
                vm.request_async_generator(this, input)
            } else {
                vm.resume_generator(this, input)
            }
        });
    }
}

impl Vm {
    #[inline(never)]
    pub(crate) fn invoke_generator(
        &mut self,
        code: Rc<Code>,
        parent: EnvId,
        invocation: Invocation<'_>,
    ) -> Result<Value, JsError> {
        let function = invocation.function.clone();
        let owner = function.as_object().unwrap();
        let env = self.function_environment(&code, parent, invocation)?;
        self.prepare(&code.body.scope, env)?;
        let asynchronous = code.kind == FunctionKind::AsyncGenerator;
        let fallback = if asynchronous {
            self.async_generator_prototype
        } else {
            self.generator_prototype
        }
        .unwrap();
        let prototype = self.prototype_from_constructor(function, fallback)?;
        let execution = Execution::new(self, owner, code.body.clone(), env)?;
        self.ensure_allocation(mem::size_of::<Execution>() + 384)?;
        let execution = Box::new(execution);
        let kind = if asynchronous {
            ObjectKind::AsyncGenerator(AsyncGenerator::new(execution))
        } else {
            ObjectKind::Generator(GeneratorState::SuspendedStart(execution))
        };
        self.allocate_constructed(kind, prototype)
    }

    #[inline(never)]
    fn resume_generator(&mut self, this: Value, input: ResumeValue) -> Result<Value, JsError> {
        let id = this
            .as_object()
            .ok_or_else(|| JsError::type_error("Generator receiver is not an object"))?;
        let state = self.generator_state(id)?;
        if matches!(state, GeneratorState::Executing) {
            return Err(JsError::type_error("Generator is already executing"));
        }
        if matches!(state, GeneratorState::Completed)
            || matches!(state, GeneratorState::SuspendedStart(_))
                && !matches!(input, ResumeValue::Normal(_))
        {
            *state = GeneratorState::Completed;
            return self.finish_closed_generator(input);
        }
        let state = mem::replace(state, GeneratorState::Executing);
        let (mut execution, input) = match state {
            GeneratorState::SuspendedStart(execution) => (execution, None),
            GeneratorState::SuspendedYield(execution) => (execution, Some(input)),
            _ => unreachable!(),
        };
        let referrer = self.referrer_for_owner(execution.owner());
        let result = match referrer {
            Ok(referrer) => {
                self.with_referrer(referrer, |vm| Ok(vm.run_execution(&mut execution, input)))
            }
            Err(error) => Err(error),
        };
        if let Ok(RunResult::Yield(value)) = result {
            *self.generator_state(id)? = GeneratorState::SuspendedYield(execution);
            return Ok(value);
        }
        *self.generator_state(id)? = GeneratorState::Completed;
        match result? {
            RunResult::Completed(Ok(Completion::Normal(_))) => {
                self.iterator_result(Value::Undefined, true)
            }
            RunResult::Completed(Ok(Completion::Return(value))) => {
                self.iterator_result(value, true)
            }
            RunResult::Completed(Err(error)) => Err(error),
            _ => Err(JsError::new(
                ErrorKind::InternalError,
                "Invalid generator completion",
            )),
        }
    }

    fn generator_state(&mut self, id: ObjectId) -> Result<&mut GeneratorState, JsError> {
        if let ObjectKind::Generator(state) = &mut self.heap.object_mut(id)?.kind {
            Ok(state)
        } else {
            Err(JsError::type_error(
                "Generator method requires a Generator receiver",
            ))
        }
    }

    fn finish_closed_generator(&mut self, input: ResumeValue) -> Result<Value, JsError> {
        match input {
            ResumeValue::Normal(_) => self.iterator_result(Value::Undefined, true),
            ResumeValue::Return(value) => self.iterator_result(value, true),
            ResumeValue::Throw(value) => Err(self.thrown_error(value)?),
        }
    }
}
