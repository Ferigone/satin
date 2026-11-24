//! Single-agent shared memory. Wrappers across realms reference the same
//! traced backing object. No host can move that store to another VM/thread.
use super::{
    arg, check_buffer_length, clamped_index, element::Element, getter, range_error,
    species_constructor, species_getter, tag, to_index,
};
use crate::{
    JsError, ObjectId, Value, Vm,
    builtins::{constructor, method, replace_construct},
    heap::ObjectKind,
};

fn shared_id(vm: &Vm, v: &Value) -> Result<ObjectId, JsError> {
    let id = v
        .as_object()
        .ok_or_else(|| JsError::type_error("Expected a SharedArrayBuffer"))?;
    if matches!(vm.heap.object(id)?.kind, ObjectKind::SharedArrayBuffer(_)) {
        Ok(id)
    } else {
        Err(JsError::type_error("Expected a SharedArrayBuffer"))
    }
}

pub(super) fn install(vm: &mut Vm) {
    install_atomics(vm);
    if !vm.limits.shared_array_buffer {
        return;
    }
    let prototype = vm.new_object();
    vm.root(prototype.clone());
    let intrinsic = prototype.as_object().unwrap();
    vm.realm.binary.shared_array_buffer = Some(intrinsic);
    let ctor = constructor(vm, "SharedArrayBuffer", 1, prototype.clone(), |_, _, _| {
        Err(JsError::type_error("SharedArrayBuffer requires new"))
    });
    vm.root(ctor.clone());
    let fallback = ctor.as_object().unwrap();
    replace_construct(vm, ctor.clone(), move |vm, _, args, target| {
        let len = to_index(vm, &arg(args, 0))?;
        let options = arg(args, 1);
        let maximum = if options.as_object().is_some() {
            let m = vm.get_property(options, "maxByteLength")?;
            if m.is_undefined() {
                None
            } else {
                Some(to_index(vm, &m)?)
            }
        } else {
            None
        };
        if maximum.is_some_and(|m| len > m) {
            return Err(range_error("Shared buffer length exceeds maximum"));
        }
        let proto = vm.prototype_from_constructor(target, intrinsic)?;
        if let Some(max) = maximum {
            check_buffer_length(max)?;
        }
        let bytes = vm.zero_bytes(len, 2)?;
        let store = vm.create_buffer(bytes, maximum, vm.array_buffer_prototype())?;
        vm.allocate_constructed(
            ObjectKind::SharedArrayBuffer(store.as_object().unwrap()),
            proto,
        )
    });
    species_getter(vm, ctor);
    for (key, name) in [
        ("byteLength", "get byteLength"),
        ("maxByteLength", "get maxByteLength"),
        ("growable", "get growable"),
    ] {
        getter(
            vm,
            prototype.clone(),
            key.into(),
            name,
            move |vm, this, _| {
                let data = vm.buffer_data(shared_id(vm, &this)?)?;
                let len = data.bytes.as_ref().unwrap().len();
                Ok(match key {
                    "growable" => data.maximum.is_some().into(),
                    "maxByteLength" => data.maximum.unwrap_or(len).into(),
                    _ => len.into(),
                })
            },
        );
    }
    method(vm, prototype.clone(), "grow", 1, |vm, this, args| {
        let id = shared_id(vm, &this)?;
        let max = vm
            .buffer_data(id)?
            .maximum
            .ok_or_else(|| JsError::type_error("SharedArrayBuffer is fixed-length"))?;
        let new = to_index(vm, &arg(args, 0))?;
        let length = vm.buffer_data(id)?.bytes.as_ref().unwrap().len();
        if new < length || new > max {
            return Err(range_error("Invalid shared buffer growth"));
        }
        vm.resize_binary_store(id, new)?;
        Ok(Value::Undefined)
    });
    method(vm, prototype.clone(), "slice", 2, move |vm, this, args| {
        let id = shared_id(vm, &this)?;
        let length = vm.buffer_data(id)?.bytes.as_ref().unwrap().len();
        let start = clamped_index(vm, &arg(args, 0), length)?;
        let end = if arg(args, 1).is_undefined() {
            length
        } else {
            clamped_index(vm, &arg(args, 1), length)?
        };
        let count = end.saturating_sub(start);
        let create = species_constructor(vm, this, fallback)?;
        let result = vm.construct(create, &[count.into()])?;
        let dst = shared_id(vm, &result)?;
        if vm.backing_id(dst)? == vm.backing_id(id)?
            || vm.buffer_data(dst)?.bytes.as_ref().unwrap().len() < count
        {
            return Err(JsError::type_error(
                "Invalid SharedArrayBuffer species result",
            ));
        }
        vm.copy_binary_between(id, start..start + count, dst, 0)?;
        Ok(result)
    });
    tag(vm, prototype, "SharedArrayBuffer");
}

