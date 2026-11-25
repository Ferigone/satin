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

#[test]
fn blocks_without_declarations_do_not_allocate_iteration_environments() {
    let mut vm = rust_browser_javascript::Vm::new();
    vm.eval("var n = 0;").unwrap();
    let initial = vm.heap_cell_count();
    vm.eval("while(n < 300) { { n++; } }").unwrap();
    assert!(vm.heap_cell_count() - initial < 30);
    assert_eq!(vm.eval("n").unwrap(), rust_browser_javascript::Value::from(300));
    assert_eq!(
        vm.eval("function f(){ { eval('var seen = 7'); } return () => seen; } f()()").unwrap(),
        rust_browser_javascript::Value::from(7)
    );
    assert_eq!(
        vm.eval("var fs = []; for (let j=0; j<2; ++j) { { let saved=j; fs.push(() => saved); } } fs[0]() + fs[1]()").unwrap(),
        rust_browser_javascript::Value::from(1)
    );
}
