use rust_browser_javascript::{Value, Vm};

#[test]
fn first_for_iteration_does_not_mutate_the_initializer_binding() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            let fromInitializer, fromBody;
            for (let i = 0, capture = (fromInitializer = () => i);
                 i < 2;
                 i++) {
                fromBody = () => i;
                i++;
            }
            fromInitializer() + ":" + fromBody();
            "#,
        )
        .unwrap(),
        Value::from("0:1")
    );
}

#[test]
fn resumable_for_copies_the_binding_after_the_initializer_resumes() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            function* run() {
                let fromInitializer;
                for (let i = yield "initializing", capture = (fromInitializer = () => i);
                     i < 2;
                     i++) {
                    i++;
                    yield i;
                }
                return fromInitializer();
            }
            let it = run();
            [it.next().value, it.next(0).value, it.next().value].join(",");
            "#,
        )
        .unwrap(),
        Value::from("initializing,1,0")
    );
}
