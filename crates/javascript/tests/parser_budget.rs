use rust_browser_javascript::{ErrorKind, Value, Vm};

fn small_stack(task: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(format!(
            "parser-budget-1mib:{}",
            std::thread::current().name().unwrap_or("unnamed")
        ))
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
fn standalone_blocks_still_charge_parser_recursion() {
    small_stack(|| {
        let source = format!(
            "function bump(x){{return x+1;}} let result; {}result = {}0{};{} result;",
            "{".repeat(17),
            "bump(".repeat(10),
            ")".repeat(10),
            "}".repeat(17),
        );
        let error = Vm::new().check_syntax(&source).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ExecutionLimit);
        assert_eq!(error.message, "Expression nesting limit exceeded");
    });
}

#[test]
fn bundled_callback_can_parse_a_nested_loop_condition() {
    small_stack(|| {
        // Mix the independently recursive statement and expression paths like
        // a minified event-removal callback inside a bundle initializer.
        let source = r#"
            var observed = 0;
            (function () {
                var listener;
                listener = { remove: function (record, test) {
                    if (record) {
                        if (test) {
                            if (false) {
                            } else {
                                for (var i = 0; i < 1; i++) {
                                    if (observed === 0)
                                        if (test) {
                                            for (var j = 0; j < 1; j++) {
                                                if (record.tag) {
                                                    if (false || test(record.tag)) {
                                                        observed += 1;
                                                        test(record.tag) && (observed += 2);
                                                    }
                                                }
                                            }
                                        }
                                }
                            }
                        }
                    }
                }};
                listener.remove({tag: 2}, function (tag) { return tag === 2; });
            })();
            observed;
        "#;
        let mut vm = Vm::new();
        vm.check_syntax(source).unwrap();
        assert_eq!(vm.eval(source).unwrap(), Value::Number(3.0));
    });
}

#[test]
fn recursive_calls_functions_and_classes_stay_bounded_on_one_mib_stack() {
    small_stack(|| {
        // Each shape keeps different parser frames live at the guard. Do
        // not assume that the same grammar depth means the same stack use.
        let cases = [
            ("calls", format!("{}0{}", "f(".repeat(128), ")".repeat(128))),
            (
                "function expressions with control blocks",
                format!(
                    "{}0{}",
                    "(function(){if(true){return ".repeat(64),
                    ";}})()".repeat(64)
                ),
            ),
            (
                "class expressions with methods",
                format!(
                    "var Root = {}0{};",
                    "class { m(){ return ".repeat(64),
                    "; } }".repeat(64)
                ),
            ),
        ];
        let mut vm = Vm::new();
        for (name, source) in cases {
            let error = vm.check_syntax(&source).expect_err(name);
            assert_eq!(error.kind, ErrorKind::ExecutionLimit, "{name}: {error}");
        }
        assert_eq!(vm.eval("2 + 3").unwrap(), Value::Number(5.0));
    });
}

