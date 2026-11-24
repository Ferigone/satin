use rust_browser_javascript::{ErrorKind, Value, Vm};

#[test]
fn independent_heaps_cannot_resolve_each_others_object_handles() {
    let mut first = Vm::new();
    let mut second = Vm::new();
    let a = first.new_object();
    let b = second.new_object();
    first
        .set_property(a.clone(), "label", "first".into())
        .unwrap();
    second
        .set_property(b.clone(), "label", "second".into())
        .unwrap();

    assert_eq!(
        second.get_property(a.clone(), "label").unwrap_err().kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        first
            .set_property(b.clone(), "label", "foreign".into())
            .unwrap_err()
            .kind,
        ErrorKind::TypeError
    );
    assert_ne!(a, b);
    assert_eq!(
        first.get_property(a, "label").unwrap(),
        Value::from("first")
    );
    assert_eq!(
        second.get_property(b, "label").unwrap(),
        Value::from("second")
    );
}

#[test]
fn a_foreign_root_cannot_mark_a_local_heap_slot() {
    let mut first = Vm::new();
    let mut second = Vm::new();
    let foreign = first.new_object();
    let local = second.new_object();

    let root = second.root(foreign);
    second.collect_garbage();
    assert_eq!(
        second.get_property(local, "unused").unwrap_err().kind,
        ErrorKind::TypeError
    );
    second.unroot(root);
}

#[test]
fn shadow_wrappers_keep_their_realm_and_never_return_inner_objects() {
    let mut vm = Vm::new();
    vm.eval("var wrapper = new ShadowRealm().evaluate('let n = 0; () => ++n');")
        .unwrap();
    vm.collect_garbage();
    assert_eq!(vm.eval("wrapper() + wrapper()").unwrap(), 3.into());
    let error = vm
        .eval("new ShadowRealm().evaluate('() => ({})')()")
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::TypeError);
    assert_eq!(vm.eval("var r = new ShadowRealm(); r.evaluate('let x = 1; var y = 2'); r.evaluate('typeof x + y')").unwrap(), "undefined2".into());
}

#[test]
fn shadow_realm_subclass_falls_back_to_new_targets_realm() {
    let mut vm = Vm::new();
    let (realm, global) = vm.new_realm().unwrap();
    vm.set_global("other", global).unwrap();
    let ctor = vm
        .with_realm(realm, |vm| vm.eval("function N(){}; N.prototype = 0; N"))
        .unwrap();
    vm.set_global("N", ctor).unwrap();
    assert_eq!(
        vm.eval("Object.getPrototypeOf(Reflect.construct(ShadowRealm, [], N)) === other.ShadowRealm.prototype").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn revoking_a_wrapped_callable_precedes_argument_observation() {
    let mut vm = Vm::new();
    let calls = vm
        .eval(
            r#"
        const r = new ShadowRealm();
        const f = r.evaluate("var target = Proxy.revocable(() => 1, {}); target.proxy");
        r.evaluate("target.revoke()");
        let calls = 0;
        function argument() {}
        Object.defineProperty(argument, "name", {get() { calls++; return "x" }});
        try { f(argument); } catch(e) { if (!(e instanceof TypeError)) throw e }
        calls;
    "#,
        )
        .unwrap();
    assert_eq!(calls, 0.into());
}
