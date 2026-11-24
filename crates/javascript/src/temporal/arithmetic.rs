//! Fixed-cost civil date conversion and correctly rounded Temporal totals.
use crate::date::calendar::{CivilTime, make_date};
use crate::{ErrorKind, JsError, Value, Vm};
use num_rational::BigRational;
use num_traits::ToPrimitive;

const MILLIS_PER_DAY: f64 = 86_400_000.0;

fn invalid_number() -> JsError {
    JsError::new(ErrorKind::RangeError, "Temporal arithmetic outside the supported range")
}

pub(super) fn epoch_days(_: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let number = |index: usize| args.get(index).and_then(Value::as_number).ok_or_else(invalid_number);
    let millis = make_date([number(0)?, number(1)? - 1.0, number(2)?, 0.0, 0.0, 0.0, 0.0]);
    Ok(Value::Number(millis / MILLIS_PER_DAY))
}

pub(super) fn iso_date(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let days = args.first().and_then(Value::as_number).ok_or_else(invalid_number)?;
    let millis = days * MILLIS_PER_DAY;
    if !millis.is_finite() || millis.abs() >= i64::MAX as f64 {
        return Err(invalid_number());
    }
    let date = CivilTime::from_epoch(millis as i64);
    let result = vm.new_object();
    for (key, n) in [("year", date.year), ("month", i64::from(date.month)), ("day", i64::from(date.day))] {
        vm.define_data_property(result.clone(), key, Value::Number(n as f64), true, true, true)?;
    }
    Ok(result)
}

// Valid epoch nanoseconds and duration quantities have bounded bit length.
// Charge the operands before taking their quotient and rounding it once.
pub(super) fn ratio(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let integer = |index: usize| args.get(index).and_then(Value::as_bigint).ok_or_else(invalid_number);
    let numerator = integer(0)?;
    let denominator = integer(1)?;
    vm.charge_work((numerator.bit_length().div_ceil(32) as usize).saturating_mul(denominator.bit_length().div_ceil(32) as usize).max(1))?;
    let fraction = BigRational::new_raw(numerator.to_owned_integer(), denominator.to_owned_integer());
    Ok(Value::Number(fraction.to_f64().ok_or_else(invalid_number)?))
}