#[test]
fn braced_controls_still_have_grammar_and_ast_limits_on_one_mib_stack() {
    small_stack(|| {
        let mut vm = Vm::new();
        let recursive = format!("{};{}", "while(false){".repeat(128), "}".repeat(128));
        let error = vm.check_syntax(&recursive).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ExecutionLimit);
        assert_eq!(error.message, "Expression nesting limit exceeded");

        // The flat expression fits its own AST and grammar budgets. Its
        // enclosing if/block pairs still contribute two AST levels each.
        for (terms, fits) in [(24, true), (40, false)] {
            let source = format!(
                "{}{}1;{}",
                "if(false){".repeat(16),
                "1+".repeat(terms - 1),
                "}".repeat(16)
            );
            if fits {
                vm.check_syntax(&source).unwrap();
                vm.eval(&source).unwrap();
            } else {
                let error = vm.check_syntax(&source).unwrap_err();
                assert_eq!(error.kind, ErrorKind::ExecutionLimit);
                assert_eq!(error.message, "AST nesting limit exceeded");
            }
        }
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
fn long_comma_sequences_preserve_effects_and_return_the_last_value() {
    small_stack(|| {
        let terms = std::iter::repeat_n("total += 1", 180)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            r#"
            let total = 0;
            const state = {{ get current() {{ return ++total; }}, label: 'present' }};
            function initialize() {{ return {terms}, state.current, total; }}
            const first = initialize();
            const second = ({terms}, state.current, total);
            const removed = delete (state.current, state.label);
            try {{ state.current, (() => {{ throw 'stop'; }})(), total += 100; }} catch {{}}
            first === 181 && second === 362 && total === 364 && removed && state.label === 'present';
            "#
        );
        assert_eq!(Vm::new().eval(&source).unwrap(), Value::Bool(true));
    });
}

#[test]
fn simple_assignment_chains_keep_reference_order_and_the_right_hand_value() {
    small_stack(|| {
        let targets = (0..28)
            .map(|index| format!("options.option{index}"))
            .collect::<Vec<_>>()
            .join(" = ");
        let source = format!(
            r#"
            const options = {{}};
            const log = [];
            function setValue(tag) {{
                log.push(tag);
                return {{ set value(value) {{ log.push(tag + value); }} }};
            }}
            function value() {{ log.push('rhs'); return 8; }}
            let a, b, c = 3, d;
            const shared = ({targets} = setValue('first').value = (setValue('second').value) = value());
            const mixed = a = c += b = 2;
            const record = d = {{ value: b }} = {{ value: 6 }};
            const extracted = b;
            a = b = function() {{}};
            shared === 8 && Object.keys(options).length === 28 && options.option0 === 8 &&
                options.option27 === 8 && log.join(',') === 'first,second,rhs,second8,first8' &&
                mixed === 5 && c === 5 && record === d && extracted === 6 && a === b && b.name === 'b';
            "#
        );
        assert_eq!(Vm::new().eval(&source).unwrap(), Value::Bool(true));
    });
}

#[test]
fn bundled_nested_data_uses_ast_depth_not_one_native_frame_per_literal() {
    small_stack(|| {
        // A bundler can emit a nested data tree in a factory passed to a chunk
        // registry. The resulting AST is bounded even though there are more
        // than 24 consecutive array and object literals.
        let source = format!(
            "var registry=[];registry.push([1,{{7:function(){{return {}42{};}}}}]);\
             let node=registry[0][1][7]();\
             for(let i=0;i<15;i++)node=node[0].child;node;",
            "[{child:".repeat(15),
            "}]".repeat(15),
        );
        let mut vm = Vm::new();
        vm.check_syntax(&source).unwrap();
        assert_eq!(vm.eval(&source).unwrap(), Value::Number(42.0));

        // The same cover grammar can become a parameter pattern; exercise
        // conversion, compilation and destruction close to the AST limit.
        let pattern = format!("{}value{}", "[{child:".repeat(30), "}]".repeat(30));
        let source = format!("const retained = ({pattern}) => value; retained;");
        vm.check_syntax(&source).unwrap();
        vm.eval(&source).unwrap();
    });
}

#[test]
fn nested_literal_values_resume_the_normal_expression_grammar() {
    small_stack(|| {
        let mut vm = Vm::new();
        assert_eq!(
            vm.eval(
                r#"
                const key = 'own';
                const prior = { inherited: 7 };
                let first, second;
                const values = [
                    [1,,3,][2] * 2,
                    {x:2}.x === 2 ? 11 : 0,
                    ...[15,16],
                    {...{a:17}, [key]:[18,], __proto__: {__proto__: prior}, get n(){return 19}},
                    {first} = {first: 20},
                    [second] = [21],
                ];
                const bind = ({a:[inner=22]}={a:[]}) => inner;
                values[0] === 6 && values[1] === 11 && values[2] === 15 &&
                    values[3] === 16 && values[4].a === 17 && values[4].own[0] === 18 &&
                    values[4].n === 19 && values[4].inherited === 7 &&
                    first === 20 && second === 21 && bind() === 22;
                "#,
            )
            .unwrap(),
            Value::Bool(true)
        );
        for invalid in ["[...[1],] = []", "[{p:1 q:2}]", "({a:[{x:1}]} => 1)"] {
            assert_eq!(
                vm.check_syntax(invalid).unwrap_err().kind,
                ErrorKind::SyntaxError,
                "{invalid}"
            );
        }
    });
}

#[test]
fn nested_literal_frames_and_mixed_grammar_remain_bounded() {
    small_stack(|| {
        let mut vm = Vm::new();
        let direct = format!("{}0{}", "[{child:".repeat(40), "}]".repeat(40));
        let mixed = format!("{}0{}", "[{child:f(".repeat(40), ")}]".repeat(40));
        for (source, message) in [
            (direct, "AST nesting limit exceeded"),
            (mixed, "Expression nesting limit exceeded"),
        ] {
            let error = vm.check_syntax(&source).unwrap_err();
            assert_eq!(error.kind, ErrorKind::ExecutionLimit);
            assert_eq!(error.message, message);
        }
        assert_eq!(vm.eval("[{x:[5]}][0].x[0]").unwrap(), Value::Number(5.0));
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
        vm.check_syntax("import('./example.js', {})").unwrap();
        assert_eq!(
            vm.check_syntax("var = ;").unwrap_err().kind,
            ErrorKind::SyntaxError
        );
    });
}
