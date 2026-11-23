use rust_browser_javascript::{Value, Vm, VmLimits};

#[test]
fn sticky_multi_member_class_fits_the_work_budget() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 170_000,
        ..VmLimits::default()
    });
    let input = format!("\"{}\"", "x".repeat(20_000));
    vm.set_global("input", Value::from(input)).unwrap();
    assert_eq!(
        vm.eval(
            r#"
            var re = /"[^"\\\n\r]*"/y;
            var match = re.exec(input);
            match !== null && match[0].length === input.length && re.lastIndex === input.length;
            "#,
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn ascii_class_differential() {
    for shard in 0usize..2 {
        let mut vm = Vm::new();
        vm.set_global("classAuditSlice", Value::from(shard))
            .unwrap();
        assert_eq!(
            vm.eval(include_str!("regexp_ascii_class_matrix.js"))
                .unwrap(),
            Value::Bool(true),
            "shard {shard}"
        );
    }
}
