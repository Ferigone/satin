use rust_browser_javascript::{Value, Vm};

fn assert_js(source: &str) {
    assert_eq!(
        Vm::new().eval(source).unwrap(),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn native_error_prototypes_own_the_default_message() {
    assert_js(
        r#"
        var constructors = [EvalError, RangeError, ReferenceError, SyntaxError, TypeError, URIError];
        Error.prototype.message = "base message";
        var correct = true;
        for (var i = 0; i < constructors.length; i++) {
            var ctor = constructors[i];
            var proto = ctor.prototype;
            var desc = Object.getOwnPropertyDescriptor(proto, "message");
            correct = correct && !!desc && desc.value === "" && desc.writable &&
                !desc.enumerable && desc.configurable &&
                !Object.prototype.hasOwnProperty.call(ctor(), "message") &&
                ctor().toString() === ctor.name;
        }
        var removed = delete TypeError.prototype.message;
        correct && removed && new TypeError().toString() === "TypeError: base message" &&
            new RangeError().toString() === "RangeError"
    "#,
    );
}
