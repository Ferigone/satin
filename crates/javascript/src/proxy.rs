//! Proxy exotic objects. All forwarding uses the VM's regular internal methods.
use crate::{
    ErrorKind, JsError, ObjectId, PropertyKey, Value, Vm,
    builtins::{method, replace_construct},
    heap::{ObjectKind, Property, PropertyKind},
    vm::same_value,
};
use std::{collections::BTreeSet, rc::Rc};

#[derive(Clone, Copy)]
pub(crate) struct Proxy {
    pub(crate) pair: Option<(ObjectId, ObjectId)>,
    pub(crate) callable: bool,
    pub(crate) constructor: bool,
}

fn key_value(key: &PropertyKey) -> Value {
    match key {
        PropertyKey::String(s) => Value::String(s.clone()),
        PropertyKey::Symbol(s) => Value::Symbol(s.clone()),
    }
}

fn create(vm: &mut Vm, args: &[Value]) -> Result<Value, JsError> {
    let target = args
        .first()
        .and_then(Value::as_object)
        .ok_or_else(|| JsError::type_error("Proxy target must be an object"))?;
    let handler = args
        .get(1)
        .and_then(Value::as_object)
        .ok_or_else(|| JsError::type_error("Proxy handler must be an object"))?;
    let data = Proxy {
        pair: Some((target, handler)),
        callable: vm.is_callable(&args[0]),
        constructor: vm.is_constructor(&args[0]),
    };
    vm.allocate_constructed(ObjectKind::Proxy(data), vm.realm.object_prototype)
}

pub(crate) fn install(vm: &mut Vm) {
    let ctor = vm.native(
        "Proxy",
        2,
        false,
        Rc::new(|_, _, _| Err(JsError::type_error("Proxy requires new"))),
    );
    replace_construct(vm, ctor.clone(), |vm, _, args, _| create(vm, args));
    method(vm, ctor.clone(), "revocable", 2, |vm, _, args| {
        let proxy = create(vm, args)?;
        let id = proxy.as_object().unwrap();
        let owner = Rc::new(std::cell::Cell::new(None));
        let active = owner.clone();
        let revoke = vm.new_native_function("", move |vm, _, _| {
            let Some(revoker) = active.take() else {
                return Ok(Value::Undefined);
            };
            vm.set_host_slot(Value::Object(revoker), 0, Value::Undefined)?;
            if let ObjectKind::Proxy(data) = &mut vm.heap.object_mut(id)?.kind {
                data.pair = None;
            }
            Ok(Value::Undefined)
        });
        // The callback captures the handle. Keep its strong JS edge in the traced heap.
        owner.set(revoke.as_object());
        vm.set_host_slot(revoke.clone(), 0, proxy.clone())?;
        let result = vm.new_object();
        vm.define_data_property(result.clone(), "proxy", proxy, true, true, true)?;
        vm.define_data_property(result.clone(), "revoke", revoke, true, true, true)?;
        Ok(result)
    });
    vm.define_data_property(vm.global(), "Proxy", ctor, true, false, true)
        .unwrap();
}

