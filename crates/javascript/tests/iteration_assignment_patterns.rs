use rust_browser_javascript::{ErrorKind, Value, Vm};

#[test]
fn iteration_patterns_reject_invalid_assignment_targets_before_execution() {
    let mut vm = Vm::new();
    for source in [
        "for ([(x, y)] in {}) {}",
        "for ({ m() {} } of []) {}",
        "for (([x]) of []) {}",
        "for ([target?.value] of []) {}",
        "'use strict'; for ([arguments] in {}) {}",
        "'use strict'; for ({x: eval} of []) {}",
        "async function f() {'use strict'; for await ([eval] of []) {}}",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    vm.check_syntax("for ([arguments] of []) {}").unwrap();
}

#[test]
fn iteration_patterns_assign_existing_names_and_members() {
    let mut vm = Vm::new();
    let result = vm
        .eval(
            r#"
            'use strict';
            let first, found, rest;
            const target = {};
            for ([first] in {key: true}) {}
            for ([found = 'has' in {has: true}, ...rest] of [[undefined, 2, 3]]) {}
            for ({value: target.eval, ...target.extra} of [{value: 7, n: 9}]) {}
            first === 'k' && found === true && rest.join(',') === '2,3' &&
                target.eval === 7 && target.extra.n === 9 && !('value' in target.extra);
            "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn for_await_assigns_destructured_values_after_resuming() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var result = '';
        async function run() {
            'use strict';
            let item;
            for await ([item] of [[7], [13]]) result += ':' + item;
        }
        run();
        "#,
    )
    .unwrap();
    vm.with_task(|vm| {
        while vm.has_pending_jobs() {
            vm.drain_jobs(1)?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(vm.eval("result").unwrap(), Value::from(":7:13"));
}
