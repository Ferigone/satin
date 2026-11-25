use rust_browser_javascript::{ErrorKind, Value, Vm};

#[test]
fn malformed_identifier_and_string_escapes_point_at_the_offending_character() {
    let mut vm = Vm::new();
    for (source, column) in [
        (r"var a\u12G4 = 0;", 10),
        (r"var a\u{12G4} = 0;", 11),
        (r"var a\q = 0;", 7),
        (r#""\x0Q""#, 5),
        (r#""\u123""#, 7),
        ("\"\\u12\r\nvar adjacent = 0;", 6),
    ] {
        let error = vm.check_syntax(source).unwrap_err();
        assert_eq!(error.kind, ErrorKind::SyntaxError, "{source:?}");
        assert_eq!(
            (error.line, error.column),
            (Some(1), Some(column)),
            "{source:?}"
        );
    }
}

#[test]
fn successful_hex_escapes_still_consume_all_digits() {
    let mut vm = Vm::new();
    let result = vm
        .eval(r#"var a\u0062 = "\x41\u0042"; ab + '\u{43}'"#)
        .unwrap();
    assert_eq!(result, Value::from("ABC"));
}

#[test]
fn eval_preserves_isolated_source_units_in_literals_comments_and_function_spans() {
    let mut vm = Vm::new();
    let source = r#"
        let valid = true;
        for (const unit of ["\uD800", "\uDBFF", "\uDC00", "\uDFFF", "\uFFFD", "😀"]) {
            const fn = "function sample(){/*" + unit + "*/return '" + unit + "'}";
            const value = eval("(" + fn + ")");
            const literal = eval("/" + unit + "/");
            const identity = eval("/\\" + unit + "/");
            const cooked = eval("`" + unit + "`");
            const raw = eval("String.raw`" + unit + "`");
            valid = valid && value.toString() === fn && value() === unit &&
                literal.source === unit && literal.test(unit) &&
                identity.source === "\\" + unit &&
                cooked === unit && raw === unit &&
                eval("//" + unit + "\n42") === 42;
        }
        let invalid = false;
        try { eval("let a" + String.fromCharCode(0xd800) + " = 1"); }
        catch (e) { invalid = e instanceof SyntaxError; }
        valid && invalid
    "#;
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true));
}
