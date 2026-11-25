use rust_browser_javascript::{Value, Vm};

#[test]
fn fractional_radices_in_polyfill_identifiers() {
    let mut vm = Vm::new();
    let result = vm
        .eval(
            r#"
            const stringify = Function.call.bind(Number.prototype.toString);
            const seed = 0.25;
            stringify(1 + seed, 36) === "1.9" &&
            stringify(2 + seed, 36) === "2.9" &&
            (0.1).toString(36) === "0.3lllllllllm" &&
            (0.1).toString(16) === "0.1999999999999a" &&
            (0.5).toString(3) === "0." + "1".repeat(33) + "2" &&
            (1 / 3).toString(3) === "0.1" &&
            (-31.5).toString(36) === "-v.i"
            "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn radix_output_at_binary64_boundaries() {
    let mut vm = Vm::new();
    let result = vm
        .eval(
            r#"
            (-0).toString(2) === "0" &&
            Number.MIN_VALUE.toString(2) === "0." + "0".repeat(1073) + "1" &&
            (2 ** -1022).toString(16) === "0." + "0".repeat(255) + "4" &&
            (1 - 2 ** -53).toString(2) === "0." + "1".repeat(53) &&
            (1 + 2 ** -52).toString(2) === "1." + "0".repeat(51) + "1" &&
            (2 ** 64).toString(16) === "10000000000000000" &&
            Number.MAX_VALUE.toString(2) === "1".repeat(53) + "0".repeat(971) &&
            Infinity.toString(36) === "Infinity" && NaN.toString(16) === "NaN"
            "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}
