use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

#[test]
fn long_sticky_delimited_hit_and_miss_need_no_repeat_choices() {
    for closed in [false, true] {
        let mut vm = Vm::new();
        let suffix = if closed { "\"" } else { "" };
        let text = format!("\"{}{suffix}", "x".repeat(80_000));
        vm.set_global("input", Value::from(text)).unwrap();
        vm.set_global("closed", Value::Bool(closed)).unwrap();
        assert_eq!(
            vm.eval(
                r#"
                var re = /"[^"\\\n\r]*"/dy;
                var match = re.exec(input);
                match === null ? !closed && re.lastIndex === 0 :
                    closed && match[0] === input && match.indices[0][1] === input.length &&
                    re.lastIndex === input.length;
                "#,
            )
            .unwrap(),
            Value::Bool(true),
        );
    }
}

#[test]
fn a_delimiter_miss_still_charges_each_scan_step() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1_500,
        ..VmLimits::default()
    });
    vm.set_global("input", Value::from("x".repeat(1_000)))
        .unwrap();
    // The single class member costs 1,000 units by itself. This miss has
    // no output copy, so that alone cannot exhaust the 1,500-unit meter.
    assert_eq!(
        vm.eval(r"/[^!]*!/y.test(input)").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
}

#[test]
fn disjoint_repeat_keeps_captures_lookaround_and_other_backtracking() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var minimum = /(aa|a)[^!]{2,4}!z/dy.exec("aax!z");
            var branch = /(?:(aa)[^!]*!z|(a)[^!]*!q)/dy.exec("aax!q");
            var capture = /(?<last>[ab])+!/dy.exec("aba!");
            var lookahead = /^(?=([^!]*!))([^!]*!)/d.exec("ab!");
            var behind = /(?<=![^!]+)x/d.exec("!abx");
            var negative = /^(?![^!]*!z)[^!]*!q/d;
            var empty = /(?:[ab]?)*/dy;
            empty.lastIndex = 1;
            var zero = empty.exec("xx");
            [
                minimum[1], branch[1] === undefined, branch[2], capture.groups.last,
                lookahead[1], lookahead[2], behind.index,
                negative.test("ab!q"), negative.test("ab!z"),
                zero[0].length, zero.index, empty.lastIndex,
                /[^!]*!/y.exec("!")[0],
                /a[^!]{1,2}!/y.exec("abcd!") === null,
                /a[^!]{1,2}!/y.exec("abc!")[0],
                /a[ab]+a/y.exec("aba")[0],
                /a[^!]*!?b/y.exec("ab")[0],
                /(?i:x(?-i:[^a])*a)/y.exec("xA")[0],
                /a[^!]*?!/y.exec("ab!")[0]
            ].join("|");
            "#,
        )
        .unwrap(),
        Value::from("a|true|a|a|ab!|ab!|3|true|false|0|1|1|!|true|abc!|aba|ab|xA|ab!"),
    );
}

#[test]
fn disjoint_repeat_agrees_with_atomic_reference() {
    for (shard, expected) in [533.0, 485.0, 485.0].into_iter().enumerate() {
        let mut vm = Vm::new();
        vm.set_global("disjointAuditSlice", Value::from(shard))
            .unwrap();
        assert_eq!(
            vm.eval(include_str!("regexp_disjoint_repeat_matrix.js"))
                .unwrap(),
            Value::Number(expected),
        );
    }
}
