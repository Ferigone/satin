use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

#[test]
fn repeated_moderate_string_copies_consume_fuel_before_heap_caps() {
    for operation in [
        "s.slice(0)",
        "s.substring(0)",
        "s.substr(0)",
        "s.repeat(1)",
        "s.concat('')",
        "s.trim()",
        "s.toUpperCase()",
        "s.padEnd(1025)",
        "s.replace('x','y')",
        "JSON.stringify(s)",
        "JSON.parse(json)",
    ] {
        let mut vm = Vm::with_limits(VmLimits {
            max_instructions: 6000,
            max_heap_bytes: 1024 * 1024,
            ..VmLimits::default()
        });
        vm.set_global("s", Value::from("x".repeat(1024))).unwrap();
        vm.set_global("json", Value::from(format!("\"{}\"", "x".repeat(1024))))
            .unwrap();
        let baseline = vm.estimated_heap_bytes();
        let script = format!("for(var i=0;i<50;i++){{{operation};}}");
        let error = vm.eval(&script).unwrap_err();
        assert_eq!(
            error.kind,
            ErrorKind::ExecutionLimit,
            "{operation}: {error}"
        );
        let additional = vm.estimated_heap_bytes().saturating_sub(baseline);
        assert!(
            additional < 64 * 1024,
            "{operation} added {additional} estimated bytes"
        );
        assert_eq!(vm.eval("1+2").unwrap(), Value::from(3));
    }
}

#[test]
fn binary_concat_and_trim_scanning_share_native_work_budget() {
    for operation in ["s+s", "blank.trim()"] {
        let mut vm = Vm::with_limits(VmLimits {
            max_instructions: 6000,
            ..VmLimits::default()
        });
        vm.set_global("s", Value::from("x".repeat(1024))).unwrap();
        vm.set_global("blank", Value::from(" ".repeat(1024)))
            .unwrap();
        assert_eq!(
            vm.eval(&format!("for(var i=0;i<50;i++){{{operation};}}"))
                .unwrap_err()
                .kind,
            ErrorKind::ExecutionLimit
        );
    }
}

#[test]
fn shared_string_reads_do_not_charge_for_nonexistent_copies() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1000,
        ..VmLimits::default()
    });
    vm.set_global("s", Value::from("x".repeat(4096))).unwrap();
    assert_eq!(
        vm.eval("String(s).valueOf()===s").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn short_appends_preserve_utf16_and_saved_prefixes_within_one_task() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var value = '', saved;
            for (var i = 0; i < 1800; i++) {
                value = value + String.fromCharCode(0xd800 + i % 1024);
                if (i === 512) saved = value;
            }
            saved.length === 513 && value.length === 1800 &&
                value.slice(0, saved.length) === saved &&
                value.charCodeAt(1799) === 0xd800 + 1799 % 1024;
        "#
        )
        .unwrap(),
        Value::Bool(true),
    );
}