impl Vm {
    pub(crate) fn is_array_exotic(&self, value: &Value) -> Result<bool, JsError> {
        let Some(mut id) = value.as_object() else {
            return Ok(false);
        };
        for _ in 0..1024 {
            match self.heap.object(id)?.kind {
                ObjectKind::Array { .. } => return Ok(true),
                ObjectKind::Proxy(data) => {
                    id = data
                        .pair
                        .ok_or_else(|| JsError::type_error("IsArray on revoked Proxy"))?
                        .0
                }
                _ => return Ok(false),
            }
        }
        Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Array proxy chain limit exceeded",
        ))
    }

    pub(crate) fn proxy(&self, id: ObjectId) -> bool {
        self.heap
            .object(id)
            .is_ok_and(|o| matches!(o.kind, ObjectKind::Proxy(_)))
    }

    fn proxy_trap<T>(
        &mut self,
        id: ObjectId,
        name: &str,
        op: impl FnOnce(&mut Self, ObjectId, Value, Option<Value>) -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        let ObjectKind::Proxy(proxy) = self.heap.object(id)?.kind else {
            unreachable!()
        };
        let (target, handler) = proxy
            .pair
            .ok_or_else(|| JsError::type_error("Operation on revoked Proxy"))?;
        if !self.can_enter_call() {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Proxy recursion limit exceeded",
            ));
        }
        self.consume_work(1)?;
        self.call_depth += 1;
        let handler = Value::Object(handler);
        let result = self.get_property(handler.clone(), name).and_then(|trap| {
            if trap.is_nullish() {
                op(self, target, handler, None)
            } else if self.is_callable(&trap) {
                op(self, target, handler, Some(trap))
            } else {
                Err(JsError::type_error("Proxy trap is not callable"))
            }
        });
        self.call_depth -= 1;
        result
    }

    pub(crate) fn proxy_get(
        &mut self,
        id: ObjectId,
        key: &PropertyKey,
        receiver: Value,
    ) -> Result<Value, JsError> {
        self.proxy_trap(id, "get", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.get_from(Value::Object(target), key, receiver);
            };
            let result = vm.invoke(trap, h, &[Value::Object(target), key_value(key), receiver])?;
            if let Some(Property {
                configurable: false,
                kind,
                ..
            }) = vm.own_descriptor_key(target, key)?
            {
                match kind {
                    PropertyKind::Data {
                        value,
                        writable: false,
                    } if !same_value(&value, &result) => {
                        return Err(JsError::type_error("Cannot override frozen data property"));
                    }
                    PropertyKind::Accessor { get: None, .. } if !result.is_undefined() => {
                        return Err(JsError::type_error("Property has no getter"));
                    }
                    _ => {}
                }
            }
            Ok(result)
        })
    }

    pub(crate) fn proxy_set(
        &mut self,
        id: ObjectId,
        key: &PropertyKey,
        value: Value,
        receiver: Value,
    ) -> Result<bool, JsError> {
        self.proxy_trap(id, "set", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.set_from(Value::Object(target), key, value, receiver);
            };
            if !vm
                .invoke(
                    trap,
                    h,
                    &[
                        Value::Object(target),
                        key_value(key),
                        value.clone(),
                        receiver,
                    ],
                )?
                .is_truthy()
            {
                return Ok(false);
            }
            if let Some(Property {
                configurable: false,
                kind,
                ..
            }) = vm.own_descriptor_key(target, key)?
            {
                match kind {
                    PropertyKind::Data {
                        value: old,
                        writable: false,
                    } if !same_value(&old, &value) => {
                        return Err(JsError::type_error("Cannot replace frozen data property"));
                    }
                    PropertyKind::Accessor { set: None, .. } => {
                        return Err(JsError::type_error("Property has no setter"));
                    }
                    _ => {}
                }
            }
            Ok(true)
        })
    }

    pub(crate) fn proxy_has(&mut self, id: ObjectId, key: &PropertyKey) -> Result<bool, JsError> {
        self.proxy_trap(id, "has", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.has_key(&Value::Object(target), key);
            };
            let result = vm
                .invoke(trap, h, &[Value::Object(target), key_value(key)])?
                .is_truthy();
            if !result {
                vm.proxy_may_hide(target, key)?
            }
            Ok(result)
        })
    }
    fn proxy_may_hide(&mut self, target: ObjectId, key: &PropertyKey) -> Result<(), JsError> {
        if let Some(p) = self.own_descriptor_key(target, key)?
            && (!p.configurable || !self.object_extensible(target)?)
        {
            return Err(JsError::type_error(
                "Proxy cannot hide a protected own property",
            ));
        }
        Ok(())
    }
    pub(crate) fn proxy_delete(
        &mut self,
        id: ObjectId,
        key: &PropertyKey,
    ) -> Result<bool, JsError> {
        self.proxy_trap(id, "deleteProperty", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.delete_key(Value::Object(target), key, false);
            };
            let result = vm
                .invoke(trap, h, &[Value::Object(target), key_value(key)])?
                .is_truthy();
            if result {
                vm.proxy_may_hide(target, key)?
            }
            Ok(result)
        })
    }
    pub(crate) fn object_extensible(&mut self, id: ObjectId) -> Result<bool, JsError> {
        if !self.proxy(id) {
            return Ok(self.heap.object(id)?.extensible);
        }
        self.proxy_trap(id, "isExtensible", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.object_extensible(target);
            };
            let result = vm.invoke(trap, h, &[Value::Object(target)])?.is_truthy();
            if result != vm.object_extensible(target)? {
                return Err(JsError::type_error(
                    "Proxy extensibility differs from target",
                ));
            }
            Ok(result)
        })
    }
    pub(crate) fn proxy_prevent(&mut self, id: ObjectId) -> Result<bool, JsError> {
        self.proxy_trap(id, "preventExtensions", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.prevent_extensions(target);
            };
            let result = vm.invoke(trap, h, &[Value::Object(target)])?.is_truthy();
            if result && vm.object_extensible(target)? {
                return Err(JsError::type_error("Target is still extensible"));
            }
            Ok(result)
        })
    }
    pub(crate) fn object_prototype_of(
        &mut self,
        id: ObjectId,
    ) -> Result<Option<ObjectId>, JsError> {
        if !self.proxy(id) {
            return Ok(self.heap.object(id)?.prototype);
        }
        self.proxy_trap(id, "getPrototypeOf", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.object_prototype_of(target);
            };
            let result = vm.invoke(trap, h, &[Value::Object(target)])?;
            let proto = result.as_object();
            if proto.is_none() && result != Value::Null {
                return Err(JsError::type_error(
                    "Proxy prototype must be an object or null",
                ));
            }
            if !vm.object_extensible(target)? && proto != vm.object_prototype_of(target)? {
                return Err(JsError::type_error(
                    "Wrong prototype for non-extensible target",
                ));
            }
            Ok(proto)
        })
    }
    pub(crate) fn proxy_set_proto(
        &mut self,
        id: ObjectId,
        proto: Option<ObjectId>,
    ) -> Result<bool, JsError> {
        self.proxy_trap(id, "setPrototypeOf", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.try_set_prototype(target, proto);
            };
            let result = vm
                .invoke(
                    trap,
                    h,
                    &[
                        Value::Object(target),
                        proto.map(Value::Object).unwrap_or(Value::Null),
                    ],
                )?
                .is_truthy();
            if result && !vm.object_extensible(target)? && vm.object_prototype_of(target)? != proto
            {
                return Err(JsError::type_error(
                    "Cannot change prototype of non-extensible target",
                ));
            }
            Ok(result)
        })
    }
    pub(crate) fn proxy_call(
        &mut self,
        id: ObjectId,
        this: Value,
        args: &[Value],
    ) -> Result<Value, JsError> {
        if !self.is_callable(&Value::Object(id)) {
            return Err(JsError::type_error("Proxy target is not callable"));
        }
        self.proxy_trap(id, "apply", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.invoke(Value::Object(target), this, args);
            };
            let list = vm.try_new_array(args)?;
            vm.invoke(trap, h, &[Value::Object(target), this, list])
        })
    }
    pub(crate) fn proxy_construct(
        &mut self,
        id: ObjectId,
        args: &[Value],
        new_target: Value,
    ) -> Result<Value, JsError> {
        if !self.is_constructor(&Value::Object(id)) {
            return Err(JsError::type_error("Proxy target is not a constructor"));
        }
        self.proxy_trap(id, "construct", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.construct_target(Value::Object(target), args, new_target);
            };
            let list = vm.try_new_array(args)?;
            let value = vm.invoke(trap, h, &[Value::Object(target), list, new_target])?;
            if value.as_object().is_none() {
                return Err(JsError::type_error(
                    "Proxy construct trap must return an object",
                ));
            }
            Ok(value)
        })
    }

    pub(crate) fn proxy_keys(&mut self, id: ObjectId) -> Result<Vec<PropertyKey>, JsError> {
        self.proxy_trap(id, "ownKeys", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.own_property_keys(Value::Object(target));
            };
            let list = vm.invoke(trap, h, &[Value::Object(target)])?;
            let items = crate::symbol::reflect_arguments(vm, list)?;
            let mut keys = Vec::with_capacity(items.len());
            let mut seen = BTreeSet::new();
            for v in items {
                let key = match v {
                    Value::String(s) => PropertyKey::String(s),
                    Value::Symbol(s) => PropertyKey::Symbol(s),
                    _ => {
                        return Err(JsError::type_error(
                            "Proxy ownKeys must return strings or symbols",
                        ));
                    }
                };
                if !seen.insert(key.clone()) {
                    return Err(JsError::type_error("Duplicate Proxy key"));
                }
                keys.push(key);
            }
            let extensible = vm.object_extensible(target)?;
            let target_keys = vm.own_property_keys(Value::Object(target))?;
            for key in target_keys {
                let fixed = vm
                    .own_descriptor_key(target, &key)?
                    .is_some_and(|p| !p.configurable);
                if !seen.remove(&key) && (fixed || !extensible) {
                    return Err(JsError::type_error("Missing required Proxy key"));
                }
            }
            if !extensible && !seen.is_empty() {
                return Err(JsError::type_error("Unexpected extra Proxy key"));
            }
            Ok(keys)
        })
    }
}

