use rust_browser_javascript::{ErrorKind, Value, Vm};

#[test]
fn comma_sequences_may_start_with_a_bare_arrow() {
    for (source, expected) in [
        ("'use strict'; var mark = 0; {} () => 1, mark = 7; mark;", 7),
        ("var mark = 0; x => x, mark = 9; mark;", 9),
        ("var mark = 0; async x => x, mark = 5; mark;", 5),
        ("var mark = 0; async (x) => {}, mark = 6; mark;", 6),
        ("function f() { return () => {}, 11; } f();", 11),
        ("'use strict'; eval('class C {}() => 1, 42;');", 42),
    ] {
        assert_eq!(Vm::new().eval(source).unwrap(), Value::from(expected));
    }
}

#[test]
fn bare_arrows_are_not_binary_or_postfix_operands() {
    let mut vm = Vm::new();
    for source in ["() => {} + 1;", "x => {}();", "async () => {}.name;"] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    assert_eq!(vm.eval("(() => 21)()").unwrap(), Value::from(21));
    assert_eq!(
        vm.eval("function f(a, b) { return a() + b; } f(() => 2, 3);")
            .unwrap(),
        Value::from(5)
    );
}
