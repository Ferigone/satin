use super::{arg, method};
use crate::{ErrorKind, JsError, Value, Vm};
use num_bigint::{BigInt, Sign};
use num_traits::{ToPrimitive, Zero};

pub(crate) fn pow(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() || (exponent.is_infinite() && base.abs() == 1.0) {
        f64::NAN
    } else {
        base.powf(exponent)
    }
}

// Every finite double is an integer multiple of 2^-1074. Accumulating in
// those units avoids intermediate overflow and loss of small addends. A sum
// of up to 2^53-1 finite doubles needs fewer than 2160 bits.
fn finite_addend(n: f64) -> BigInt {
    let bits = n.to_bits();
    let exponent = (bits >> 52) & 0x7ff;
    let fraction = bits & ((1 << 52) - 1);
    let units = if exponent == 0 {
        BigInt::from(fraction)
    } else {
        BigInt::from(fraction | (1 << 52)) << (exponent - 1) as usize
    };
    if n.is_sign_negative() { -units } else { units }
}

fn round_total(total: &BigInt) -> f64 {
    let mag = total.magnitude();
    let mut shift = mag.bits().saturating_sub(53);
    let mut top = (mag >> shift as usize).to_u64().expect("at most 53 bits");
    if shift != 0
        && mag.bit(shift - 1)
        && (mag.trailing_zeros().is_some_and(|n| n < shift - 1) || top & 1 != 0)
    {
        top += 1;
        if top == 1 << 53 {
            top >>= 1;
            shift += 1;
        }
    }
    let sign = u64::from(total.sign() == Sign::Minus) << 63;
    let bits = if top < 1 << 52 {
        top
    } else if shift >= 2046 {
        f64::INFINITY.to_bits()
    } else {
        ((shift + 1) << 52) | (top & ((1 << 52) - 1))
    };
    f64::from_bits(sign | bits)
}

fn sum_precise(vm: &mut Vm, values: Value) -> Result<Value, JsError> {
    let mut it = vm.get_iterator(values)?;
    let mut total = BigInt::zero();
    let mut negative_zero = true;
    let mut exceptional = 0.0f64;
    let mut count = 0u64;
    while let Some(value) = vm.iterator_step(&mut it)? {
        if count >= (1u64 << 53) - 1 {
            return vm.iterator_close(
                &mut it,
                Err(JsError::new(ErrorKind::RangeError, "Too many summands")),
            );
        }
        count += 1;
        let Value::Number(n) = value else {
            return vm.iterator_close(
                &mut it,
                Err(JsError::type_error("Math.sumPrecise requires numbers")),
            );
        };
        negative_zero &= n == 0.0 && n.is_sign_negative();
        if !n.is_finite() {
            exceptional += n;
        } else if exceptional.is_finite() && n != 0.0 {
            vm.consume_work(34)?;
            total += finite_addend(n);
        }
    }
    Ok(Value::Number(if !exceptional.is_finite() {
        exceptional
    } else if negative_zero {
        -0.0
    } else {
        round_total(&total)
    }))
}
pub(super) fn install(vm: &mut Vm, math: Value) {
    let unary: [(&str, fn(f64) -> f64); 7] = [
        ("sinh", f64::sinh),
        ("cosh", f64::cosh),
        ("tanh", f64::tanh),
        ("asinh", f64::asinh),
        ("acosh", f64::acosh),
        ("atanh", f64::atanh),
        ("f16round", crate::binary::f16_round),
    ];
    for (name, f) in unary {
        method(vm, math.clone(), name, 1, move |vm, _, args| {
            Ok(Value::Number(f(vm.to_number(&arg(args, 0))?)))
        });
    }
    method(vm, math.clone(), "hypot", 2, |vm, _, args| {
        let mut result = 0.0f64;
        for value in args {
            let n = vm.to_number(value)?;
            result = result.hypot(n);
        }
        Ok(Value::Number(result))
    });
    method(vm, math, "sumPrecise", 1, |vm, _, args| {
        sum_precise(vm, arg(args, 0))
    });
}
