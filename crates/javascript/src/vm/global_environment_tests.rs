use super::*;

#[test]
fn global_bindings_and_global_this_have_distinct_traced_objects() {
    let mut vm = Vm::new();
    let bindings = vm.global();
    let receiver = vm.new_object();
    vm.realm.global_env = vm.heap.allocate_env(Environment::new_global(
        bindings.as_object().unwrap(),
        receiver.as_object().unwrap(),
    ));
    // At this point the receiver is reachable only through the environment,
    // not through a property on the global or an explicit host root.
    vm.collect_garbage();
    vm.install_global_this().unwrap();

    assert_eq!(vm.global(), bindings);
    assert_eq!(vm.eval("this").unwrap(), receiver);
    assert_eq!(
        vm.eval(
            "var binding = 17;
             globalThis === this &&
             (() => this)() === this &&
             (function () { return this; })() === this &&
             (function () { 'use strict'; return this; })() === undefined"
        )
        .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(vm.get_property(bindings, "binding").unwrap(), 17.into());
    assert_eq!(
        vm.get_property(receiver.clone(), "binding").unwrap(),
        Value::Undefined
    );
    assert_eq!(
        vm.eval("delete globalThis; (function () { return this; })()")
            .unwrap(),
        receiver
    );
}

#[test]
fn escaped_native_function_traces_and_releases_its_owner_environment() {
    let mut vm = Vm::new();
    let first_global = vm.global();
    let bindings = vm.new_object();
    let receiver = vm.new_object();
    let owner = vm.heap.allocate_env(Environment::new_global(
        bindings.as_object().unwrap(),
        receiver.as_object().unwrap(),
    ));

    // Select the environment only for native allocation. This does not
    // bootstrap a second set of intrinsics or execute code in a new realm.
    let first_env = std::mem::replace(&mut vm.realm.global_env, owner);
    let first_object =
        std::mem::replace(&mut vm.realm.global_object, bindings.as_object().unwrap());
    let callback = vm.new_native_function("escaped", |_, _, _| Ok(Value::Undefined));
    vm.realm.global_object = first_object;
    vm.realm.global_env = first_env;
    assert_eq!(vm.global(), first_global);

    vm.set_property(bindings.clone(), "escaped", callback.clone())
        .unwrap();
    vm.set_property(receiver.clone(), "marker", 17.into())
        .unwrap();
    let root = vm.root(callback.clone());
    vm.collect_garbage();
    assert_eq!(
        vm.get_property(bindings.clone(), "escaped").unwrap(),
        callback
    );
    assert_eq!(
        vm.get_property(receiver.clone(), "marker").unwrap(),
        17.into()
    );

    // The function and its owner form a cycle, not permanent roots.
    vm.unroot(root);
    vm.collect_garbage();
    for object in [bindings, receiver, callback] {
        assert!(vm.heap.object(object.as_object().unwrap()).is_err());
    }
    assert!(vm.heap.object(first_object).is_ok());
}
