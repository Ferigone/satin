use rust_browser_javascript::{ErrorKind, Value, Vm};

fn small_stack(task: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name("parser-budget-1mib".into())
        .stack_size(1024 * 1024)
        .spawn(task)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn ordinary_grammar_compile_execute_and_drop_on_one_mib_stack() {
    small_stack(|| {
        let mut vm = Vm::new();
        assert_eq!(
            vm.eval("var a={b:{c:2}};function f(x){return x+1;}a.b.c+f(3)")
                .unwrap(),
            Value::Number(6.0)
        );
        // Start with modest accepted depths before the boundary tests. Each
        // check_syntax call parses, compiles and drops both AST and bytecode.
        for terms in [8, 16, 32, 63] {
            let arithmetic = std::iter::repeat_n("1", terms)
                .collect::<Vec<_>>()
                .join("+");
            vm.check_syntax(&arithmetic).unwrap();
            assert_eq!(vm.eval(&arithmetic).unwrap(), Value::Number(terms as f64));
            let member = format!("x{}", ".y".repeat(terms - 1));
            vm.check_syntax(&member).unwrap();
            let call = format!("f{}", "()".repeat(terms - 1));
            vm.check_syntax(&call).unwrap();
            let sequence = std::iter::repeat_n("1", terms)
                .collect::<Vec<_>>()
                .join(",");
            vm.check_syntax(&sequence).unwrap();
        }
        vm.check_syntax(&format!("obj{}++", ".x".repeat(61)))
            .unwrap();
        assert_eq!(
            vm.check_syntax(&format!("obj{}++", ".x".repeat(62)))
                .unwrap_err()
                .kind,
            ErrorKind::ExecutionLimit
        );
        let nested = format!(
            "function retained(){{return {};}}retained",
            std::iter::repeat_n("1", 60).collect::<Vec<_>>().join("+")
        );
        vm.check_syntax(&nested).unwrap();
        vm.eval(&nested).unwrap();
        vm.check_syntax("function retained(){return function(){return 3;};} retained")
            .unwrap();
        vm.eval("function retained(){return function(){return 3;};} retained")
            .unwrap();
        // VM destruction releases retained Code/Rc<Chunk> on this same stack.
        drop(vm);
    });
}

#[test]
fn long_valid_flat_chains_report_execution_limit_on_one_mib_stack() {
    small_stack(|| {
        let chains = [
            std::iter::repeat_n("1", 4096).collect::<Vec<_>>().join("+"),
            std::iter::repeat_n("true", 4096)
                .collect::<Vec<_>>()
                .join("&&"),
            std::iter::repeat_n("1", 4096).collect::<Vec<_>>().join(","),
            format!("obj{}", ".property".repeat(4096)),
            format!("obj{}", "[0]".repeat(4096)),
            format!("f{}", "()".repeat(4096)),
            format!("obj{}", ".method()[0]".repeat(2048)),
            format!("new obj{}()", ".property".repeat(4096)),
            format!("{}0", "a=".repeat(4096)),
            std::iter::repeat_n("1", 4096)
                .collect::<Vec<_>>()
                .join("**"),
            format!("{}1", "0?1:".repeat(4096)),
        ];
        for source in chains {
            let mut vm = Vm::new();
            let error = vm.check_syntax(&source).expect_err("valid oversized AST must be rejected before compilation/drop can exhaust the stack");
            assert_eq!(error.kind, ErrorKind::ExecutionLimit, "{error}");
            assert_eq!(
                vm.eval(&source).unwrap_err().kind,
                ErrorKind::ExecutionLimit
            );
        }
    });
}

#[test]
fn recursive_grammar_and_dynamic_function_share_the_budget() {
    small_stack(|| {
        let mut vm = Vm::new();
        for source in [
            format!("{}1{}", "(".repeat(128), ")".repeat(128)),
            format!("{}1;{}", "{".repeat(128), "}".repeat(128)),
            format!("{}true", "!".repeat(128)),
            format!("{}1{}", "[".repeat(128), "]".repeat(128)),
            format!("{}1{}", "function f(){return ".repeat(64), ";}".repeat(64)),
        ] {
            assert_eq!(
                vm.check_syntax(&source).unwrap_err().kind,
                ErrorKind::ExecutionLimit
            );
        }
        let body = std::iter::repeat_n("1", 4096).collect::<Vec<_>>().join("+");
        let source = format!("Function('return {body}')");
        assert_eq!(
            vm.eval(&source).unwrap_err().kind,
            ErrorKind::ExecutionLimit
        );
        // Unsupported valid syntax must retain its own category; resource guards
        // must never turn it into a SyntaxError that could pass a negative test.
        assert_eq!(
            vm.check_syntax("import('./example.js', {})")
                .unwrap_err()
                .kind,
            ErrorKind::UnsupportedFeature
        );
        assert_eq!(
            vm.check_syntax("var = ;").unwrap_err().kind,
            ErrorKind::SyntaxError
        );
    });
}
