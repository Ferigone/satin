use super::*;

#[test]
fn executable_realms_keep_intrinsics_and_live_values_in_the_defining_realm() {
    let mut vm = Vm::new();
    vm.eval("var shared = {}; function receive(value) { shared.value = value; return [value]; }")
        .unwrap();
    let (realm, child) = vm.new_realm().unwrap();
    vm.set_global("child", child).unwrap();
    let callback = vm.get_global("receive").unwrap();
    let shared = vm.get_global("shared").unwrap();
    vm.with_realm(realm, |vm| {
        vm.set_global("receive", callback)?;
        vm.set_global("shared", shared)?;
        vm.eval(
            r#"
                var own = {};
                var received = receive(own);
                var symbol = Symbol.iterator;
                var registered = Symbol.for('shared');
                var dynamic = Function('return []');
            "#,
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        vm.eval(
            r#"
                child.Object !== Object && child.Array !== Array &&
                child.symbol === Symbol.iterator && child.registered === Symbol.for('shared') &&
                shared.value === child.own && child.received[0] === child.own &&
                child.received instanceof Array && child.dynamic() instanceof child.Array &&
                Object.getPrototypeOf(child.own) === child.Object.prototype
            "#,
        )
        .unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn released_executable_realm_is_collected_with_its_builtin_graph() {
    let mut vm = Vm::new();
    let baseline = vm.collect_garbage().after;
    let (realm, global) = vm.new_realm().unwrap();
    let root = vm.root(global);
    vm.collect_garbage();
    vm.with_realm(realm, |vm| vm.eval("var saved = Uint8Array.of(9)"))
        .unwrap();
    vm.unroot(root);
    assert_eq!(vm.collect_garbage().after, baseline);
    assert!(vm.with_realm(realm, |vm| vm.eval("saved")).is_err());
}

#[test]
fn awaiting_a_parent_promise_resumes_with_child_intrinsics() {
    let mut vm = Vm::new();
    let promise = vm
        .eval("var resolve; new Promise(function (r) { resolve = r; });")
        .unwrap();
    let (realm, child) = vm.new_realm().unwrap();
    vm.set_global("child", child).unwrap();
    vm.with_realm(realm, |vm| {
        vm.set_global("pending", promise)?;
        vm.eval(
            r#"
            var result;
            async function after() { return [await pending]; }
            after().then(function (value) { result = value; });
        "#,
        )
    })
    .unwrap();
    vm.eval("resolve(7)").unwrap();
    vm.drain_jobs(100).unwrap();
    assert_eq!(
        vm.eval("child.result[0] === 7 && child.result instanceof child.Array")
            .unwrap(),
        Value::Bool(true)
    );
}
use crate::{ErrorKind, Vm};
use entry::with_current_core;

#[test]
fn additional_core_has_distinct_intrinsics_without_copying_host_state() {
    let mut vm = Vm::new();
    vm.set_wasm_compilation_allowed(false);
    let capability = vm.new_native_function("hostCapability", |_, _, _| Ok(Value::Bool(true)));
    vm.set_global("hostCapability", capability).unwrap();
    let agent_symbol = vm.well_known_symbol("iterator").unwrap();
    let primary_global = vm.global();
    let primary = vm.heap.realm_core(vm.realm.global_env).unwrap().1.objects();

    let other_env = allocate_additional_core(&mut vm).unwrap();
    let (other_global, other) = vm.heap.realm_core(other_env).unwrap();
    let prototypes = other.objects();
    assert_ne!(Value::Object(other_global), primary_global);
    assert_eq!(
        vm.heap.object(other_global).unwrap().prototype,
        Some(prototypes[0])
    );
    assert_eq!(vm.get_this(other_env).unwrap(), Value::Object(other_global));
    for (first, second) in primary.into_iter().zip(prototypes) {
        assert_ne!(first, second);
    }
    for &id in &prototypes[1..] {
        assert_eq!(vm.heap.object(id).unwrap().prototype, Some(prototypes[0]));
    }

    let other_global = Value::Object(other_global);
    assert_eq!(
        vm.get_property(other_global.clone(), "hostCapability")
            .unwrap(),
        Value::Undefined
    );
    assert_eq!(vm.global(), primary_global);
    assert_eq!(vm.well_known_symbol("iterator"), Some(agent_symbol));
    assert!(!vm.realm.wasm_compilation_allowed);

    vm.set_property(Value::Object(prototypes[0]), "isolatedMark", 17.into())
        .unwrap();
    assert_eq!(
        vm.get_property(other_global.clone(), "isolatedMark")
            .unwrap(),
        17.into()
    );
    assert_eq!(
        vm.get_property(primary_global.clone(), "isolatedMark")
            .unwrap(),
        Value::Undefined
    );

    let shared = vm.new_object();
    vm.set_property(primary_global.clone(), "shared", shared.clone())
        .unwrap();
    vm.set_property(other_global.clone(), "shared", shared.clone())
        .unwrap();
    assert_eq!(vm.get_property(primary_global, "shared").unwrap(), shared);
    assert_eq!(vm.get_property(other_global, "shared").unwrap(), shared);
}

#[test]
fn rooted_native_keeps_its_additional_core_alive_without_a_permanent_root() {
    let mut vm = Vm::new();
    let other_env = allocate_additional_core(&mut vm).unwrap();
    let (global, intrinsics) = vm.heap.realm_core(other_env).unwrap();
    let function = intrinsics.function_prototype;
    let objects = intrinsics.objects();

    // The host retains only %Function.prototype%, not the global, the
    // environment, or each sibling intrinsic.
    let root = vm.root(Value::Object(function));
    vm.collect_garbage();
    assert!(vm.heap.realm_core(other_env).is_some());
    assert!(vm.heap.object(global).is_ok());
    assert!(objects.iter().all(|id| vm.heap.object(*id).is_ok()));

    vm.unroot(root);
    vm.collect_garbage();
    assert!(vm.heap.realm_core(other_env).is_none());
    assert!(vm.heap.object(global).is_err());
    assert!(objects.iter().all(|id| vm.heap.object(*id).is_err()));
    assert!(vm.heap.realm_core(vm.realm.global_env).is_some());

    let replacement = allocate_additional_core(&mut vm).unwrap();
    assert_ne!(replacement, other_env);
    assert!(vm.heap.realm_core(other_env).is_none());
}

#[test]
fn realm_core_lookup_rejects_foreign_and_wrong_kind_handles() {
    let vm = Vm::new();
    let foreign = Vm::new();
    assert!(vm.heap.realm_core(foreign.realm.global_env).is_none());
    assert!(vm.heap.realm_core(EnvId(vm.realm.global_object)).is_none());
}

#[test]
fn additional_core_admission_preserves_shared_task_and_heap_limits() {
    let mut vm = Vm::new();
    let cells = vm.heap.live;
    let bytes = vm.estimated_heap_bytes();
    let core_bytes = 9 * Heap::EMPTY_CELL_BYTES + Heap::REALM_CORE_BYTES;
    vm.limits.max_heap_objects = cells + 8;
    assert_eq!(
        allocate_additional_core(&mut vm).unwrap_err().kind,
        ErrorKind::HeapLimit
    );

    vm.limits.max_heap_objects = cells + 9;
    vm.limits.max_heap_bytes = bytes + core_bytes - 1;
    assert_eq!(
        allocate_additional_core(&mut vm).unwrap_err().kind,
        ErrorKind::HeapLimit
    );

    vm.limits.max_heap_bytes = bytes + core_bytes;
    vm.limits.max_instructions = 9;
    let error = vm
        .with_task(|vm| {
            vm.charge_work(1)?;
            allocate_additional_core(vm)
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
    assert_eq!(vm.heap.live, cells);
    assert_eq!(vm.estimated_heap_bytes(), bytes);

    let other = vm.with_task(allocate_additional_core).unwrap();
    assert!(vm.heap.realm_core(other).is_some());
    assert_eq!(vm.remaining, 0);
    assert_eq!(vm.heap.live, cells + 9);
    assert_eq!(vm.estimated_heap_bytes(), bytes + core_bytes);
}

fn stored_core(vm: &Vm, env: EnvId) -> (EnvId, Value, [ObjectId; 7]) {
    let (global, intrinsics) = vm.heap.realm_core(env).unwrap();
    (env, Value::Object(global), intrinsics.objects())
}

#[test]
fn current_core_restores_after_nested_errors_and_keeps_parked_roots_live() {
    let mut vm = Vm::new();
    let primary = stored_core(&vm, vm.realm.global_env);
    let outer_env = allocate_additional_core(&mut vm).unwrap();
    let inner_env = allocate_additional_core(&mut vm).unwrap();
    let unused_env = allocate_additional_core(&mut vm).unwrap();
    let outer = stored_core(&vm, outer_env);
    let inner = stored_core(&vm, inner_env);
    let roots_before = vm.roots.len();

    let error = with_current_core::<()>(&mut vm, outer_env, |scope| {
        assert_eq!(scope.current(), outer);
        let error = scope
            .with_core::<()>(inner_env, |leaf| {
                assert_eq!(leaf.current(), inner);
                assert!(!leaf.collect_garbage().skipped_active_execution);
                assert!(leaf.is_live(primary.0));
                // Not current and not otherwise reachable: this specifically
                // exercises the saved parent's scoped GC root.
                assert!(leaf.is_live(outer_env));
                assert!(leaf.is_live(inner_env));
                assert!(!leaf.is_live(unused_env));
                Err(crate::JsError::type_error("inner error"))
            })
            .unwrap_err();
        assert_eq!(error.message, "inner error");
        assert_eq!(scope.current(), outer);
        assert!(!scope.collect_garbage().skipped_active_execution);
        assert!(!scope.is_live(inner_env));
        Err(crate::JsError::new(ErrorKind::RangeError, "outer error"))
    })
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::RangeError);
    assert_eq!(error.message, "outer error");
    assert_eq!(vm.realm.global_env, primary.0);
    assert_eq!(vm.global(), primary.1);
    assert_eq!(vm.realm.object_prototype, primary.2[0]);
    assert_eq!(vm.realm.array_prototype, primary.2[2]);
    assert_eq!(vm.call_depth, 0);
    assert_eq!(vm.roots.len(), roots_before);
    vm.collect_garbage();
    assert!(vm.heap.realm_core(primary.0).is_some());
    assert!(vm.heap.realm_core(outer_env).is_none());
}

#[test]
fn current_core_success_leaves_parent_current_and_unpins_the_scope() {
    let mut vm = Vm::new();
    let primary = vm.global();
    let other_env = allocate_additional_core(&mut vm).unwrap();
    let other = stored_core(&vm, other_env);
    let before = vm.roots.len();
    let expected_fuel = vm.remaining - 1;
    let result = with_current_core(&mut vm, other_env, |scope| Ok(scope.current())).unwrap();
    assert_eq!(result, other);
    assert_eq!(vm.global(), primary);
    assert_eq!(vm.remaining, expected_fuel);
    assert_eq!(vm.roots.len(), before);
    vm.collect_garbage();
    assert!(vm.heap.realm_core(other_env).is_none());
}

#[test]
fn current_core_uses_shared_task_fuel_and_regular_gc_checkpoint() {
    let mut vm = Vm::new();
    let primary = vm.global();
    let other = allocate_additional_core(&mut vm).unwrap();
    let roots_before = vm.roots.len();
    vm.limits.max_instructions = 4;
    let error = vm
        .with_task(|vm| {
            vm.charge_work(1)?;
            with_current_core::<()>(vm, other, |scope| {
                assert!(scope.collect_garbage().skipped_active_execution);
                scope.charge_work(2)?;
                scope.with_core(other, |_| panic!("entry should have no fuel left"))
            })
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
    assert_eq!(vm.global(), primary);
    assert_eq!(vm.remaining, 0);
    assert_eq!(vm.call_depth, 0);
    assert_eq!(vm.roots.len(), roots_before);
}

#[test]
fn current_core_rejects_invalid_handles_and_checks_native_depth_before_entering() {
    let mut vm = Vm::new();
    let primary = vm.global();
    let stale = allocate_additional_core(&mut vm).unwrap();
    vm.collect_garbage();
    let foreign = Vm::new();
    let fuel_before = vm.remaining;
    let roots_before = vm.roots.len();
    let object_env = EnvId(vm.realm.global_object);
    for invalid in [stale, foreign.realm.global_env, object_env] {
        let error = with_current_core::<()>(&mut vm, invalid, |_| panic!("invalid core entered"))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::TypeError);
    }
    assert_eq!(vm.remaining, fuel_before);
    assert_eq!(vm.roots.len(), roots_before);
    assert_eq!(vm.global(), primary);

    let other = allocate_additional_core(&mut vm).unwrap();
    let selected = stored_core(&vm, other);
    let original_object =
        std::mem::replace(&mut vm.realm.global_object, selected.1.as_object().unwrap());
    let error = with_current_core::<()>(&mut vm, other, |_| panic!("inconsistent parent entered"))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::InternalError);
    assert_eq!(vm.global(), selected.1);
    assert_eq!(vm.roots.len(), roots_before);
    vm.realm.global_object = original_object;

    vm.limits.max_call_depth = 1;
    with_current_core(&mut vm, other, |scope| {
        assert_eq!(scope.current(), selected);
        let error = scope
            .with_core::<()>(other, |_| panic!("entry should exceed native depth"))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ExecutionLimit);
        assert_eq!(scope.current(), selected);
        Ok(())
    })
    .unwrap();
    assert_eq!(vm.global(), primary);
    assert_eq!(vm.call_depth, 0);
    assert_eq!(vm.roots.len(), roots_before);
}
