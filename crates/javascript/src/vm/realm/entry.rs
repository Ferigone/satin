//! A test-only proof of scoped activation and lifetime. Cold cores are not
//! executable: the entered view intentionally cannot invoke guest or host
//! code or access the unswitched builtin, job, or source-referrer state.

use super::{CoreIntrinsics, EnvId};
use crate::{ErrorKind, GcStats, JsError, ObjectId, RootId, Value, Vm};

struct Selection {
    env: EnvId,
    global: ObjectId,
    intrinsics: CoreIntrinsics,
}

pub(super) type Current = (EnvId, Value, [ObjectId; 7]);

fn current(vm: &Vm) -> Current {
    (
        vm.global_env,
        vm.global(),
        [
            vm.object_prototype,
            vm.function_prototype,
            vm.array_prototype,
            vm.string_prototype,
            vm.number_prototype,
            vm.boolean_prototype,
            vm.error_prototype,
        ],
    )
}

impl Selection {
    fn load(vm: &Vm, env: EnvId) -> Result<Self, JsError> {
        let (global, intrinsics) = vm
            .heap
            .realm_core(env)
            .ok_or_else(|| JsError::type_error("Invalid or collected realm core"))?;
        Ok(Self {
            env,
            global,
            intrinsics: *intrinsics,
        })
    }

    fn activate(&self, vm: &mut Vm) {
        let CoreIntrinsics {
            object_prototype,
            function_prototype,
            array_prototype,
            string_prototype,
            number_prototype,
            boolean_prototype,
            error_prototype,
        } = self.intrinsics;
        vm.global_env = self.env;
        vm.global_object = self.global;
        vm.object_prototype = object_prototype;
        vm.function_prototype = function_prototype;
        vm.array_prototype = array_prototype;
        vm.string_prototype = string_prototype;
        vm.number_prototype = number_prototype;
        vm.boolean_prototype = boolean_prototype;
        vm.error_prototype = error_prototype;
    }
}

pub(super) struct EnteredCore<'a> {
    vm: &'a mut Vm,
    parent: Selection,
    parent_root: RootId,
    parent_depth: usize,
}

pub(super) fn with_current_core<T>(
    vm: &mut Vm,
    env: EnvId,
    body: impl FnOnce(&mut EnteredCore<'_>) -> Result<T, JsError>,
) -> Result<T, JsError> {
    let target = Selection::load(vm, env)?;
    let parent = Selection::load(vm, vm.global_env)?;
    let expected = (
        parent.env,
        Value::Object(parent.global),
        parent.intrinsics.objects(),
    );
    if current(vm) != expected {
        return Err(JsError::new(
            ErrorKind::InternalError,
            "Current realm core does not match its environment",
        ));
    }
    if !vm.can_enter_call() {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Realm entry depth budget exhausted",
        ));
    }
    vm.ensure_allocation(0)?;
    // An entry is not a new task. In particular it never resets remaining
    // fuel, even if a host-only scope enters while the VM is quiescent.
    vm.consume_work(1)?;

    let parent_root = vm.root(Value::Object(parent.env.0));
    let parent_depth = vm.call_depth;
    vm.call_depth += 1;
    target.activate(vm);
    let mut scope = EnteredCore {
        vm,
        parent,
        parent_root,
        parent_depth,
    };
    body(&mut scope)
}

impl Drop for EnteredCore<'_> {
    fn drop(&mut self) {
        self.parent.activate(self.vm);
        self.vm.call_depth = self.parent_depth;
        self.vm.unroot(self.parent_root);
    }
}

impl EnteredCore<'_> {
    pub(super) fn with_core<T>(
        &mut self,
        env: EnvId,
        body: impl FnOnce(&mut EnteredCore<'_>) -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        with_current_core(self.vm, env, body)
    }

    pub(super) fn current(&self) -> Current {
        current(self.vm)
    }

    pub(super) fn charge_work(&mut self, units: usize) -> Result<(), JsError> {
        self.vm.consume_work(units)
    }

    pub(super) fn collect_garbage(&mut self) -> GcStats {
        // This still skips active tasks; it is not a new GC checkpoint.
        self.vm.collect_garbage()
    }

    pub(super) fn is_live(&self, env: EnvId) -> bool {
        self.vm.heap.realm_core(env).is_some()
    }
}
