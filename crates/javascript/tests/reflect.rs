use rust_browser_javascript::{ErrorKind, Value, Vm};

#[test]
fn reflect_prototype_changes_report_rejection_without_throwing() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            "use strict";
            const root = Object.create(null);
            const child = {};
            Reflect.getPrototypeOf(root) === null &&
                Reflect.setPrototypeOf(child, root) === true &&
                Reflect.getPrototypeOf(child) === root &&
                Reflect.setPrototypeOf(root, child) === false &&
                Reflect.setPrototypeOf(root, root) === false &&
                Reflect.getPrototypeOf(root) === null &&
                Reflect.isExtensible(child) === true &&
                Reflect.preventExtensions(child) === true &&
                Reflect.isExtensible(child) === false &&
                Reflect.setPrototypeOf(child, root) === true &&
                Reflect.setPrototypeOf(child, null) === false &&
                Reflect.getPrototypeOf(child) === root &&
                Reflect.setPrototypeOf(Object.prototype, root) === false &&
                Reflect.setPrototypeOf(Object.prototype, null) === true &&
                Reflect.isExtensible(Object.prototype) === true
            "#
        )
        .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        vm.eval("Object.setPrototypeOf(Object.preventExtensions({}), null)")
            .unwrap_err()
            .kind,
        ErrorKind::TypeError
    );
}

#[test]
fn reflect_prototype_and_extensibility_operations_require_objects() {
    for source in [
        "Reflect.getPrototypeOf('text')",
        "Reflect.isExtensible(1)",
        "Reflect.setPrototypeOf(false, null)",
        "Reflect.setPrototypeOf({}, Symbol())",
        "Reflect.setPrototypeOf({})",
    ] {
        assert_eq!(
            Vm::new().eval(source).unwrap_err().kind,
            ErrorKind::TypeError,
            "{source}"
        );
    }
}

#[test]
fn reflect_has_an_own_to_string_tag() {
    assert_eq!(
        Vm::new()
            .eval(
                r#"
                const tag = Object.getOwnPropertyDescriptor(Reflect, Symbol.toStringTag);
                Object.prototype.toString.call(Reflect) === "[object Reflect]" &&
                    tag.value === "Reflect" && !tag.writable &&
                    !tag.enumerable && tag.configurable
                "#
            )
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn proxy_traps_preserve_receivers_target_invariants_and_revocation() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
        const key = Symbol();
        const target = { get who() { return this; }, [key]: 7 };
        const log = [];
        const proxy = new Proxy(target, {
            get(t,k,r) { log.push(k); return Reflect.get(t,k,r); }
        });
        const child = Object.create(proxy);
        const pair = Proxy.revocable([], {});
        let ok = child.who === child && proxy[key] === 7 && log[1] === key &&
                 Array.isArray(pair.proxy);
        Object.defineProperty(target, "locked", {value: 4});
        try { new Proxy(target, { get() {return 5;} }).locked; ok=false; }
        catch(e) { ok = ok && e instanceof TypeError; }
        pair.revoke(); pair.revoke();
        try { Array.isArray(pair.proxy); ok=false; }
        catch(e) { ok = ok && e instanceof TypeError; }
        ok
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}
