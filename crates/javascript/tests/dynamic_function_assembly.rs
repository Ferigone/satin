use rust_browser_javascript::{Value, Vm};

#[test]
fn dynamic_function_finishes_argument_coercion_before_checking_source_encoding() {
    assert_eq!(
        Vm::new()
            .eval(
                r#"
        function check(construct) {
            var trace = '', marker = {}, caught;
            var first = { toString: function () { trace += 'first;'; return '/*\uD800*/a'; } };
            var later = { toString: function () { trace += 'later;'; return 'b'; } };
            var body = { toString: function () { trace += 'body;'; throw marker; } };
            try {
                if (construct) new Function(first, later, body);
                else Function(first, later, body);
            } catch (error) {
                caught = error;
            }
            return trace === 'first;later;body;' && caught === marker;
        }
        check(false) && check(true);
    "#,
            )
            .unwrap(),
        Value::Bool(true),
    );
}
