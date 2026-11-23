//! Class construction, element initialization, and receiver-aware super references.
use crate::{
    ErrorKind, JsError, JsString, ObjectId, PropertyKey, Value, Vm,
    bytecode::{Chunk, ClassCode, ClassElementCode, ClassKeyCode, Code},
    heap::{
        Binding, Cell, EnvId, EnvKind, Environment, Object, ObjectKind, Property, PropertyKind,
        TraceEdges,
    },
    parser::{EvalContext, MethodKind},
    patterns::Invocation,
    private::{PrivateElement, PrivateId},
    vm::{Completion, Reference},
};
use std::{collections::BTreeMap, rc::Rc};
pub(crate) enum ElementName {
    Public(PropertyKey),
    Private(PrivateId, JsString),
}
impl ElementName {
    fn function_name(&self) -> PropertyKey {
        match self {
            Self::Public(key) => key.clone(),
            Self::Private(_, name) => PropertyKey::String(name.clone()),
        }
    }
}
pub(crate) struct FieldData {
    pub key: ElementName,
    pub value: Option<Rc<Chunk>>,
    pub environment: EnvId,
    pub home: ObjectId,
    pub infer_name: bool,
}
enum StaticElement {
    Field(FieldData),
    Block(Rc<Chunk>),
}
pub(crate) struct ClassData {
    pub derived: bool,
    pub default: bool,
    pub fields: Vec<FieldData>,
    private_methods: BTreeMap<PrivateId, PrivateElement>,
}
impl ClassData {
    const BASE_BYTES: usize = 128;

    pub fn trace(&self, pending: &mut TraceEdges) {
        for f in &self.fields {
            pending.push(f.environment.0);
            pending.push(f.home);
            if let ElementName::Public(PropertyKey::Symbol(symbol)) = &f.key {
                pending.symbol(symbol);
            }
        }
        for element in self.private_methods.values() {
            element.visit(|value| pending.value(value));
        }
    }

