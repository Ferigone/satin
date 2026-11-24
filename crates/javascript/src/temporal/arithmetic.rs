//! Fixed-cost civil date conversion and correctly rounded Temporal totals.
use crate::date::calendar::{CivilTime, make_date};
use crate::{ErrorKind, JsError, Value, Vm};
use num_rational::BigRational;
use num_traits::ToPrimitive;

const MILLIS_PER_DAY: f64 = 86_400_000.0;

fn invalid_number() -> JsError {
    JsError::new(
        ErrorKind::RangeError,
        "Temporal arithmetic outside the supported range",
    )
}

pub(super) fn epoch_days(_: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let number = |index: usize| {
        args.get(index)
            .and_then(Value::as_number)
            .ok_or_else(invalid_number)
    };
    let millis = make_date([number(0)?, number(1)? - 1.0, number(2)?, 0.0, 0.0, 0.0, 0.0]);
    Ok(Value::Number(millis / MILLIS_PER_DAY))
}

pub(super) fn iso_date(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let days = args
        .first()
        .and_then(Value::as_number)
        .ok_or_else(invalid_number)?;
    let millis = days * MILLIS_PER_DAY;
    if !millis.is_finite() || millis.abs() >= i64::MAX as f64 {
        return Err(invalid_number());
    }
    let date = CivilTime::from_epoch(millis as i64);
    let result = vm.new_object();
    for (key, n) in [
        ("year", date.year),
        ("month", i64::from(date.month)),
        ("day", i64::from(date.day)),
    ] {
        vm.define_data_property(
            result.clone(),
            key,
            Value::Number(n as f64),
            true,
            true,
            true,
        )?;
    }
    Ok(result)
}

// Valid epoch nanoseconds and duration quantities have bounded bit length.
// Charge the operands before taking their quotient and rounding it once.
pub(super) fn ratio(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let integer = |index: usize| {
        args.get(index)
            .and_then(Value::as_bigint)
            .ok_or_else(invalid_number)
    };
    let numerator = integer(0)?;
    let denominator = integer(1)?;
    vm.charge_work(
        (numerator.bit_length().div_ceil(32) as usize)
            .saturating_mul(denominator.bit_length().div_ceil(32) as usize)
            .max(1),
    )?;
    let fraction =
        BigRational::new_raw(numerator.to_owned_integer(), denominator.to_owned_integer());
    Ok(Value::Number(fraction.to_f64().ok_or_else(invalid_number)?))
}

const NANOS_PER_DAY: i128 = 86_400_000_000_000;

pub(super) fn epoch_to_civil(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let n = args
        .first()
        .and_then(Value::as_bigint)
        .ok_or_else(invalid_number)?;
    if n.bit_length() > 96 {
        return Err(invalid_number());
    }
    let n = n.to_owned_integer().to_i128().ok_or_else(invalid_number)?;
    let ms = i64::try_from(n.div_euclid(1_000_000)).map_err(|_| invalid_number())?;
    let date = CivilTime::from_epoch(ms);
    let result = vm.new_object();
    for (key, v) in [
        ("year", date.year),
        ("month", i64::from(date.month)),
        ("day", i64::from(date.day)),
    ] {
        vm.define_data_property(result.clone(), key, (v as f64).into(), true, true, true)?;
    }
    let mut rem = n.rem_euclid(NANOS_PER_DAY) as i64;
    for (key, _, scale) in super::records::CLOCK {
        vm.define_data_property(
            result.clone(),
            key,
            ((rem / scale) as f64).into(),
            true,
            true,
            true,
        )?;
        rem %= scale;
    }
    Ok(result)
}

pub(super) fn civil_to_epoch(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let civil = args[0].clone();
    let mut get = |key: &str| {
        vm.get_property(civil.clone(), key)?
            .as_number()
            .ok_or_else(invalid_number)
    };
    let days = make_date([
        get("year")?,
        get("month")? - 1.0,
        get("day")?,
        0.0,
        0.0,
        0.0,
        0.0,
    ]) / MILLIS_PER_DAY;
    if !days.is_finite() || days.abs() > 300_000_000.0 {
        return Err(invalid_number());
    }
    let mut n = days as i128 * NANOS_PER_DAY;
    for (name, _, scale) in super::records::CLOCK {
        n += i128::from(get(name)? as i64) * i128::from(scale);
    }
    if let Some(offset) = args.get(1).and_then(Value::as_number) {
        n -= i128::from(offset as i64);
        if n.abs() > 100_000_000 * NANOS_PER_DAY {
            return Err(invalid_number());
        }
    }
    vm.account_value(Value::BigInt(crate::JsBigInt::from_integer(n.into())?))
}
