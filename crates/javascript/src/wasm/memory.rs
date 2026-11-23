//! Non-shared wasm32 linear memory using the native fixed-length ArrayBuffer.
use super::{arg, class, tag};
use crate::{
    ErrorKind, JsError, ObjectId, Value, Vm,
    builtins::method,
    heap::{ObjectKind, Property, PropertyKind},
};

pub(super) const PAGE_BYTES: usize = 64 * 1024;
pub(super) const MAX_PAGES: usize = 65_536;

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub(super) minimum: usize,
    pub(super) maximum: Option<usize>,
}

#[derive(Clone, Copy)]
pub(crate) struct Memory {
    pub(crate) buffer: ObjectId,
    maximum: Option<usize>,
}

fn range_error() -> JsError {
    JsError::new(
        ErrorKind::RangeError,
        "WebAssembly memory size is out of range",
    )
}

fn page_count(vm: &mut Vm, value: &Value) -> Result<usize, JsError> {
    let number = vm.to_number(value)?.trunc();
    if !(0.0..=f64::from(u32::MAX)).contains(&number) {
        return Err(JsError::type_error(
            "Expected a WebAssembly uint32 page count",
        ));
    }
    Ok(number as usize)
}

fn byte_length(pages: usize) -> Result<usize, JsError> {
    if pages > MAX_PAGES {
        return Err(range_error());
    }
    pages.checked_mul(PAGE_BYTES).ok_or_else(range_error)
}

fn allocation_to_range(error: JsError) -> JsError {
    if error.kind == ErrorKind::HeapLimit {
        range_error()
    } else {
        error
    }
}

pub(super) fn create(vm: &mut Vm, limits: Limits, prototype: ObjectId) -> Result<Value, JsError> {
    let buffer = vm
        .new_wasm_buffer(byte_length(limits.minimum)?)
        .map_err(allocation_to_range)?;
    vm.allocate_constructed(
        ObjectKind::WasmMemory(Memory {
            buffer,
            maximum: limits.maximum,
        }),
        prototype,
    )
}

fn construct(vm: &mut Vm, prototype: ObjectId, args: &[Value]) -> Result<Value, JsError> {
    let descriptor = arg(args, 0);
    if descriptor.as_object().is_none() {
        return Err(JsError::type_error(
            "WebAssembly memory descriptor must be an object",
        ));
    }
    let initial = vm.get_property(descriptor.clone(), "initial")?;
    let minimum = page_count(vm, &initial)?;
    if minimum > MAX_PAGES {
        return Err(range_error());
    }
    let maximum = vm.get_property(descriptor.clone(), "maximum")?;
    let maximum = if maximum.is_undefined() {
        None
    } else {
        let maximum = page_count(vm, &maximum)?;
        if !(minimum..=MAX_PAGES).contains(&maximum) {
            return Err(range_error());
        }
        Some(maximum)
    };
    if vm.get_property(descriptor, "shared")?.is_truthy() {
        return Err(JsError::type_error(
            "Shared WebAssembly memory is not supported",
        ));
    }
    create(vm, Limits { minimum, maximum }, prototype)
}

pub(super) fn from_value(vm: &Vm, value: &Value) -> Result<(ObjectId, Memory), JsError> {
    let error = || JsError::type_error("Expected a WebAssembly.Memory");
    let id = value.as_object().ok_or_else(error)?;
    match vm.heap.object(id)?.kind {
        ObjectKind::WasmMemory(memory) => Ok((id, memory)),
        _ => Err(error()),
    }
}

pub(super) fn pages(vm: &Vm, memory: Memory) -> usize {
    vm.wasm_buffer_bytes(memory.buffer).len() / PAGE_BYTES
}

pub(super) fn matches_limits(vm: &Vm, memory: Memory, expected: Limits) -> bool {
    pages(vm, memory) >= expected.minimum
        && expected
            .maximum
            .is_none_or(|limit| memory.maximum.is_some_and(|actual| actual <= limit))
}

pub(super) fn grow(vm: &mut Vm, object: ObjectId, delta: usize) -> Result<usize, JsError> {
    let ObjectKind::WasmMemory(memory) = vm.heap.object(object)?.kind else {
        unreachable!("branded WebAssembly memory")
    };
    let previous = pages(vm, memory);
    let target = previous
        .checked_add(delta)
        .filter(|target| *target <= memory.maximum.unwrap_or(MAX_PAGES))
        .ok_or_else(range_error)?;
    let buffer = vm
        .grow_wasm_buffer(memory.buffer, byte_length(target)?)
        .map_err(allocation_to_range)?;
    vm.heap.object_mut(object)?.kind = ObjectKind::WasmMemory(Memory { buffer, ..memory });
    Ok(previous)
}

pub(super) fn install(vm: &mut Vm, namespace: &Value) -> ObjectId {
    let prototype = vm.new_object();
    let intrinsic = prototype.as_object().unwrap();
    vm.root(prototype.clone());
    tag(vm, &prototype, "WebAssembly.Memory");
    class(vm, namespace, "Memory", &prototype, false, construct);
    let read = vm.new_native_function("get buffer", |vm, this, _| {
        let (_, memory) = from_value(vm, &this)?;
        Ok(Value::Object(memory.buffer))
    });
    vm.define_property(
        prototype.clone(),
        "buffer".into(),
        Property {
            kind: PropertyKind::Accessor {
                get: Some(read),
                set: None,
            },
            enumerable: true,
            configurable: true,
        },
    )
    .unwrap();
    method(vm, prototype, "grow", 1, |vm, this, args| {
        let (object, _) = from_value(vm, &this)?;
        let delta = page_count(vm, &arg(args, 0))?;
        // Conversion can execute JS and grow this same memory.
        grow(vm, object, delta).map(Value::from)
    });
    intrinsic
}