    pub fn extra_bytes(&self) -> usize {
        Self::BASE_BYTES
            + self.fields.len() * 256
            + self.private_methods.len() * PrivateElement::ESTIMATED_BYTES
    }
}
impl Vm {
    pub(crate) fn get_new_target(&self, mut env: EnvId) -> Value {
        loop {
            let e = self.heap.environment(env);
            if let Some(v) = &e.new_target {
                return v.clone();
            }
            if let Some(p) = e.parent {
                env = p;
            } else {
                return Value::Undefined;
            }
        }
    }
    fn this_environment(&self, mut env: EnvId) -> Result<EnvId, JsError> {
        loop {
            let e = self.heap.environment(env);
            if e.this.is_some() {
                return Ok(env);
            }
            env = e
                .parent
                .ok_or_else(|| JsError::new(ErrorKind::ReferenceError, "No this environment"))?;
        }
    }
    pub(crate) fn eval_context(&mut self, env: EnvId) -> Result<EvalContext, JsError> {
        let private_names = self.private_eval_names(env)?;
        let env = self.heap.environment(self.this_environment(env)?);
        let derived = env
            .function
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|id| self.heap.object(id).ok())
            .and_then(|o| o.class_data.as_ref())
            .is_some_and(|class| class.derived);
        Ok(EvalContext {
            super_property: env.home_object.is_some(),
            super_call: derived,
            new_target: env.new_target.is_some(),
            class_initializer: env.class_initializer,
            private_names,
        })
    }
    pub(crate) fn super_reference(
        &self,
        env: EnvId,
        receiver: Value,
        key: Value,
    ) -> Result<Reference, JsError> {
        let target = self.this_environment(env)?;
        let home = self
            .heap
            .environment(target)
            .home_object
            .ok_or_else(|| JsError::new(ErrorKind::ReferenceError, "No super home object"))?;
        let base = self
            .heap
            .object(home)?
            .prototype
            .map(Value::Object)
            .unwrap_or(Value::Null);
        Ok(Reference::Super(base, receiver, key))
    }
    pub(crate) fn get_from(
        &mut self,
        base: Value,
        key: &PropertyKey,
        receiver: Value,
    ) -> Result<Value, JsError> {
        let mut current = Some(
            base.as_object()
                .ok_or_else(|| JsError::type_error("Super property base is null"))?,
        );
        for _ in 0..1024 {
            self.tick()?;
            let Some(id) = current else {
                return Ok(Value::Undefined);
            };
            if let PropertyKey::String(name) = key
                && matches!(self.heap.object(id)?.kind, ObjectKind::TypedArray(_))
                && let Some(index) = crate::binary::numeric_index(name)
            {
                return self.typed_index_get(id, index);
            }
            let descriptor = self.own_descriptor_key(id, key)?;
            current = self.heap.object(id)?.prototype;
            if let Some(p) = descriptor {
                return match p.kind {
                    PropertyKind::Data { value, .. } => Ok(value),
                    PropertyKind::Accessor { get: Some(get), .. } => {
                        self.invoke(get, receiver, &[])
                    }
                    _ => Ok(Value::Undefined),
                };
            }
        }
        Err(JsError::new(
            ErrorKind::RangeError,
            "Prototype chain limit exceeded",
        ))
    }
    pub(crate) fn set_from(
        &mut self,
        base: Value,
        key: &PropertyKey,
        value: Value,
        receiver: Value,
    ) -> Result<bool, JsError> {
        let mut current = Some(
            base.as_object()
                .ok_or_else(|| JsError::type_error("Super property base is null"))?,
        );
        for _ in 0..1024 {
            self.tick()?;
            let Some(id) = current else { break };
            if let PropertyKey::String(name) = key
                && receiver.as_object() == Some(id)
                && let Some(provider) = self.host_named_setter(id)?
            {
                provider.set(self, name.clone(), value)?;
                return Ok(true);
            }
            let object = self.heap.object(id)?;
            if matches!(object.kind, ObjectKind::ModuleNamespace(_)) {
                return Ok(false);
            }
            current = object.prototype;
            if let PropertyKey::String(name) = key
                && matches!(object.kind, ObjectKind::TypedArray(_))
                && let Some(index) = crate::binary::numeric_index(name)
            {
                if receiver.as_object() == Some(id) {
                    self.typed_index_set(id, index, &value)?;
                    return Ok(true);
                }
                if !self.typed_element_exists(id, index)? {
                    return Ok(true);
                }
            }
            if let PropertyKey::String(name) = key
                && self.host_indexed_has(id, name)?
            {
                return Ok(false);
            }
            if let Some(property) = self.own_descriptor_without_host(id, key)? {
                match property.kind {
                    PropertyKind::Accessor {
                        set: Some(setter), ..
                    } => {
                        self.invoke(setter, receiver, &[value])?;
                        return Ok(true);
                    }
                    PropertyKind::Accessor { set: None, .. }
                    | PropertyKind::Data {
                        writable: false, ..
                    } => return Ok(false),
                    PropertyKind::Data { .. } => {
                        current = None;
                        break;
                    }
                }
            }
        }
        if current.is_some() {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "Prototype chain limit exceeded",
            ));
        }
        let Some(id) = receiver.as_object() else {
            return Ok(false);
        };
        if let PropertyKey::String(name) = key
            && self.host_indexed_key(id, name)?.is_some()
        {
            return Ok(false);
        }
        if let PropertyKey::String(name) = key
            && matches!(self.heap.object(id)?.kind, ObjectKind::TypedArray(_))
            && let Some(index) = crate::binary::numeric_index(name)
        {
            if !self.typed_element_exists(id, index)? {
                return Ok(false);
            }
            self.typed_index_set(id, index, &value)?;
            return Ok(true);
        }
        let namespace_receiver =
            matches!(self.heap.object(id)?.kind, ObjectKind::ModuleNamespace(_));
        if let Some(mut property) = self.own_descriptor_key(id, key)? {
            if !matches!(property.kind, PropertyKind::Data { writable: true, .. }) {
                return Ok(false);
            }
            property.kind = PropertyKind::Data {
                value,
                writable: true,
            };
            match self.define_key(receiver, key.clone(), property) {
                Err(error) if namespace_receiver && error.kind == ErrorKind::TypeError => {
                    return Ok(false);
                }
                result => result?,
            }
        } else if self.heap.object(id)?.extensible {
            self.define_key(receiver, key.clone(), Property::data(value))?;
        } else {
            return Ok(false);
        }
        Ok(true)
    }
    pub(crate) fn make_class(
        &mut self,
        code: Rc<ClassCode>,
        parent: EnvId,
        inferred_name: Option<&PropertyKey>,
    ) -> Result<Value, JsError> {
        self.tick()?;
        self.ensure_allocation(2048 + code.elements.len() * 256)?;
        let class_env =
            self.heap
                .allocate_env(Environment::new(Some(parent), EnvKind::Block, None));
        if let Some(name) = &code.name {
            self.reserve_binding(name)?;
            self.heap.environment_mut(class_env).bindings.insert(
                name.clone(),
                Binding {
                    value: Value::Undefined,
                    mutable: false,
                    initialized: false,
                    lexical: true,
                    strict_immutable: true,
                },
            );
        }
        let mut constructor_parent = self.function_prototype;
        let prototype_parent = if let Some(heritage) = &code.heritage {
            let superclass = self.expression_value(heritage, class_env)?;
            if superclass == Value::Null {
                None
            } else {
                if !self.is_constructor(&superclass) {
                    return Err(JsError::type_error(
                        "Class extends value is not a constructor or null",
                    ));
                }
                constructor_parent = superclass.as_object().unwrap();
                let prototype = self.get_property(superclass, "prototype")?;
                if !prototype.is_nullish() && prototype.as_object().is_none() {
                    return Err(JsError::type_error(
                        "Superclass prototype is not an object or null",
                    ));
                }
                if prototype == Value::Undefined {
                    return Err(JsError::type_error("Superclass prototype is undefined"));
                }
                prototype.as_object()
            }
        } else {
            Some(self.object_prototype)
        };
        // Heritage expressions can create closures. Those closures retain the
        // outer private names, so the class body needs a distinct environment.
        let env = self.private_environment(class_env, &code.private_names)?;
        let prototype = Value::Object(self.heap.allocate(Cell::Object(Object::new(
            ObjectKind::Ordinary,
            prototype_parent,
        ))));
        let explicit = code.elements.iter().find_map(|e| {
            if let ClassElementCode::Method {
                constructor: true,
                code,
                ..
            } = e
            {
                Some(code.clone())
            } else {
                None
            }
        });
        let constructor_code = explicit.clone().unwrap_or_else(|| {
            Rc::new(Code {
                name: None,
                params: Vec::new(),
                param_names: Vec::new(),
                parameter_expressions: false,
                mapped_arguments: false,
                length: 0,
                body: Rc::new(Chunk {
                    strict: true,
                    ..Chunk::default()
                }),
                strict: true,
                arrow: false,
                kind: crate::parser::FunctionKind::Ordinary,
                self_binding: false,
                method: true,
            })
        });
        let function = self.make_function(constructor_code, env)?;
        let id = function.as_object().unwrap();
        self.heap.object_mut(id)?.prototype = Some(constructor_parent);
        self.heap.object_mut(id)?.home_object = prototype.as_object();
        let mut class_data = ClassData {
            derived: code.heritage.is_some(),
            default: explicit.is_none(),
            fields: Vec::new(),
            private_methods: BTreeMap::new(),
        };
        if let Some(name) = &code.name {
            self.set_literal_function_name(function.clone(), &PropertyKey::String(name.clone()))?;
        } else if let Some(name) = inferred_name {
            self.set_literal_function_name(function.clone(), name)?;
        }
        self.define_data_property(
            function.clone(),
            "prototype",
            prototype.clone(),
            false,
            false,
            false,
        )?;
        self.define_data_property(
            prototype.clone(),
            "constructor",
            function.clone(),
            true,
            false,
            true,
        )?;
        let mut statics = Vec::new();
        let mut static_methods = BTreeMap::new();
        for element in &code.elements {
            self.tick()?;
            match element {
                ClassElementCode::Method {
                    constructor: true, ..
                } => {}
                ClassElementCode::Method {
                    key,
                    code,
                    kind,
                    is_static,
                    ..
                } => {
                    let key = self.class_element_name(key, env)?;
                    let target = if *is_static {
                        function.clone()
                    } else {
                        prototype.clone()
                    };
                    let method = self.make_function(code.clone(), env)?;
                    self.heap
                        .object_mut(method.as_object().unwrap())?
                        .home_object = target.as_object();
                    self.name_property_method(method.clone(), &key.function_name(), *kind)?;
                    match key {
                        ElementName::Public(key) => {
                            self.define_method_property(target, key, method, *kind, false)?;
                        }
                        ElementName::Private(id, _) => {
                            let methods = if *is_static {
                                &mut static_methods
                            } else {
                                &mut class_data.private_methods
                            };
                            self.collect_private_method(methods, id, method, *kind)?;
                        }
                    }
                }
                ClassElementCode::Field {
                    key,
                    value,
                    is_static,
                    infer_name,
                } => {
                    let key = self.class_element_name(key, env)?;
                    self.reserve_allocation(256)?;
                    let field = FieldData {
                        key,
                        value: value.clone(),
                        environment: env,
                        home: if *is_static {
                            id
                        } else {
                            prototype.as_object().unwrap()
                        },
                        infer_name: *infer_name,
                    };
                    if *is_static {
                        statics.push(StaticElement::Field(field));
                    } else {
                        class_data.fields.push(field);
                    }
                }
                ClassElementCode::StaticBlock(body) => {
                    self.reserve_allocation(128)?;
                    statics.push(StaticElement::Block(body.clone()));
                }
            }
        }
        if let Some(name) = &code.name {
            let binding = self
                .heap
                .environment_mut(class_env)
                .bindings
                .get_mut(name)
                .unwrap();
            binding.value = function.clone();
            binding.initialized = true;
        }
        self.reserve_allocation(ClassData::BASE_BYTES)?;
        self.heap.object_mut(id)?.class_data = Some(Rc::new(class_data));
        self.heap.object_mut(id)?.private_elements = static_methods;
        for element in statics {
            match element {
                StaticElement::Field(field) => self.initialize_field(&field, function.clone())?,
                StaticElement::Block(body) => {
                    let scope = self.initializer_environment(env, id, function.clone());
                    self.run_chunk(&body, scope)?;
                }
            }
        }
        Ok(function)
    }

    fn class_element_name(
        &mut self,
        key: &ClassKeyCode,
        env: EnvId,
    ) -> Result<ElementName, JsError> {
        match key {
            ClassKeyCode::Public(code) => {
                let value = self.expression_value(code, env)?;
                Ok(ElementName::Public(self.to_property_key(&value)?))
            }
            ClassKeyCode::Private(name) => Ok(ElementName::Private(
                self.resolve_private(env, name)?,
                name.clone(),
            )),
        }
    }

    pub(crate) fn name_property_method(
        &mut self,
        method: Value,
        key: &PropertyKey,
        kind: MethodKind,
    ) -> Result<(), JsError> {
        self.set_literal_function_name(method.clone(), key)?;
        let prefix = match kind {
            MethodKind::Method => return Ok(()),
            MethodKind::Getter => "get ",
            MethodKind::Setter => "set ",
        };
        let name = self.get_property(method.clone(), "name")?;
        let name = self.to_string(&name)?;
        let units = name.len().saturating_add(prefix.len());
        self.ensure_string_allocation(units)?;
        self.consume_work(units.saturating_mul(2))?;
        let name = JsString::from(prefix).concat(&name);
        self.define_data_property(method, "name", Value::String(name), false, false, true)
    }

    pub(crate) fn define_method_property(
        &mut self,
        target: Value,
        key: PropertyKey,
        method: Value,
        kind: MethodKind,
        enumerable: bool,
    ) -> Result<(), JsError> {
        let kind = if kind == MethodKind::Method {
            PropertyKind::Data {
                value: method,
                writable: true,
            }
        } else {
            let old = self.own_descriptor_key(target.as_object().unwrap(), &key)?;
            let (mut get, mut set) = if let Some(Property {
                kind: PropertyKind::Accessor { get, set },
                ..
            }) = old
            {
                (get, set)
            } else {
                (None, None)
            };
            if kind == MethodKind::Getter {
                get = Some(method);
            } else {
                set = Some(method);
            }
            PropertyKind::Accessor { get, set }
        };
        self.define_key(
            target,
            key,
            Property {
                kind,
                enumerable,
                configurable: true,
            },
        )
    }

    fn collect_private_method(
        &mut self,
        methods: &mut BTreeMap<PrivateId, PrivateElement>,
        id: PrivateId,
        method: Value,
        kind: MethodKind,
    ) -> Result<(), JsError> {
        if !methods.contains_key(&id) {
            self.reserve_allocation(PrivateElement::ESTIMATED_BYTES)?;
        }
        if kind == MethodKind::Method {
            methods.insert(id, PrivateElement::Method(method));
        } else {
            let slot = methods.entry(id).or_insert(PrivateElement::Accessor {
                get: None,
                set: None,
            });
            let PrivateElement::Accessor { get, set } = slot else {
                unreachable!("private name declarations were validated during parsing")
            };
            if kind == MethodKind::Getter {
                *get = Some(method);
            } else {
                *set = Some(method);
            }
        }
        Ok(())
    }

    fn initializer_environment(&mut self, parent: EnvId, home: ObjectId, this: Value) -> EnvId {
        let mut env = Environment::new(Some(parent), EnvKind::Function, Some(this));
        env.home_object = Some(home);
        env.new_target = Some(Value::Undefined);
        env.class_initializer = true;
        self.heap.allocate_env(env)
    }
    fn initialize_field(&mut self, field: &FieldData, this: Value) -> Result<(), JsError> {
        self.tick()?;
        self.ensure_allocation(256)?;
        let env = self.initializer_environment(field.environment, field.home, this.clone());
        let value = if let Some(code) = &field.value {
            if field.infer_name
                && let [crate::bytecode::Op::Class(class, _)] = code.ops.as_slice()
            {
                self.make_class(class.clone(), env, Some(&field.key.function_name()))?
            } else {
                let value = self.expression_value(code, env)?;
                if field.infer_name {
                    self.set_literal_function_name(value.clone(), &field.key.function_name())?;
                }
                value
            }
        } else {
            Value::Undefined
        };
        match &field.key {
            ElementName::Public(key) => {
                self.define_data_property_key(this, key.clone(), value, true, true, true)
            }
            ElementName::Private(id, _) => {
                self.private_add(&this, *id, PrivateElement::Field(value))
            }
        }
    }
    fn initialize_instance_elements(
        &mut self,
        class: ObjectId,
        this: Value,
    ) -> Result<(), JsError> {
        let Some(class) = self.heap.object(class)?.class_data.clone() else {
            return Ok(());
        };
        for (id, element) in &class.private_methods {
            self.tick()?;
            self.private_add(&this, *id, element.clone())?;
        }
        for field in &class.fields {
            self.initialize_field(field, this.clone())?;
        }
        Ok(())
    }
    pub(crate) fn construct_class(
        &mut self,
        id: ObjectId,
        args: &[Value],
        new_target: Value,
    ) -> Result<Value, JsError> {
        self.tick()?;
        if self.call_depth >= self.limits.max_call_depth {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Constructor depth budget exhausted",
            ));
        }
        self.call_depth += 1;
        let result = self.construct_class_inner(id, args, new_target);
        self.call_depth -= 1;
        result
    }
    fn construct_class_inner(
        &mut self,
        id: ObjectId,
        args: &[Value],
        new_target: Value,
    ) -> Result<Value, JsError> {
        let data = self.heap.object(id)?.class_data.as_ref().unwrap();
        let (is_default, derived) = (data.default, data.derived);
        if is_default && derived {
            let parent = self
                .heap
                .object(id)?
                .prototype
                .map(Value::Object)
                .unwrap_or_default();
            let object = self.construct_target(parent, args, new_target)?;
            self.initialize_instance_elements(id, object.clone())?;
            return Ok(object);
        }
        let object = if derived {
            Value::Undefined
        } else {
            let prototype = self
                .get_property(new_target.clone(), "prototype")?
                .as_object()
                .unwrap_or(self.object_prototype);
            let object = Value::Object(self.heap.allocate(Cell::Object(Object::new(
                ObjectKind::Ordinary,
                Some(prototype),
            ))));
            self.initialize_instance_elements(id, object.clone())?;
            object
        };
        if is_default {
            return Ok(object);
        }
        let (code, parent) = if let ObjectKind::Function {
            code, environment, ..
        } = &self.heap.object(id)?.kind
        {
            (code.clone(), *environment)
        } else {
            unreachable!()
        };
        let body = self.function_environment(
            &code,
            parent,
            Invocation {
                function: Value::Object(id),
                this: object.clone(),
                args,
                new_target,
                this_initialized: !derived,
            },
        )?;
        match self.run_chunk(&code.body, body)? {
            Completion::Return(value) if value.as_object().is_some() => Ok(value),
            Completion::Return(value) if derived && !value.is_undefined() => Err(
                JsError::type_error("Derived constructor returned a primitive"),
            ),
            Completion::Return(_) | Completion::Normal(_) => {
                if derived {
                    self.get_this(body)
                } else {
                    Ok(object)
                }
            }
            _ => Err(JsError::new(
                ErrorKind::InternalError,
                "Invalid class constructor completion",
            )),
        }
    }
    pub(crate) fn super_constructor(&self, env: EnvId) -> Result<Value, JsError> {
        let env = self.this_environment(env)?;
        let function = self
            .heap
            .environment(env)
            .function
            .as_ref()
            .and_then(Value::as_object)
            .ok_or_else(|| JsError::new(ErrorKind::ReferenceError, "No super constructor"))?;
        let object = self.heap.object(function)?;
        if !object
            .class_data
            .as_ref()
            .is_some_and(|class| class.derived)
        {
            return Err(JsError::new(
                ErrorKind::ReferenceError,
                "super call outside a derived constructor",
            ));
        }
        Ok(object.prototype.map(Value::Object).unwrap_or(Value::Null))
    }
    pub(crate) fn super_construct(
        &mut self,
        env: EnvId,
        parent: Value,
        args: &[Value],
    ) -> Result<Value, JsError> {
        let env = self.this_environment(env)?;
        let id = self
            .heap
            .environment(env)
            .function
            .as_ref()
            .and_then(Value::as_object)
            .expect("derived constructor environment");
        let target = self.get_new_target(env);
        let object = self.construct_target(parent, args, target)?;
        if self.heap.environment(env).this_initialized {
            return Err(JsError::new(
                ErrorKind::ReferenceError,
                "super() has already initialized this",
            ));
        }
        {
            let record = self.heap.environment_mut(env);
            record.this = Some(object.clone());
            record.this_initialized = true;
        }
        self.initialize_instance_elements(id, object.clone())?;
        Ok(object)
    }
}
