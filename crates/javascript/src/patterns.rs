//! Binding/assignment initialization over actual property and iterator protocols.
use crate::{
    ErrorKind, JsError, PropertyKey, Value, Vm,
    bytecode::{Code, PatternCode, pattern_names},
    heap::{Binding, EnvId, EnvKind, Environment, Property, PropertyKind},
    parser::VarKind,
    vm::Reference,
};
#[derive(Clone, Copy)]
pub(crate) enum PatternMode {
    Assignment,
    Binding(VarKind),
    Parameter,
}
pub(crate) struct Invocation<'a> {
    pub function: Value,
    pub this: Value,
    pub args: &'a [Value],
    pub new_target: Value,
    pub this_initialized: bool,
}
impl Vm {
    pub(crate) fn iteration_scope(
        &mut self,
        env: EnvId,
        kind: Option<VarKind>,
        pattern: &PatternCode,
    ) -> Result<EnvId, JsError> {
        let Some(kind @ (VarKind::Let | VarKind::Const)) = kind else {
            return Ok(env);
        };
        self.ensure_allocation(384)?;
        let scope = self
            .heap
            .allocate_env(Environment::new(Some(env), EnvKind::Block, None));
        self.declare_pattern(pattern, scope, kind == VarKind::Let, true)?;
        Ok(scope)
    }

    pub(crate) fn bind_iteration_value(
        &mut self,
        env: EnvId,
        kind: Option<VarKind>,
        pattern: &PatternCode,
        value: Value,
        strict: bool,
    ) -> Result<EnvId, JsError> {
        self.tick()?;
        let scope = self.iteration_scope(env, kind, pattern)?;
        let mode = kind.map_or(PatternMode::Assignment, PatternMode::Binding);
        self.bind_pattern(pattern, value, scope, mode, strict, None)?;
        Ok(scope)
    }

