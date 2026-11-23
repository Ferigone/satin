use rust_browser_javascript::{Value, Vm};

#[test]
fn for_in_skips_unvisited_deletions_and_does_not_repeat_shadowed_names() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var prototype = {removed: 1, reveal: 1, hidden: 1, repeated: 1};
            var object = Object.create(prototype);
            object.first = 1;
            object.deleted = 1;
            object.reveal = 1;
            object.repeated = 1;
            Object.defineProperty(object, 'hidden', {value: 1});
            var keys = [];
            for (var key in object) {
                keys.push(key);
                if (key === 'first') {
                    delete object.deleted;
                    delete object.reveal;
                    delete prototype.removed;
                    object.newKey = 1;
                } else if (key === 'repeated') {
                    delete object.repeated;
                }
            }
            keys.join(',');
            "#,
        )
        .unwrap(),
        Value::from("first,repeated,reveal")
    );
}

#[test]
fn for_in_checks_current_enumerability_without_invoking_property_getters() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var object = {
                first: 1,
                get second() { throw 'should not read property values'; },
                get last() { throw 'should not read property values'; }
            };
            var keys = [];
            for (var key in object) {
                keys.push(key);
                if (key === 'first') {
                    Object.defineProperty(object, 'second', {enumerable: false});
                }
            }
            keys.join(',');
            "#,
        )
        .unwrap(),
        Value::from("first,last")
    );
}

#[test]
fn suspended_for_in_retains_its_source_and_checks_for_deletion_after_resume() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var exposed;
            function source() {
                var object = Object.create({tail: 1, repeated: 1});
                object.first = 1;
                object.deleted = 1;
                object.repeated = 1;
                return exposed = object;
            }
            function* enumerate() {
                for (var key in source()) yield key;
            }
            var generator = enumerate();
            generator.next().value;
            "#,
        )
        .unwrap(),
        Value::from("first")
    );
    vm.eval("delete exposed.deleted; exposed = null;").unwrap();
    assert!(!vm.collect_garbage().skipped_active_execution);
    assert_eq!(
        vm.eval(
            "[generator.next().value, generator.next().value, generator.next().done].join(',');",
        )
        .unwrap(),
        Value::from("repeated,tail,true")
    );
}