#[derive(Default)]
pub(crate) struct PartialDescriptor {
    enumerable: Option<bool>,
    configurable: Option<bool>,
    value: Option<Value>,
    writable: Option<bool>,
    get: Option<Value>,
    set: Option<Value>,
}

impl PartialDescriptor {
    pub(crate) fn read(vm: &mut Vm, object: Value) -> Result<Self, JsError> {
        if object.as_object().is_none() {
            return Err(JsError::type_error("Property descriptor must be an object"));
        }
        let mut field = |name: &str| -> Result<Option<Value>, JsError> {
            if vm.has(&object, &name.into())? {
                vm.get_property(object.clone(), name).map(Some)
            } else {
                Ok(None)
            }
        };
        let enumerable = field("enumerable")?.map(|v| v.is_truthy());
        let configurable = field("configurable")?.map(|v| v.is_truthy());
        let value = field("value")?;
        let writable = field("writable")?.map(|v| v.is_truthy());
        let mut accessor = |name: &str| -> Result<Option<Value>, JsError> {
            let member = if vm.has(&object, &name.into())? {
                Some(vm.get_property(object.clone(), name)?)
            } else {
                None
            };
            if member
                .as_ref()
                .is_some_and(|v| !v.is_undefined() && !vm.is_callable(v))
            {
                return Err(JsError::type_error(
                    "Accessor must be callable or undefined",
                ));
            }
            Ok(member)
        };
        let get = accessor("get")?;
        let set = accessor("set")?;
        if (get.is_some() || set.is_some()) && (value.is_some() || writable.is_some()) {
            return Err(JsError::type_error(
                "Descriptor mixes data and accessor fields",
            ));
        }
        Ok(Self {
            enumerable,
            configurable,
            value,
            writable,
            get,
            set,
        })
    }
    pub(crate) fn apply_array_length(self, vm: &mut Vm, id: ObjectId) -> Result<bool, JsError> {
        // ArraySetLength performs both numeric conversions before fetching the
        // current length. Converting an object can itself mutate the array.
        let length = self
            .value
            .as_ref()
            .map(|value| {
                let unsigned = crate::vm::to_u32(vm.to_number(value)?);
                if vm.to_number(value)? != f64::from(unsigned) {
                    return Err(JsError::new(ErrorKind::RangeError, "Invalid array length"));
                }
                Ok(unsigned)
            })
            .transpose()?;
        if self.configurable == Some(true)
            || self.enumerable == Some(true)
            || self.get.is_some()
            || self.set.is_some()
        {
            return Ok(false);
        }
        vm.resize_sparse_array(id, length, self.writable)
    }

