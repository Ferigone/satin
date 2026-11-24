use super::{
    Buffer, allocation_error, arg, check_buffer_length, clamped_index, getter, kind_id,
    range_error, species_constructor, species_getter, tag, to_index, type_error,
};
use crate::{
    JsError, ObjectId, Value, Vm,
    builtins::{constructor, method, replace_construct},
    heap::ObjectKind,
};

type BufferGetter = (&'static str, &'static str, fn(&Buffer) -> Value);

impl Vm {
    pub(super) fn resize_binary_store(
        &mut self,
        id: ObjectId,
        length: usize,
    ) -> Result<(), JsError> {
        check_buffer_length(length)?;
        let bytes = self
            .buffer_data(id)?
            .bytes
            .as_ref()
            .ok_or_else(type_error)?;
        let old_length = bytes.len();
        let old_capacity = bytes.capacity();
        let extra = length.saturating_sub(old_capacity);
        self.ensure_allocation(extra)?;
        self.binary_work(
            length
                .saturating_sub(old_length)
                .saturating_add(if extra > 0 { old_length } else { 0 }),
        )?;
        let buffer = self.buffer_data_mut(id)?.bytes.as_mut().unwrap();
        if extra > 0 {
            buffer
                .try_reserve_exact(length - old_length)
                .map_err(|_| allocation_error())?;
        }
        buffer.resize(length, 0);
        let new_capacity = buffer.capacity();
        self.heap.estimated_bytes = self
            .heap
            .estimated_bytes
            .saturating_add(new_capacity - old_capacity);
        Ok(())
    }

    pub(super) fn transfer_buffer(
        &mut self,
        id: ObjectId,
        length: Value,
        fixed: bool,
    ) -> Result<Value, JsError> {
        self.require_unshared(id)?;
        let length = if length.is_undefined() {
            self.buffer_data(id)?.bytes.as_ref().map_or(0, Vec::len)
        } else {
            to_index(self, &length)?
        };
        let buffer = self.buffer_data(id)?;
        let bytes = buffer.bytes.as_ref().ok_or_else(type_error)?;
        if buffer.wasm {
            return Err(JsError::type_error("Cannot transfer a WebAssembly buffer"));
        }
        let maximum = if fixed { None } else { buffer.maximum };
        if maximum.is_some_and(|maximum| length > maximum) {
            return Err(range_error("Transfer exceeds the ArrayBuffer maximum"));
        }
        check_buffer_length(length)?;
        self.check_binary_allocation(length.saturating_sub(bytes.capacity()), 1)?;
        self.resize_binary_store(id, length)?;
        let bytes = self.buffer_data_mut(id)?.bytes.take().unwrap();
        self.heap.estimated_bytes = self.heap.estimated_bytes.saturating_sub(bytes.capacity());
        self.create_buffer(bytes, maximum, self.array_buffer_prototype())
    }
}

