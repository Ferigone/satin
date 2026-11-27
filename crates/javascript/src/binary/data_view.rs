use super::{View, arg, element::Element, getter, kind_id, range_error, tag, to_index, type_error};
use crate::{
    JsError, Value, Vm,
    builtins::{constructor, method, replace_construct},
    heap::ObjectKind,
};

fn receiver(vm: &Vm, value: &Value) -> Result<View, JsError> {
    let id = kind_id(vm, value, "DataView receiver must be an object")?;
    match vm.heap.object(id)?.kind {
        ObjectKind::DataView(view) => Ok(view),
        _ => Err(JsError::type_error("Receiver is not a DataView")),
    }
}

pub(super) fn install(vm: &mut Vm) {
    let prototype = vm.new_object();
    let intrinsic = prototype.as_object().unwrap();
    vm.root(prototype.clone());
    vm.realm.binary.data_view = Some(intrinsic);
    let constructor = constructor(vm, "DataView", 1, prototype.clone(), |_, _, _| {
        Err(JsError::type_error("DataView requires new"))
    });
    replace_construct(vm, constructor, move |vm, _, args, target| {
        let buffer = kind_id(vm, &arg(args, 0), "DataView requires an ArrayBuffer")?;
        vm.buffer_data(buffer)?;
        let offset = to_index(vm, &arg(args, 1))?;
        let data = vm.buffer_data(buffer)?;
        let available = data
            .read()
            .ok_or_else(type_error)?
            .len()
            .checked_sub(offset)
            .ok_or_else(|| range_error("DataView offset exceeds buffer length"))?;
        let resizable = data.maximum.is_some();
        let length = arg(args, 2);
        let bytes = if length.is_undefined() {
            (!resizable).then_some(available)
        } else {
            let length = to_index(vm, &length)?;
            if length > available {
                return Err(range_error("DataView length exceeds buffer length"));
            }
            Some(length)
        };
        let prototype = vm.prototype_from_constructor(target, intrinsic)?;
        let view = View {
            buffer,
            offset,
            bytes,
        };
        if vm.buffer_data(buffer)?.length().is_none() {
            return Err(type_error());
        }
        if vm.view_range(view, 1)?.is_none() {
            return Err(range_error("DataView became out of bounds"));
        }
        vm.check_binary_allocation(0, 1)?;
        vm.allocate_constructed(ObjectKind::DataView(view), prototype)
    });
    getter(
        vm,
        prototype.clone(),
        "buffer".into(),
        "get buffer",
        |vm, this, _| Ok(Value::Object(receiver(vm, &this)?.buffer)),
    );
    for (key, name, offset) in [
        ("byteLength", "get byteLength", false),
        ("byteOffset", "get byteOffset", true),
    ] {
        getter(
            vm,
            prototype.clone(),
            key.into(),
            name,
            move |vm, this, _| {
                let range = vm.view_range(receiver(vm, &this)?, 1)?;
                Ok(range
                    .map_or(0, |range| if offset { range.start } else { range.len() })
                    .into())
            },
        );
    }
    for (get, set, element) in [
        ("getInt8", "setInt8", Element::Int8),
        ("getUint8", "setUint8", Element::Uint8),
        ("getInt16", "setInt16", Element::Int16),
        ("getUint16", "setUint16", Element::Uint16),
        ("getInt32", "setInt32", Element::Int32),
        ("getUint32", "setUint32", Element::Uint32),
        ("getFloat16", "setFloat16", Element::Float16),
        ("getFloat32", "setFloat32", Element::Float32),
        ("getFloat64", "setFloat64", Element::Float64),
        ("getBigInt64", "setBigInt64", Element::BigInt64),
        ("getBigUint64", "setBigUint64", Element::BigUint64),
    ] {
        method(vm, prototype.clone(), get, 1, move |vm, this, args| {
            let view = receiver(vm, &this)?;
            let offset = to_index(vm, &arg(args, 0))?;
            let range = access_range(vm, view, offset, element.width())?;
            let bytes = &vm.buffer_data(view.buffer)?.read().unwrap()[range];
            Ok(element.value(Element::load_bits(bytes, arg(args, 1).is_truthy())))
        });
        method(vm, prototype.clone(), set, 2, move |vm, this, args| {
            let view = receiver(vm, &this)?;
            vm.require_writable_buffer(view.buffer)?;
            let offset = to_index(vm, &arg(args, 0))?;
            let bits = element.coerce_bits(vm, &arg(args, 1))?;
            let range = access_range(vm, view, offset, element.width())?;
            let data = element.store_bits(bits, arg(args, 2).is_truthy());
            vm.buffer_data_mut(view.buffer)?.write().unwrap()[range]
                .copy_from_slice(&data[..element.width()]);
            Ok(Value::Undefined)
        });
    }
    tag(vm, prototype, "DataView");
}

fn access_range(
    vm: &Vm,
    view: View,
    offset: usize,
    width: usize,
) -> Result<std::ops::Range<usize>, JsError> {
    let extent = vm.require_view_range(view, 1)?;
    let end = offset
        .checked_add(width)
        .filter(|end| *end <= extent.len())
        .ok_or_else(|| range_error("DataView access is outside the view"))?;
    Ok(extent.start + offset..extent.start + end)
}
