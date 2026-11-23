use rust_browser_javascript::{ErrorKind, Value, Vm};

#[test]
fn bare_rest_or_spread_is_not_an_expression() {
    let mut vm = Vm::new();
    for source in [
        "var af = ...x => x;",
        "'use strict'; var af = ...x => x;",
        "function f() { return ...xs; }",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
}

#[test]
fn parameter_rest_and_list_spread_are_valid() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("'use strict'; var collect = (...items) => [...items, 4]; collect(...[1, 2, 3]).join(',');")
            .unwrap(),
        Value::from("1,2,3,4")
    );
    assert_eq!(
        vm.eval("function f(...xs) { return xs.length; } f(...[1, 2]);")
            .unwrap(),
        Value::from(2)
    );
    vm.check_syntax("async (...args) => args[0];").unwrap();
}
