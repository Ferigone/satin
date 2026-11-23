use super::{Integer, JsBigInt, MAX_BITS, size_limit, words};
use crate::{ErrorKind, JsError, Value, Vm};
use num_traits::{FromPrimitive, ToPrimitive, Zero};
use std::cmp::Ordering;

impl Vm {
    pub(crate) fn coerce_numeric(&mut self, value: &Value) -> Result<Value, JsError> {
        let primitive = self.coerce_primitive(value.clone(), false)?;
        self.numeric_primitive(primitive)
    }

    pub(crate) fn numeric_primitive(&mut self, value: Value) -> Result<Value, JsError> {
        if matches!(value, Value::BigInt(_)) {
            self.account_value(value)
        } else {
            Ok(Value::Number(self.to_number(&value)?))
        }
    }

    pub(crate) fn bigint_unary(&mut self, op: &str, input: &JsBigInt) -> Result<Value, JsError> {
        self.prepare_bigint_work(input.bit_length() as usize + 1, input.words())?;
        let input = input.integer();
        self.bigint_result(if op == "-" {
            -input.as_ref()
        } else {
            !input.as_ref()
        })
    }

    pub(crate) fn bigint_binary(
        &mut self,
        op: &str,
        left: &JsBigInt,
        right: &JsBigInt,
    ) -> Result<Value, JsError> {
        match op {
            ">>>" => return Err(JsError::type_error("BigInt has no unsigned right shift")),
            "<<" | ">>" => return self.bigint_shift(op == "<<", left, right),
            "**" => return self.bigint_pow(left, right),
            "/" | "%" if right.is_zero() => {
                return Err(JsError::new(
                    ErrorKind::RangeError,
                    "BigInt division by zero",
                ));
            }
            _ => {}
        }
        let left = left.integer();
        let right = right.integer();
        if op == "*" {
            let result = self.bigint_multiply(&left, &right)?;
            return self.bigint_result(result);
        }
        let (a, b) = (left.bits() as usize, right.bits() as usize);
        let work = if matches!(op, "/" | "%") {
            words(a).saturating_mul(words(b))
        } else {
            words(a).saturating_add(words(b))
        };
        self.prepare_bigint_work(a.max(b) + 1, work)?;
        let (left, right) = (left.as_ref(), right.as_ref());
        let value = match op {
            "+" => left + right,
            "-" => left - right,
            "/" => left / right,
            "%" => left % right,
            "|" => left | right,
            "&" => left & right,
            "^" => left ^ right,
            _ => {
                return Err(JsError::new(
                    ErrorKind::InternalError,
                    "Unknown BigInt binary operator",
                ));
            }
        };
        self.bigint_result(value)
    }

    fn bigint_multiply(&mut self, left: &Integer, right: &Integer) -> Result<Integer, JsError> {
        if left.is_zero() || right.is_zero() {
            return Ok(Integer::ZERO);
        }
        let (a, b) = (left.bits() as usize, right.bits() as usize);
        if a.saturating_add(b) > MAX_BITS + 1 {
            return Err(size_limit());
        }
        self.prepare_bigint_work(a + b, words(a).saturating_mul(words(b)))?;
        let result = left * right;
        if result.bits() > MAX_BITS as u64 {
            return Err(size_limit());
        }
        Ok(result)
    }

    fn bigint_shift(
        &mut self,
        mut left: bool,
        value: &JsBigInt,
        count: &JsBigInt,
    ) -> Result<Value, JsError> {
        if value.is_zero() || count.is_zero() {
            return Ok(Value::BigInt(value.clone()));
        }
        left ^= count.negative();
        let bits = value.bit_length() as usize;
        let amount = count.integer().magnitude().to_usize();
        if !left && amount.is_none_or(|amount| amount >= bits) {
            return self.bigint_from_i64(if value.negative() { -1 } else { 0 });
        }
        let amount = amount.ok_or_else(size_limit)?;
        let result_bits = if left {
            bits.saturating_add(amount)
        } else {
            bits - amount
        };
        if result_bits > MAX_BITS {
            return Err(size_limit());
        }
        self.prepare_bigint_work(
            bits.max(result_bits),
            value.words().saturating_add(words(result_bits)),
        )?;
        let value = value.integer();
        self.bigint_result(if left {
            value.as_ref() << amount
        } else {
            value.as_ref() >> amount
        })
    }

