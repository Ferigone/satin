use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

#[test]
fn locale_compare_coerces_receiver_first_and_defaults_to_undefined() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var order = "";
            var left = new String("unused");
            left.toString = function () { order += "l"; return "undefined"; };
            var right = {toString: function () { order += "r"; return "undefined"; }};
            String.prototype.localeCompare.length === 1
              && String.prototype.localeCompare.call(left, right) === 0
              && order === "lr" && left.localeCompare() === 0;
            "#,
        )
        .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        vm.eval(
            r#"
            String.prototype.localeCompare.call(null, {toString: function () { throw 42; }});
            "#,
        )
        .unwrap_err()
        .kind,
        ErrorKind::TypeError
    );
}

#[test]
fn locale_compare_observes_canonical_equivalence_and_order() {
    let source = r#"
        "\u00e9".localeCompare("e\u0301") === 0
          && "\u1e0b\u0323".localeCompare("d\u0323\u0307") === 0
          && "\ud800\u00e9".localeCompare("\ud800e\u0301") === 0
          && "\ud800".localeCompare("\ufffd") !== 0
          && "\ud800".localeCompare("\ud801") !== 0
          && "".localeCompare("a") < 0
          && "a".localeCompare("b") < 0
          && "b".localeCompare("a") > 0;
    "#;
    assert_eq!(Vm::new().eval(source).unwrap(), Value::Bool(true));
}

#[test]
fn locale_compare_meters_native_normalization() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 10_000,
        ..VmLimits::default()
    });
    vm.set_global("left", Value::from("e\u{0301}".repeat(300)))
        .unwrap();
    vm.set_global("right", Value::from("é".repeat(300)))
        .unwrap();
    assert_eq!(
        vm.eval("left.localeCompare(right)").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
}
