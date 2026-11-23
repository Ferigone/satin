use super::*;

#[test]
fn global_bindings_and_global_this_have_distinct_traced_objects() {
    let mut vm = Vm::new();
    let bindings = vm.global();
    let receiver = vm.new_object();
    vm.global_env = vm.heap.allocate_env(Environment::new_global(
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
