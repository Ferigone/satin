use rust_browser_javascript::{Value, Vm};

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
