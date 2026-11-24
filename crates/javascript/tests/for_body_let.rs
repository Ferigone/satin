use rust_browser_javascript::{ErrorKind, Value, Vm};

#[test]
fn ungrouped_let_bracket_cannot_start_a_for_body_expression() {
    let mut vm = Vm::new();
    for source in [
        "for (;;) let [a] = [0];",
        "for (; false;) let\n[a] = 0;",
        "for (var x in null) let\n[a] = 0;",
        "for (var x of []) let\n[a] = 0;",
        "async function f() { for await (var x of []) let\n[a] = 0; }",
        "'use strict'; for (; false;) let\n{}",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
}

#[test]
fn a_sloppy_let_body_can_end_before_the_following_statement() {
    for (source, expected) in [
        ("var hits = 0; for (; false;) let\nhits = 3; hits;", 3),
        (
            "var let = 1, hits = 0; for (var x in {a: 1, b: 2}) let\n{ hits++; } hits;",
            1,
        ),
        ("var hits = 0; for (var x of []) let\n{ hits++; } hits;", 1),
    ] {
        assert_eq!(
            Vm::new().eval(source).unwrap(),
            Value::from(expected),
            "{source}"
        );
    }
}

#[test]
fn sloppy_for_bodies_accept_let_labels() {
    for (head, expected) in [
        ("var x = 0; x < 2; x++", "01"),
        ("var x in {a: 1, b: 2}", "ab"),
        ("var x of [3, 4]", "34"),
    ] {
        let source = format!(
            "var seen = ''; for ({head}) let: {{ seen += x; break let; seen += '!'; }} seen;"
        );
        assert_eq!(
            Vm::new().eval(&source).unwrap(),
            Value::from(expected),
            "{source}"
        );
    }
}

#[test]
fn sloppy_for_await_body_accepts_a_let_label() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var seen = '';
        async function run() {
            for await (var x of [5, 6]) let: {
                seen += x;
                break let;
                seen += '!';
            }
            seen += 'done';
        }
        run();
        "#,
    )
    .unwrap();
    vm.drain_jobs(32).unwrap();
    assert_eq!(vm.eval("seen").unwrap(), Value::from("56done"));
}

#[test]
fn for_bodies_still_reject_strict_let_labels_and_bare_let_declarations() {
    let mut vm = Vm::new();
    for source in [
        "'use strict'; for (;;) let: ;",
        "'use strict'; for (var x in {}) let: ;",
        "'use strict'; for (var x of []) let: ;",
        "async function f() { 'use strict'; for await (var x of []) let: ; }",
        "for (;;) let x = 1;",
        "for (var x in {}) let y = 1;",
        "for (var x of []) let y = 1;",
        "async function f() { for await (var x of []) let y = 1; }",
        "async function f() { for await (var x of []) const y = 1; }",
        "for (;;) let: let x = 1;",
        "async function f() { for await (var x of []) let: function invalid() {} }",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
}

#[test]
fn grouping_and_braces_keep_their_existing_meaning() {
    for (source, expected) in [
        (
            "var let = [0]; for (var x of [4]) (let[0] = x); (let[0]);",
            4,
        ),
        (
            "var out = 0; for (var x of [5]) { let [a] = [x]; out = a; } out;",
            5,
        ),
    ] {
        assert_eq!(
            Vm::new().eval(source).unwrap(),
            Value::from(expected),
            "{source}"
        );
    }
}