    pub(crate) fn value_only(value:Value)->Self {Self{value:Some(value),..Self::default()}}
    pub(crate) fn full(property: Property) -> Self {
        let mut d = Self {
            enumerable: Some(property.enumerable),
            configurable: Some(property.configurable),
            ..Self::default()
        };
        match property.kind {
            PropertyKind::Data { value, writable } => {
                d.value = Some(value);
                d.writable = Some(writable)
            }
            PropertyKind::Accessor { get, set } => {
                d.get = Some(get.unwrap_or_default());
                d.set = Some(set.unwrap_or_default())
            }
        }
        d
    }
    fn materialize(&self, vm: &mut Vm) -> Result<Value, JsError> {
        let object = vm.new_object();
        for (k, v) in [
            ("value", self.value.clone()),
            ("writable", self.writable.map(Value::Bool)),
            ("get", self.get.clone()),
            ("set", self.set.clone()),
            ("enumerable", self.enumerable.map(Value::Bool)),
            ("configurable", self.configurable.map(Value::Bool)),
        ] {
            if let Some(v) = v {
                vm.define_data_property(object.clone(), k, v, true, true, true)?;
            }
        }
        Ok(object)
    }
    fn complete(&self, old: Option<Property>) -> Property {
        let mut p = old.unwrap_or(Property {
            kind: PropertyKind::Data {
                value: Value::Undefined,
                writable: false,
            },
            configurable: false,
            enumerable: false,
        });
        if let Some(e) = self.enumerable {
            p.enumerable = e
        }
        if let Some(c) = self.configurable {
            p.configurable = c
        }
        if self.get.is_some() || self.set.is_some() {
            let (og, os) = match p.kind {
                PropertyKind::Accessor { get, set } => (get, set),
                _ => (None, None),
            };
            let choose = |v: &Option<Value>, old: Option<Value>| {
                v.clone().or(old).filter(|v| !v.is_undefined())
            };
            p.kind = PropertyKind::Accessor {
                get: choose(&self.get, og),
                set: choose(&self.set, os),
            };
        } else if self.value.is_some() || self.writable.is_some() {
            let (ov, ow) = match p.kind {
                PropertyKind::Data { value, writable } => (value, writable),
                _ => (Value::Undefined, false),
            };
            p.kind = PropertyKind::Data {
                value: self.value.clone().unwrap_or(ov),
                writable: self.writable.unwrap_or(ow),
            };
        }
        p
    }
}

