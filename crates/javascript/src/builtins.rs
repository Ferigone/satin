use crate::{
    ErrorKind, JsError, JsString, PropertyKey, Value, Vm,
    binary::{NumericIndex, TypedDescriptor, numeric_index},
    heap::{Cell, Object, ObjectKind, Property, PropertyKind},
    parser::FunctionKind,
    vm::{same_value, to_i32, to_u32},
};
use std::{cmp::Ordering, mem::size_of, rc::Rc};

mod array_iteration;
mod array_methods;
mod disposable;
pub(crate) mod math;
mod number_format;
mod shadow_realm;
mod web_legacy;

pub(crate) fn install(vm: &mut Vm) {
    for (name, value) in [
        ("undefined", Value::Undefined),
        ("NaN", Value::Number(f64::NAN)),
        ("Infinity", Value::Number(f64::INFINITY)),
    ] {
        vm.define_data_property(vm.global(), name, value, false, false, false)
            .unwrap();
    }
    vm.install_global_this().unwrap();
    install_object(vm);
    install_function(vm);
    install_array(vm);
    install_string(vm);
    web_legacy::install(vm);
    install_number(vm);
    install_math(vm);
    install_errors(vm);
    crate::uri::install(vm);
    crate::symbol::install(vm);
    crate::proxy::install(vm);
    array_methods::install_unscopables(vm);
    let math = vm.get_global("Math").expect("Math is installed");
    vm.define_data_property_key(
        math,
        vm.well_known_key("toStringTag"),
        Value::from("Math"),
        false,
        false,
        true,
    )
    .unwrap();
    crate::bigint::install(vm);
    crate::date::install(vm);
    crate::regexp::install(vm);
    install_async_function(vm);
    crate::iterator::install(vm);
    crate::generator::install(vm);
    crate::binary::install(vm);
    crate::regexp::install_protocols(vm);
    crate::collections::install(vm);
    crate::promise::install(vm);
    disposable::install(vm);
    shadow_realm::install(vm);
    crate::json::install(vm);
    crate::intl::install(vm);
    crate::wasm::install(vm);
    let global = vm.global();
    let eval = method(vm, global, "eval", 1, |vm, _, a| {
        vm.direct_eval(arg(a, 0), vm.realm.global_env, false)
    });
    vm.realm.intrinsic_eval = eval.as_object();
    crate::temporal::install(vm);
}
pub(crate) fn method<F>(vm: &mut Vm, object: Value, name: &str, length: usize, callback: F) -> Value
where
    F: Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
{
    let function = vm.native(name, length, false, Rc::new(callback));
    vm.define_data_property(object, name, function.clone(), true, false, true)
        .unwrap();
    function
}
pub(crate) fn constructor<F>(
    vm: &mut Vm,
    name: &str,
    length: usize,
    prototype: Value,
    callback: F,
) -> Value
where
    F: Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
{
    let function = vm.native(name, length, true, Rc::new(callback));
    vm.define_data_property(
        function.clone(),
        "prototype",
        prototype.clone(),
        false,
        false,
        false,
    )
    .unwrap();
    vm.define_data_property(
        prototype,
        "constructor",
        function.clone(),
        true,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property(vm.global(), name, function.clone(), true, false, true)
        .unwrap();
    function
}
fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or_default()
}
pub(crate) fn replace_construct<F>(vm: &mut Vm, function: Value, callback: F)
where
    F: Fn(&mut Vm, Value, &[Value], Value) -> Result<Value, JsError> + 'static,
{
    let object = vm.heap.object_mut(function.as_object().unwrap()).unwrap();
    if let ObjectKind::Native { construct, .. } = &mut object.kind {
        *construct = Some(Rc::new(callback));
    }
}
fn install_object(vm: &mut Vm) {
    let proto = Value::Object(vm.realm.object_prototype);
    let object = constructor(vm, "Object", 1, proto.clone(), |vm, _, a| {
        let v = arg(a, 0);
        if v.is_nullish() {
            Ok(vm.new_object())
        } else {
            vm.coerce_object(v)
        }
    });
    replace_construct(vm, object.clone(), |vm, function, args, target| {
        if target != function {
            let prototype = vm.prototype_from_constructor(target, vm.realm.object_prototype)?;
            vm.allocate_constructed(ObjectKind::Ordinary, prototype)
        } else {
            let value = arg(args, 0);
            if value.is_nullish() {
                vm.allocate_constructed(ObjectKind::Ordinary, vm.realm.object_prototype)
            } else {
                vm.coerce_object(value)
            }
        }
    });
    method(vm, proto.clone(), "toString", 0, |vm, this, _| {
        let tag = match &this {
            Value::Undefined => "Undefined",
            Value::Null => "Null",
            Value::String(_) => "String",
            Value::Number(_) => "Number",
            Value::BigInt(_) | Value::Symbol(_) => "Object",
            Value::Bool(_) => "Boolean",
            Value::Object(id) => match &vm.heap.object(*id)?.kind {
                ObjectKind::Proxy(_) if vm.is_array_exotic(&this)? => "Array",
                ObjectKind::Proxy(p) if p.callable => "Function",
                ObjectKind::Array { .. } => "Array",
                ObjectKind::Arguments { .. } => "Arguments",
                ObjectKind::Function { .. }
                | ObjectKind::Native { .. }
                | ObjectKind::Bound { .. }
                | ObjectKind::PromiseFunction(..) => "Function",
                ObjectKind::Boxed(Value::String(_)) => "String",
                ObjectKind::Boxed(Value::Number(_)) => "Number",
                ObjectKind::Boxed(Value::Bool(_)) => "Boolean",
                ObjectKind::RegExp(..) => "RegExp",
                ObjectKind::Date(_) => "Date",
                ObjectKind::Error(..) => "Error",
                _ => "Object",
            },
        };
        let tag = if !this.is_nullish() && vm.well_known_symbol("toStringTag").is_some() {
            let receiver = vm.coerce_object(this)?;
            match vm.get_property_key(receiver, vm.well_known_key("toStringTag"))? {
                Value::String(tag) => tag,
                _ => tag.into(),
            }
        } else {
            tag.into()
        };
        vm.ensure_string_allocation(tag.len().saturating_add(9))?;
        let mut units: Vec<_> = "[object ".encode_utf16().collect();
        vm.consume_work(tag.len().saturating_add(9))?;
        units.extend_from_slice(tag.as_utf16());
        units.push(93);
        vm.finish_string(units)
    });
    method(vm, proto.clone(), "valueOf", 0, |vm, this, _| {
        vm.coerce_object(this)
    });
    method(vm, proto.clone(), "toLocaleString", 0, |vm, this, _| {
        let method = vm.get_property(this.clone(), "toString")?;
        vm.invoke(method, this, &[])
    });
    method(vm, proto.clone(), "hasOwnProperty", 1, |vm, this, a| {
        let object = vm.coerce_object(this)?;
        let key = vm.to_property_key(&arg(a, 0))?;
        Ok(Value::Bool(
            vm.own_enumerability(object.as_object().unwrap(), &key)?
                .is_some(),
        ))
    });
    method(
        vm,
        proto.clone(),
        "propertyIsEnumerable",
        1,
        |vm, this, a| {
            let object = vm.coerce_object(this)?;
            let key = vm.to_property_key(&arg(a, 0))?;
            Ok(Value::Bool(
                vm.own_enumerability(object.as_object().unwrap(), &key)?
                    .unwrap_or(false),
            ))
        },
    );
    method(vm, proto.clone(), "isPrototypeOf", 1, |vm, this, a| {
        let this = vm.coerce_object(this)?;
        let mut current = arg(a, 0).as_object();
        for _ in 0..1024 {
            let Some(id) = current else {
                return Ok(Value::Bool(false));
            };
            current = vm.object_prototype_of(id)?;
            if current == this.as_object() {
                return Ok(Value::Bool(true));
            }
        }
        Err(JsError::new(
            ErrorKind::RangeError,
            "Prototype chain limit exceeded",
        ))
    });
    let get = vm.new_native_function("get __proto__", |vm, this, _| {
        let object = vm.coerce_object(this)?;
        Ok(vm
            .object_prototype_of(object.as_object().unwrap())?
            .map(Value::Object)
            .unwrap_or(Value::Null))
    });
    let set = vm.new_native_function("set __proto__", |vm, this, a| {
        let proto = arg(a, 0);
        if let Some(id) = this.as_object()
            && (proto == Value::Null || proto.as_object().is_some())
        {
            vm.set_prototype(id, proto.as_object())?;
        }
        Ok(Value::Undefined)
    });
    vm.define_property(
        proto,
        "__proto__".into(),
        Property {
            kind: PropertyKind::Accessor {
                get: Some(get),
                set: Some(set),
            },
            enumerable: false,
            configurable: true,
        },
    )
    .unwrap();
    method(vm, object.clone(), "keys", 1, |vm, _, a| {
        let keys = vm
            .own_keys(arg(a, 0), true)?
            .into_iter()
            .map(Value::String)
            .collect::<Vec<_>>();
        vm.try_new_array(&keys)
    });
    method(vm, object.clone(), "getOwnPropertyNames", 1, |vm, _, a| {
        let keys = vm
            .own_keys(arg(a, 0), false)?
            .into_iter()
            .map(Value::String)
            .collect::<Vec<_>>();
        vm.try_new_array(&keys)
    });
    for (name, entries) in [("values", false), ("entries", true)] {
        method(vm, object.clone(), name, 1, move |vm, _, a| {
            let target = vm.coerce_object(arg(a, 0))?;
            let id = target.as_object().unwrap();
            let keys = vm.own_keys(target.clone(), false)?;
            let mut values = Vec::new();
            for key in keys {
                vm.tick()?;
                // Previous getters can delete a key or change its enumerability.
                if vm.own_enumerability(id, &PropertyKey::String(key.clone()))? != Some(true) {
                    continue;
                }
                let value = vm.get(target.clone(), &key)?;
                values.push(if entries {
                    vm.try_new_array(&[Value::String(key), value])?
                } else {
                    value
                });
            }
            vm.try_new_array(&values)
        });
    }
    method(vm, object.clone(), "hasOwn", 2, |vm, _, a| {
        let target = vm.coerce_object(arg(a, 0))?;
        let key = vm.to_property_key(&arg(a, 1))?;
        Ok(Value::Bool(
            vm.own_enumerability(target.as_object().unwrap(), &key)?
                .is_some(),
        ))
    });
    method(vm, object.clone(), "is", 2, |vm, _, a| {
        let (a, b) = (arg(a, 0), arg(a, 1));
        vm.charge_bigint_comparison(&a, &b)?;
        Ok(Value::Bool(same_value(&a, &b)))
    });
    method(vm, object.clone(), "create", 2, |vm, _, a| {
        let prototype = arg(a, 0);
        if prototype != Value::Null && prototype.as_object().is_none() {
            return Err(JsError::type_error(
                "Object prototype must be an object or null",
            ));
        }
        let object = Value::Object(vm.heap.allocate(Cell::Object(Object::new(
            ObjectKind::Ordinary,
            prototype.as_object(),
        ))));
        if !arg(a, 1).is_undefined() {
            define_properties(vm, object.clone(), arg(a, 1))?;
        }
        Ok(object)
    });
    method(vm, object.clone(), "getPrototypeOf", 1, |vm, _, a| {
        let object = vm.coerce_object(arg(a, 0))?;
        Ok(vm
            .object_prototype_of(object.as_object().unwrap())?
            .map(Value::Object)
            .unwrap_or(Value::Null))
    });
    method(vm, object.clone(), "setPrototypeOf", 2, |vm, _, a| {
        let target = arg(a, 0);
        if target.is_nullish() {
            return Err(JsError::type_error(
                "Cannot set prototype of null or undefined",
            ));
        }
        let proto = arg(a, 1);
        if proto != Value::Null && proto.as_object().is_none() {
            return Err(JsError::type_error("Prototype must be object or null"));
        }
        if let Some(id) = target.as_object() {
            vm.set_prototype(id, proto.as_object())?;
        }
        Ok(target)
    });
    method(vm, object.clone(), "defineProperty", 3, |vm, _, a| {
        let target = arg(a, 0);
        let id = target
            .as_object()
            .ok_or_else(|| JsError::type_error("Property target is not an object"))?;
        let key = vm.to_property_key(&arg(a, 1))?;
        let descriptor = descriptor(vm, id, &key, arg(a, 2))?;
        descriptor.apply(vm, target.clone(), key, true)?;
        Ok(target)
    });
    method(vm, object.clone(), "defineProperties", 2, |vm, _, a| {
        let target = arg(a, 0);
        define_properties(vm, target.clone(), arg(a, 1))?;
        Ok(target)
    });
    method(
        vm,
        object.clone(),
        "getOwnPropertyDescriptor",
        2,
        |vm, _, a| {
            let target = vm.coerce_object(arg(a, 0))?;
            let key = vm.to_property_key(&arg(a, 1))?;
            own_property_descriptor_value(vm, target.as_object().unwrap(), &key)
        },
    );
    method(vm, object.clone(), "getOwnPropertyDescriptors", 1, |vm, _, args| {
        let target = vm.coerce_object(arg(args, 0))?;
        let id = target.as_object().unwrap();
        let result = vm.new_object();
        for key in vm.own_property_keys(target)? {
            vm.tick()?;
            let descriptor = own_property_descriptor_value(vm, id, &key)?;
            if !descriptor.is_undefined() {
                vm.define_data_property_key(result.clone(), key, descriptor, true, true, true)?;
            }
        }
        Ok(result)
    });
    method(vm, object.clone(), "groupBy", 2, |vm, _, args| {
        crate::collections::group_by(vm, arg(args, 0), arg(args, 1), None)
    });
    method(vm, object.clone(), "fromEntries", 1, |vm, _, args| {
        let mut iterator = vm.get_iterator(arg(args, 0))?;
        let result = vm.new_object();
        while let Some(entry) = vm.iterator_step(&mut iterator)? {
            let added = (|| {
                if entry.as_object().is_none() {
                    return Err(JsError::type_error("Iterator entry must be an object"));
                }
                let key = vm.get_property(entry.clone(), "0")?;
                let value = vm.get_property(entry, "1")?;
                let key = vm.to_property_key(&key)?;
                vm.define_data_property_key(result.clone(), key, value, true, true, true)
            })();
            if let Err(error) = added {
                return vm.iterator_close(&mut iterator, Err(error));
            }
        }
        Ok(result)
    });
    method(vm, object.clone(), "assign", 2, |vm, _, a| {
        let target = vm.coerce_object(arg(a, 0))?;
        for source in a.iter().skip(1) {
            if source.is_nullish() {
                continue;
            }
            let source = vm.coerce_object(source.clone())?;
            for key in vm.own_property_keys(source.clone())? {
                vm.tick()?;
                if vm
                    .own_enumerability(source.as_object().unwrap(), &key)?
                    .unwrap_or(false)
                {
                    let v = vm.get_property_key(source.clone(), key.clone())?;
                    vm.set_key(target.clone(), &key, v, true)?;
                }
            }
        }
        Ok(target)
    });
    for (name, seal, freeze) in [
        ("preventExtensions", false, false),
        ("seal", true, false),
        ("freeze", true, true),
    ] {
        method(vm, object.clone(), name, 1, move |vm, _, a| {
            let target = arg(a, 0);
            if let Some(id) = target.as_object() {
                if !vm.prevent_extensions(id)? {
                    return Err(JsError::type_error("Object refuses to prevent extensions"));
                }
                if seal {
                    for key in vm.own_property_keys(target.clone())? {
                        let mut p = vm.own_descriptor_key(id, &key)?.unwrap();
                        p.configurable = false;
                        if freeze && let PropertyKind::Data { writable, .. } = &mut p.kind {
                            *writable = false;
                        }
                        vm.define_key(target.clone(), key, p)?;
                    }
                }
            }
            Ok(target)
        });
    }
    for (name, frozen) in [("isSealed", false), ("isFrozen", true)] {
        method(vm, object.clone(), name, 1, move |vm, _, a| {
            let value = arg(a, 0);
            let Some(id) = value.as_object() else {
                return Ok(Value::Bool(true));
            };
            if vm.object_extensible(id)? {
                return Ok(Value::Bool(false));
            }
            for key in vm.own_property_keys(value)? {
                vm.tick()?;
                if let Some(property) = vm.own_descriptor_key(id, &key)?
                    && (property.configurable
                        || (frozen
                            && matches!(property.kind, PropertyKind::Data { writable: true, .. })))
                {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        });
    }
    method(vm, object, "isExtensible", 1, |vm, _, a| {
        Ok(Value::Bool(if let Some(id) = arg(a, 0).as_object() {
            vm.object_extensible(id)?
        } else {
            false
        }))
    });
}
pub(crate) fn own_property_descriptor_value(
    vm: &mut Vm,
    id: crate::ObjectId,
    key: &PropertyKey,
) -> Result<Value, JsError> {
    let Some(property) = vm.own_descriptor_key(id, key)? else {
        return Ok(Value::Undefined);
    };
    let result = vm.new_object();
    let contents = match property.kind {
        PropertyKind::Data { value, writable } => {
            crate::temporal::force_property(vm, key, &value)?;
            [("value", value), ("writable", writable.into())]
        }
        PropertyKind::Accessor { get, set } => [
            ("get", get.unwrap_or_default()),
            ("set", set.unwrap_or_default()),
        ],
    };
    for (name, value) in contents.into_iter().chain([
        ("enumerable", property.enumerable.into()),
        ("configurable", property.configurable.into()),
    ]) {
        vm.define_property(result.clone(), name.into(), Property::data(value))?;
    }
    Ok(result)
}
pub(crate) enum Definition {
    Ordinary(Property),
    ArrayLength(crate::proxy::PartialDescriptor),
    Proxy(crate::proxy::PartialDescriptor),
    Typed(NumericIndex, TypedDescriptor),
    Named(Option<Value>),
}

impl Definition {
    pub(crate) fn apply(
        self,
        vm: &mut Vm,
        target: Value,
        key: PropertyKey,
        throw: bool,
    ) -> Result<bool, JsError> {
        let id = target.as_object().unwrap();
        match self {
            Self::Proxy(descriptor) => {
                let ok = vm.proxy_define(id, &key, descriptor)?;
                if !ok && throw {
                    return Err(JsError::type_error("Proxy rejected property definition"));
                }
                Ok(ok)
            }
            Self::Named(value) => {
                let PropertyKey::String(name) = key else {
                    unreachable!()
                };
                let Some(value) = value else {
                    if throw {
                        return Err(JsError::type_error(
                            "Named host properties require a data descriptor",
                        ));
                    }
                    return Ok(false);
                };
                vm.host_named_definition(id, &name)?
                    .expect("named provider persists after descriptor conversion")
                    .set(vm, name, value)?;
                Ok(true)
            }
            Self::Typed(index, descriptor) => {
                let success = vm.apply_typed_descriptor(id, index, descriptor)?;
                if !success && throw {
                    return Err(JsError::type_error(
                        "Invalid TypedArray property definition",
                    ));
                }
                Ok(success)
            }
            Self::ArrayLength(descriptor) => {
                let success = descriptor.apply_array_length(vm, id)?;
                if !success && throw {
                    return Err(JsError::type_error("Cannot redefine array length"));
                }
                Ok(success)
            }
            Self::Ordinary(property) => {
                match vm.define_key(target, key, property) {
                    Ok(()) => Ok(true),
                    Err(error) if !throw && error.kind == ErrorKind::TypeError => Ok(false),
                    Err(error) => Err(error),
                }
            }
        }
    }
}

pub(crate) fn descriptor(
    vm: &mut Vm,
    id: crate::ObjectId,
    key: &PropertyKey,
    descriptor: Value,
) -> Result<Definition, JsError> {
    if descriptor.as_object().is_none() {
        return Err(JsError::type_error("Property descriptor must be an object"));
    }
    if vm.proxy(id) {
        return Ok(Definition::Proxy(crate::proxy::PartialDescriptor::read(
            vm, descriptor,
        )?));
    }
    if key == &PropertyKey::from("length")
        && matches!(vm.heap.object(id)?.kind, ObjectKind::Array { .. })
    {
        return Ok(Definition::ArrayLength(
            crate::proxy::PartialDescriptor::read(vm, descriptor)?,
        ));
    }
    if let PropertyKey::String(name) = key
        && matches!(vm.heap.object(id)?.kind, ObjectKind::TypedArray(_))
        && let Some(index) = numeric_index(name)
    {
        return Ok(Definition::Typed(
            index,
            TypedDescriptor::read(vm, descriptor)?,
        ));
    }
    let old = vm.own_descriptor_without_host(id, key)?;
    let mut p = old.clone().unwrap_or(Property {
        kind: PropertyKind::Data {
            value: Value::Undefined,
            writable: false,
        },
        enumerable: false,
        configurable: false,
    });
    if vm.has(&descriptor, &"enumerable".into())? {
        p.enumerable = vm
            .get_property(descriptor.clone(), "enumerable")?
            .is_truthy();
    }
    if vm.has(&descriptor, &"configurable".into())? {
        p.configurable = vm
            .get_property(descriptor.clone(), "configurable")?
            .is_truthy();
    }
    let get = vm.has(&descriptor, &"get".into())?;
    let set = vm.has(&descriptor, &"set".into())?;
    let value = vm.has(&descriptor, &"value".into())?;
    let writable = vm.has(&descriptor, &"writable".into())?;
    if (get || set) && (value || writable) {
        return Err(JsError::type_error(
            "Descriptor cannot be both data and accessor",
        ));
    }
    if get || set {
        let (mut getter, mut setter) = match p.kind {
            PropertyKind::Accessor { get, set } => (get, set),
            _ => (None, None),
        };
        if get {
            let v = vm.get_property(descriptor.clone(), "get")?;
            if !v.is_undefined() && !vm.is_callable(&v) {
                return Err(JsError::type_error("Getter must be callable or undefined"));
            }
            getter = if v.is_undefined() { None } else { Some(v) };
        }
        if set {
            let v = vm.get_property(descriptor, "set")?;
            if !v.is_undefined() && !vm.is_callable(&v) {
                return Err(JsError::type_error("Setter must be callable or undefined"));
            }
            setter = if v.is_undefined() { None } else { Some(v) };
        }
        p.kind = PropertyKind::Accessor {
            get: getter,
            set: setter,
        };
    } else if value || writable {
        let (mut v, mut w) = match p.kind {
            PropertyKind::Data { value, writable } => (value, writable),
            _ => (Value::Undefined, false),
        };
        if value {
            v = vm.get_property(descriptor.clone(), "value")?;
        }
        if writable {
            w = vm.get_property(descriptor, "writable")?.is_truthy();
        }
        p.kind = PropertyKind::Data {
            value: v,
            writable: w,
        };
    }
    if let PropertyKey::String(name) = key
        && vm.host_named_definition(id, name)?.is_some()
    {
        let item = match p.kind {
            PropertyKind::Data { value: v, .. } if value || writable => Some(v),
            _ => None,
        };
        return Ok(Definition::Named(item));
    }
    Ok(Definition::Ordinary(p))
}
fn define_properties(vm: &mut Vm, target: Value, properties: Value) -> Result<(), JsError> {
    let id = target
        .as_object()
        .ok_or_else(|| JsError::type_error("Property target is not an object"))?;
    let source = vm.coerce_object(properties)?;
    let mut descriptors = Vec::new();
    for key in vm.own_property_keys(source.clone())? {
        if vm
            .own_enumerability(source.as_object().unwrap(), &key)?
            .unwrap_or(false)
        {
            let raw = vm.get_property_key(source.clone(), key.clone())?;
            descriptors.push((key.clone(), descriptor(vm, id, &key, raw)?));
        }
    }
    for (key, p) in descriptors {
        p.apply(vm, target.clone(), key, true)?;
    }
    Ok(())
}
fn install_function(vm: &mut Vm) {
    let proto = Value::Object(vm.realm.function_prototype);
    vm.define_data_property(proto.clone(), "length", Value::from(0), false, false, true)
        .unwrap();
    vm.define_data_property(proto.clone(), "name", Value::from(""), false, false, true)
        .unwrap();
    let function = constructor(vm, "Function", 1, proto.clone(), |vm, _, args| {
        create_dynamic_function(vm, args, None, FunctionKind::Ordinary)
    });
    replace_construct(vm, function, |vm, _, args, target| {
        create_dynamic_function(vm, args, Some(target), FunctionKind::Ordinary)
    });
    let throw_type_error = vm.native(
        "",
        0,
        false,
        Rc::new(|_, _, _| Err(JsError::type_error("Restricted function property"))),
    );
    vm.realm.throw_type_error = throw_type_error.as_object();
    let thrower = vm.heap.object_mut(throw_type_error.as_object().unwrap()).unwrap();
    for key in ["length", "name"] {
        thrower.properties.get_mut(&JsString::from(key)).unwrap().configurable = false;
    }
    thrower.extensible = false;
    for name in ["caller", "arguments"] {
        vm.define_key(
            proto.clone(),
            name.into(),
            Property {
                kind: PropertyKind::Accessor {
                    get: Some(throw_type_error.clone()),
                    set: Some(throw_type_error.clone()),
                },
                enumerable: false,
                configurable: true,
            },
        )
        .unwrap();
    }
    method(vm, proto.clone(), "call", 1, |vm, this, a| {
        vm.invoke(this, arg(a, 0), a.get(1..).unwrap_or_default())
    });
    method(vm, proto.clone(), "apply", 2, |vm, this, a| {
        if !vm.is_callable(&this) {
            return Err(JsError::type_error("Apply target is not callable"));
        }
        let args = arg(a, 1);
        let values = if args.is_nullish() {
            Vec::new()
        } else {
            if args.as_object().is_none() {
                return Err(JsError::type_error("Apply arguments must be an object"));
            }
            let len = length(vm, args.clone())?;
            vm.reserve_allocation(len.saturating_mul(std::mem::size_of::<Value>()))?;
            let mut values = Vec::with_capacity(len);
            for i in 0..len {
                vm.tick()?;
                values.push(vm.get_property(args.clone(), &i.to_string())?);
            }
            values
        };
        vm.invoke(this, arg(a, 0), &values)
    });
    method(vm, proto.clone(), "bind", 1, |vm, this, a| {
        if !vm.is_callable(&this) {
            return Err(JsError::type_error("Bind target is not callable"));
        }
        let target = this.as_object().unwrap();
        let prototype = vm.heap.object(target)?.prototype;
        let bound_args = a.get(1..).unwrap_or_default();
        vm.ensure_allocation(
            bound_args
                .len()
                .saturating_mul(std::mem::size_of::<Value>())
                .saturating_add(256),
        )?;
        let bound = Value::Object(vm.heap.allocate(Cell::Object(Object::new(
            ObjectKind::Bound {
                target: this.clone(),
                this: arg(a, 0),
                args: bound_args.to_vec(),
            },
            prototype,
        ))));
        let length = if vm.own_descriptor_key(target, &"length".into())?.is_some() {
            vm.get_property(this.clone(), "length")?
                .as_number()
                .map_or(0.0, |n| (n.trunc() - bound_args.len() as f64).max(0.0))
        } else {
            0.0
        };
        vm.define_data_property(
            bound.clone(),
            "length",
            Value::Number(length),
            false,
            false,
            true,
        )?;
        let name = match vm.get_property(this, "name")? {
            Value::String(name) => name,
            _ => JsString::default(),
        };
        let prefix = "bound ";
        let length = name.len().saturating_add(prefix.len());
        vm.ensure_string_allocation(length)?;
        vm.consume_work(length)?;
        let mut units = Vec::with_capacity(length);
        units.extend(prefix.encode_utf16());
        name.copy_units(&mut units);
        let bound_name = vm.finish_written_string(units)?;
        vm.define_data_property(
            bound.clone(),
            "name",
            bound_name,
            false,
            false,
            true,
        )?;
        Ok(bound)
    });
    method(vm, proto, "toString", 0, |vm, this, _| {
        if !vm.is_callable(&this) {
            return Err(JsError::type_error(
                "Function.toString receiver is not callable",
            ));
        }
        let id = this.as_object().unwrap();
        let name = match &vm.heap.object(id)?.kind {
            ObjectKind::Function { code, .. } => {
                let source = code.source.clone();
                vm.consume_work(source.len())?;
                vm.ensure_string_allocation(source.utf16_len(0..source.len()))?;
                return vm.finish_string(source.utf16_range(0..source.len()));
            }
            ObjectKind::Native { initial_name, .. } => initial_name.clone(),
            _ => JsString::default(),
        };
        vm.ensure_string_allocation(name.len().saturating_add(32))?;
        vm.consume_work(name.len().saturating_add(32))?;
        Ok(Value::from(format!("function {}() {{ [native code] }}", name.to_string_lossy())))
    });
}
fn install_async_function(vm: &mut Vm) {
    let prototype = vm
        .new_object_with_prototype(Value::Object(vm.realm.function_prototype))
        .unwrap();
    vm.realm.async_function_prototype = prototype.as_object();
    let constructor = vm.native(
        "AsyncFunction",
        1,
        false,
        Rc::new(|vm, _, args| create_dynamic_function(vm, args, None, FunctionKind::Async)),
    );
    replace_construct(vm, constructor.clone(), |vm, _, args, target| {
        create_dynamic_function(vm, args, Some(target), FunctionKind::Async)
    });
    let function = vm.get_global("Function").unwrap();
    vm.set_prototype(constructor.as_object().unwrap(), function.as_object())
        .unwrap();
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
        constructor,
        false,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property_key(
        prototype,
        vm.well_known_key("toStringTag"),
        Value::from("AsyncFunction"),
        false,
        false,
        true,
    )
    .unwrap();
}

pub(crate) fn create_dynamic_function(
    vm: &mut Vm,
    a: &[Value],
    new_target: Option<Value>,
    kind: FunctionKind,
) -> Result<Value, JsError> {
    let mut params = Vec::new();
    let mut source_units = 0usize;
    for value in a.iter().take(a.len().saturating_sub(1)) {
        vm.tick()?;
        let s = vm.to_string(value)?;
        source_units = source_units.saturating_add(s.len()).saturating_add(1);
        vm.ensure_dynamic_source(source_units)?;
        vm.consume_work(s.len().saturating_mul(3))?;
        params.push(s);
    }
    let body = if let Some(value) = a.last() {
        let s = vm.to_string(value)?;
        vm.ensure_dynamic_source(source_units.saturating_add(s.len()))?;
        vm.consume_work(s.len().saturating_mul(3))?;
        s
    } else {
        JsString::default()
    };
    // Finish the observable ToString calls before converting to the parser's
    // UTF-8 input. A later conversion hook can throw even if earlier source
    // text contains a code unit the parser cannot represent.
    let params = params
        .into_iter()
        .map(|s| {
            String::from_utf16(s.as_utf16()).map_err(|_| {
                JsError::new(
                    ErrorKind::UnsupportedFeature,
                    "Function parameters containing isolated surrogates are not implemented",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let body = String::from_utf16(body.as_utf16()).map_err(|_| {
        JsError::new(
            ErrorKind::UnsupportedFeature,
            "Function source containing isolated surrogates is not implemented",
        )
    })?;
    vm.dynamic_function(&params.join(","), &body, new_target, kind)
}
const MAX_SAFE_LENGTH: u64 = (1 << 53) - 1;

fn array_like_length(vm: &mut Vm, object: Value) -> Result<u64, JsError> {
    let value = vm.get_property(object, "length")?;
    Ok(integer(vm, value)?.clamp(0.0, MAX_SAFE_LENGTH as f64) as u64)
}

fn relative_index(index: f64, len: u64) -> u64 {
    if index < 0.0 {
        (len as f64 + index).max(0.0) as u64
    } else {
        index.min(len as f64) as u64
    }
}

fn length(vm: &mut Vm, object: Value) -> Result<usize, JsError> {
    let value = vm.get_property(object, "length")?;
    let n = vm.to_number(&value)?;
    let n = if n.is_nan() || n <= 0.0 {
        0.0
    } else {
        n.floor()
    };
    if n > vm.limits.max_array_length as f64 {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Array-like length budget exhausted",
        ));
    }
    Ok(n as usize)
}
fn integer(vm: &mut Vm, value: Value) -> Result<f64, JsError> {
    let n = vm.to_number(&value)?;
    Ok(if n.is_nan() { 0.0 } else { n.trunc() })
}
fn relative(n: f64, len: usize) -> usize {
    if n < 0.0 {
        (len as f64 + n).max(0.0) as usize
    } else {
        n.min(len as f64) as usize
    }
}
fn create_array(vm: &mut Vm, args: &[Value], prototype: crate::ObjectId) -> Result<Value, JsError> {
    if args.len() == 1 && matches!(args[0], Value::Number(_)) {
        let array = vm.try_new_array_with_prototype(&[], prototype)?;
        vm.set_property(array.clone(), "length", args[0].clone())?;
        Ok(array)
    } else {
        vm.try_new_array_with_prototype(args, prototype)
    }
}

fn move_indexed_property(
    vm: &mut Vm,
    object: &Value,
    from: u64,
    to: u64,
) -> Result<(), JsError> {
    vm.tick()?;
    let from = JsString::from(from.to_string());
    let to = JsString::from(to.to_string());
    if vm.has(object, &from)? {
        let value = vm.get(object.clone(), &from)?;
        vm.set(object.clone(), &to, value, true)?;
    } else {
        vm.delete(object.clone(), &to, true)?;
    }
    Ok(())
}

fn splice_array(vm: &mut Vm, receiver: Value, args: &[Value]) -> Result<Value, JsError> {
    let object = vm.coerce_object(receiver)?;
    let len = array_like_length(vm, object.clone())?;
    let start = relative_index(integer(vm, arg(args, 0))?, len);
    let removed_count = match args.len() {
        0 => 0,
        1 => len - start,
        _ => integer(vm, arg(args, 1))?.clamp(0.0, (len - start) as f64) as u64,
    };
    let items = args.get(2..).unwrap_or_default();
    let new_len = (len - removed_count).saturating_add(items.len() as u64);
    if new_len > MAX_SAFE_LENGTH {
        return Err(JsError::type_error(
            "Array splice exceeds maximum safe length",
        ));
    }
    let inserted = items.len() as u64;
    let tail_count = if inserted == removed_count {
        0
    } else {
        len - start - removed_count
    };
    let visits = removed_count + tail_count + len.saturating_sub(new_len) + inserted;
    if visits > u64::from(vm.limits.max_array_length) {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Array splice traversal budget exhausted",
        ));
    }
    let removed = vm.array_species_create(object.clone(), removed_count)?;

    for i in 0..removed_count {
        vm.tick()?;
        let from = JsString::from((start + i).to_string());
        if vm.has(&object, &from)? {
            let value = vm.get(object.clone(), &from)?;
            vm.define_data_property(removed.clone(), &i.to_string(), value, true, true, true)?;
        }
    }
    vm.set_property(removed.clone(), "length", Value::from(removed_count as f64))?;

    let tail = start + removed_count..len;
    match inserted.cmp(&removed_count) {
        Ordering::Less => {
            for from in tail {
                move_indexed_property(vm, &object, from, from - removed_count + inserted)?;
            }
            for i in (new_len..len).rev() {
                vm.tick()?;
                vm.delete(object.clone(), &i.to_string().into(), true)?;
            }
        }
        Ordering::Greater => {
            for from in tail.rev() {
                move_indexed_property(vm, &object, from, from - removed_count + inserted)?;
            }
        }
        Ordering::Equal => {}
    }
    for (i, item) in items.iter().enumerate() {
        vm.tick()?;
        vm.set_property(
            object.clone(),
            &(start + i as u64).to_string(),
            item.clone(),
        )?;
    }
    vm.set_property(object, "length", Value::from(new_len as f64))?;
    Ok(removed)
}

fn reserve_sort_items(
    vm: &mut Vm,
    items: &mut Vec<Value>,
    additional: usize,
) -> Result<(), JsError> {
    vm.reserve_allocation(additional.saturating_mul(size_of::<Value>()))?;
    items
        .try_reserve_exact(additional)
        .map_err(|_| JsError::new(ErrorKind::HeapLimit, "Array sort allocation failed"))
}

fn compare_array_elements(
    vm: &mut Vm,
    comparator: &Value,
    left: &Value,
    right: &Value,
) -> Result<Ordering, JsError> {
    vm.tick()?;
    match (left.is_undefined(), right.is_undefined()) {
        (true, true) => return Ok(Ordering::Equal),
        (true, false) => return Ok(Ordering::Greater),
        (false, true) => return Ok(Ordering::Less),
        (false, false) => {}
    }
    if !comparator.is_undefined() {
        let result = vm.invoke(
            comparator.clone(),
            Value::Undefined,
            &[left.clone(), right.clone()],
        )?;
        return Ok(vm
            .to_number(&result)?
            .partial_cmp(&0.0)
            .unwrap_or(Ordering::Equal));
    }
    let left = vm.to_string(left)?;
    let right = vm.to_string(right)?;
    if left.storage_id() == right.storage_id() {
        return Ok(Ordering::Equal);
    }
    vm.consume_work(left.len().min(right.len()))?;
    Ok(left.cmp(&right))
}

fn sort_array(vm: &mut Vm, receiver: Value, args: &[Value]) -> Result<Value, JsError> {
    let comparator = arg(args, 0);
    if !comparator.is_undefined() && !vm.is_callable(&comparator) {
        return Err(JsError::type_error("Array sort comparator is not callable"));
    }
    let object = vm.coerce_object(receiver)?;
    let length = length(vm, object.clone())?;
    let mut items = Vec::new();
    for index in 0..length {
        vm.tick()?;
        let key = JsString::from(index.to_string());
        if vm.has(&object, &key)? {
            if items.len() == items.capacity() {
                let additional = items.capacity().max(8).min(length - items.len());
                reserve_sort_items(vm, &mut items, additional)?;
            }
            items.push(vm.get(object.clone(), &key)?);
        }
    }

    let item_count = items.len();
    if item_count > 1 {
        let mut scratch = Vec::new();
        reserve_sort_items(vm, &mut scratch, item_count)?;
        items = stable_merge_sort(vm, items, scratch, |vm, left, right| {
            compare_array_elements(vm, &comparator, left, right)
        })?;
    }
    for (index, item) in items.into_iter().enumerate() {
        vm.tick()?;
        vm.set(object.clone(), &index.to_string().into(), item, true)?;
    }
    for index in item_count..length {
        vm.tick()?;
        vm.delete(object.clone(), &index.to_string().into(), true)?;
    }
    Ok(object)
}

pub(crate) fn stable_merge_sort<T: Clone>(
    vm: &mut Vm,
    mut items: Vec<T>,
    mut scratch: Vec<T>,
    mut compare: impl FnMut(&mut Vm, &T, &T) -> Result<Ordering, JsError>,
) -> Result<Vec<T>, JsError> {
    // Comparisons can run JavaScript and throw. Sort the snapshot before any
    // writes, and take the left item first when two items compare equal.
    let length = items.len();
    let mut width = 1;
    while width < length {
        scratch.clear();
        for start in (0..length).step_by(width * 2) {
            let end = (start + width * 2).min(length);
            let middle = (start + width).min(end);
            let mut left = start;
            let mut right = middle;
            while left < middle && right < end {
                if compare(vm, &items[left], &items[right])? != Ordering::Greater {
                    scratch.push(items[left].clone());
                    left += 1;
                } else {
                    scratch.push(items[right].clone());
                    right += 1;
                }
            }
            vm.consume_work(middle - left + end - right)?;
            scratch.extend_from_slice(&items[left..middle]);
            scratch.extend_from_slice(&items[right..end]);
        }
        std::mem::swap(&mut items, &mut scratch);
        width *= 2;
    }
    Ok(items)
}

fn install_array(vm: &mut Vm) {
    let proto = Value::Object(vm.realm.array_prototype);
    let array = constructor(vm, "Array", 1, proto.clone(), |vm, _, args| {
        create_array(vm, args, vm.realm.array_prototype)
    });
    replace_construct(vm, array.clone(), |vm, _, args, target| {
        let prototype = vm.prototype_from_constructor(target, vm.realm.array_prototype)?;
        create_array(vm, args, prototype)
    });
    method(vm, array.clone(), "isArray", 1, |vm, _, a| {
        Ok(Value::Bool(vm.is_array_exotic(&arg(a, 0))?))
    });
    method(vm, array, "of", 0, array_methods::array_of);
    method(vm, proto.clone(), "at", 1, |vm, this, args| {
        let object = vm.coerce_object(this)?;
        let length = vm.get_property(object.clone(), "length")?;
        // LengthOfArrayLike: at needs at most one property, even on a huge
        // generic receiver. The Array storage/traversal cap does not apply.
        let length = integer(vm, length)?.clamp(0.0, 9_007_199_254_740_991.0);
        let index = integer(vm, arg(args, 0))?;
        let index = if index.is_finite() && index < 0.0 {
            length + index
        } else {
            index
        };
        if index < 0.0 || index >= length {
            return Ok(Value::Undefined);
        }
        vm.get_property(object, &(index as u64).to_string())
    });
    method(vm, proto.clone(), "sort", 1, sort_array);
    method(vm, proto.clone(), "splice", 2, splice_array);
    method(vm, proto.clone(), "push", 1, |vm, this, a| {
        let object = vm.coerce_object(this)?;
        let len = array_like_length(vm, object.clone())?;
        let new_len = len + a.len() as u64;
        if new_len > MAX_SAFE_LENGTH {
            return Err(JsError::type_error(
                "Array push exceeds maximum safe length",
            ));
        }
        for (i, value) in a.iter().enumerate() {
            vm.tick()?;
            vm.set_property(object.clone(), &(len + i as u64).to_string(), value.clone())?;
        }
        vm.set_property(object, "length", Value::from(new_len as f64))?;
        Ok(Value::from(new_len as f64))
    });
    method(vm, proto.clone(), "pop", 0, |vm, this, _| {
        let object = vm.coerce_object(this)?;
        let len = array_like_length(vm, object.clone())?;
        if len == 0 {
            vm.set_property(object, "length", Value::from(0))?;
            return Ok(Value::Undefined);
        }
        let key = JsString::from((len - 1).to_string());
        let value = vm.get(object.clone(), &key)?;
        vm.delete(object.clone(), &key, true)?;
        vm.set_property(object, "length", Value::from((len - 1) as f64))?;
        Ok(value)
    });
    method(vm, proto.clone(), "shift", 0, |vm, this, _| {
        let object = vm.coerce_object(this)?;
        let len = array_like_length(vm, object.clone())?;
        if len == 0 {
            vm.set_property(object, "length", Value::from(0))?;
            return Ok(Value::Undefined);
        }
        let value = vm.get_property(object.clone(), "0")?;
        for i in 1..len {
            move_indexed_property(vm, &object, i, i - 1)?;
        }
        vm.delete(object.clone(), &(len - 1).to_string().into(), true)?;
        vm.set_property(object, "length", Value::from((len - 1) as f64))?;
        Ok(value)
    });
    method(vm, proto.clone(), "unshift", 1, |vm, this, a| {
        let object = vm.coerce_object(this)?;
        let len = length(vm, object.clone())?;
        if len.saturating_add(a.len()) > vm.limits.max_array_length as usize {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Array length budget exhausted",
            ));
        }
        for i in (0..len).rev() {
            vm.tick()?;
            let from = JsString::from(i.to_string());
            let to = JsString::from((i + a.len()).to_string());
            if vm.has(&object, &from)? {
                let value = vm.get(object.clone(), &from)?;
                vm.set(object.clone(), &to, value, true)?;
            } else {
                vm.delete(object.clone(), &to, true)?;
            }
        }
        for (i, v) in a.iter().enumerate() {
            vm.set_property(object.clone(), &i.to_string(), v.clone())?;
        }
        vm.set_property(object, "length", Value::from(len + a.len()))?;
        Ok(Value::from(len + a.len()))
    });
    method(vm, proto.clone(), "join", 1, |vm, this, a| {
        let object = vm.coerce_object(this)?;
        let len = length(vm, object.clone())?;
        let separator = if arg(a, 0).is_undefined() {
            JsString::from(",")
        } else {
            vm.to_string(&arg(a, 0))?
        };
        join_indexed_properties(vm, len, &separator, None, |vm, index| {
            vm.get_property(object.clone(), &index.to_string())
        })
    });
    method(vm, proto.clone(), "toLocaleString", 0, |vm, this, args| {
        let object = vm.coerce_object(this)?;
        let len = length(vm, object.clone())?;
        let locale_args = [arg(args, 0), arg(args, 1)];
        join_indexed_properties(vm, len, &",".into(), Some(&locale_args), |vm, index| {
            vm.get_property(object.clone(), &index.to_string())
        })
    });
    method(vm, proto.clone(), "toString", 0, |vm, this, _| {
        let join = vm.get_property(this.clone(), "join")?;
        if vm.is_callable(&join) {
            vm.invoke(join, this, &[])
        } else {
            let method = vm.get_property(Value::Object(vm.realm.object_prototype), "toString")?;
            vm.invoke(method, this, &[])
        }
    });
    method(vm, proto.clone(), "slice", 2, |vm, this, a| {
        let object = vm.coerce_object(this)?;
        let len = array_like_length(vm, object.clone())?;
        let start = relative_index(integer(vm, arg(a, 0))?, len);
        let end = if arg(a, 1).is_undefined() {
            len
        } else {
            relative_index(integer(vm, arg(a, 1))?, len)
        };
        let count = end.saturating_sub(start);
        let array = vm.array_species_create(object.clone(), count)?;
        for i in 0..count {
            vm.tick()?;
            let key = JsString::from((start + i).to_string());
            if vm.has(&object, &key)? {
                let v = vm.get(object.clone(), &key)?;
                vm.define_data_property(array.clone(), &i.to_string(), v, true, true, true)?;
            }
        }
        vm.set_property(array.clone(), "length", Value::from(count as f64))?;
        Ok(array)
    });
    method(vm, proto.clone(), "concat", 1, |vm, this, a| {
        let target = vm.coerce_object(this)?;
        vm.ensure_allocation(
            a.len()
                .saturating_add(1)
                .saturating_mul(std::mem::size_of::<Value>()),
        )?;
        let array = vm.array_species_create(target.clone(), 0)?;
        let mut sources = vec![target];
        sources.extend_from_slice(a);
        let mut offset = 0;
        for source in sources {
            let spread = if source.as_object().is_some() {
                let spread =
                    vm.get_property_key(source.clone(), vm.well_known_key("isConcatSpreadable"))?;
                if spread.is_undefined() {
                    vm.is_array_exotic(&source)?
                } else {
                    spread.is_truthy()
                }
            } else {
                false
            };
            if spread {
                let len = length(vm, source.clone())?;
                if offset + len > vm.limits.max_array_length as usize {
                    return Err(JsError::new(
                        ErrorKind::ExecutionLimit,
                        "Array length budget exhausted",
                    ));
                }
                for i in 0..len {
                    vm.tick()?;
                    let key = JsString::from(i.to_string());
                    if vm.has(&source, &key)? {
                        let v = vm.get(source.clone(), &key)?;
                        vm.define_data_property(
                            array.clone(),
                            &(offset + i).to_string(),
                            v,
                            true,
                            true,
                            true,
                        )?;
                    }
                }
                offset += len;
            } else {
                if offset >= vm.limits.max_array_length as usize {
                    return Err(JsError::new(
                        ErrorKind::ExecutionLimit,
                        "Array concat length budget exhausted",
                    ));
                }
                vm.define_data_property(
                    array.clone(),
                    &offset.to_string(),
                    source,
                    true,
                    true,
                    true,
                )?;
                offset += 1;
            }
        }
        vm.set_property(array.clone(), "length", Value::from(offset))?;
        Ok(array)
    });
    for (name, includes) in [("indexOf", false), ("includes", true)] {
        method(vm, proto.clone(), name, 1, move |vm, this, a| {
            let object = vm.coerce_object(this)?;
            let len = array_like_length(vm, object.clone())?;
            if len == 0 {
                return Ok(if includes {
                    Value::Bool(false)
                } else {
                    Value::from(-1)
                });
            }
            let start = relative_index(integer(vm, arg(a, 1))?, len);
            let needle = arg(a, 0);
            for i in start..len {
                vm.tick()?;
                let key = JsString::from(i.to_string());
                if includes || vm.has(&object, &key)? {
                    let v = vm.get(object.clone(), &key)?;
                    vm.charge_bigint_comparison(&v, &needle)?;
                    if v == needle
                        || (includes
                            && matches!((&v,&needle),(Value::Number(a),Value::Number(b))if a.is_nan()&&b.is_nan()))
                    {
                        return Ok(if includes {
                            Value::Bool(true)
                        } else {
                            Value::from(i as f64)
                        });
                    }
                }
            }
            Ok(if includes {
                Value::Bool(false)
            } else {
                Value::from(-1)
            })
        });
    }
    array_iteration::install(vm, &proto);
    array_methods::install(vm, &proto);
    method(vm, proto, "reverse", 0, |vm, this, _| {
        let object = vm.coerce_object(this)?;
        let len = array_like_length(vm, object.clone())?;
        for i in 0..len / 2 {
            vm.tick()?;
            let left = JsString::from(i.to_string());
            let right = JsString::from((len - i - 1).to_string());
            let lower = if vm.has(&object, &left)? {
                Some(vm.get(object.clone(), &left)?)
            } else {
                None
            };
            let upper = if vm.has(&object, &right)? {
                Some(vm.get(object.clone(), &right)?)
            } else {
                None
            };
            match (lower, upper) {
                (Some(a), Some(b)) => {
                    vm.set(object.clone(), &left, b, true)?;
                    vm.set(object.clone(), &right, a, true)?;
                }
                (None, Some(b)) => {
                    vm.set(object.clone(), &left, b, true)?;
                    vm.delete(object.clone(), &right, true)?;
                }
                (Some(a), None) => {
                    vm.delete(object.clone(), &left, true)?;
                    vm.set(object.clone(), &right, a, true)?;
                }
                (None, None) => {}
            }
        }
        Ok(object)
    });
}
fn string_this(vm: &mut Vm, this: Value) -> Result<JsString, JsError> {
    if this.is_nullish() {
        return Err(JsError::type_error(
            "String method called on null or undefined",
        ));
    }
    if let Some(id) = this.as_object()
        && let ObjectKind::Boxed(Value::String(s)) = &vm.heap.object(id)?.kind
    {
        return Ok(s.clone());
    }
    vm.to_string(&this)
}
fn equal_units(vm: &mut Vm, a: &[u16], b: &[u16]) -> Result<bool, JsError> {
    if a.len() != b.len() {
        return Ok(false);
    }
    for (a, b) in a.iter().zip(b) {
        vm.tick()?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}
pub(crate) fn find_units(
    vm: &mut Vm,
    haystack: &[u16],
    needle: &[u16],
    start: usize,
) -> Result<Option<usize>, JsError> {
    if start > haystack.len() {
        return Ok(None);
    }
    if needle.is_empty() {
        return Ok(Some(start));
    }
    if needle.len() > haystack.len() - start {
        return Ok(None);
    }
    if let [unit] = needle {
        const CHUNK: usize = 256;
        for (block, units) in haystack[start..].chunks(CHUNK).enumerate() {
            let found = units.iter().position(|candidate| candidate == unit);
            vm.charge_buffer_work(found.map_or(units.len(), |index| index + 1))?;
            if let Some(index) = found {
                return Ok(Some(start + block * CHUNK + index));
            }
        }
        return Ok(None);
    }
    for i in start..=haystack.len() - needle.len() {
        vm.tick()?;
        if equal_units(vm, &haystack[i..i + needle.len()], needle)? {
            return Ok(Some(i));
        }
    }
    Ok(None)
}
pub(crate) fn join_indexed_properties(
    vm: &mut Vm,
    length: usize,
    separator: &JsString,
    locale_args: Option<&[Value; 2]>,
    mut get_element: impl FnMut(&mut Vm, usize) -> Result<Value, JsError>,
) -> Result<Value, JsError> {
    let mut units = Vec::new();
    for index in 0..length {
        vm.tick()?;
        if index > 0 {
            append_units(vm, &mut units, separator.as_utf16())?;
        }
        let mut value = get_element(vm, index)?;
        if value.is_nullish() {
            continue;
        }
        if let Some(args) = locale_args {
            let formatter = vm.get_property(value.clone(), "toLocaleString")?;
            value = vm.invoke(formatter, value, args)?;
        }
        let string = vm.to_string(&value)?;
        append_units(vm, &mut units, string.as_utf16())?;
    }
    vm.finish_string(units)
}

fn append_units(vm: &mut Vm, out: &mut Vec<u16>, units: &[u16]) -> Result<(), JsError> {
    vm.tick()?;
    vm.ensure_string_allocation(out.len().saturating_add(units.len()))?;
    vm.consume_work(units.len())?;
    out.extend_from_slice(units);
    Ok(())
}

fn string_normalize(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    use icu_normalizer::{
        ComposingNormalizerBorrowed as Compose, DecomposingNormalizerBorrowed as Decompose,
    };
    let string = string_this(vm, this)?;
    let form = match args.first() {
        None | Some(Value::Undefined) => JsString::from("NFC"),
        Some(value) => vm.to_string(value)?,
    };
    enum Form {
        Compose(Compose<'static>),
        Decompose(Decompose<'static>),
    }
    let form = match form.to_string_lossy().as_str() {
        "NFC" => Form::Compose(Compose::new_nfc()),
        "NFD" => Form::Decompose(Decompose::new_nfd()),
        "NFKC" => Form::Compose(Compose::new_nfkc()),
        "NFKD" => Form::Decompose(Decompose::new_nfkd()),
        _ => {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "Invalid normalization form",
            ));
        }
    };
    let units = string.as_utf16();
    vm.consume_work(units.len())?;
    if units.iter().all(|&unit| unit < 128) {
        return Ok(Value::String(string));
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut index = 0;
    // Normalization applies to scalar runs. Unpaired UTF-16 surrogates are
    // retained verbatim by ECMAScript, while ICU's lossy UTF-16 input replaces them.
    while index <= units.len() {
        if let Some(&unit) = units.get(index) {
            if (0xd800..=0xdbff).contains(&unit)
                && units
                    .get(index + 1)
                    .is_some_and(|trail| (0xdc00..=0xdfff).contains(trail))
            {
                index += 2;
                continue;
            }
            if !(0xd800..=0xdfff).contains(&unit) {
                index += 1;
                continue;
            }
        }
        let scalars = char::decode_utf16(units[start..index].iter().copied())
            .map(|value| value.expect("validated UTF-16 run"));
        let normalized: Box<dyn Iterator<Item = char> + '_> = match &form {
            Form::Compose(normalizer) => Box::new(normalizer.normalize_iter(scalars)),
            Form::Decompose(normalizer) => Box::new(normalizer.normalize_iter(scalars)),
        };
        for ch in normalized {
            append_units(vm, &mut out, ch.encode_utf16(&mut [0; 2]))?;
        }
        if let Some(unit) = units.get(index) {
            append_units(vm, &mut out, std::slice::from_ref(unit))?;
        }
        index += 1;
        start = index;
    }
    vm.finish_string(out)
}

// None is Case_Ignorable; otherwise the result records whether the point is Cased.
fn sigma_context(c: char) -> Option<bool> {
    // Match the Unicode version of Rust 1.99.1's scalar case mappings.
    // Each entry packs a range's starting code point and its two-bit state.
    const RANGES: &[[u8; 4]] = include_bytes!("string_case_data/context-17.0.0.bin")
        .as_chunks::<4>()
        .0;
    let after = RANGES.partition_point(|entry| (u32::from_le_bytes(*entry) >> 2) <= c as u32);
    match u32::from_le_bytes(RANGES[after - 1]) & 3 {
        0 => Some(false),
        1 => Some(true),
        2 => None,
        _ => unreachable!("invalid generated casing context"),
    }
}

fn string_case(vm: &mut Vm, this: Value, upper: bool) -> Result<Value, JsError> {
    if this.is_nullish() {
        return Err(JsError::type_error(
            "String method called on null or undefined",
        ));
    }
    let s = vm.to_string(&this)?;
    let mut units = Vec::new();
    let mut previous_cased = false;
    let mut pending_final_sigma = None;
    for decoded in char::decode_utf16(s.as_utf16().iter().copied()) {
        vm.tick()?;
        let c = match decoded {
            Ok(c) => c,
            Err(error) => {
                // An unpaired surrogate is neither cased nor case-ignorable.
                pending_final_sigma = None;
                previous_cased = false;
                append_units(vm, &mut units, &[error.unpaired_surrogate()])?;
                continue;
            }
        };
        let mut buffer = [0; 2];
        if upper {
            for mapped in c.to_uppercase() {
                append_units(vm, &mut units, mapped.encode_utf16(&mut buffer))?;
            }
        } else {
            // Final_Sigma uses the nearest non-ignorable code point on each
            // side in the original string. Keep a candidate in the output
            // until its right context is known, without rescanning the input.
            let preceded_by_cased = previous_cased;
            if let Some(current_cased) = sigma_context(c) {
                previous_cased = current_cased;
                if let Some(index) = pending_final_sigma.take()
                    && previous_cased
                {
                    units[index] = 0x03c3; // ordinary small sigma
                }
            }
            if c == '\u{03a3}' && preceded_by_cased {
                pending_final_sigma = Some(units.len());
                append_units(vm, &mut units, &[0x03c2])?; // final small sigma
            } else {
                for mapped in c.to_lowercase() {
                    append_units(vm, &mut units, mapped.encode_utf16(&mut buffer))?;
                }
            }
        }
    }
    vm.finish_string(units)
}

fn install_string(vm: &mut Vm) {
    let proto = Value::Object(vm.realm.string_prototype);
    let string = constructor(vm, "String", 1, proto.clone(), |vm, _, a| {
        Ok(Value::String(if a.is_empty() {
            JsString::default()
        } else if matches!(a[0], Value::Symbol(_)) {
            vm.descriptive_symbol(a[0].clone())?
        } else {
            vm.to_string(&a[0])?
        }))
    });
    replace_construct(vm, string.clone(), |vm, _, a, target| {
        let string = if a.is_empty() {
            JsString::default()
        } else {
            vm.to_string(&a[0])?
        };
        let prototype = vm.prototype_from_constructor(target, vm.realm.string_prototype)?;
        vm.allocate_constructed(ObjectKind::Boxed(Value::String(string)), prototype)
    });
    method(vm, string.clone(), "raw", 1, |vm, _, args| {
        let cooked = vm.coerce_object(arg(args, 0))?;
        let raw = vm.get_property(cooked, "raw")?;
        let raw = vm.coerce_object(raw)?;
        let count = length(vm, raw.clone())?;
        let mut out = Vec::new();
        for index in 0..count {
            vm.tick()?;
            let value = vm.get_property(raw.clone(), &index.to_string())?;
            let value = vm.to_string(&value)?;
            append_units(vm, &mut out, value.as_utf16())?;
            if index + 1 < count && index + 1 < args.len() {
                let value = vm.to_string(&args[index + 1])?;
                append_units(vm, &mut out, value.as_utf16())?;
            }
        }
        vm.finish_string(out)
    });
    method(vm, string.clone(), "fromCharCode", 1, |vm, _, a| {
        vm.ensure_string_allocation(a.len())?;
        vm.consume_work(a.len())?;
        let mut units = Vec::with_capacity(a.len());
        for value in a {
            units.push(to_u32(vm.to_number(value)?) as u16);
        }
        vm.finish_written_string(units)
    });
    method(vm, string, "fromCodePoint", 1, |vm, _, a| {
        vm.ensure_string_allocation(a.len())?;
        let mut units = Vec::with_capacity(a.len());
        for value in a {
            let n = vm.to_number(value)?;
            vm.tick()?;
            if !n.is_finite() || n.fract() != 0.0 || !(0.0..=0x10ffff as f64).contains(&n) {
                return Err(JsError::new(ErrorKind::RangeError, "Invalid code point"));
            }
            let n = n as u32;
            if n <= 0xffff {
                units.push(n as u16);
            } else {
                // The per-argument tick covers the first unit.
                vm.consume_work(1)?;
                let n = n - 0x10000;
                units.push((0xd800 + (n >> 10)) as u16);
                units.push((0xdc00 + (n & 1023)) as u16);
            }
        }
        vm.finish_written_string(units)
    });
    for name in ["toString", "valueOf"] {
        method(vm, proto.clone(), name, 0, |vm, this, _| match this {
            Value::String(_) => Ok(this),
            Value::Object(id) => match &vm.heap.object(id)?.kind {
                ObjectKind::Boxed(Value::String(s)) => Ok(Value::String(s.clone())),
                _ => Err(JsError::type_error("String receiver is not a string")),
            },
            _ => Err(JsError::type_error("String receiver is not a string")),
        });
    }
    for name in ["charAt", "charCodeAt", "codePointAt", "at"] {
        method(vm, proto.clone(), name, 1, move |vm, this, a| {
            let s = string_this(vm, this)?;
            let mut index = integer(vm, arg(a, 0))?;
            if name == "at" && index < 0.0 {
                index += s.len() as f64;
            }
            if index < 0.0 || index >= s.len() as f64 {
                return Ok(match name {
                    "charCodeAt" => Value::Number(f64::NAN),
                    "charAt" => Value::from(""),
                    _ => Value::Undefined,
                });
            }
            let index = index as usize;
            let unit = s.as_utf16()[index];
            if matches!(name, "charAt" | "at") {
                return vm.copy_string(&[unit]);
            }
            let mut code = unit as u32;
            if name == "codePointAt"
                && (0xd800..=0xdbff).contains(&unit)
                && let Some(low) = s.as_utf16().get(index + 1)
                && (0xdc00..=0xdfff).contains(low)
            {
                code = 0x10000 + (((unit as u32) - 0xd800) << 10) + (*low as u32 - 0xdc00);
            }
            Ok(Value::from(code))
        });
    }
    for name in ["slice", "substring", "substr"] {
        method(vm, proto.clone(), name, 2, move |vm, this, a| {
            let s = string_this(vm, this)?;
            let start = integer(vm, arg(a, 0))?;
            let mut start = if name == "substring" {
                start.max(0.0).min(s.len() as f64) as usize
            } else {
                relative(start, s.len())
            };
            let mut end = if arg(a, 1).is_undefined() {
                s.len()
            } else {
                let end = integer(vm, arg(a, 1))?;
                match name {
                    "substring" => end.max(0.0).min(s.len() as f64) as usize,
                    "substr" => start
                        .saturating_add(end.max(0.0).min(s.len() as f64) as usize)
                        .min(s.len()),
                    _ => relative(end, s.len()),
                }
            };
            if name == "substring" && end < start {
                std::mem::swap(&mut start, &mut end);
            }
            end = end.max(start);
            vm.copy_string(&s.as_utf16()[start..end])
        });
    }
    for name in [
        "indexOf",
        "includes",
        "startsWith",
        "endsWith",
        "lastIndexOf",
    ] {
        method(vm, proto.clone(), name, 1, move |vm, this, a| {
            if this.is_nullish() {
                return Err(JsError::type_error(
                    "String method called on null or undefined",
                ));
            }
            let s = vm.to_string(&this)?;
            let needle = arg(a, 0);
            if matches!(name, "startsWith" | "endsWith" | "includes") {
                crate::regexp::reject_regexp(vm, &needle)?;
            }
            let needle = vm.to_string(&needle)?;
            let pos = arg(a, 1);
            let position = if name == "lastIndexOf" {
                let n = vm.to_number(&pos)?;
                if n.is_nan() { s.len() } else { n.clamp(0.0, s.len() as f64) as usize }
            } else if name == "endsWith" && pos.is_undefined() {
                s.len()
            } else {
                integer(vm, pos)?.clamp(0.0, s.len() as f64) as usize
            };
            let found = match name {
                "startsWith" => {
                    if let Some(units) = s
                        .as_utf16()
                        .get(position..position.saturating_add(needle.len()))
                    {
                        equal_units(vm, units, needle.as_utf16())?.then_some(position)
                    } else {
                        None
                    }
                }
                "endsWith" => {
                    if let Some(start) = position.checked_sub(needle.len()) {
                        equal_units(vm, &s.as_utf16()[start..position], needle.as_utf16())?
                            .then_some(start)
                    } else {
                        None
                    }
                }
                "lastIndexOf" => {
                    if needle.is_empty() {
                        Some(position)
                    } else {
                        let mut found = None;
                        if needle.len() <= s.len() {
                            for i in (0..=position.min(s.len() - needle.len())).rev() {
                                vm.tick()?;
                                if equal_units(
                                    vm,
                                    &s.as_utf16()[i..i + needle.len()],
                                    needle.as_utf16(),
                                )? {
                                    found = Some(i);
                                    break;
                                }
                            }
                        }
                        found
                    }
                }
                _ => find_units(vm, s.as_utf16(), needle.as_utf16(), position)?,
            };
            Ok(if matches!(name, "indexOf" | "lastIndexOf") {
                Value::Number(found.map(|i| i as f64).unwrap_or(-1.0))
            } else {
                Value::Bool(found.is_some())
            })
        });
    }
    method(vm, proto.clone(), "split", 2, |vm, this, a| {
        let separator = arg(a, 0);
        let limit = arg(a, 1);
        if let Some(value) =
            crate::regexp::string_hook(vm, &this, &separator, "split", Some(&limit), false)?
        {
            return Ok(value);
        }
        let s = vm.to_string(&this)?;
        let limit = if limit.is_undefined() {
            u32::MAX
        } else {
            to_u32(vm.to_number(&limit)?)
        } as usize;
        let delimiter = vm.to_string(&separator)?;
        if limit == 0 {
            return vm.try_new_array(&[]);
        }
        if separator.is_undefined() {
            return vm.try_new_array(&[Value::String(s)]);
        }
        let result = vm.try_new_array(&[])?;
        if delimiter.is_empty() {
            let requested = s.len().min(limit);
            if requested > vm.limits.max_array_length as usize {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Split array length budget exhausted",
                ));
            }
            vm.ensure_allocation(requested.saturating_mul(320))?;
            for unit in s.as_utf16().iter().take(limit) {
                vm.tick()?;
                let piece = vm.copy_string(&[*unit])?;
                vm.append_array_literal(result.clone(), Some(piece))?;
            }
        } else {
            let mut start = 0;
            for _ in 0..limit {
                vm.tick()?;
                let end = find_units(vm, s.as_utf16(), delimiter.as_utf16(), start)?;
                let piece = vm.copy_string(&s.as_utf16()[start..end.unwrap_or(s.len())])?;
                vm.append_array_literal(result.clone(), Some(piece))?;
                if let Some(end) = end {
                    start = end + delimiter.len();
                } else {
                    break;
                }
            }
        }
        Ok(result)
    });
    method(vm, proto.clone(), "concat", 1, |vm, this, a| {
        let s = string_this(vm, this)?;
        vm.ensure_string_allocation(s.len())?;
        vm.consume_work(s.len())?;
        let mut units = s.as_utf16().to_vec();
        for value in a {
            let s = vm.to_string(value)?;
            if units.len().saturating_add(s.len()) > vm.limits.max_string_units {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "String code-unit budget exhausted",
                ));
            }
            append_units(vm, &mut units, s.as_utf16())?;
        }
        vm.finish_string(units)
    });
    method(vm, proto.clone(), "normalize", 0, string_normalize);
    for (name, upper) in [
        ("toLowerCase", false),
        ("toUpperCase", true),
    ] {
        method(vm, proto.clone(), name, 0, move |vm, this, _| {
            string_case(vm, this, upper)
        });
    }
    for name in ["trim", "trimStart", "trimEnd"] {
        method(vm, proto.clone(), name, 0, move |vm, this, _| {
            let s = string_this(vm, this)?;
            let units = s.as_utf16();
            let mut start = 0;
            let mut end = units.len();
            if name != "trimEnd" {
                while start < end && js_whitespace(units[start]) {
                    vm.tick()?;
                    start += 1;
                }
            }
            if name != "trimStart" {
                while end > start && js_whitespace(units[end - 1]) {
                    vm.tick()?;
                    end -= 1;
                }
            }
            vm.copy_string(&units[start..end])
        });
    }
    for (name, repair) in [("isWellFormed", false), ("toWellFormed", true)] {
        method(vm, proto.clone(), name, 0, move |vm, this, _| {
            if this.is_nullish() {
                return Err(JsError::type_error("String method requires a receiver"));
            }
            let text = vm.to_string(&this)?;
            let units = text.as_utf16();
            vm.consume_work(units.len())?;
            if char::decode_utf16(units.iter().copied()).all(|c| c.is_ok()) {
                return Ok(if repair {
                    Value::String(text)
                } else {
                    true.into()
                });
            }
            if !repair {
                return Ok(false.into());
            }
            vm.ensure_string_allocation(units.len())?;
            let mut out = Vec::with_capacity(units.len());
            for c in char::decode_utf16(units.iter().copied()) {
                out.extend_from_slice(
                    c.unwrap_or(char::REPLACEMENT_CHARACTER)
                        .encode_utf16(&mut [0; 2]),
                );
            }
            vm.finish_string(out)
        });
    }
    method(vm, proto.clone(), "repeat", 1, |vm, this, a| {
        let s = string_this(vm, this)?;
        let count = integer(vm, arg(a, 0))?;
        if count < 0.0 || !count.is_finite() {
            return Err(JsError::new(ErrorKind::RangeError, "Invalid repeat count"));
        }
        let count = count as usize;
        if s.len()
            .checked_mul(count)
            .is_none_or(|len| len > vm.limits.max_string_units)
        {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "String code-unit budget exhausted",
            ));
        }
        vm.ensure_string_allocation(s.len().saturating_mul(count))?;
        vm.consume_work(s.len().saturating_mul(count))?;
        vm.finish_written_string(s.as_utf16().repeat(count))
    });
    for (name, start) in [("padStart", true), ("padEnd", false)] {
        method(vm, proto.clone(), name, 1, move |vm, this, a| {
            let s = string_this(vm, this)?;
            let target = integer(vm, arg(a, 0))?.max(0.0);
            if target > vm.limits.max_string_units as f64 {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "String code-unit budget exhausted",
                ));
            }
            let target = target as usize;
            if target <= s.len() {
                return Ok(Value::String(s));
            }
            let pad = if arg(a, 1).is_undefined() {
                JsString::from(" ")
            } else {
                vm.to_string(&arg(a, 1))?
            };
            if pad.is_empty() {
                return Ok(Value::String(s));
            }
            let missing = target - s.len();
            vm.ensure_string_allocation(target)?;
            vm.consume_work(target)?;
            let mut units = Vec::with_capacity(target);
            if !start {
                units.extend_from_slice(s.as_utf16());
            }
            for i in 0..missing {
                units.push(pad.as_utf16()[i % pad.len()]);
            }
            if start {
                units.extend_from_slice(s.as_utf16());
            }
            vm.finish_string(units)
        });
    }
}
pub(crate) fn js_whitespace(c: u16) -> bool {
    matches!(c,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)
}
fn install_number(vm: &mut Vm) {
    let proto = Value::Object(vm.realm.number_prototype);
    let number = constructor(vm, "Number", 1, proto.clone(), |vm, _, a| {
        Ok(Value::Number(if a.is_empty() {
            0.0
        } else {
            vm.number_constructor_value(&a[0])?
        }))
    });
    replace_construct(vm, number.clone(), |vm, _, a, target| {
        let number = if a.is_empty() {
            0.0
        } else {
            vm.number_constructor_value(&a[0])?
        };
        let prototype = vm.prototype_from_constructor(target, vm.realm.number_prototype)?;
        vm.allocate_constructed(ObjectKind::Boxed(Value::Number(number)), prototype)
    });
    for (name, value) in [
        ("NaN", f64::NAN),
        ("POSITIVE_INFINITY", f64::INFINITY),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", f64::from_bits(1)),
        ("MAX_SAFE_INTEGER", 9007199254740991.0),
        ("MIN_SAFE_INTEGER", -9007199254740991.0),
        ("EPSILON", f64::EPSILON),
    ] {
        vm.define_data_property(
            number.clone(),
            name,
            Value::Number(value),
            false,
            false,
            false,
        )
        .unwrap();
    }
    for name in ["isNaN", "isFinite", "isInteger", "isSafeInteger"] {
        method(vm, number.clone(), name, 1, move |_, _, a| {
            let yes = if let Value::Number(n) = arg(a, 0) {
                match name {
                    "isNaN" => n.is_nan(),
                    "isFinite" => n.is_finite(),
                    "isInteger" => n.is_finite() && n.fract() == 0.0,
                    _ => n.is_finite() && n.fract() == 0.0 && n.abs() <= 9007199254740991.0,
                }
            } else {
                false
            };
            Ok(Value::Bool(yes))
        });
    }
    method(vm, proto.clone(), "valueOf", 0, |vm, this, _| {
        number_this(vm, this).map(Value::Number)
    });
    method(vm, proto.clone(), "toLocaleString", 0, |vm, this, _| {
        Ok(crate::vm::number_string(number_this(vm, this)?).into())
    });
    method(vm, proto.clone(), "toString", 1, |vm, this, a| {
        let n = number_this(vm, this)?;
        let radix = if arg(a, 0).is_undefined() {
            10
        } else {
            integer(vm, arg(a, 0))? as i32
        };
        if !(2..=36).contains(&radix) {
            return Err(JsError::new(ErrorKind::RangeError, "Invalid radix"));
        }
        if radix == 10 || !n.is_finite() {
            return Ok(Value::from(crate::vm::number_string(n)));
        }
        Ok(number_format::non_decimal(n, radix as u32).into())
    });
    number_format::install(vm, proto);
    let boolean_proto = Value::Object(vm.realm.boolean_prototype);
    let boolean = constructor(vm, "Boolean", 1, boolean_proto.clone(), |_, _, a| {
        Ok(Value::Bool(arg(a, 0).is_truthy()))
    });
    replace_construct(vm, boolean, |vm, _, a, target| {
        let prototype = vm.prototype_from_constructor(target, vm.realm.boolean_prototype)?;
        vm.allocate_constructed(
            ObjectKind::Boxed(Value::Bool(arg(a, 0).is_truthy())),
            prototype,
        )
    });
    method(vm, boolean_proto.clone(), "valueOf", 0, |vm, this, _| {
        bool_this(vm, this).map(Value::Bool)
    });
    method(vm, boolean_proto, "toString", 0, |vm, this, _| {
        Ok(Value::from(if bool_this(vm, this)? {
            "true"
        } else {
            "false"
        }))
    });
    let global = vm.global();
    method(vm, global.clone(), "isNaN", 1, |vm, _, a| {
        Ok(Value::Bool(vm.to_number(&arg(a, 0))?.is_nan()))
    });
    method(vm, global.clone(), "isFinite", 1, |vm, _, a| {
        Ok(Value::Bool(vm.to_number(&arg(a, 0))?.is_finite()))
    });
    let parse_int = method(vm, global.clone(), "parseInt", 2, |vm, _, a| {
        let text = vm.to_string(&arg(a, 0))?;
        let text = text.to_string_lossy();
        let text = text.trim_start_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}');
        let (negative, mut text) = if let Some(s) = text.strip_prefix('-') {
            (true, s)
        } else {
            (false, text.strip_prefix('+').unwrap_or(text))
        };
        let mut radix = to_i32(vm.to_number(&arg(a, 1))?);
        if radix != 0 && !(2..=36).contains(&radix) {
            return Ok(Value::Number(f64::NAN));
        }
        if (radix == 0 || radix == 16) && (text.starts_with("0x") || text.starts_with("0X")) {
            text = &text[2..];
            radix = 16;
        }
        if radix == 0 {
            radix = 10;
        }
        let mut value = 0f64;
        let mut any = false;
        for c in text.chars() {
            let Some(d) = c.to_digit(radix as u32) else {
                break;
            };
            value = value * radix as f64 + d as f64;
            any = true;
        }
        Ok(Value::Number(if !any {
            f64::NAN
        } else if negative {
            -value
        } else {
            value
        }))
    });
    vm.define_data_property(number.clone(), "parseInt", parse_int, true, false, true)
        .unwrap();
    let parse_float = method(vm, global, "parseFloat", 1, |vm, _, a| {
        let text = vm.to_string(&arg(a, 0))?;
        let text = text.to_string_lossy();
        let text = text.trim_start_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}');
        let sign = if text.starts_with(['+', '-']) { 1 } else { 0 };
        if text[sign..].starts_with("Infinity") {
            return Ok(Value::Number(if text.starts_with('-') {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            }));
        }
        let bytes = text.as_bytes();
        let mut i = sign;
        let mut digits = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
        if bytes.get(i) == Some(&b'.') {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            return Ok(Value::Number(f64::NAN));
        }
        let before = i;
        if matches!(bytes.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(bytes.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            let exp = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == exp {
                i = before;
            }
        }
        Ok(Value::Number(text[..i].parse().unwrap_or(f64::NAN)))
    });
    vm.define_data_property(number, "parseFloat", parse_float, true, false, true)
        .unwrap();
}
fn number_this(vm: &Vm, this: Value) -> Result<f64, JsError> {
    match this {
        Value::Number(n) => Ok(n),
        Value::Object(id) => match vm.heap.object(id)?.kind {
            ObjectKind::Boxed(Value::Number(n)) => Ok(n),
            _ => Err(JsError::type_error("Number receiver is not a number")),
        },
        _ => Err(JsError::type_error("Number receiver is not a number")),
    }
}
fn bool_this(vm: &Vm, this: Value) -> Result<bool, JsError> {
    match this {
        Value::Bool(n) => Ok(n),
        Value::Object(id) => match vm.heap.object(id)?.kind {
            ObjectKind::Boxed(Value::Bool(n)) => Ok(n),
            _ => Err(JsError::type_error("Boolean receiver is not a boolean")),
        },
        _ => Err(JsError::type_error("Boolean receiver is not a boolean")),
    }
}
fn install_math(vm: &mut Vm) {
    let math = vm.new_object();
    vm.define_data_property(vm.global(), "Math", math.clone(), true, false, true)
        .unwrap();
    for (name, n) in [
        ("E", std::f64::consts::E),
        ("PI", std::f64::consts::PI),
        ("LN2", std::f64::consts::LN_2),
        ("LN10", std::f64::consts::LN_10),
        ("LOG2E", std::f64::consts::LOG2_E),
        ("LOG10E", std::f64::consts::LOG10_E),
        ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
        ("SQRT2", std::f64::consts::SQRT_2),
    ] {
        vm.define_data_property(math.clone(), name, Value::Number(n), false, false, false)
            .unwrap();
    }
    math::install(vm, math.clone());
    for name in [
        "abs", "floor", "ceil", "round", "trunc", "sqrt", "cbrt", "sin", "cos", "tan", "asin",
        "acos", "atan", "exp", "expm1", "log", "log2", "log10", "log1p", "sign", "fround", "clz32",
    ] {
        method(vm, math.clone(), name, 1, move |vm, _, a| {
            let n = vm.to_number(&arg(a, 0))?;
            Ok(Value::Number(match name {
                "abs" => n.abs(),
                "floor" => n.floor(),
                "ceil" => n.ceil(),
                "round" => {
                    if n == 0.0 || !n.is_finite() || n.fract() == 0.0 {
                        n
                    } else if (-0.5..0.0).contains(&n) {
                        -0.0
                    } else {
                        let floor = n.floor();
                        if n - floor < 0.5 {
                            floor
                        } else {
                            floor + 1.0
                        }
                    }
                }
                "trunc" => n.trunc(),
                "sqrt" => n.sqrt(),
                "cbrt" => n.cbrt(),
                "sin" => n.sin(),
                "cos" => n.cos(),
                "tan" => n.tan(),
                "asin" => n.asin(),
                "acos" => n.acos(),
                "atan" => n.atan(),
                "exp" => n.exp(),
                "expm1" => n.exp_m1(),
                "log" => n.ln(),
                "log2" => n.log2(),
                "log10" => n.log10(),
                "log1p" => n.ln_1p(),
                "sign" => {
                    if n == 0.0 || n.is_nan() {
                        n
                    } else if n > 0.0 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                "fround" => (n as f32) as f64,
                _ => to_u32(n).leading_zeros() as f64,
            }))
        });
    }
    for name in ["pow", "atan2", "imul"] {
        method(vm, math.clone(), name, 2, move |vm, _, a| {
            let x = vm.to_number(&arg(a, 0))?;
            let y = vm.to_number(&arg(a, 1))?;
            Ok(Value::Number(match name {
                "pow" => math::pow(x, y),
                "atan2" => x.atan2(y),
                _ => to_i32(x).wrapping_mul(to_i32(y)) as f64,
            }))
        });
    }
    for (name, max) in [("min", false), ("max", true)] {
        method(vm, math.clone(), name, 2, move |vm, _, a| {
            let mut result = if max {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
            for value in a {
                let n = vm.to_number(value)?;
                if n.is_nan() || result.is_nan() {
                    result = f64::NAN;
                    continue;
                }
                result = if max {
                    if n == 0.0 && result == 0.0 {
                        if n.is_sign_positive() || result.is_sign_positive() {
                            0.0
                        } else {
                            -0.0
                        }
                    } else {
                        result.max(n)
                    }
                } else if n == 0.0 && result == 0.0 {
                    if n.is_sign_negative() || result.is_sign_negative() {
                        -0.0
                    } else {
                        0.0
                    }
                } else {
                    result.min(n)
                };
            }
            Ok(Value::Number(result))
        });
    }
    let seed = std::rc::Rc::new(std::cell::Cell::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x4a53)
            .max(1),
    ));
    method(vm, math, "random", 0, move |_, _, _| {
        let mut x = seed.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        seed.set(x);
        Ok(Value::Number((x >> 11) as f64 / (1u64 << 53) as f64))
    });
}
fn install_errors(vm: &mut Vm) {
    let error_proto = Value::Object(vm.realm.error_prototype);
    vm.define_data_property(
        error_proto.clone(),
        "name",
        Value::from("Error"),
        true,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property(
        error_proto.clone(),
        "message",
        Value::from(""),
        true,
        false,
        true,
    )
    .unwrap();
    let get_stack = vm.native("get stack", 0, false, Rc::new(|vm, this, _| {
        let id = this.as_object().ok_or_else(|| JsError::type_error("Stack receiver must be an object"))?;
        if let ObjectKind::Error(_, trace) = &vm.heap.object(id)?.kind {
            return Ok(Value::String(trace.clone()));
        }
        Ok(Value::Undefined)
    }));
    let set_stack = vm.native("set stack", 1, false, Rc::new(|vm, this, args| {
        let id = this.as_object().ok_or_else(|| JsError::type_error("Stack receiver must be an object"))?;
        let value = arg(args, 0);
        if id == vm.realm.error_prototype || !matches!(value, Value::String(_)) {
            return Err(JsError::type_error("Cannot assign this error stack"));
        }
        let key = PropertyKey::from("stack");
        if vm.own_descriptor_key(id, &key)?.is_some() {
            vm.set_key(this, &key, value, true)?;
        } else {
            vm.define_data_property_key(this, key, value, true, true, true)?;
        }
        Ok(Value::Undefined)
    }));
    vm.define_property(error_proto.clone(), "stack".into(), Property {
        kind: PropertyKind::Accessor { get: Some(get_stack), set: Some(set_stack) },
        configurable: true, enumerable: false,
    }).unwrap();
    method(vm, error_proto.clone(), "toString", 0, |vm, this, _| {
        if this.as_object().is_none() {
            return Err(JsError::type_error("Error.toString requires an object"));
        }
        let name = vm.get_property(this.clone(), "name")?;
        let name = if name.is_undefined() {
            JsString::from("Error")
        } else {
            vm.to_string(&name)?
        };
        let message = vm.get_property(this, "message")?;
        let message = if message.is_undefined() {
            JsString::default()
        } else {
            vm.to_string(&message)?
        };
        Ok(Value::String(if name.is_empty() {
            message
        } else if message.is_empty() {
            name
        } else {
            let length = name.len().saturating_add(message.len()).saturating_add(2);
            vm.ensure_string_allocation(length)?;
            vm.consume_work(
                name.len()
                    .saturating_add(2)
                    .saturating_add(length)
                    .saturating_mul(2),
            )?;
            name.concat(&JsString::from(": ")).concat(&message)
        }))
    });
    for kind in [
        ErrorKind::Error,
        ErrorKind::TypeError,
        ErrorKind::ReferenceError,
        ErrorKind::RangeError,
        ErrorKind::SyntaxError,
        ErrorKind::EvalError,
        ErrorKind::UriError,
    ] {
        let proto = if kind == ErrorKind::Error {
            error_proto.clone()
        } else {
            let proto = vm.new_object_with_prototype(error_proto.clone()).unwrap();
            vm.define_data_property(proto.clone(), "message", "".into(), true, false, true)
                .unwrap();
            proto
        };
        vm.realm
            .error_prototypes
            .insert(kind, proto.as_object().unwrap());
        vm.define_data_property(
            proto.clone(),
            "name",
            Value::from(kind.name()),
            true,
            false,
            true,
        )
        .unwrap();
        let constructor = constructor(vm, kind.name(), 1, proto, move |vm, _, args| {
            create_error(vm, kind, args, vm.realm.error_prototypes[&kind])
        });
        if kind == ErrorKind::Error {
            method(vm, constructor.clone(), "isError", 1, |vm, _, args| {
                let is_error = if let Some(id) = arg(args, 0).as_object() {
                    matches!(vm.heap.object(id)?.kind, ObjectKind::Error(..))
                } else { false };
                Ok(is_error.into())
            });
        } else {
            let error = vm.get_global("Error").unwrap();
            vm.set_prototype(constructor.as_object().unwrap(), error.as_object())
                .unwrap();
        }
        replace_construct(vm, constructor, move |vm, _, args, target| {
            let prototype =
                vm.prototype_from_constructor(target, vm.realm.error_prototypes[&kind])?;
            create_error(vm, kind, args, prototype)
        });
    }
}
pub(crate) fn create_error(
    vm: &mut Vm,
    kind: ErrorKind,
    args: &[Value],
    prototype: crate::ObjectId,
) -> Result<Value, JsError> {
    let error = vm.allocate_error(kind, prototype)?;
    let message = arg(args, 0);
    if !message.is_undefined() {
        let message = vm.to_string(&message)?;
        vm.define_data_property(
            error.clone(),
            "message",
            Value::String(message),
            true,
            false,
            true,
        )?;
    }
    let options = arg(args, 1);
    if options.as_object().is_some() && vm.has(&options, &"cause".into())? {
        let cause = vm.get_property(options, "cause")?;
        vm.define_data_property(error.clone(), "cause", cause, true, false, true)?;
    }
    Ok(error)
}
