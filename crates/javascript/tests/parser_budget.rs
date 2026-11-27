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
fn ordinary_method_call_chains_keep_receiver_and_evaluation_order_on_a_small_stack() {
    small_stack(|| {
        let calls = ".put(next())".repeat(240);
        let script = format!(r#"
            let seq = 0, total = 0;
            function next() {{ return ++seq; }}
            const builder = {{ get put() {{
                const expected = seq + 1;
                return function(value) {{
                    if (this !== builder || value !== expected) throw 'order or receiver';
                    total += value;
                    return this;
                }};
            }}, build() {{ return this === builder ? total : -1; }} }};
            builder{calls}.build()
        "#);
        let mut vm = Vm::new();
        vm.check_syntax(&script).unwrap();
        assert_eq!(vm.eval(&script).unwrap(), Value::Number(28_920.0));
        assert_eq!(
            vm.eval(r#"
                class Parent { start() { return this; } }
                class Child extends Parent {
                    run() { return super.start().finish().finish() === this; }
                    finish() { return this; }
                }
                let child = new Child(), missing = null, effects = 0;
                missing?.start(++effects).finish(++effects);
                let threw = false;
                try { (missing?.start)(++effects).finish(); } catch (e) { threw = true; }
                child.run() && threw && effects === 1 && (child.finish().finish).call(child) === child
            "#).unwrap(),
            Value::Bool(true)
        );
    });
}

#[test]
fn nested_first_arguments_preserve_callee_lookup_and_later_argument_order() {
    small_stack(|| {
        let mut vm = Vm::new();
        let deep = format!(
            "function plus(a,b){{return a+b}} {}0{}",
            "plus(".repeat(180),
            ", 1)".repeat(180)
        );
        vm.check_syntax(&deep).unwrap();
        assert_eq!(vm.eval(&deep).unwrap(), Value::Number(180.0));
        assert_eq!(vm.eval(r#"
            var log = '', ns = { get f() {
                log += 'g';
                return function(a,b) { log += this === ns ? 't' : 'x'; return a+b; };
            } };
            function side(s) { log += s; return 1; }
            var sum;
            with (ns) { sum = f(f(f(side('a'), side('b')), ...[side('c')]), side('d')); }
            function lexical() { let secret = 42; return plus(eval('secret'), 0); }
            sum === 4 && log === 'gggabtctdt' && lexical() === 42
        "#).unwrap(), Value::Bool(true));
        assert_eq!(vm.eval(r#"
            function id(x) { return x; }
            function obj(x) { return { add(y) { return x+y } }; }
            plus(id(obj(4).add(2)), 3) === 9
        "#).unwrap(), Value::Bool(true));
    });
}

#[test]
fn standalone_blocks_still_charge_parser_recursion() {
    small_stack(|| {
        // Parenthesized arguments still recurse through cover grammar.
        let source = format!(
            "function bump(x){{return x+1;}} let result; {}result = {}0{};{} result;",
            "{".repeat(17),
            "bump((".repeat(10),
            "))".repeat(10),
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
fn nested_iterator_conditions_in_a_bundle_do_not_recurse_for_assignments() {
    small_stack(|| {
        let source = r#"
            var answer = 0, hooks = {};
            ([0], function () {
              if (hooks) {
                hooks.setup = function () {
                  var make = function () {
                    if (hooks) {
                      hooks.visit = Object.freeze({scan: function (groups) {
                        try {
                          for (var a = groups[Symbol.iterator](), row, outer; !(outer = (row = a.next()).done);) {
                            try {
                              for (var b = row.value[Symbol.iterator](), cell, inner; !(inner = (cell = b.next()).done);) {
                                answer += cell.value;
                              }
                            } finally {}
                          }
                        } finally {}
                      }}).scan;
                    }
                  }();
                };
              }
            })();
            hooks.setup(); hooks.visit([[10, 20], [12]]); answer;
        "#;
        assert_eq!(Vm::new().eval(source).unwrap(), Value::Number(42.0));
    });
}

#[test]
fn nested_callback_arguments_compile_and_execute_on_one_mib_stack() {
    small_stack(|| {
        // A returned call often receives an object with another rendering callback.
        let source = format!(
            "const queue=[]; function enqueue(options){{queue.push(options.read);}} \
             {}42{}; let result; while(queue.length) result=queue.pop()(); result;",
            "enqueue({read: function(){return ".repeat(8),
            ";}})".repeat(8),
        );
        assert_eq!(Vm::new().eval(&source).unwrap(), Value::Number(42.0));
    });
}

#[test]
fn recursive_calls_functions_and_classes_stay_bounded_on_one_mib_stack() {
    small_stack(|| {
        // Each shape keeps different parser frames live at the guard. Do
        // not assume that the same grammar depth means the same stack use.
        let cases = [
            ("member calls", format!("{}0{}", "o.f(".repeat(128), ")".repeat(128))),
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
            format!("{}1{}", "0?".repeat(4096), ":1".repeat(4096)),
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
fn conditional_alternates_select_one_arm_on_one_mib_stack() {
    small_stack(|| {
        let arms = (0..180)
            .map(|i| format!("visit({i}) ? selected({i}) : "))
            .collect::<String>();
        let source = format!(r#"
            let visits = 0, selections = 0, target = 137;
            function visit(i) {{ visits++; return i === target; }}
            function selected(i) {{ selections++; return i; }}
            function pick() {{ return {arms} -1; }}
            const first = pick(); target = 200;
            first === 137 && visits === 138 && pick() === -1 && visits === 318 && selections === 1;
        "#);
        assert_eq!(Vm::new().eval(&source).unwrap(), Value::Bool(true));
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


#[test]
fn nested_identifier_expression_statements_use_their_expression_budget() {
    small_stack(|| {
        let script = r#"
            let count = 0;
            const collect = () => {
                const load = (headers) => (0, (() => {
                    return new Promise(((resolve) => {
                        const send = () => {
                            count += 1, (() => {
                                const result = ((values) => {
                                    const receiver = {open(){}, set(key, value){count += value;}};
                                    return receiver.open(), Object.keys(values).forEach((key => {
                                        receiver.set(key, values[key]);
                                    })), receiver;
                                })(headers);
                                resolve(result);
                            })();
                        };
                        send();
                    }));
                })());
                return load;
            };
            collect()({a: 3});
            count;
        "#;
        assert_eq!(Vm::new().eval(script).unwrap(), Value::Number(4.0));
    });
}

#[test]
fn else_if_ladders_remain_flat_through_execution() {
    small_stack(|| {
        let mut vm = Vm::new();
        let alternatives = (0..40)
            .map(|n| format!("if (++seen === {n}) {n}; else "))
            .collect::<String>();
        let source = format!("var seen=0; {alternatives} 99;");
        assert_eq!(vm.eval(&source).unwrap(), Value::Number(99.0));
        assert_eq!(vm.eval("seen").unwrap(), Value::Number(40.0));
        // The unbraced inner if consumes its own else before the outer tail.
        assert_eq!(
            vm.eval("if (true) if (false) 1; else 2; else if (true) 3;").unwrap(),
            Value::Number(2.0)
        );
        let wide = format!("{} 7;", "if(false) 1; else ".repeat(512));
        assert_eq!(vm.eval(&wide).unwrap(), Value::Number(7.0));
        let generator = r#"
            function* select() {
                if (yield 1) { return 11; }
                else if (yield 2) { return yield 22; }
                else { return 33; }
            }
            const it = select();
            it.next().value === 1 && it.next(false).value === 2 &&
                it.next(true).value === 22 && it.next(44).value === 44;
        "#;
        assert_eq!(vm.eval(generator).unwrap(), Value::Bool(true));
    });
}

#[test]
fn large_strict_scripts_validate_unused_functions_and_preserve_closures() {
    small_stack(|| {
        let padding = format!("/*{}*/", " ".repeat(64 * 1024));
        let mut vm = Vm::new();
        vm.eval("var observed = 0").unwrap();
        let invalid = format!(r#"{padding}"use strict";
            observed = 1;
            (function() {{ function unused(a) {{ let a; }} }})();
        "#);
        assert_eq!(vm.eval(&invalid).unwrap_err().kind, ErrorKind::SyntaxError);
        assert_eq!(vm.eval("observed").unwrap(), Value::Number(0.0));
        let valid = format!(r#"{padding}"use strict";
            (function(seed) {{
                function Counter(value) {{ this.value = value; }}
                function factory(offset) {{
                    return function step(n) {{ return n ? step(n-1)+1 : seed+offset; }};
                }}
                var named = function inner() {{ return inner.name }};
                var counter = new Counter(3);
                var a = factory(counter.value), b = factory(10);
                return a(2)===12 && b(1)===18 && named()==="inner" &&
                    factory.length===1 && /function factory\(offset\)/.test(factory.toString());
            }})(7)
        "#);
        assert_eq!(vm.eval(&valid).unwrap(), Value::Bool(true));
    });
}

#[test]
fn long_string_literal_sums_preserve_utf16_and_do_not_form_directive_prologues() {
    let mut vm = Vm::new();
    let parts = std::iter::repeat_n(r#"'\ud800'"#, 1024).collect::<Vec<_>>().join("+");
    let source = format!("var s = {parts}; function f() {{ 'use '+'strict'; return this; }} s.length === 1024 && s.charCodeAt(1023) === 55296 && f() === globalThis && ('a' + '2' * 3) === 'a6'");
    assert_eq!(vm.eval(&source).unwrap(), Value::Bool(true));
}

#[test]
fn deferred_functions_distinguish_sloppy_and_nested_strict_scopes() {
    small_stack(|| {
        let padding = format!("/*{}*/", " ".repeat(64 * 1024));
        let script = format!(r#"{padding}(function() {{
            function sloppy(a) {{
                arguments[0] = 5;
                with ({{ b: 4 }}) {{ return a + b; }}
            }}
            function localStrict() {{
                "use strict";
                function inherited() {{ return this; }}
                return inherited() === undefined;
            }}
            function Outer(n) {{
                this.read = function() {{ return function(value) {{ return n + value; }}; }};
            }}
            return sloppy(1) === 9 && localStrict() && new Outer(12).read()(3) === 15;
        }})()"#);
        let mut vm = Vm::new();
        assert_eq!(vm.eval(&script).unwrap(), Value::Bool(true));
        let invalid = format!(r#"{padding}(function() {{
            function strictOuter() {{
                'use strict';
                function unused() {{ with ({{}}) {{}} }}
            }}
        }})()"#);
        assert_eq!(vm.check_syntax(&invalid).unwrap_err().kind, ErrorKind::SyntaxError);
    });
}

#[test]
fn nested_grouped_assignment_method_calls_preserve_values_on_one_mib_stack() {
    small_stack(|| {
        // Minifiers emit repeated parenthesized assignments as method receivers.
        // OpenStreetMap's public date formatter uses 25 of these in one return.
        let mut expression = String::from("text");
        for _ in 0..25 {
            expression = format!("(text={expression}.replace('a','b'))");
        }
        let script = format!(
            r#"(function(){{
                class Formatter {{
                    format(text) {{
                        return function(text) {{
                            const result = {expression}.replace('b','c');
                            return text === 'b'.repeat(25) && result === 'c' + 'b'.repeat(24);
                        }}(text);
                    }}
                }}
                return new Formatter().format('a'.repeat(25));
            }})()"#
        );
        let mut vm = Vm::new();
        vm.check_syntax(&script).unwrap();
        assert_eq!(vm.eval(&script).unwrap(), Value::Bool(true));
    });
}