    // Setup is a separate frame: it must be gone while recursively executing
    // the function body on the Windows 1 MiB main stack.
    pub(crate) fn function_environment(
        &mut self,
        code: &Code,
        parent: EnvId,
        invocation: Invocation<'_>,
    ) -> Result<EnvId, JsError> {
        let Invocation {
            function,
            this,
            args,
            new_target,
            this_initialized,
        } = invocation;
        let receiver = if code.arrow {
            None
        } else if code.strict {
            Some(this)
        } else if this.is_nullish() {
            Some(self.global_this())
        } else {
            Some(self.coerce_object(this)?)
        };
        self.ensure_allocation(384)?;
        let env =
            self.heap
                .allocate_env(Environment::new(Some(parent), EnvKind::Function, receiver));
        if !code.arrow {
            let home = function
                .as_object()
                .and_then(|id| self.heap.object(id).ok())
                .and_then(|object| object.home_object);
            let record = self.heap.environment_mut(env);
            record.function = Some(function);
            record.home_object = home;
            record.new_target = Some(new_target);
            record.this_initialized = this_initialized;
        }
        let arguments_name = crate::JsString::from("arguments");
        let body_shadows_arguments = code
            .body
            .scope
            .lexical
            .iter()
            .any(|(name, _)| name == &arguments_name)
            || code
                .body
                .scope
                .functions
                .iter()
                .any(|(name, _, _)| name == &arguments_name);
        let arguments_needed = !code.arrow
            && !code.param_names.contains(&arguments_name)
            && (code.parameter_expressions || !body_shadows_arguments);
        if arguments_needed {
            let arguments = self.try_new_array(args)?;
            let callee = if code.mapped_arguments {
                Property {
                    kind: PropertyKind::Data {
                        value: self.heap.environment(env).function.clone().unwrap(),
                        writable: true,
                    },
                    enumerable: false,
                    configurable: true,
                }
            } else {
                let poison = self.throw_type_error.map(Value::Object);
                Property {
                    kind: PropertyKind::Accessor {
                        get: poison.clone(),
                        set: poison,
                    },
                    enumerable: false,
                    configurable: false,
                }
            };
            self.define_key(arguments.clone(), "callee".into(), callee)?;
            self.reserve_binding(&arguments_name)?;
            self.heap.environment_mut(env).bindings.insert(
                arguments_name,
                Binding {
                    value: arguments,
                    mutable: true,
                    initialized: true,
                    lexical: code.parameter_expressions,
                    strict_immutable: true,
                },
            );
        }
        for parameter in &code.params {
            self.declare_pattern(&parameter.pattern, env, true, code.parameter_expressions)?;
        }
        for (i, parameter) in code.params.iter().enumerate() {
            let value = if parameter.rest {
                self.try_new_array(args.get(i..).unwrap_or_default())?
            } else {
                args.get(i).cloned().unwrap_or_default()
            };
            self.bind_pattern(
                &parameter.pattern,
                value,
                env,
                PatternMode::Parameter,
                code.strict,
                None,
            )?;
        }
        if !code.parameter_expressions {
            return Ok(env);
        }
        let body = self
            .heap
            .allocate_env(Environment::new(Some(env), EnvKind::Function, None));
        for name in &code.body.scope.vars {
            if let Some(mut binding) = self.heap.environment(env).bindings.get(name).cloned() {
                binding.lexical = false;
                self.reserve_binding(name)?;
                self.heap
                    .environment_mut(body)
                    .bindings
                    .insert(name.clone(), binding);
            }
        }
        Ok(body)
    }
    pub(crate) fn declare_pattern(
        &mut self,
        pattern: &PatternCode,
        env: EnvId,
        mutable: bool,
        lexical: bool,
    ) -> Result<(), JsError> {
        for name in pattern_names(pattern) {
            self.tick()?;
            if !self.heap.environment(env).bindings.contains_key(&name) {
                self.reserve_binding(&name)?;
                self.heap.environment_mut(env).bindings.insert(
                    name,
                    Binding {
                        value: Value::Undefined,
                        mutable,
                        initialized: false,
                        lexical,
                        strict_immutable: true,
                    },
                );
            }
        }
        Ok(())
    }
    fn pattern_reference(
        &mut self,
        pattern: &PatternCode,
        env: EnvId,
        mode: PatternMode,
    ) -> Result<Option<Reference>, JsError> {
        if !matches!(mode, PatternMode::Assignment) {
            return Ok(None);
        }
        match pattern {
            PatternCode::Name(name) => self.resolve(env, name).map(Some),
            PatternCode::Target(object, key) => {
                let object = self.expression_value(object, env)?;
                let key = self.expression_value(key, env)?;
                Ok(Some(Reference::Property(object, key)))
            }
            PatternCode::PrivateTarget(object, name) => {
                let object = self.expression_value(object, env)?;
                self.private_reference(env, object, name).map(Some)
            }
            PatternCode::SuperTarget(key) => {
                let receiver = self.get_this(env)?;
                let key = self.expression_value(key, env)?;
                self.super_reference(env, receiver, key).map(Some)
            }
            PatternCode::Default { target, .. } => self.pattern_reference(target, env, mode),
            _ => Ok(None),
        }
    }
    pub(crate) fn bind_pattern(
        &mut self,
        pattern: &PatternCode,
        value: Value,
        env: EnvId,
        mode: PatternMode,
        strict: bool,
        reference: Option<Reference>,
    ) -> Result<(), JsError> {
        if self.pattern_depth >= 16 {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Destructuring depth budget exhausted",
            ));
        }
        self.pattern_depth += 1;
        let result = self.bind_pattern_inner(pattern, value, env, mode, strict, reference);
        self.pattern_depth -= 1;
        result
    }
    fn bind_pattern_inner(
        &mut self,
        pattern: &PatternCode,
        mut value: Value,
        env: EnvId,
        mode: PatternMode,
        strict: bool,
        reference: Option<Reference>,
    ) -> Result<(), JsError> {
        self.tick()?;
        match pattern {
            PatternCode::Default {
                target,
                value: default,
            } => {
                let reference = match reference {
                    Some(r) => Some(r),
                    None => self.pattern_reference(target, env, mode)?,
                };
                if value.is_undefined() {
                    value = self.expression_value(default, env)?;
                }
                self.bind_pattern(target, value, env, mode, strict, reference)
            }
            PatternCode::Name(name) => match mode {
                PatternMode::Assignment => {
                    let reference = match reference {
                        Some(r) => r,
                        None => self.resolve(env, name)?,
                    };
                    self.write_reference(reference, value, strict)
                }
                PatternMode::Binding(kind) => self.initialize(env, kind, name, value, true),
                PatternMode::Parameter => {
                    let value = self.account_value(value)?;
                    let binding = self
                        .heap
                        .environment_mut(env)
                        .bindings
                        .get_mut(name)
                        .ok_or_else(|| {
                            JsError::new(ErrorKind::InternalError, "Missing parameter binding")
                        })?;
                    binding.value = value;
                    binding.initialized = true;
                    Ok(())
                }
            },
            PatternCode::Target(..)
            | PatternCode::PrivateTarget(..)
            | PatternCode::SuperTarget(_) => {
                let reference = match reference {
                    Some(r) => r,
                    None => self.pattern_reference(pattern, env, mode)?.unwrap(),
                };
                self.write_reference(reference, value, strict)
            }
            PatternCode::Array(items, rest) => {
                let mut iterator = self.get_iterator(value)?;
                let completion = (|| {
                    for item in items {
                        if let Some(item) = item {
                            let reference = self.pattern_reference(item, env, mode)?;
                            let value = self.iterator_step(&mut iterator)?.unwrap_or_default();
                            self.bind_pattern(item, value, env, mode, strict, reference)?;
                        } else {
                            self.iterator_skip(&mut iterator)?;
                        }
                    }
                    if let Some(rest) = rest {
                        let reference = self.pattern_reference(rest, env, mode)?;
                        let mut values = Vec::new();
                        while let Some(value) = self.iterator_step(&mut iterator)? {
                            if values.len() >= self.limits.max_array_length as usize {
                                return Err(JsError::new(
                                    ErrorKind::ExecutionLimit,
                                    "Destructuring rest length budget exhausted",
                                ));
                            }
                            self.reserve_allocation(std::mem::size_of::<Value>() * 2)?;
                            values.push(value);
                        }
                        let array = self.try_new_array(&values)?;
                        self.bind_pattern(rest, array, env, mode, strict, reference)?;
                    }
                    Ok(Value::Undefined)
                })();
                self.iterator_close(&mut iterator, completion).map(|_| ())
            }
            PatternCode::Object(properties, rest) => {
                if value.is_nullish() {
                    return Err(JsError::type_error("Cannot destructure null or undefined"));
                }
                let mut excluded = Vec::new();
                for (key, pattern) in properties {
                    let key = self.expression_value(key, env)?;
                    let key = self.to_property_key(&key)?;
                    let reference = self.pattern_reference(pattern, env, mode)?;
                    let property = self.get_property_key(value.clone(), key.clone())?;
                    self.bind_pattern(pattern, property, env, mode, strict, reference)?;
                    self.reserve_allocation(std::mem::size_of::<PropertyKey>() * 2)?;
                    excluded.push(key);
                }
                if let Some(rest) = rest {
                    let reference = self.pattern_reference(rest, env, mode)?;
                    self.ensure_allocation(256)?;
                    let object = self.new_object();
                    self.copy_data_properties(object.clone(), value, &excluded)?;
                    self.bind_pattern(rest, object, env, mode, strict, reference)?;
                }
                Ok(())
            }
        }
    }
}
