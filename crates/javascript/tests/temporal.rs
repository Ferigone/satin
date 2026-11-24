use rust_browser_javascript::{Value, Vm};

#[test]
fn durations_calendar_dates_and_bigint_instants_are_available_to_scripts() {
    let mut vm = Vm::new();
    let result = vm
        .eval(
            r#"
        const start = Temporal.PlainDate.from("2024-02-28");
        const next = start.add({ days: 1 });
        const duration = Temporal.Duration.from("PT1.000000007S");
        const instant = Temporal.Instant.fromEpochNanoseconds(-1n);
        next.toString() === "2024-02-29" &&
          next.since(start).days === 1 && duration.nanoseconds === 7 &&
          instant.toString() === "1969-12-31T23:59:59.999999999Z" &&
          new Date(-1).toTemporalInstant().epochNanoseconds === -1000000n
    "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn temporal_uses_intrinsics_and_preserves_subclass_and_realm_identity() {
    let mut vm = Vm::new();
    let (child, other) = vm.new_realm().unwrap();
    vm.define_data_property(vm.global(), "other", other.clone(), true, false, true)
        .unwrap();
    vm.with_realm(child, |vm| {
        vm.eval("Math.sign = Number.isFinite = WeakMap.prototype.get = () => { throw 7; };")
    })
    .unwrap();
    assert_eq!(
        vm.eval(
            r#"
       let OtherDate = Object.getOwnPropertyDescriptor(other, "Temporal").value.PlainDate;
       let foreignMethod = OtherDate.prototype.add;
       class MyDate extends OtherDate {}
       let date = new MyDate(2024, 1, 31);
       let threw = false;
       try { new OtherDate(2024, 13, 5); } catch (error) {
         threw = error instanceof other.RangeError && !(error instanceof RangeError);
       }
       let duration = new other.Temporal.Duration(1, 1);
       Object.getPrototypeOf(OtherDate) === other.Function.prototype &&
         Object.getPrototypeOf(OtherDate.prototype) === other.Object.prototype &&
         Object.getPrototypeOf(foreignMethod) === other.Function.prototype &&
         date instanceof MyDate && date instanceof OtherDate &&
         duration.toString() === "P1Y1M" && date.add({ days: 1 }).month === 2 && threw
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}