    fn bigint_pow(&mut self, base: &JsBigInt, exponent: &JsBigInt) -> Result<Value, JsError> {
        if exponent.negative() {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "BigInt exponent is negative",
            ));
        }
        if exponent.is_zero() {
            return self.bigint_from_i64(1);
        }
        if base.bit_length() <= 1 {
            return self.bigint_from_i64(if base.is_zero() {
                0
            } else if base.negative() && exponent.to_wrapping_u64() & 1 != 0 {
                -1
            } else {
                1
            });
        }
        let mut exponent = exponent.integer().to_usize().ok_or_else(size_limit)?;
        if (base.bit_length() as usize - 1).saturating_mul(exponent) >= MAX_BITS {
            return Err(size_limit());
        }
        let mut factor = base.integer().into_owned();
        let mut result = Integer::from(1);
        while exponent > 0 {
            if exponent & 1 != 0 {
                result = self.bigint_multiply(&result, &factor)?;
            }
            exponent >>= 1;
            if exponent > 0 {
                factor = self.bigint_multiply(&factor, &factor)?;
            }
        }
        self.bigint_result(result)
    }

    pub(super) fn wrap_bigint(
        &mut self,
        value: JsBigInt,
        width: usize,
        signed: bool,
    ) -> Result<Value, JsError> {
        if width == 0 {
            return self.bigint_from_i64(0);
        }
        let bits = value.bit_length() as usize;
        if (signed && bits < width) || (!signed && !value.negative() && bits <= width) {
            return Ok(Value::BigInt(value));
        }
        if width > MAX_BITS {
            return Err(size_limit());
        }
        self.prepare_bigint_work(
            bits.max(width + 1),
            words(bits).saturating_add(2 * words(width + 1)),
        )?;
        let modulus = Integer::from(1) << width;
        let mut reduced = value.integer().as_ref() & (&modulus - 1u32);
        if signed && reduced.bits() == width as u64 {
            reduced -= &modulus;
        }
        self.bigint_result(reduced)
    }

    pub(crate) fn charge_bigint_comparison(
        &mut self,
        left: &Value,
        right: &Value,
    ) -> Result<(), JsError> {
        if let (Value::BigInt(left), Value::BigInt(right)) = (left, right) {
            self.consume_work(left.words().min(right.words()))?;
        }
        Ok(())
    }

    pub(crate) fn compare_bigint_number(
        &mut self,
        integer: &JsBigInt,
        number: f64,
    ) -> Result<Option<Ordering>, JsError> {
        let extremum = match number {
            n if n.is_nan() => return Ok(None),
            f64::INFINITY => return Ok(Some(Ordering::Less)),
            f64::NEG_INFINITY => return Ok(Some(Ordering::Greater)),
            n => n,
        };
        self.prepare_bigint_work(1024, integer.words().min(32))?;
        let truncated = extremum.trunc();
        let bound = Integer::from_f64(truncated).expect("finite Number");
        let order = integer.integer().cmp(&std::borrow::Cow::Borrowed(&bound));
        Ok(Some(if order == Ordering::Equal && truncated != extremum {
            if extremum.is_sign_negative() {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        } else {
            order
        }))
    }

    pub(crate) fn compare_bigint_string(
        &mut self,
        integer: &JsBigInt,
        text: &crate::JsString,
    ) -> Result<Option<Ordering>, JsError> {
        let Some(other) = self.parse_bigint_string(text)? else {
            return Ok(None);
        };
        self.consume_work(integer.words().min(other.words()))?;
        Ok(Some(integer.cmp(&other)))
    }
}
