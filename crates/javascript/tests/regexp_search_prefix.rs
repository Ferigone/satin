use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

#[test]
fn absolute_start_uses_local_flags_and_preserves_captures_and_global_index() {
    let mut vm = Vm::new();
    let answer = vm.eval(r#"
        var g = /(^😀)/dgu;
        g.lastIndex = 1;
        var first = g.exec("😀z"), later = g.exec("😀z");
        var y = /^z/y; y.lastIndex = 1;
        first.indices[1].join() === "0,2" && later === null && g.lastIndex === 0 &&
          y.exec("az") === null && y.lastIndex === 0 &&
          /(^b)/m.exec("a\nb").index === 2 && /(?m:^b)/.test("a\nb") &&
          !/(?-m:^b)/m.test("a\nb") &&
          /(^x)|z/.exec("az").index === 1 && /(?:^a)?b/.exec("zb").index === 1;
    "#).unwrap();
    assert_eq!(answer, Value::Bool(true));
}

#[test]
fn sparse_ascii_starts_fit_the_work_budget_across_replacements() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 280_000,
        ..VmLimits::default()
    });
    let left = "A".repeat(16_384);
    let right = "B".repeat(16_384);
    vm.set_global("input", Value::from(format!(" \t{left}\\n{right}\t ")))
        .unwrap();
    let actual = vm
        .eval(
            r#"
            var trimmed = input.replace(/^[ \t]+|[ \t]+$/g, "");
            trimmed.replace(/\\(?:x[0-9a-fA-F]{2}|[nrt])/g, "@");
            "#,
        )
        .unwrap();
    assert_eq!(actual, Value::from(format!("{left}@{right}")));
}

#[test]
fn ascii_starts_do_not_change_unicode_sticky_or_empty_match_behavior() {
    let mut vm = Vm::new();
    let actual = vm
        .eval(
            r#"
            var global = /x(.)/dgu, sticky = /x(.)/duy, astral = /😀x/dgu;
            global.lastIndex = sticky.lastIndex = astral.lastIndex = 1;
            var a = global.exec("😀x😀");
            var b = sticky.exec("😀x😀");
            var c = astral.exec("😀x");
            var empty = /z?|x/g.exec("abc");
            var folded = /(?i:x)|z/.exec("aXz");
            var scoped = /(?-i:x)|z/i.exec("aXz");
            var classes = /[^\D]/g.exec("abc7");
            var optional = /(?:x?|y)z/.exec("az");
            var look = /(?<=a)x/.exec("zxax");
            var backref = /\1x(y)?/.exec("zx");
            [
                a.indices[0].join(","), a.indices[1].join(","), global.lastIndex,
                b === null, sticky.lastIndex, c.indices[0].join(","), astral.lastIndex,
                empty.index, empty[0].length, folded.index, scoped.index, classes.index,
                optional.index, look.index, backref.index
            ].join("|");
            "#,
        )
        .unwrap();
    assert_eq!(
        actual,
        Value::from("2,5|3,5|5|true|0|0,3|3|0|0|1|2|3|1|3|1")
    );
}

#[test]
fn rejecting_ascii_starts_still_charges_work() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1000,
        ..VmLimits::default()
    });
    vm.set_global("input", Value::from("x".repeat(10_000)))
        .unwrap();
    assert_eq!(
        vm.eval("/z/.test(input)").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
}
