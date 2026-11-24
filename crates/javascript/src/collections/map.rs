use super::{Kind, arg, canonical_key, receiver};
use crate::{ErrorKind, JsError, ObjectId, Value, Vm, builtins::method, heap::ObjectKind};

impl Vm {
    fn map_value(&mut self, id: ObjectId, key: &Value) -> Result<Option<Value>, JsError> {
        let (_, order) = self.collection_lookup(id, key)?;
        let data = self.collection(id)?;
        Ok(order.map(|order| data.entries[&order].value.clone()))
    }

    fn append_to_group(&mut self, map: ObjectId, key: Value, item: Value) -> Result<(), JsError> {
        if let Some(array) = self.map_value(map, &key)? {
            let ObjectKind::Array { length, .. } =
                self.heap.object(array.as_object().unwrap())?.kind
            else {
                unreachable!("hidden Map.groupBy result only holds arrays");
            };
            self.define_data_property(array, &length.to_string(), item, true, true, true)
        } else {
            let array = self.try_new_array(&[item])?;
            self.collection_insert(map, key, array)
        }
    }
}

pub(crate) fn group_by(
    vm: &mut Vm,
    iterable: Value,
    callback: Value,
    map_prototype: Option<ObjectId>,
) -> Result<Value, JsError> {
    if iterable.is_nullish() || !vm.is_callable(&callback) {
        return Err(JsError::type_error(
            "groupBy requires items and a callable callback",
        ));
    }
    let mut iterator = vm.get_iterator(iterable)?;
    let result = match map_prototype {
        Some(prototype) => vm.new_collection(Kind::Map, prototype)?,
        None => vm.new_object_with_prototype(Value::Null)?,
    };
    let id = result.as_object().unwrap();
    let mut index = 0usize;
    while let Some(item) = vm.iterator_step(&mut iterator)? {
        // Repeated values can all enter one group. Bound total items before
        // appending, independently of the number of distinct group keys.
        if index >= vm.limits.max_array_length as usize {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "GroupBy item budget exhausted",
            ));
        }
        let key = vm.invoke(
            callback.clone(),
            Value::Undefined,
            &[item.clone(), index.into()],
        );
        let added = key.and_then(|key| {
            if map_prototype.is_some() {
                return vm.append_to_group(id, key, item);
            }
            let key = vm.to_property_key(&key)?;
            let group = vm.get_property_key(result.clone(), key.clone())?;
            if group.is_undefined() {
                let group = vm.try_new_array(&[item])?;
                vm.define_data_property_key(result.clone(), key, group, true, true, true)
            } else {
                vm.append_array_literal(group, Some(item))
            }
        });
        if let Err(error) = added {
            return vm.iterator_close(&mut iterator, Err(error));
        }
        index += 1;
    }
    Ok(result)
}

pub(super) fn install(vm: &mut Vm, constructor: Value, prototype: Value, intrinsic: ObjectId) {
    method(vm, constructor, "groupBy", 2, move |vm, _, args| {
        group_by(vm, arg(args, 0), arg(args, 1), Some(intrinsic))
    });
    method(vm, prototype.clone(), "get", 1, |vm, this, args| {
        let id = receiver(vm, &this, Kind::Map)?;
        Ok(vm.map_value(id, &arg(args, 0))?.unwrap_or_default())
    });
    method(vm, prototype.clone(), "getOrInsert", 2, |vm, this, args| {
        let id = receiver(vm, &this, Kind::Map)?;
        let key = arg(args, 0);
        if let Some(value) = vm.map_value(id, &key)? {
            return Ok(value);
        }
        let value = arg(args, 1);
        vm.collection_insert(id, key, value.clone())?;
        Ok(value)
    });
    method(vm, prototype, "getOrInsertComputed", 2, |vm, this, args| {
        let id = receiver(vm, &this, Kind::Map)?;
        let callback = arg(args, 1);
        if !vm.is_callable(&callback) {
            return Err(JsError::type_error(
                "Map.getOrInsertComputed callback is not callable",
            ));
        }
        let key = canonical_key(arg(args, 0));
        if let Some(value) = vm.map_value(id, &key)? {
            return Ok(value);
        }
        let value = vm.invoke(callback, Value::Undefined, std::slice::from_ref(&key))?;
        // The callback may already have inserted, deleted or reordered the key.
        vm.collection_insert(id, key, value.clone())?;
        Ok(value)
    });
}
