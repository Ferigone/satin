use super::{Kind, arg, receiver};
use crate::{
    ErrorKind, IteratorRecord, JsError, ObjectId, Value, Vm, builtins::method, heap::ObjectKind,
};

struct SetLike {
    object: Value,
    size: f64,
    has: Value,
    keys: Value,
}

impl SetLike {
    fn get(vm: &mut Vm, object: Value) -> Result<Self, JsError> {
        if object.as_object().is_none() {
            return Err(JsError::type_error("Set method argument must be an object"));
        }
        let size = vm.get_property(object.clone(), "size")?;
        let size = vm.to_number(&size)?.trunc();
        if size.is_nan() {
            return Err(JsError::type_error("Set-like size converts to NaN"));
        }
        if size < 0.0 {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "Set-like size is negative",
            ));
        }
        let has = vm.get_property(object.clone(), "has")?;
        if !vm.is_callable(&has) {
            return Err(JsError::type_error("Set-like has is not callable"));
        }
        let keys = vm.get_property(object.clone(), "keys")?;
        if !vm.is_callable(&keys) {
            return Err(JsError::type_error("Set-like keys is not callable"));
        }
        Ok(Self {
            object,
            size,
            has,
            keys,
        })
    }

    fn has(&self, vm: &mut Vm, value: &Value) -> Result<bool, JsError> {
        Ok(vm
            .invoke(
                self.has.clone(),
                self.object.clone(),
                std::slice::from_ref(value),
            )?
            .is_truthy())
    }

    fn iterator(&self, vm: &mut Vm) -> Result<IteratorRecord, JsError> {
        vm.iterator_from_method(self.object.clone(), self.keys.clone())
    }
}

impl Vm {
    fn copy_set(&mut self, id: ObjectId, prototype: ObjectId) -> Result<Value, JsError> {
        let set = self.collection(id)?;
        self.ensure_allocation(384usize.saturating_add(set.extra_bytes()))?;
        self.consume_work(set.entries.len().saturating_mul(2))?;
        let data = Box::new(self.collection(id)?.clone());
        self.allocate_constructed(ObjectKind::Collection(data), prototype)
    }

    fn next_set_value(&mut self, id: ObjectId, cursor: &mut u64) -> Result<Option<Value>, JsError> {
        Ok(self
            .collection_entry(id, *cursor)?
            .map(|(order, value, _)| {
                *cursor = order + 1;
                value
            }))
    }

    fn set_contains(&mut self, id: ObjectId, value: &Value) -> Result<bool, JsError> {
        Ok(self.collection_lookup(id, value)?.1.is_some())
    }
}

fn union(vm: &mut Vm, id: ObjectId, other: SetLike, prototype: ObjectId) -> Result<Value, JsError> {
    let mut keys = other.iterator(vm)?;
    // Calling keys can mutate the original Set before the copy is taken.
    let result = vm.copy_set(id, prototype)?;
    let output = result.as_object().unwrap();
    while let Some(value) = vm.iterator_step(&mut keys)? {
        vm.collection_insert(output, value, Value::Undefined)?;
    }
    Ok(result)
}

fn difference(
    vm: &mut Vm,
    id: ObjectId,
    other: SetLike,
    prototype: ObjectId,
) -> Result<Value, JsError> {
    let result = vm.copy_set(id, prototype)?;
    let output = result.as_object().unwrap();
    if vm.collection(id)?.entries.len() as f64 <= other.size {
        let mut cursor = 0;
        while let Some(value) = vm.next_set_value(output, &mut cursor)? {
            if other.has(vm, &value)? {
                vm.collection_delete(output, value)?;
            }
        }
    } else {
        let mut keys = other.iterator(vm)?;
        while let Some(value) = vm.iterator_step(&mut keys)? {
            vm.collection_delete(output, value)?;
        }
    }
    Ok(result)
}