#[derive(Clone, Copy)]
enum Operation {
    Load,
    Store,
    Add,
    Sub,
    And,
    Or,
    Xor,
    Exchange,
    CompareExchange,
}

fn atomic_view(vm: &Vm, value: &Value, waitable: bool) -> Result<super::TypedArray, JsError> {
    let id = value
        .as_object()
        .ok_or_else(|| JsError::type_error("Expected an integer TypedArray"))?;
    let a = vm.array_data(id)?;
    let valid = if waitable {
        matches!(a.element, Element::Int32 | Element::BigInt64)
    } else {
        matches!(
            a.element,
            Element::Int8
                | Element::Uint8
                | Element::Int16
                | Element::Uint16
                | Element::Int32
                | Element::Uint32
                | Element::BigInt64
                | Element::BigUint64
        )
    };
    if !valid {
        return Err(JsError::type_error("Incompatible integer TypedArray"));
    }
    vm.require_view_range(a.view, a.element.width())?;
    Ok(a)
}

fn position(vm: &mut Vm, array: super::TypedArray, index: &Value) -> Result<usize, JsError> {
    let len = vm
        .require_view_range(array.view, array.element.width())?
        .len()
        / array.element.width();
    let index = to_index(vm, index)?;
    if index >= len {
        return Err(range_error("Atomic index is out of range"));
    }
    Ok(index)
}

