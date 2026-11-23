//! Primitive identities, string/symbol property keys and well-known protocols.
use crate::{
    ErrorKind, JsError, JsString, ObjectId, PropertyKey, SymbolId, Value, Vm,
    builtins::method,
    heap::{ObjectKind, Property, PropertyKind},
    value::SymbolData,
    vm::same_value,
};
use std::rc::Rc;

impl Vm {
    pub fn new_symbol(&mut self, description: Option<JsString>) -> Result<Value, JsError> {
        self.ensure_allocation(128)?;
        if let Some(text) = &description {
            self.account_value(Value::String(text.clone()))?;
        }
        let symbol = SymbolId(Rc::new(SymbolData {
            description,
            registry_key: None,
        }));
        self.account_symbol(&symbol)?;
        Ok(Value::Symbol(symbol))
    }
    pub(crate) fn account_symbol(&mut self, symbol: &SymbolId) -> Result<(), JsError> {
        if !self.symbol_storage.contains_key(&symbol.id()) {
            self.reserve_allocation(128)?;
            if let Some(description) = symbol.description() {
                self.account_value(Value::String(description.clone()))?;
            }
            self.symbol_storage
                .insert(symbol.id(), Rc::downgrade(&symbol.0));
        }
        Ok(())
    }
    pub fn well_known_symbol(&self, name: &str) -> Option<Value> {
        self.well_known_symbols
            .get(name)
            .cloned()
            .map(Value::Symbol)
    }
    pub(crate) fn well_known_key(&self, name: &str) -> PropertyKey {
        PropertyKey::Symbol(self.well_known_symbols[name].clone())
    }
    pub fn to_property_key(&mut self, value: &Value) -> Result<PropertyKey, JsError> {
        let primitive = self.coerce_primitive(value.clone(), true)?;
        if let Value::Symbol(symbol) = primitive {
            Ok(PropertyKey::Symbol(symbol))
        } else {
            Ok(PropertyKey::String(self.to_string(&primitive)?))
        }
    }
    pub fn get_property_key(
        &mut self,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Value, JsError> {
        match key {
            PropertyKey::String(key) => self.get(receiver, &key),
            PropertyKey::Symbol(symbol) => {
                if receiver.is_nullish() {
                    return Err(JsError::type_error(
                        "Cannot read a property of null or undefined",
                    ));
                }
                let mut current = self.property_start(&receiver);
                for _ in 0..1024 {
                    let Some(id) = current else {
                        return Ok(Value::Undefined);
                    };
                    self.consume_work(1)?;
                    let object = self.heap.object(id)?;
                    let property = object.symbols.get(&symbol).cloned();
                    current = object.prototype;
                    if let Some(property) = property {
                        return match property.kind {
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
        }
    }
    fn property_start(&self, value: &Value) -> Option<ObjectId> {
        match value {
            Value::Object(id) => Some(*id),
            Value::String(_) => Some(self.string_prototype),
            Value::Number(_) => Some(self.number_prototype),
            Value::BigInt(_) => self.bigint_prototype,
            Value::Bool(_) => Some(self.boolean_prototype),
            Value::Symbol(_) => self.symbol_prototype,
            _ => None,
        }
    }
    pub fn set_property_key(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), JsError> {
        self.set_key(receiver, &key, value, true)
    }
    pub(crate) fn set_key(
        &mut self,
        receiver: Value,
        key: &PropertyKey,
        value: Value,
        strict: bool,
    ) -> Result<(), JsError> {
        let PropertyKey::Symbol(symbol) = key else {
            if let PropertyKey::String(key) = key {
                return self.set(receiver, key, value, strict);
            }
            unreachable!()
        };
        if receiver.is_nullish() {
            return Err(JsError::type_error(
                "Cannot assign a property of null or undefined",
            ));
        }
        let mut current = self.property_start(&receiver);
        for _ in 0..1024 {
            let Some(id) = current else {
                break;
            };
            self.consume_work(1)?;
            let object = self.heap.object(id)?;
            if matches!(object.kind, ObjectKind::ModuleNamespace(_)) {
                return if strict {
                    Err(JsError::type_error("Module namespace is read-only"))
                } else {
                    Ok(())
                };
            }
            let property = object.symbols.get(symbol).cloned();
            current = object.prototype;
            if let Some(mut property) = property {
                match property.kind {
                    PropertyKind::Accessor { set: Some(set), .. } => {
                        self.invoke(set, receiver, &[value])?;
                        return Ok(());
                    }
                    PropertyKind::Accessor { set: None, .. }
                    | PropertyKind::Data {
                        writable: false, ..
                    } => {
                        return if strict {
                            Err(JsError::type_error("Property is not writable"))
                        } else {
                            Ok(())
                        };
                    }
                    PropertyKind::Data { .. } => {
                        if receiver.as_object() == Some(id) {
                            property.kind = PropertyKind::Data {
                                value,
                                writable: true,
                            };
                            return self.define_key(receiver, key.clone(), property);
                        }
                        break;
                    }
                }
            }
        }
        if receiver.as_object().is_none() {
            return if strict {
                Err(JsError::type_error(
                    "Cannot create a property on a primitive",
                ))
            } else {
                Ok(())
            };
        }
        if !self.heap.object(receiver.as_object().unwrap())?.extensible {
            return if strict {
                Err(JsError::type_error("Object is not extensible"))
            } else {
                Ok(())
            };
        }
        self.define_key(receiver, key.clone(), Property::data(value))
    }
    pub(crate) fn own_descriptor_key(
        &mut self,
        id: ObjectId,
        key: &PropertyKey,
    ) -> Result<Option<Property>, JsError> {
        if let PropertyKey::String(name) = key
            && let Some(value) = self.host_indexed_get(id, name)?
        {
            return Ok(Some(Property {
                kind: PropertyKind::Data {
                    value,
                    writable: false,
                },
                enumerable: true,
                configurable: true,
            }));
        }
        if let PropertyKey::String(name) = key
            && let Some(value) = self.host_named_value(id, name)?
        {
            return Ok(Some(Property::data(self.account_value(value)?)));
        }
        self.own_descriptor_without_host(id, key)
    }
    pub(crate) fn own_enumerability(
        &self,
        id: ObjectId,
        key: &PropertyKey,
    ) -> Result<Option<bool>, JsError> {
        if let PropertyKey::String(name) = key
            && (self.host_indexed_has(id, name)? || self.host_named_value(id, name)?.is_some())
        {
            return Ok(Some(true));
        }
        Ok(self
            .own_descriptor_without_host(id, key)?
            .map(|property| property.enumerable))
    }
    pub(crate) fn own_descriptor_without_host(
        &self,
        id: ObjectId,
        key: &PropertyKey,
    ) -> Result<Option<Property>, JsError> {
        match key {
            PropertyKey::String(key) => self.own_descriptor(id, key),
            PropertyKey::Symbol(symbol) => Ok(self.heap.object(id)?.symbols.get(symbol).cloned()),
        }
    }
    pub(crate) fn has_key(&mut self, receiver: &Value, key: &PropertyKey) -> Result<bool, JsError> {
        if receiver.as_object().is_none() {
            return Err(JsError::type_error(
                "Right-hand side of in is not an object",
            ));
        }
        if let PropertyKey::String(key) = key {
            return self.has(receiver, key);
        }
        let mut current = receiver.as_object();
        for _ in 0..1024 {
            let Some(id) = current else {
                return Ok(false);
            };
            self.consume_work(1)?;
            if self.own_descriptor_key(id, key)?.is_some() {
                return Ok(true);
            }
            current = self.heap.object(id)?.prototype;
        }
        Err(JsError::new(
            ErrorKind::RangeError,
            "Prototype chain limit exceeded",
        ))
    }
    pub(crate) fn define_key(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        property: Property,
    ) -> Result<(), JsError> {
        let PropertyKey::Symbol(symbol) = key else {
            if let PropertyKey::String(key) = key {
                return self.define_property(receiver, key, property);
            }
            unreachable!()
        };
        let id = receiver
            .as_object()
            .ok_or_else(|| JsError::type_error("Property target must be an object"))?;
        let previous = self.heap.object(id)?.symbols.get(&symbol).cloned();
        if let Some(old) = &previous {
            if !old.configurable
                && let (
                    PropertyKind::Data {
                        value: a,
                        writable: false,
                    },
                    PropertyKind::Data { value: b, .. },
                ) = (&old.kind, &property.kind)
            {
                self.charge_bigint_comparison(a, b)?;
            }
            validate_descriptor(old, &property)?;
        } else {
            if !self.heap.object(id)?.extensible {
                return Err(JsError::type_error("Object is not extensible"));
            }
            self.reserve_allocation(256)?;
        }
        self.account_symbol(&symbol)?;
        if let PropertyKind::Data { value, .. } = &property.kind {
            self.account_value(value.clone())?;
        }
        let object = self.heap.object_mut(id)?;
        if previous.is_none() {
            object.symbol_order.push(symbol.clone());
        }
        object.symbols.insert(symbol, property);
        Ok(())
    }
    pub fn define_data_property_key(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        value: Value,
        writable: bool,
        enumerable: bool,
        configurable: bool,
    ) -> Result<(), JsError> {
        self.define_key(
            receiver,
            key,
            Property {
                kind: PropertyKind::Data { value, writable },
                enumerable,
                configurable,
            },
        )
    }
    pub fn define_accessor_key(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        get: Option<Value>,
        set: Option<Value>,
    ) -> Result<(), JsError> {
        for function in get.iter().chain(set.iter()) {
            if !self.is_callable(function) {
                return Err(JsError::type_error("Accessor must be callable"));
            }
        }
        self.define_key(
            receiver,
            key,
            Property {
                kind: PropertyKind::Accessor { get, set },
                enumerable: true,
                configurable: true,
            },
        )
    }
    pub(crate) fn delete_key(
        &mut self,
        receiver: Value,
        key: &PropertyKey,
        strict: bool,
    ) -> Result<bool, JsError> {
        if let PropertyKey::String(key) = key {
            return self.delete(receiver, key, strict);
        }
        if receiver.is_nullish() {
            return Err(JsError::type_error(
                "Cannot delete a property of null or undefined",
            ));
        }
        let Some(id) = receiver.as_object() else {
            return Ok(true);
        };
        if self
            .own_descriptor_key(id, key)?
            .is_some_and(|p| !p.configurable)
        {
            return if strict {
                Err(JsError::type_error(
                    "Cannot delete non-configurable property",
                ))
            } else {
                Ok(false)
            };
        }
        if let PropertyKey::Symbol(symbol) = key {
            let count = self.heap.object(id)?.symbol_order.len();
            self.consume_work(count)?;
            let object = self.heap.object_mut(id)?;
            object.symbols.remove(symbol);
            object.symbol_order.retain(|old| old != symbol);
        }
        Ok(true)
    }
    pub fn own_property_keys(&mut self, value: Value) -> Result<Vec<PropertyKey>, JsError> {
        let value = self.coerce_object(value)?;
        let id = value.as_object().unwrap();
        let mut keys: Vec<_> = self
            .own_keys(value, false)?
            .into_iter()
            .map(PropertyKey::String)
            .collect();
        let count = self.heap.object(id)?.symbols.len();
        if keys.len().saturating_add(count) > self.limits.max_array_length as usize {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Property key count budget exceeded",
            ));
        }
        self.reserve_allocation(count.saturating_mul(96))?;
        let object = self.heap.object(id)?;
        keys.extend(
            object
                .symbol_order
                .iter()
                .filter(|key| object.symbols.contains_key(*key))
                .cloned()
                .map(PropertyKey::Symbol),
        );
        Ok(keys)
    }
    pub(crate) fn descriptive_symbol(&mut self, value: Value) -> Result<JsString, JsError> {
        let symbol = self.symbol_value(&value)?;
        let description = symbol.description().cloned().unwrap_or_default();
        let length = description.len().saturating_add(8);
        self.ensure_string_allocation(length)?;
        self.consume_work(length)?;
        let mut units = "Symbol(".encode_utf16().collect::<Vec<_>>();
        units.extend_from_slice(description.as_utf16());
        units.push(41);
        Ok(self.finish_string(units)?.as_string().unwrap().clone())
    }
    fn symbol_value(&self, value: &Value) -> Result<SymbolId, JsError> {
        match value {
            Value::Symbol(id) => Ok(id.clone()),
            Value::Object(id) => match &self.heap.object(*id)?.kind {
                ObjectKind::Boxed(Value::Symbol(symbol)) => Ok(symbol.clone()),
                _ => Err(JsError::type_error("Symbol receiver is not a symbol")),
            },
            _ => Err(JsError::type_error("Symbol receiver is not a symbol")),
        }
    }
    pub(crate) fn primitive_hint(&mut self, value: Value, hint: &str) -> Result<Value, JsError> {
        if value.as_object().is_none() {
            return Ok(value);
        }
        if self.well_known_symbols.contains_key("toPrimitive") {
            let method =
                self.get_property_key(value.clone(), self.well_known_key("toPrimitive"))?;
            if !method.is_nullish() {
                if !self.is_callable(&method) {
                    return Err(JsError::type_error(
                        "Symbol.toPrimitive method is not callable",
                    ));
                }
                let result = self.invoke(method, value.clone(), &[Value::from(hint)])?;
                if result.as_object().is_some() {
                    return Err(JsError::type_error(
                        "Symbol.toPrimitive must return a primitive",
                    ));
                }
                return Ok(result);
            }
        }
        self.ordinary_primitive(value, hint == "string")
    }
    pub(crate) fn instanceof_operator(
        &mut self,
        left: Value,
        right: Value,
    ) -> Result<Value, JsError> {
        if right.as_object().is_none() {
            return Err(JsError::type_error(
                "Right-hand side of instanceof is not an object",
            ));
        }
        if self.well_known_symbols.contains_key("hasInstance") {
            let method =
                self.get_property_key(right.clone(), self.well_known_key("hasInstance"))?;
            if !method.is_nullish() {
                if !self.is_callable(&method) {
                    return Err(JsError::type_error("Symbol.hasInstance is not callable"));
                }
                return Ok(Value::Bool(
                    self.invoke(method, right, &[left])?.is_truthy(),
                ));
            }
        }
        if !self.is_callable(&right) {
            return Err(JsError::type_error(
                "Right-hand side of instanceof is not callable",
            ));
        }
        self.ordinary_has_instance(left, right)
    }
    pub(crate) fn ordinary_has_instance(
        &mut self,
        left: Value,
        right: Value,
    ) -> Result<Value, JsError> {
        if !self.is_callable(&right) {
            return Ok(Value::Bool(false));
        }
        if let ObjectKind::Bound { target, .. } =
            &self.heap.object(right.as_object().unwrap())?.kind
        {
            return self.instanceof_operator(left, target.clone());
        }
        let Some(id) = left.as_object() else {
            return Ok(Value::Bool(false));
        };
        let prototype = self
            .get_property(right, "prototype")?
            .as_object()
            .ok_or_else(|| JsError::type_error("Constructor prototype is not an object"))?;
        let mut current = self.heap.object(id)?.prototype;
        for _ in 0..1024 {
            self.tick()?;
            let Some(id) = current else {
                return Ok(Value::Bool(false));
            };
            if id == prototype {
                return Ok(Value::Bool(true));
            }
            current = self.heap.object(id)?.prototype;
        }
        Err(JsError::new(
            ErrorKind::RangeError,
            "Prototype chain limit exceeded",
        ))
    }
    pub(crate) fn is_constructor(&self, value: &Value) -> bool {
        let mut value = value.clone();
        for _ in 0..1024 {
            let Some(id) = value.as_object() else {
                return false;
            };
            let Ok(object) = self.heap.object(id) else {
                return false;
            };
            match &object.kind {
                ObjectKind::Function { code, .. } => {
                    return self.heap.object(id).is_ok_and(|o| o.class_data.is_some())
                        || code.constructible();
                }
                ObjectKind::Native { construct, .. } => return construct.is_some(),
                ObjectKind::Bound { target, .. } => value = target.clone(),
                _ => return false,
            }
        }
        false
    }
    pub(crate) fn array_species_create(
        &mut self,
        original: Value,
        length: usize,
    ) -> Result<Value, JsError> {
        if length > self.limits.max_array_length as usize {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Array species length budget exhausted",
            ));
        }
        let mut constructor = Value::Undefined;
        if self.is_array(&original) {
            constructor = self.get_property(original, "constructor")?;
            if constructor.as_object().is_some() {
                constructor = self.get_property_key(constructor, self.well_known_key("species"))?;
                if constructor == Value::Null {
                    constructor = Value::Undefined;
                }
            }
        }
        if constructor.is_undefined() {
            let result = self.try_new_array(&[])?;
            self.set_property(result.clone(), "length", Value::from(length))?;
            Ok(result)
        } else {
            self.construct(constructor, &[Value::from(length)])
        }
    }
}
fn validate_descriptor(old: &Property, new: &Property) -> Result<(), JsError> {
    if old.configurable {
        return Ok(());
    }
    if new.configurable || old.enumerable != new.enumerable {
        return Err(JsError::type_error(
            "Cannot redefine non-configurable property",
        ));
    }
    match (&old.kind, &new.kind) {
        (
            PropertyKind::Data {
                value: a,
                writable: false,
            },
            PropertyKind::Data { value: b, writable },
        ) if *writable || !same_value(a, b) => {
            Err(JsError::type_error("Cannot redefine non-writable property"))
        }
        (PropertyKind::Data { .. }, PropertyKind::Accessor { .. })
        | (PropertyKind::Accessor { .. }, PropertyKind::Data { .. }) => Err(JsError::type_error(
            "Cannot change non-configurable property kind",
        )),
        (PropertyKind::Accessor { get: a, set: b }, PropertyKind::Accessor { get: c, set: d })
            if a != c || b != d =>
        {
            Err(JsError::type_error("Cannot redefine accessor"))
        }
        _ => Ok(()),
    }
}
pub(crate) fn install(vm: &mut Vm) {
    let prototype = vm.new_object();
    vm.symbol_prototype = prototype.as_object();
    let constructor = vm.native(
        "Symbol",
        0,
        false,
        Rc::new(|vm, _, args| {
            let description = if args.first().is_none_or(Value::is_undefined) {
                None
            } else {
                Some(vm.to_string(&args[0])?)
            };
            vm.new_symbol(description)
        }),
    );
    crate::builtins::replace_construct(vm, constructor.clone(), |_, _, _, _| {
        Err(JsError::type_error("Symbol cannot be called with new"))
    });
    vm.define_data_property(
        constructor.clone(),
        "prototype",
        prototype.clone(),
        false,
        false,
        false,
    )
    .unwrap();
    vm.define_data_property(
        prototype.clone(),
        "constructor",
        constructor.clone(),
        true,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property(
        vm.global(),
        "Symbol",
        constructor.clone(),
        true,
        false,
        true,
    )
    .unwrap();
    for name in [
        "iterator",
        "asyncIterator",
        "species",
        "toPrimitive",
        "toStringTag",
        "hasInstance",
        "isConcatSpreadable",
        "unscopables",
        "match",
        "matchAll",
        "replace",
        "search",
        "split",
        "dispose",
        "asyncDispose",
    ] {
        let Value::Symbol(symbol) = vm
            .new_symbol(Some(format!("Symbol.{name}").into()))
            .unwrap()
        else {
            unreachable!()
        };
        vm.well_known_symbols.insert(name, symbol.clone());
        vm.define_data_property(
            constructor.clone(),
            name,
            Value::Symbol(symbol),
            false,
            false,
            false,
        )
        .unwrap();
    }
    method(vm, constructor.clone(), "for", 1, |vm, _, args| {
        let key = vm.to_string(args.first().unwrap_or(&Value::Undefined))?;
        vm.consume_work(key.len())?;
        if let Some(symbol) = vm.symbol_registry.get(&key) {
            return Ok(Value::Symbol(symbol.clone()));
        }
        vm.reserve_allocation(128)?;
        let symbol = SymbolId(Rc::new(SymbolData {
            description: Some(key.clone()),
            registry_key: Some(key.clone()),
        }));
        vm.account_symbol(&symbol)?;
        vm.symbol_registry.insert(key, symbol.clone());
        Ok(Value::Symbol(symbol))
    });
    method(vm, constructor, "keyFor", 1, |_, _, args| {
        let Some(Value::Symbol(symbol)) = args.first() else {
            return Err(JsError::type_error("Symbol.keyFor requires a symbol"));
        };
        Ok(symbol
            .0
            .registry_key
            .clone()
            .map(Value::String)
            .unwrap_or_default())
    });
    method(vm, prototype.clone(), "toString", 0, |vm, this, _| {
        vm.descriptive_symbol(this).map(Value::String)
    });
    method(vm, prototype.clone(), "valueOf", 0, |vm, this, _| {
        vm.symbol_value(&this).map(Value::Symbol)
    });
    let get = vm.new_native_function("get description", |vm, this, _| {
        Ok(vm
            .symbol_value(&this)?
            .description()
            .cloned()
            .map(Value::String)
            .unwrap_or_default())
    });
    vm.define_property(
        prototype.clone(),
        "description".into(),
        Property {
            kind: PropertyKind::Accessor {
                get: Some(get),
                set: None,
            },
            enumerable: false,
            configurable: true,
        },
    )
    .unwrap();
    let primitive = vm.native(
        "[Symbol.toPrimitive]",
        1,
        false,
        Rc::new(|vm, this, _| vm.symbol_value(&this).map(Value::Symbol)),
    );
    vm.define_data_property_key(
        prototype.clone(),
        vm.well_known_key("toPrimitive"),
        primitive,
        false,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property_key(
        prototype,
        vm.well_known_key("toStringTag"),
        Value::from("Symbol"),
        false,
        false,
        true,
    )
    .unwrap();
    let has_instance = vm.native(
        "[Symbol.hasInstance]",
        1,
        false,
        Rc::new(|vm, this, args| {
            vm.ordinary_has_instance(args.first().cloned().unwrap_or_default(), this)
        }),
    );
    vm.define_data_property_key(
        Value::Object(vm.function_prototype),
        vm.well_known_key("hasInstance"),
        has_instance,
        false,
        false,
        false,
    )
    .unwrap();
    let object = vm.get_global("Object").unwrap();
    method(vm, object, "getOwnPropertySymbols", 1, |vm, _, args| {
        let keys = vm
            .own_property_keys(args.first().cloned().unwrap_or_default())?
            .into_iter()
            .filter_map(|key| {
                if let PropertyKey::Symbol(symbol) = key {
                    Some(Value::Symbol(symbol))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        vm.try_new_array(&keys)
    });
    let reflect = vm.new_object();
    method(vm, reflect.clone(), "ownKeys", 1, |vm, _, args| {
        let value = args.first().cloned().unwrap_or_default();
        if value.as_object().is_none() {
            return Err(JsError::type_error("Reflect.ownKeys requires an object"));
        }
        let keys = vm
            .own_property_keys(value)?
            .into_iter()
            .map(|key| match key {
                PropertyKey::String(s) => Value::String(s),
                PropertyKey::Symbol(s) => Value::Symbol(s),
            })
            .collect::<Vec<_>>();
        vm.try_new_array(&keys)
    });
    method(vm, reflect.clone(), "apply", 3, |vm, _, args| {
        let target = args.first().cloned().unwrap_or_default();
        if !vm.is_callable(&target) {
            return Err(JsError::type_error("Reflect.apply target is not callable"));
        }
        let receiver = args.get(1).cloned().unwrap_or_default();
        let values = reflect_arguments(vm, args.get(2).cloned().unwrap_or_default())?;
        vm.invoke(target, receiver, &values)
    });
    method(vm, reflect.clone(), "construct", 2, |vm, _, args| {
        let target = args.first().cloned().unwrap_or_default();
        if !vm.is_constructor(&target) {
            return Err(JsError::type_error(
                "Reflect.construct target is not a constructor",
            ));
        }
        let new_target = args.get(2).cloned().unwrap_or_else(|| target.clone());
        if !vm.is_constructor(&new_target) {
            return Err(JsError::type_error(
                "Reflect.construct newTarget is not a constructor",
            ));
        }
        let values = reflect_arguments(vm, args.get(1).cloned().unwrap_or_default())?;
        vm.construct_with_new_target(target, &values, new_target)
    });
    method(vm, reflect.clone(), "has", 2, |vm, _, args| {
        let target = reflect_target(args)?;
        let key = vm.to_property_key(&args.get(1).cloned().unwrap_or_default())?;
        Ok(Value::Bool(vm.has_key(&target, &key)?))
    });
    method(vm, reflect.clone(), "deleteProperty", 2, |vm, _, args| {
        let target = reflect_target(args)?;
        let key = vm.to_property_key(&args.get(1).cloned().unwrap_or_default())?;
        Ok(Value::Bool(vm.delete_key(target, &key, false)?))
    });
    method(vm, reflect.clone(), "get", 2, |vm, _, args| {
        let target = reflect_target(args)?;
        let key = vm.to_property_key(&args.get(1).cloned().unwrap_or_default())?;
        let receiver = args.get(2).cloned().unwrap_or_else(|| target.clone());
        vm.get_from(target, &key, receiver)
    });
    method(vm, reflect.clone(), "set", 3, |vm, _, args| {
        let target = reflect_target(args)?;
        let key = vm.to_property_key(&args.get(1).cloned().unwrap_or_default())?;
        let value = args.get(2).cloned().unwrap_or_default();
        let receiver = args.get(3).cloned().unwrap_or_else(|| target.clone());
        Ok(Value::Bool(vm.set_from(target, &key, value, receiver)?))
    });
    method(
        vm,
        reflect.clone(),
        "preventExtensions",
        1,
        |vm, _, args| {
            let target = reflect_target(args)?;
            Ok(Value::Bool(
                vm.prevent_extensions(target.as_object().unwrap())?,
            ))
        },
    );
    method(
        vm,
        reflect.clone(),
        "getOwnPropertyDescriptor",
        2,
        |vm, _, args| {
            let target = reflect_target(args)?;
            let key = vm.to_property_key(&args.get(1).cloned().unwrap_or_default())?;
            crate::builtins::own_property_descriptor_value(vm, target.as_object().unwrap(), &key)
        },
    );
    method(vm, reflect.clone(), "defineProperty", 3, |vm, _, args| {
        let target = reflect_target(args)?;
        let key = vm.to_property_key(&args.get(1).cloned().unwrap_or_default())?;
        let id = target.as_object().unwrap();
        let property =
            crate::builtins::descriptor(vm, id, &key, args.get(2).cloned().unwrap_or_default())?;
        property.apply(vm, target, key, false).map(Value::Bool)
    });
    vm.define_data_property(vm.global(), "Reflect", reflect, true, false, true)
        .unwrap();
    let species = vm.native(
        "get [Symbol.species]",
        0,
        false,
        Rc::new(|_, this, _| Ok(this)),
    );
    let array = vm.get_global("Array").unwrap();
    vm.define_key(
        array,
        vm.well_known_key("species"),
        Property {
            kind: PropertyKind::Accessor {
                get: Some(species),
                set: None,
            },
            enumerable: false,
            configurable: true,
        },
    )
    .unwrap();
}

fn reflect_target(args: &[Value]) -> Result<Value, JsError> {
    args.first()
        .filter(|value| value.as_object().is_some())
        .cloned()
        .ok_or_else(|| JsError::type_error("Reflect target must be an object"))
}

fn reflect_arguments(vm: &mut Vm, list: Value) -> Result<Vec<Value>, JsError> {
    if list.as_object().is_none() {
        return Err(JsError::type_error(
            "Reflect argumentsList must be an object",
        ));
    }
    let length = vm.get_property(list.clone(), "length")?;
    let length = vm.to_number(&length)?;
    let length = if length.is_nan() || length <= 0.0 {
        0
    } else if !length.is_finite() || length.floor() > vm.limits.max_array_length as f64 {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Argument list length budget exhausted",
        ));
    } else {
        length.floor() as usize
    };
    vm.reserve_allocation(length.saturating_mul(std::mem::size_of::<Value>()))?;
    let mut values = Vec::with_capacity(length);
    for index in 0..length {
        vm.tick()?;
        values.push(vm.get_property(list.clone(), &index.to_string())?);
    }
    Ok(values)
}
