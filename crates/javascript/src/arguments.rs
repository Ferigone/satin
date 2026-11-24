//! Mapped and unmapped arguments objects use ordinary own properties with
//! parameter aliases for the supplied indices of a non-strict simple function.
use crate::{
    ErrorKind, JsError, JsString, ObjectId, Value, Vm,
    heap::{Cell, EnvId, Object, ObjectKind, Property, PropertyKind, array_index},
};
use std::collections::BTreeMap;

impl Vm {
    pub(crate) fn materialize_arguments(&mut self, env: EnvId) -> Result<(), JsError> {
        let record = self.heap.environment(env);
        let function = record
            .function
            .as_ref()
            .and_then(Value::as_object)
            .ok_or_else(|| {
                JsError::new(
                    ErrorKind::InternalError,
                    "Arguments binding has no function",
                )
            })?;
        let ObjectKind::Function { code, .. } = &self.heap.object(function)?.kind else {
            return Err(JsError::new(
                ErrorKind::InternalError,
                "Invalid arguments owner",
            ));
        };
        let code = code.clone();
        let args = record
            .pending_arguments
            .as_deref()
            .expect("pending arguments");
        self.ensure_allocation(1024 + args.len() * 384)?;
        let mapped: BTreeMap<_, _> = code
            .mapped_indices
            .iter()
            .copied()
            .filter(|index| *index < args.len())
            .map(|i| (i as u32, code.param_names[i].clone()))
            .collect();
        let mut object = Object::new(
            ObjectKind::Arguments {
                environment: (!mapped.is_empty()).then_some(env),
                mapped,
            },
            Some(self.realm.object_prototype),
        );
        let data = |value| Property {
            kind: PropertyKind::Data {
                value,
                writable: true,
            },
            enumerable: false,
            configurable: true,
        };
        object.insert("length".into(), data(Value::from(args.len())));
        for (index, value) in args.iter().enumerate() {
            object.insert(index.to_string().into(), Property::data(value.clone()));
        }
        let callee = if code.mapped_arguments {
            data(Value::Object(function))
        } else {
            let poison = self.realm.throw_type_error.map(Value::Object);
            Property {
                kind: PropertyKind::Accessor {
                    get: poison.clone(),
                    set: poison,
                },
                enumerable: false,
                configurable: false,
            }
        };
        object.insert("callee".into(), callee);
        let symbol = self
            .well_known_symbol("iterator")
            .expect("installed iterator symbol");
        let Value::Symbol(symbol) = symbol else {
            unreachable!()
        };
        object.symbol_order.push(symbol.clone());
        object.symbols.insert(
            symbol,
            data(Value::Object(
                self.realm.array_values.expect("installed Array values"),
            )),
        );
        let id = self.heap.allocate(Cell::Object(object));
        let record = self.heap.environment_mut(env);
        record.pending_arguments = None;
        record
            .bindings
            .get_mut(&JsString::from("arguments"))
            .unwrap()
            .value = Value::Object(id);
        Ok(())
    }

    pub(crate) fn stored_argument_property(
        &self,
        object: &Object,
        key: &JsString,
    ) -> Option<Property> {
        let mut property = object.properties.get(key)?.clone();
        if let ObjectKind::Arguments {
            environment: Some(env),
            mapped,
        } = &object.kind
            && let Some(name) = array_index(key).and_then(|i| mapped.get(&i))
            && let PropertyKind::Data { value, .. } = &mut property.kind
        {
            *value = self
                .heap
                .environment(*env)
                .bindings
                .get(name)?
                .value
                .clone();
        }
        Some(property)
    }

    pub(crate) fn update_argument_mapping(
        &mut self,
        id: ObjectId,
        key: &JsString,
        mut property: Property,
    ) -> Result<Property, JsError> {
        let ObjectKind::Arguments {
            environment: Some(env),
            mapped,
        } = &self.heap.object(id)?.kind
        else {
            return Ok(property);
        };
        let Some(index) = array_index(key) else {
            return Ok(property);
        };
        let Some(name) = mapped.get(&index).cloned() else {
            return Ok(property);
        };
        let env = *env;
        if let PropertyKind::Data { value, writable } = &mut property.kind {
            self.heap
                .environment_mut(env)
                .bindings
                .get_mut(&name)
                .expect("mapped parameter")
                .value = value.clone();
            if *writable {
                *value = Value::Undefined;
                return Ok(property);
            }
        }
        let ObjectKind::Arguments {
            environment,
            mapped,
        } = &mut self.heap.object_mut(id)?.kind
        else {
            unreachable!()
        };
        mapped.remove(&index);
        if mapped.is_empty() {
            *environment = None;
        }
        Ok(property)
    }
}