fn install_atomics(vm: &mut Vm) {
    let object = vm.new_object();
    vm.define_data_property(vm.global(), "Atomics", object.clone(), true, false, true)
        .unwrap();
    tag(vm, object.clone(), "Atomics");
    method(vm, object.clone(), "isLockFree", 1, |vm, _, args| {
        let n = vm.to_number(&arg(args, 0))?.trunc();
        Ok((n == 1. || n == 2. || n == 4. || (n == 8. && cfg!(target_has_atomic = "64"))).into())
    });
    method(vm, object.clone(), "pause", 0, |_, _, args| {
        if !arg(args, 0).is_undefined()
            && !matches!(arg(args,0), Value::Number(n) if n.is_finite() && n.fract()==0.)
        {
            return Err(JsError::type_error(
                "Atomics.pause requires an integral number",
            ));
        }
        std::hint::spin_loop();
        Ok(Value::Undefined)
    });
    for (name, op, len) in [
        ("load", Operation::Load, 2),
        ("store", Operation::Store, 3),
        ("add", Operation::Add, 3),
        ("sub", Operation::Sub, 3),
        ("and", Operation::And, 3),
        ("or", Operation::Or, 3),
        ("xor", Operation::Xor, 3),
        ("exchange", Operation::Exchange, 3),
        ("compareExchange", Operation::CompareExchange, 4),
    ] {
        method(vm, object.clone(), name, len, move |vm, _, args| {
            let a = atomic_view(vm, &arg(args, 0), false)?;
            if !matches!(op, Operation::Load) {
                vm.require_writable_buffer(a.view.buffer)?;
            }
            let i = position(vm, a, &arg(args, 1))?;
            let mut operand = 0u64;
            let mut replacement = 0u64;
            let mut stored = Value::Undefined;
            if !matches!(op, Operation::Load) {
                if matches!(op, Operation::Store) {
                    stored = if matches!(a.element, Element::BigInt64 | Element::BigUint64) {
                        Value::BigInt(vm.to_bigint(&arg(args, 2))?)
                    } else {
                        let n = vm.to_number(&arg(args, 2))?.trunc();
                        Value::Number(if n.is_nan() || n == 0. { 0. } else { n })
                    };
                    operand = a.element.coerce_bits(vm, &stored)?;
                } else {
                    operand = a.element.coerce_bits(vm, &arg(args, 2))?;
                }
                if matches!(op, Operation::CompareExchange) {
                    replacement = a.element.coerce_bits(vm, &arg(args, 3))?;
                }
            }
            // Coercions can resize or detach the buffer. From this read through
            // the write no JavaScript is called, so this is one agent event.
            let old = vm
                .read_typed_bits(a, i)?
                .ok_or_else(|| JsError::type_error("Atomic access is out of bounds"))?;
            let mask = u64::MAX >> (64 - a.element.width() * 8);
            let next = match op {
                Operation::Load => return Ok(a.element.value(old)),
                Operation::Store | Operation::Exchange => operand,
                Operation::Add => old.wrapping_add(operand),
                Operation::Sub => old.wrapping_sub(operand),
                Operation::And => old & operand,
                Operation::Or => old | operand,
                Operation::Xor => old ^ operand,
                Operation::CompareExchange => {
                    if old == operand & mask {
                        replacement
                    } else {
                        old
                    }
                }
            };
            vm.write_typed_bits(a, i, next)?;
            Ok(if matches!(op, Operation::Store) {
                stored
            } else {
                a.element.value(old)
            })
        });
    }
    for (name, asynchronous) in [("wait", false), ("waitAsync", true)] {
        method(vm, object.clone(), name, 4, move |vm, _, args| {
            let a = atomic_view(vm, &arg(args, 0), true)?;
            shared_id(vm, &Value::Object(a.view.buffer))?;
            let index = position(vm, a, &arg(args, 1))?;
            let expected = a.element.coerce_bits(vm, &arg(args, 2))?;
            let timeout = vm.to_number(&arg(args, 3))?;
            if !asynchronous {
                return Err(JsError::type_error("This agent cannot suspend"));
            }
            let actual = vm
                .read_typed_bits(a, index)?
                .ok_or_else(|| JsError::type_error("Atomic access is out of bounds"))?;
            let result =
                vm.allocate_constructed(ObjectKind::Ordinary, vm.realm.object_prototype)?;
            let timeout = if timeout.is_nan() {
                f64::INFINITY
            } else {
                timeout.max(0.0)
            };
            let pending = actual == expected && timeout > 0.0;
            let value = if pending {
                vm.new_promise_capability()?.promise
            } else if actual != expected {
                Value::from("not-equal")
            } else {
                Value::from("timed-out")
            };
            vm.define_data_property(result.clone(), "async", pending.into(), true, true, true)?;
            vm.define_data_property(result.clone(), "value", value.clone(), true, true, true)?;
            if let Some(promise) = value.as_object() {
                let store = vm.backing_id(a.view.buffer)?;
                vm.add_atomic_waiter(
                    store,
                    a.view.offset + index * a.element.width(),
                    timeout,
                    promise,
                )?;
            }
            Ok(result)
        });
    }
    method(vm, object, "notify", 3, |vm, _, args| {
        let a = atomic_view(vm, &arg(args, 0), true)?;
        let index = position(vm, a, &arg(args, 1))?;
        let count = if arg(args, 2).is_undefined() {
            usize::MAX
        } else {
            vm.to_number(&arg(args, 2))?.trunc() as usize
        };
        let ObjectKind::SharedArrayBuffer(store) = vm.heap.object(a.view.buffer)?.kind else {
            return Ok(0.into());
        };
        let offset = a.view.offset + index * a.element.width();
        Ok(vm.notify_atomic_waiters(store, offset, count)?.into())
    });
}
