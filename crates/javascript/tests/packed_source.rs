use rust_browser_javascript::{ErrorKind, Value, Vm};

#[test]
fn large_mixed_source_preserves_slices_and_function_reflection() {
    let before = format!("/*{}*/\n", "a".repeat(65_310));
    let function = "function tést(v){ const c = '🙂'; const r = /ł/u; return r.test(v) && c.length === 2 && 0x12 === 18; }";
    let source = format!(r#"{before}
        {function}
        tést('ł') && tést.toString() === {function:?};
    "#);
    let mut vm = Vm::new();
    assert_eq!(vm.eval(&source).unwrap(), Value::Bool(true));
    vm.collect_garbage();
    assert_eq!(vm.eval("tést('ł') && !tést('l')").unwrap(), Value::Bool(true));
}

#[test]
fn unicode_line_terminators_keep_their_original_error_position() {
    let source = format!("/*{}🙂*/\u{2028}const answer = '\\xQ0';", "a".repeat(66_000));
    let mut vm = Vm::new();
    let error = vm.check_syntax(&source).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SyntaxError);
    assert_eq!(error.line, Some(2));
    assert_eq!(error.column, Some(19));
}
