//! Cold, heap-owned realm bootstrap. A core is not an executable realm:
//! builtin installation and callee/continuation realm entry are separate.

use crate::{
    ObjectId, Value,
    heap::{Cell, EnvId, Environment, Heap, Object, ObjectKind},
};
use std::rc::Rc;

#[cfg_attr(test, derive(Clone, Copy))]
pub(crate) struct CoreIntrinsics {
    pub(super) object_prototype: ObjectId,
    pub(super) function_prototype: ObjectId,
    pub(super) array_prototype: ObjectId,
    pub(super) string_prototype: ObjectId,
    pub(super) number_prototype: ObjectId,
    pub(super) boolean_prototype: ObjectId,
    pub(super) error_prototype: ObjectId,
}

impl CoreIntrinsics {
    pub(crate) fn objects(&self) -> [ObjectId; 7] {
        [
            self.object_prototype,
            self.function_prototype,
            self.array_prototype,
            self.string_prototype,
            self.number_prototype,
            self.boolean_prototype,
            self.error_prototype,
        ]
    }
}

fn allocate_object(heap: &mut Heap, kind: ObjectKind, prototype: Option<ObjectId>) -> ObjectId {
    heap.allocate(Cell::Object(Object::new(kind, prototype)))
}

/// The primary realm is part of infallible VM startup, not a guest operation.
pub(super) fn allocate_primary_core(heap: &mut Heap) -> EnvId {
    assert_eq!(heap.live, 0, "a primary core requires a fresh heap");
    allocate_core(heap)
}

// Keep this primitive private: initializing any additional core is a charged
// operation. The second-core tests reuse it after budget admission below.
fn allocate_core(heap: &mut Heap) -> EnvId {
    let object_prototype = allocate_object(heap, ObjectKind::Ordinary, None);
    let prototype = Some(object_prototype);
    let global = allocate_object(heap, ObjectKind::Ordinary, prototype);
    let env = heap.allocate_env(Environment::new_global(global, global));
    let function_prototype = allocate_object(
        heap,
        ObjectKind::Native {
            callback: Rc::new(|_, _, _| Ok(Value::Undefined)),
            construct: None,
            realm_environment: env,
        },
        prototype,
    );
    let array_prototype = allocate_object(
        heap,
        ObjectKind::Array {
            length: 0,
            length_writable: true,
        },
        prototype,
    );
    let string_prototype = allocate_object(heap, ObjectKind::Boxed(Value::from("")), prototype);
    let number_prototype = allocate_object(heap, ObjectKind::Boxed(Value::Number(0.0)), prototype);
    let boolean_prototype = allocate_object(heap, ObjectKind::Boxed(Value::Bool(false)), prototype);
    let error_prototype = allocate_object(heap, ObjectKind::Ordinary, prototype);
    heap.attach_core_intrinsics(
        env,
        CoreIntrinsics {
            object_prototype,
            function_prototype,
            array_prototype,
            string_prototype,
            number_prototype,
            boolean_prototype,
            error_prototype,
        },
    );
    env
}

#[cfg(test)]
fn allocate_additional_core(vm: &mut crate::Vm) -> Result<EnvId, crate::JsError> {
    // Fixed seed: seven prototypes, one binding object, one environment.
    // Only tests can allocate a second core until the full installer is safe
    // to call under the same limits. No partial environment reaches callers.
    const CELLS: usize = 9;
    const BYTES: usize = CELLS * Heap::EMPTY_CELL_BYTES + std::mem::size_of::<CoreIntrinsics>();
    if vm.heap.live.saturating_add(CELLS) > vm.limits.max_heap_objects {
        return Err(crate::JsError::new(
            crate::ErrorKind::HeapLimit,
            "Realm core exceeds the shared heap cell budget",
        ));
    }
    vm.ensure_allocation(BYTES)?;
    vm.consume_work(CELLS)?;
    Ok(allocate_core(&mut vm.heap))
}

#[cfg(test)]
mod entry;
#[cfg(test)]
mod tests;
