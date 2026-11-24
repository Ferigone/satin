//! Convert and validate the ten fields of a Temporal Duration in a single pass.
use crate::{ErrorKind, JsError, Value, Vm};
use num_bigint::BigInt;
use num_traits::FromPrimitive;

const NAMES: [&str; 10] = [
    "years",
    "months",
    "weeks",
    "days",
    "hours",
    "minutes",
    "seconds",
    "milliseconds",
    "microseconds",
    "nanoseconds",
];
// ToTemporalPartialDurationRecord reads a property bag in lexicographic order.
const BAG_ORDER: [usize; 10] = [3, 4, 8, 7, 5, 1, 9, 6, 2, 0];
pub(super) const NANOS: [i64; 7] = [
    86_400_000_000_000,
    3_600_000_000_000,
    60_000_000_000,
    1_000_000_000,
    1_000_000,
    1_000,
    1,
];

fn number(vm: &mut Vm, value: &Value) -> Result<f64, JsError> {
    let n = vm.to_number(value)?;
    if !n.is_finite() || n.fract() != 0.0 {
        Err(JsError::new(
            ErrorKind::RangeError,
            "Duration fields must be finite integers",
        ))
    } else {
        Ok(if n == 0.0 { 0.0 } else { n })
    }
}

pub(super) fn from_args(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let mut fields = [0.0; 10];
    for (i, v) in args.iter().take(fields.len()).enumerate() {
        fields[i] = number(vm, v)?;
    }
    record(vm, fields)
}

pub(super) fn from_bag(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let bag = args.first().cloned().unwrap_or_default();
    let mut fields = [0.0; 10];
    let mut any = false;
    for i in BAG_ORDER {
        let value = vm.get_property(bag.clone(), NAMES[i])?;
        if !value.is_undefined() {
            fields[i] = number(vm, &value)?;
            any = true;
        }
    }
    if !any {
        return Err(JsError::type_error("Missing Temporal duration fields"));
    }
    record(vm, fields)
}

pub(super) fn from_fields(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let source = args.first().cloned().unwrap_or_default();
    let mut fields = [0.0; 10];
    for (n, k) in fields.iter_mut().zip(NAMES) {
        let value = vm.get_property(source.clone(), k)?;
        *n = number(vm, &value)?;
    }
    record(vm, fields)
}

pub(super) fn record(vm: &mut Vm, fields: [f64; 10]) -> Result<Value, JsError> {
    let sign = fields
        .iter()
        .find(|x| **x != 0.0)
        .map_or(0.0, |x| x.signum());
    if fields.iter().any(|x| *x * sign < 0.0)
        || fields[..3].iter().any(|x| x.abs() >= 4_294_967_296.0)
    {
        return Err(JsError::new(
            ErrorKind::RangeError,
            "Invalid Temporal duration",
        ));
    }
    // Trivially small durations cannot exceed 2^53 seconds. At the boundary
    // sum exact integers, so durations one nanosecond below it stay valid.
    if fields[3..]
        .iter()
        .zip(NANOS)
        .any(|(n, scale)| n.abs() * scale as f64 > 1e24)
    {
        let mut sum = BigInt::default();
        for (n, scale) in fields[3..].iter().zip(NANOS) {
            vm.charge_work(1 + (n.abs().log2().max(0.0) as usize / 32))?;
            sum += BigInt::from_f64(n.abs()).expect("finite integer") * scale;
        }
        if sum >= (BigInt::from(1u64 << 53) * 1_000_000_000u32) {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "Temporal duration exceeds 2^53 seconds",
            ));
        }
    }
    let value = vm.new_object();
    for (name, n) in NAMES.into_iter().zip(fields).chain([("sign", sign)]) {
        vm.define_data_property(value.clone(), name, Value::Number(n), true, true, true)?;
    }
    Ok(value)
}
