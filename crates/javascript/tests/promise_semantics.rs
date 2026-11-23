use rust_browser_javascript::{Value, Vm};

#[test]
fn with_resolvers_defines_own_data_properties_without_using_inherited_setters() {
    let mut vm = Vm::new();
    let result = vm
        .eval(
            r#"
            var calls = 0;
            Object.defineProperty(Object.prototype, "promise", {
                set: function () { ++calls; },
                configurable: true
            });
            Object.defineProperty(Object.prototype, "resolve", {
                value: 0,
                configurable: true
            });
            var record = Promise.withResolvers();
            calls === 0 && Object.keys(record).join(",") === "promise,resolve,reject" &&
                ["promise", "resolve", "reject"].every(function (key) {
                    var desc = Object.getOwnPropertyDescriptor(record, key);
                    return desc.writable && desc.enumerable && desc.configurable &&
                        (key === "promise" ? desc.value instanceof Promise
                                           : typeof desc.value === "function");
                });
            "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn final_aggregate_element_returns_custom_capability_result() {
    let mut vm = Vm::new();
    let result = vm
        .eval(
            r#"
            function check(combine, firstRejects, lastRejects, expectedCall) {
                var callbacks = [], completion, marker = {};
                function C(executor) {
                    executor(function () { completion = "resolve"; return marker; },
                             function () { completion = "reject"; return marker; });
                }
                C.resolve = function () {
                    return { then: function (resolve, reject) {
                        callbacks.push({ ok: resolve, bad: reject });
                    } };
                };

                var returned = combine.call(C, [1, 2]);
                var first = firstRejects ? callbacks[0].bad : callbacks[0].ok;
                var last = lastRejects ? callbacks[1].bad : callbacks[1].ok;
                return returned instanceof C && first(1) === undefined &&
                    first(9) === undefined && completion === undefined &&
                    last(2) === marker && completion === expectedCall &&
                    last(9) === undefined;
            }
            check(Promise.all, false, false, "resolve") &&
                check(Promise.allSettled, true, false, "resolve") &&
                check(Promise.allSettled, false, true, "resolve") &&
                check(Promise.any, true, true, "reject");
            "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}
