use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

#[test]
fn values_and_entries_snapshot_keys_but_check_live_own_enumerability() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            function source() {
                var object = Object.create({
                    get removed() { throw new Error('An inherited value must not be read'); }
                });
                Object.defineProperty(object, 'first', {
                    enumerable: true,
                    get: function () {
                        if (this !== object) throw new Error('Wrong receiver');
                        delete this.removed;
                        Object.defineProperty(this, 'hidden', {enumerable: false});
                        Object.defineProperty(this, 'shown', {enumerable: true});
                        this.added = 'added';
                        return 'first';
                    }
                });
                Object.defineProperty(object, 'removed', {
                    value: 'removed', enumerable: true, configurable: true
                });
                object.hidden = 'hidden';
                Object.defineProperty(object, 'shown', {value: 'shown', configurable: true});
                return object;
            }
            var values = Object.values(source());
            var entries = Object.entries(source());
            values.join(',') === 'first,shown' &&
                entries.length === 2 &&
                entries[0].join(',') === 'first,first' &&
                entries[1].join(',') === 'shown,shown';
            "#,
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn values_and_entries_charge_work_even_when_no_keys_are_enumerable() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 256,
        ..VmLimits::default()
    });
    let target = vm.new_object();
    vm.set_global("target", target.clone()).unwrap();
    let calls = [
        "Object.values(target).length",
        "Object.entries(target).length",
    ];
    for call in calls {
        assert_eq!(vm.eval(call).unwrap(), Value::from(0));
    }
    // Build the input outside the script task so its creation cannot exhaust fuel.
    for i in 0..1024 {
        vm.define_data_property(
            target.clone(),
            &format!("hidden{i}"),
            Value::Null,
            true,
            false,
            true,
        )
        .unwrap();
    }
    for call in calls {
        let error = vm.eval(call).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ExecutionLimit, "{call}: {error}");
    }
}