impl Vm {
    pub(crate) fn proxy_descriptor(
        &mut self,
        id: ObjectId,
        key: &PropertyKey,
    ) -> Result<Option<Property>, JsError> {
        self.proxy_trap(id, "getOwnPropertyDescriptor", |vm, target, h, trap| {
            let Some(trap) = trap else {
                return vm.own_descriptor_key(target, key);
            };
            let raw = vm.invoke(trap, h, &[Value::Object(target), key_value(key)])?;
            if !raw.is_undefined() && raw.as_object().is_none() {
                return Err(JsError::type_error("Invalid proxy descriptor result"));
            }
            let old = vm.own_descriptor_key(target, key)?;
            if raw.is_undefined() {
                if let Some(old) = old
                    && (!old.configurable || !vm.object_extensible(target)?)
                {
                    return Err(JsError::type_error(
                        "Proxy cannot hide protected own property",
                    ));
                }
                return Ok(None);
            }
            let extensible = vm.object_extensible(target)?;
            let result = PartialDescriptor::read(vm, raw)?.complete(None);
            match old {
                None if !extensible || !result.configurable => {
                    return Err(JsError::type_error(
                        "Invalid proxy descriptor for absent property",
                    ));
                }
                Some(old) => {
                    crate::symbol::validate_descriptor(&old, &result)?;
                    if !result.configurable
                        && (old.configurable
                            || matches!(
                                (&old.kind, &result.kind),
                                (
                                    PropertyKind::Data { writable: true, .. },
                                    PropertyKind::Data {
                                        writable: false,
                                        ..
                                    }
                                )
                            ))
                    {
                        return Err(JsError::type_error(
                            "Proxy cannot report a stronger property invariant",
                        ));
                    }
                }
                _ => {}
            }
            Ok(Some(result))
        })
    }

    pub(crate) fn proxy_define(
        &mut self,
        id: ObjectId,
        key: &PropertyKey,
        d: PartialDescriptor,
    ) -> Result<bool, JsError> {
        self.proxy_trap(id, "defineProperty", |vm, target, h, trap| {
            let desc = d.materialize(vm)?;
            let Some(trap) = trap else {
                let definition = crate::builtins::descriptor(vm, target, key, desc)?;
                return definition.apply(vm, Value::Object(target), key.clone(), false);
            };
            if !vm
                .invoke(trap, h, &[Value::Object(target), key_value(key), desc])?
                .is_truthy()
            {
                return Ok(false);
            }
            let old = vm.own_descriptor_key(target, key)?;
            let extensible = vm.object_extensible(target)?;
            if let Some(old) = old {
                crate::symbol::validate_descriptor(&old, &d.complete(Some(old.clone())))?;
                if d.configurable == Some(false) && old.configurable
                    || !old.configurable
                        && matches!(old.kind, PropertyKind::Data { writable: true, .. })
                        && d.writable == Some(false)
                {
                    return Err(JsError::type_error("Invalid proxy property transition"));
                }
            } else if !extensible || d.configurable == Some(false) {
                return Err(JsError::type_error(
                    "Proxy cannot define this property on its target",
                ));
            }
            Ok(true)
        })
    }
}