pub(super) fn install(vm: &mut Vm) {
    let prototype = vm.new_object();
    let intrinsic = prototype.as_object().unwrap();
    vm.root(prototype.clone());
    vm.realm.binary.array_buffer = Some(intrinsic);
    let constructor = constructor(vm, "ArrayBuffer", 1, prototype.clone(), |_, _, _| {
        Err(JsError::type_error("ArrayBuffer requires new"))
    });
    vm.root(constructor.clone());
    let default_constructor = constructor.as_object().unwrap();
    replace_construct(vm, constructor.clone(), move |vm, _, args, target| {
        let length = to_index(vm, &arg(args, 0))?;
        let options = arg(args, 1);
        let maximum = if options.as_object().is_some() {
            let limit = vm.get_property(options, "maxByteLength")?;
            if limit.is_undefined() {
                None
            } else {
                Some(to_index(vm, &limit)?)
            }
        } else {
            None
        };
        if maximum.is_some_and(|maximum| length > maximum) {
            return Err(range_error("ArrayBuffer length exceeds its maximum"));
        }
        let prototype = vm.prototype_from_constructor(target, intrinsic)?;
        if let Some(maximum) = maximum {
            check_buffer_length(maximum)?;
        }
        let bytes = vm.zero_bytes(length, 1)?;
        vm.create_buffer(bytes, maximum, prototype)
    });
    method(vm, constructor.clone(), "isView", 1, |vm, _, args| {
        Ok(arg(args, 0)
            .as_object()
            .and_then(|id| vm.heap.object(id).ok())
            .is_some_and(|object| {
                matches!(
                    object.kind,
                    ObjectKind::TypedArray(_) | ObjectKind::DataView(_)
                )
            })
            .into())
    });
    species_getter(vm, constructor);
    let getters: [BufferGetter; 4] = [
        ("byteLength", "get byteLength", |buffer| {
            buffer.bytes.as_ref().map_or(0, Vec::len).into()
        }),
        ("maxByteLength", "get maxByteLength", |buffer| {
            buffer
                .bytes
                .as_ref()
                .map_or(0, |bytes| buffer.maximum.unwrap_or(bytes.len()))
                .into()
        }),
        ("resizable", "get resizable", |buffer| {
            buffer.maximum.is_some().into()
        }),
        ("detached", "get detached", |buffer| {
            buffer.bytes.is_none().into()
        }),
    ];
    for (key, name, read) in getters {
        getter(
            vm,
            prototype.clone(),
            key.into(),
            name,
            move |vm, this, _| {
                let id = kind_id(vm, &this, "ArrayBuffer receiver must be an object")?;
                vm.require_unshared(id)?;
                Ok(read(vm.buffer_data(id)?))
            },
        );
    }
    method(vm, prototype.clone(), "resize", 1, |vm, this, args| {
        let id = kind_id(vm, &this, "ArrayBuffer receiver must be an object")?;
        vm.require_unshared(id)?;
        let maximum = vm
            .buffer_data(id)?
            .maximum
            .ok_or_else(|| JsError::type_error("ArrayBuffer is fixed length"))?;
        let length = to_index(vm, &arg(args, 0))?;
        if vm.buffer_data(id)?.bytes.is_none() {
            return Err(type_error());
        }
        if length > maximum {
            return Err(range_error("Resize exceeds the ArrayBuffer maximum"));
        }
        vm.resize_binary_store(id, length)?;
        Ok(Value::Undefined)
    });
    method(vm, prototype.clone(), "slice", 2, move |vm, this, args| {
        let id = kind_id(vm, &this, "ArrayBuffer receiver must be an object")?;
        vm.require_unshared(id)?;
        let length = vm
            .buffer_data(id)?
            .bytes
            .as_ref()
            .ok_or_else(type_error)?
            .len();
        let start = clamped_index(vm, &arg(args, 0), length)?;
        let end = arg(args, 1);
        let end = if end.is_undefined() {
            length
        } else {
            clamped_index(vm, &end, length)?
        };
        let count = end.saturating_sub(start);
        let create = species_constructor(vm, this, default_constructor)?;
        let result = vm.construct(create, &[count.into()])?;
        let destination = kind_id(vm, &result, "Invalid ArrayBuffer species result")?;
        vm.require_unshared(destination)?;
        if id == destination
            || vm
                .buffer_data(destination)?
                .bytes
                .as_ref()
                .ok_or_else(type_error)?
                .len()
                < count
        {
            return Err(JsError::type_error("Invalid ArrayBuffer species result"));
        }
        let current = vm
            .buffer_data(id)?
            .bytes
            .as_ref()
            .ok_or_else(type_error)?
            .len();
        let copy = count.min(current.saturating_sub(start));
        if copy > 0 {
            vm.copy_binary_between(id, start..start + copy, destination, 0)?;
        }
        Ok(result)
    });
    for (name, fixed) in [("transfer", false), ("transferToFixedLength", true)] {
        method(vm, prototype.clone(), name, 0, move |vm, this, args| {
            let id = kind_id(vm, &this, "ArrayBuffer receiver must be an object")?;
            vm.transfer_buffer(id, arg(args, 0), fixed)
        });
    }
    tag(vm, prototype, "ArrayBuffer");
}