fn intersection(
    vm: &mut Vm,
    id: ObjectId,
    other: SetLike,
    prototype: ObjectId,
) -> Result<Value, JsError> {
    let result = vm.new_collection(Kind::Set, prototype)?;
    let output = result.as_object().unwrap();
    if vm.collection(id)?.entries.len() as f64 <= other.size {
        let mut cursor = 0;
        while let Some(value) = vm.next_set_value(id, &mut cursor)? {
            if other.has(vm, &value)? {
                vm.collection_insert(output, value, Value::Undefined)?;
            }
        }
    } else {
        let mut keys = other.iterator(vm)?;
        while let Some(value) = vm.iterator_step(&mut keys)? {
            if vm.set_contains(id, &value)? {
                vm.collection_insert(output, value, Value::Undefined)?;
            }
        }
    }
    Ok(result)
}

fn symmetric_difference(
    vm: &mut Vm,
    id: ObjectId,
    other: SetLike,
    prototype: ObjectId,
) -> Result<Value, JsError> {
    let mut keys = other.iterator(vm)?;
    let result = vm.copy_set(id, prototype)?;
    let output = result.as_object().unwrap();
    while let Some(value) = vm.iterator_step(&mut keys)? {
        if vm.set_contains(id, &value)? {
            vm.collection_delete(output, value)?;
        } else {
            vm.collection_insert(output, value, Value::Undefined)?;
        }
    }
    Ok(result)
}

fn compare_with_has(
    vm: &mut Vm,
    id: ObjectId,
    other: &SetLike,
    expected: bool,
) -> Result<Value, JsError> {
    let mut cursor = 0;
    while let Some(value) = vm.next_set_value(id, &mut cursor)? {
        if other.has(vm, &value)? != expected {
            return Ok(false.into());
        }
    }
    Ok(true.into())
}

fn compare_with_keys(
    vm: &mut Vm,
    id: ObjectId,
    other: &SetLike,
    expected: bool,
) -> Result<Value, JsError> {
    let mut keys = other.iterator(vm)?;
    while let Some(value) = vm.iterator_step(&mut keys)? {
        if vm.set_contains(id, &value)? != expected {
            return vm.iterator_close(&mut keys, Ok(false.into()));
        }
    }
    Ok(true.into())
}

type Algebra = fn(&mut Vm, ObjectId, SetLike, ObjectId) -> Result<Value, JsError>;

pub(super) fn install(vm: &mut Vm, prototype: Value, intrinsic: ObjectId) {
    let algebra: [(&str, Algebra); 4] = [
        ("union", union),
        ("difference", difference),
        ("intersection", intersection),
        ("symmetricDifference", symmetric_difference),
    ];
    for (name, operation) in algebra {
        method(vm, prototype.clone(), name, 1, move |vm, this, args| {
            let id = receiver(vm, &this, Kind::Set)?;
            let other = SetLike::get(vm, arg(args, 0))?;
            operation(vm, id, other, intrinsic)
        });
    }
    method(
        vm,
        prototype.clone(),
        "isDisjointFrom",
        1,
        |vm, this, args| {
            let id = receiver(vm, &this, Kind::Set)?;
            let other = SetLike::get(vm, arg(args, 0))?;
            if vm.collection(id)?.entries.len() as f64 <= other.size {
                compare_with_has(vm, id, &other, false)
            } else {
                compare_with_keys(vm, id, &other, false)
            }
        },
    );
    method(vm, prototype.clone(), "isSubsetOf", 1, |vm, this, args| {
        let id = receiver(vm, &this, Kind::Set)?;
        let other = SetLike::get(vm, arg(args, 0))?;
        if vm.collection(id)?.entries.len() as f64 > other.size {
            return Ok(false.into());
        }
        compare_with_has(vm, id, &other, true)
    });
    method(vm, prototype, "isSupersetOf", 1, |vm, this, args| {
        let id = receiver(vm, &this, Kind::Set)?;
        let other = SetLike::get(vm, arg(args, 0))?;
        if (vm.collection(id)?.entries.len() as f64) < other.size {
            return Ok(false.into());
        }
        compare_with_keys(vm, id, &other, true)
    });
}
