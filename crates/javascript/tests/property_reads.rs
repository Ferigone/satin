use rust_browser_javascript::{ErrorKind, Value, Vm};

#[test]
fn repeated_same_key_reads_fit_the_unchanged_task_budget() {
    let mut vm = Vm::new();
    let source = include_str!("fixtures/media_bootstrap_property_reads.js");
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true));
}

#[test]
fn own_data_cache_respects_updates_deletion_getters_receivers_and_collection() {
    let mut vm = Vm::new();
    let valid = vm.eval(r#"
        var base = { attribute: 4 };
        var target = Object.create(base);
        target.attribute = 1;
        function read(target) { return target.attribute; }
        var hits = 0;
        function check() {
            if (read(target) !== 1 || read(target) !== 1 || read(base) !== 4) return false;
            target.attribute = 2;
            if (read(target) !== 2 || read(target) !== 2) return false;
            Object.defineProperty(target, 'attribute', {
                configurable: true,
                get() { hits++; return this === target ? 3 : -1; }
            });
            if (read(target) !== 3 || read(target) !== 3 || hits !== 2) return false;
            delete target.attribute;
            if (read(target) !== 4) return false;
            base.attribute = 5;
            if (read(target) !== 5) return false;
            target.attribute = 6;
            if (read(target) !== 6 || read(target) !== 6) return false;
            const p = new Proxy(target, {
                get(t, k) { return t[k] + 10; }
            });
            return read(p) === 16 && read(target) === 6;
        }
        check();
    "#).unwrap();
    assert_eq!(valid, Value::Bool(true));
    vm.collect_garbage();
    assert_eq!(vm.eval("read(target)").unwrap(), Value::Number(6.0));
    assert_eq!(vm.eval("target=null; base=null;").unwrap(), Value::Null);
    vm.collect_garbage();
    assert_eq!(vm.eval("read({ attribute: 9 })").unwrap(), Value::Number(9.0));
}

#[test]
fn uncached_large_computed_keys_still_pay_work_and_coerce_once() {
    let mut vm = Vm::new();
    vm.eval(r#"
        var key = "long-key-".repeat(2000), calls = 0;
        var wrapper = { [Symbol.toPrimitive]() { ++calls; return key; } };
        var target = { [key]: 7 };
    "#).unwrap();
    assert_eq!(vm.eval("target[wrapper] + target[wrapper]").unwrap(), Value::Number(14.0));
    assert_eq!(vm.eval("calls").unwrap(), Value::Number(2.0));
    let error = vm.with_task_limits(4_000, None, |vm| vm.eval("target[key]")).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
    assert_eq!(error.message, "Native work budget exhausted");
}
