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

#[test]
fn exact_buffers_and_borrowed_slices_charge_one_copy() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 4300,
        ..VmLimits::default()
    });
    vm.set_global("s", Value::from("x".repeat(2048))).unwrap();
    assert_eq!(vm.eval("s.repeat(2).length").unwrap(), Value::from(4096));
    assert_eq!(
        vm.eval("s.repeat(3)").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    vm.set_global("s", Value::from("x".repeat(4096))).unwrap();
    assert_eq!(
        vm.eval("s.slice(1,4095).length").unwrap(),
        Value::from(4094)
    );
}

#[test]
fn materialized_strings_release_unreferenced_append_prefixes() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var strings = [], prefixes = [];
        function build(id) {
            var result = 'a'.repeat(1000);
            for (var i = 0; i < 25; i++) {
                result += String.fromCharCode(i ? 65 + i : id);
                if (i === 5) prefixes.push(result);
            }
            strings.push(result);
            return result.charCodeAt(1024);
        }
    "#,
    )
    .unwrap();
    vm.collect_garbage();
    let baseline = vm.estimated_heap_bytes();
    for id in 0..64 {
        assert_eq!(vm.eval(&format!("build({id})")).unwrap(), Value::from(89));
    }
    assert_eq!(
        vm.eval("prefixes.every((s, i) => s.length === 1006 && s.charCodeAt(1000) === i)")
            .unwrap(),
        Value::Bool(true)
    );
    vm.collect_garbage();
    let retained = vm.estimated_heap_bytes() - baseline;
    // Both arrays intentionally retain distinct strings; each needs about 2 KiB.
    // The temporary prefix strings used to assemble each result are unobservable.
    assert!(retained < 512 * 1024, "retained {retained} bytes");
}

#[test]
fn ten_thousand_short_appends_share_storage_and_preserve_intermediate_values() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
        let text = '', snapshot, mixed;
        for (let i = 0; i < 10000; i++) {
            text += '.';
            if (i === 34) {
                snapshot = text;
                mixed = text + '\ud800';
                if (mixed.charCodeAt(35) !== 0xd800) throw 'lost code unit';
            }
        }
        snapshot === '.'.repeat(35) && mixed.length === 36 && text === '.'.repeat(10000)
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn large_string_reservations_follow_live_aliases_between_collections() {
    let mut vm = Vm::new();
    vm.collect_garbage();
    let baseline = vm.estimated_heap_bytes();
    let mut strings = Vec::new();
    for i in 0..24 {
        strings.push(vm.finish_buffer_string(vec![i; 32 * 1024]).unwrap());
    }
    let full = vm.estimated_heap_bytes();
    assert!(full - baseline >= 24 * 64 * 1024);
    let alias = strings[0].clone();
    drop(strings);
    assert!(vm.estimated_heap_bytes() >= baseline + 64 * 1024);
    assert!(full - vm.estimated_heap_bytes() >= 23 * 64 * 1024);
    drop(alias);
    assert_eq!(vm.estimated_heap_bytes(), baseline);
}

#[test]
fn shared_large_strings_keep_separate_vm_charges_across_collection() {
    let mut left = Vm::new();
    let mut right = Vm::new();
    let value = left.finish_buffer_string(vec![0xD800; 32 * 1024]).unwrap();
    left.set_global("retained", value.clone()).unwrap();
    right.set_global("retained", value.clone()).unwrap();
    drop(value);
    for _ in 0..3 {
        left.collect_garbage();
        right.collect_garbage();
    }
    left.set_global("retained", Value::Null).unwrap();
    left.collect_garbage();
    assert_eq!(
        right.eval("retained.charCodeAt(0)").unwrap(),
        Value::from(0xD800)
    );
    let before = right.estimated_heap_bytes();
    right.set_global("retained", Value::Null).unwrap();
    assert!(before - right.estimated_heap_bytes() >= 64 * 1024);
}

#[test]
fn string_equality_rejects_different_lengths_without_scanning_code_units() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1000,
        ..VmLimits::default()
    });
    vm.set_global("a", Value::from("x".repeat(4096))).unwrap();
    vm.set_global("b", Value::from("x".repeat(4097))).unwrap();
    vm.set_global("c", Value::from("x".repeat(4096))).unwrap();
    assert_eq!(vm.eval("a !== b && a != b").unwrap(), Value::Bool(true));
    assert_eq!(
        vm.eval("a === c").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(
        vm.eval("a < b").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
}
