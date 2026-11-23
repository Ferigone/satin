use rust_browser_javascript::{ErrorKind, Value, Vm};

#[test]
fn nested_new_keeps_constructor_arguments_and_members_in_order() {
    assert_eq!(
        Vm::new()
            .eval(
                r#"
                const seen = [];
                function argument(value) { seen.push(value); return value; }
                function Factory(first) {
                    seen.push(first + 10);
                    return function (second) {
                        seen.push(second + 10);
                        this.total = first + second;
                    };
                }
                function Bag() {
                    this.Build = function (value) { this.value = value; };
                }
                function WithTarget() { return new new.target.Build(4); }
                WithTarget.Build = function (value) { this.value = value; };
                const total = new new Factory(argument(1))(argument(2)).total;
                total === 3 && seen.join() === '1,11,2,12'
                    && new new Bag().Build(6).value === 6 && new WithTarget().value === 4
                "#,
            )
            .unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn nested_new_keeps_early_errors_separate_from_constructibility() {
    let mut vm = Vm::new();
    let source = "new new Boolean(true)";
    vm.check_syntax(source).unwrap();
    assert_eq!(vm.eval(source).unwrap_err().kind, ErrorKind::TypeError);
    for source in [
        "new new C()?.x",
        "new new import('')",
        "new new.target",
        "new new C().'x'",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}",
        );
    }
    vm.check_syntax("new new C()()?.x").unwrap();
}

#[test]
fn long_constructor_prefix_chains_observe_the_ast_limit() {
    let source = format!("{}Object", "new ".repeat(4096));
    assert_eq!(
        Vm::new().check_syntax(&source).unwrap_err().kind,
        ErrorKind::ExecutionLimit,
    );
}
