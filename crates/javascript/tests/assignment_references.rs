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
