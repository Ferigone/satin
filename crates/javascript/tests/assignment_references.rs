use rust_browser_javascript::{Value, Vm};

fn assert_script(source: &str) {
    let result = Vm::new()
        .eval(source)
        .unwrap_or_else(|error| panic!("{error}\n{source}"));
    assert_eq!(result, Value::Bool(true), "{source}");
}

#[test]
fn strict_write_rechecks_a_resolved_global_binding() {
    for expression in ["x = (delete globalThis.x, 2)", "x += 1", "x++", "--x"] {
        assert_script(&format!(
            r#"
            Object.defineProperty(globalThis, "x", {{
                configurable: true,
                get() {{ delete globalThis.x; return 2; }}
            }});
            (function () {{
                "use strict";
                try {{ {expression}; }} catch (error) {{
                    return error instanceof ReferenceError && !("x" in globalThis);
                }}
                return false;
            }})();
            "#,
        ));
    }
}

#[test]
fn sloppy_global_and_strict_property_writes_can_recreate_a_deleted_property() {
    assert_script(
        r#"
        Object.defineProperty(globalThis, "x", {
            configurable: true,
            get() { delete globalThis.x; return 2; }
        });
        x += 1;
        var object = {
            get x() { delete this.x; return 4; }
        };
        (function () { "use strict"; object.x++; })();
        globalThis.x === 3 && object.x === 5;
        "#,
    );
}

#[test]
fn simple_assignment_defers_key_coercion_and_nullish_base_errors() {
    assert_script(
        r#"
        const events = [];
        const key = { toString() { events.push("key coercion"); return "x"; } };
        const object = {};
        object[(events.push("key expression"), key)] = (events.push("rhs"), 3);
        try {
            null[key] = (events.push("null rhs"), 4);
        } catch (error) {
            events.push(error instanceof TypeError);
        }
        object.x === 3 && events.join("/") === "key expression/rhs/key coercion/null rhs/true";
        "#,
    );
}

#[test]
fn compound_update_and_delete_use_the_computed_key_once() {
    assert_script(
        r#"
        const events = [];
        const symbol = Symbol();
        const key = { [Symbol.toPrimitive]() { events.push("key"); return symbol; } };
        const object = { [symbol]: 1 };
        const sum = object[key] += (events.push("rhs"), 2);
        const old = object[key]++;
        const next = ++object[key];
        const skipped = object[key] ||= (events.push("skipped rhs"), 6);
        const deleted = delete object[key];
        sum === 3 && old === 3 && next === 5 && skipped === 5 && deleted &&
            !(symbol in object) && events.join("/") === "key/rhs/key/key/key/key";
        "#,
    );
}

#[test]
fn destructuring_gets_the_source_value_before_coercing_the_target_key() {
    assert_script(
        r#"
        const events = [];
        const target = {};
        const key = { toString() { events.push("coerce"); return "x"; } };
        const source = { get x() { events.push("read"); return 3; } };
        ({ x: target[(events.push("target"), key)] } = source);
        target.x === 3 && events.join("/") === "target/read/coerce";
        "#,
    );
}

#[test]
fn suspended_assignment_keeps_the_uncoerced_key_alive() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var called = false;
            var object = {};
            var iterator = (function* () {
                object[{ toString() { called = true; return "x"; } }] = yield "ready";
            })();
            iterator.next().value === "ready" && !called;
            "#,
        )
        .unwrap(),
        Value::Bool(true)
    );
    vm.collect_garbage();
    assert_eq!(
        vm.eval("iterator.next(7).done && called && object.x === 7")
            .unwrap(),
        Value::Bool(true)
    );
}
