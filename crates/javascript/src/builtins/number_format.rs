//! Decimal formatting for ECMAScript's bounded precision and fraction digits.
use super::{arg, integer, method, number_this};
use crate::{ErrorKind, JsError, Value, Vm, vm::number_string};
use num_bigint::BigUint;

// An IEEE binary fraction m / 2^s is the decimal integer m * 5^s scaled
// by 10^-s. This avoids rounding the binary number twice, even at 100 digits.
fn decimal(value: f64) -> (String, i32) {
    if value == 0.0 {
        return ("0".into(), 0);
    }
    let bits = value.abs().to_bits();
    let exponent = ((bits >> 52) & 2047) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let significand = if exponent == 0 {
        fraction
    } else {
        fraction | (1 << 52)
    };
    let shift = exponent.max(1) - 1023 - 52;
    let integer = BigUint::from(significand);
    let digits = if shift < 0 {
        (integer * BigUint::from(5u32).pow((-shift) as u32)).to_str_radix(10)
    } else {
        (integer << shift as usize).to_str_radix(10)
    };
    let exponent = digits.len() as i32 - 1 + shift.min(0);
    (digits, exponent)
}

// Return round(value * 10^scale) for the chosen number of leading digits.
// ECMAScript chooses the larger decimal integer on a tie.
fn round_digits(mut digits: String, keep: i32) -> String {
    if keep < 0 {
        return "0".into();
    }
    let keep = keep as usize;
    if keep >= digits.len() {
        digits.extend(std::iter::repeat_n('0', keep - digits.len()));
        return digits;
    }
    let upward = digits.as_bytes()[keep] >= b'5';
    digits.truncate(keep);
    if upward {
        let prefix = digits.trim_end_matches('9').len();
        let zeros = digits.len() - prefix;
        if prefix == 0 {
            digits = "1".into();
        } else {
            digits.truncate(prefix);
            let last = digits.pop().unwrap() as u8;
            digits.push((last + 1) as char);
        }
        digits.extend(std::iter::repeat_n('0', zeros));
    }
    if digits.is_empty() {
        "0".into()
    } else {
        digits
    }
}

fn scientific(mut digits: String, exponent: i32) -> String {
    if digits.len() > 1 {
        digits.insert(1, '.');
    }
    format!("{digits}e{exponent:+}")
}

fn fixed(mut digits: String, fraction: usize) -> String {
    if fraction == 0 {
        return digits;
    }
    if digits.len() <= fraction {
        return format!("0.{}{digits}", "0".repeat(fraction - digits.len()));
    }
    digits.insert(digits.len() - fraction, '.');
    digits
}

pub(super) fn install(vm: &mut Vm, prototype: Value) {
    for name in ["toPrecision", "toExponential", "toFixed"] {
        method(vm, prototype.clone(), name, 1, move |vm, this, args| {
            let value = number_this(vm, this)?;
            let requested = arg(args, 0);
            let absent = requested.is_undefined();
            if name == "toPrecision" && absent {
                return Ok(number_string(value).into());
            }
            let count = integer(vm, requested)?;
            if name != "toFixed" && !value.is_finite() {
                return Ok(number_string(value).into());
            }
            let minimum = if name == "toPrecision" { 1.0 } else { 0.0 };
            if !(minimum..=100.0).contains(&count) {
                return Err(JsError::new(
                    ErrorKind::RangeError,
                    "Invalid decimal precision",
                ));
            }
            if !value.is_finite() || (name == "toFixed" && value.abs() >= 1e21) {
                return Ok(number_string(value).into());
            }
            let count = count as i32;
            let formatted = if name == "toExponential" && absent {
                let (significand, exponent) = format!("{:e}", value.abs())
                    .split_once('e')
                    .map(|(s, e)| (s.to_owned(), e.parse::<i32>().unwrap()))
                    .unwrap();
                format!("{significand}e{exponent:+}")
            } else {
                let (digits, mut exponent) = decimal(value);
                if name == "toFixed" {
                    fixed(round_digits(digits, exponent + 1 + count), count as usize)
                } else {
                    let precision = if name == "toPrecision" {
                        count
                    } else {
                        count + 1
                    };
                    let mut rounded = round_digits(digits, precision);
                    if rounded.len() > precision as usize {
                        rounded.pop();
                        exponent += 1;
                    }
                    if name == "toExponential" || exponent < -6 || exponent >= precision {
                        scientific(rounded, exponent)
                    } else {
                        fixed(rounded, (precision - exponent - 1) as usize)
                    }
                }
            };
            Ok(if value < 0.0 {
                format!("-{formatted}")
            } else {
                formatted
            }
            .into())
        });
    }
}
