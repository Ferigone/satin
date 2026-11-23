//! JavaScript BigInt values and VM-owned storage accounting.
mod builtins;
mod numeric;
mod parsing;

use crate::{ErrorKind, JsError, JsString, Value, Vm};
use num_bigint::{BigInt as Integer, Sign};
use num_traits::ToPrimitive;
use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::BTreeMap,
    fmt,
    rc::{Rc, Weak},
};

pub(crate) use builtins::install;
pub(crate) use parsing::{literal, literal_work};
const MAX_BITS: usize = 65_536;

/// An exact ECMAScript integer. Every signed or unsigned 64-bit integer fits
/// inline, so reading a typed-array element needs no BigInt heap allocation.
#[derive(Clone, Eq, PartialEq)]
pub struct JsBigInt(Repr);

#[derive(Clone, Eq, PartialEq)]
enum Repr {
    Small(i64),
    High(u64),
    Large(Rc<Integer>),
}

impl JsBigInt {
    pub const fn from_i64(value: i64) -> Self {
        Self(Repr::Small(value))
    }

    pub const fn from_u64(value: u64) -> Self {
        if value <= i64::MAX as u64 {
            Self::from_i64(value as i64)
        } else {
            Self(Repr::High(value))
        }
    }

    pub const fn is_zero(&self) -> bool {
        matches!(self.0, Repr::Small(0))
    }

    pub fn bit_length(&self) -> u64 {
        match &self.0 {
            Repr::Small(value) => u64::from(64 - value.unsigned_abs().leading_zeros()),
            Repr::High(_) => 64,
            Repr::Large(value) => value.bits(),
        }
    }

    /// Returns the two's-complement low 64 bits.
    pub fn to_wrapping_u64(&self) -> u64 {
        match &self.0 {
            Repr::Small(value) => *value as u64,
            Repr::High(value) => *value,
            Repr::Large(value) => {
                let low = value.iter_u64_digits().next().unwrap_or(0);
                if value.sign() == Sign::Minus {
                    low.wrapping_neg()
                } else {
                    low
                }
            }
        }
    }

    fn negative(&self) -> bool {
        match &self.0 {
            Repr::Small(value) => *value < 0,
            Repr::High(_) => false,
            Repr::Large(value) => value.sign() == Sign::Minus,
        }
    }

    fn words(&self) -> usize {
        words(self.bit_length() as usize)
    }

    fn integer(&self) -> Cow<'_, Integer> {
        match &self.0 {
            Repr::Small(value) => Cow::Owned(Integer::from(*value)),
            Repr::High(value) => Cow::Owned(Integer::from(*value)),
            Repr::Large(value) => Cow::Borrowed(value),
        }
    }

    fn from_integer(value: Integer) -> Result<Self, JsError> {
        if let Some(value) = value.to_i64() {
            return Ok(Self::from_i64(value));
        }
        if let Some(value) = value.to_u64() {
            return Ok(Self::from_u64(value));
        }
        if value.bits() > MAX_BITS as u64 {
            return Err(size_limit());
        }
        // Remove any spare capacity left after cancellation or truncation.
        Ok(Self(Repr::Large(Rc::new(value.clone()))))
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        if matches!(self.0, Repr::Large(_)) {
            192 + self.words() * 4
        } else {
            0
        }
    }
}

impl Ord for JsBigInt {
    fn cmp(&self, other: &Self) -> Ordering {
        use Repr::*;
        match (&self.0, &other.0) {
            (Small(a), Small(b)) => a.cmp(b),
            (High(a), High(b)) => a.cmp(b),
            (Small(_), High(_)) => Ordering::Less,
            (High(_), Small(_)) => Ordering::Greater,
            (Large(a), Large(b)) => a.cmp(b),
            (Large(_), _) => {
                if self.negative() {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (_, Large(_)) => {
                if other.negative() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
        }
    }
}

impl PartialOrd for JsBigInt {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for JsBigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Repr::Small(value) => fmt::Display::fmt(value, f),
            Repr::High(value) => fmt::Display::fmt(value, f),
            Repr::Large(value) => fmt::Display::fmt(value, f),
        }
    }
}

impl fmt::Debug for JsBigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.bit_length() > 512 {
            return write!(f, "BigInt({} bits)", self.bit_length());
        }
        write!(f, "{self}n")
    }
}

#[derive(Default)]
pub(crate) struct Storage {
    values: BTreeMap<usize, (Weak<Integer>, usize)>,
    pub(crate) bytes: usize,
}

impl Storage {
    pub(crate) fn reclaim(&mut self) {
        self.values
            .retain(|_, (value, _)| value.strong_count() != 0);
        self.bytes = self.values.values().map(|(_, bytes)| bytes).sum();
    }
}

impl Vm {
    pub fn bigint_from_i64(&mut self, value: i64) -> Result<Value, JsError> {
        self.account_value(Value::BigInt(JsBigInt::from_i64(value)))
    }

    pub fn bigint_from_u64(&mut self, value: u64) -> Result<Value, JsError> {
        self.account_value(Value::BigInt(JsBigInt::from_u64(value)))
    }

    /// Implements ToBigInt. Number operands are accepted only by BigInt().
    pub fn to_bigint(&mut self, value: &Value) -> Result<JsBigInt, JsError> {
        let value = self.coerce_primitive(value.clone(), false)?;
        self.bigint_from_primitive(value)
    }

    pub fn to_bigint_low_u64(&mut self, value: &Value) -> Result<u64, JsError> {
        Ok(self.to_bigint(value)?.to_wrapping_u64())
    }

    fn bigint_from_primitive(&mut self, value: Value) -> Result<JsBigInt, JsError> {
        let result = match value {
            Value::BigInt(value) => value,
            Value::Bool(value) => JsBigInt::from_i64(i64::from(value)),
            Value::String(value) => self.parse_bigint_string(&value)?.ok_or_else(|| {
                JsError::new(
                    ErrorKind::SyntaxError,
                    "Invalid string for BigInt conversion",
                )
            })?,
            _ => return Err(JsError::type_error("Cannot convert value to BigInt")),
        };
        self.account_bigint(&result)?;
        Ok(result)
    }

    pub(crate) fn account_bigint(&mut self, value: &JsBigInt) -> Result<(), JsError> {
        if let Repr::Large(integer) = &value.0 {
            let id = Rc::as_ptr(integer) as usize;
            if !self.bigint_storage.values.contains_key(&id) {
                let bytes = value.retained_bytes();
                self.ensure_allocation(bytes)?;
                self.bigint_storage
                    .values
                    .insert(id, (Rc::downgrade(integer), bytes));
                self.bigint_storage.bytes += bytes;
            }
        }
        Ok(())
    }

    fn bigint_result(&mut self, value: Integer) -> Result<Value, JsError> {
        self.account_value(Value::BigInt(JsBigInt::from_integer(value)?))
    }

    fn prepare_bigint_work(&mut self, bits: usize, work: usize) -> Result<(), JsError> {
        self.ensure_allocation(512 + words(bits) * 32)?;
        self.consume_work(work)
    }

    pub(crate) fn bigint_text(
        &mut self,
        value: &JsBigInt,
        radix: u32,
    ) -> Result<JsString, JsError> {
        let digits = (value.bit_length() as usize)
            .max(1)
            .div_ceil(radix.ilog2() as usize)
            + usize::from(value.negative());
        self.ensure_allocation(digits.saturating_mul(5).saturating_add(192))?;
        let words = value.words();
        self.consume_work(digits.saturating_add(if radix.is_power_of_two() {
            words
        } else {
            words.saturating_mul(words)
        }))?;
        let text = if radix == 10 {
            value.to_string()
        } else {
            value.integer().to_str_radix(radix)
        };
        self.ensure_string_allocation(text.len())?;
        let text = JsString::from(text);
        self.account_value(Value::String(text.clone()))?;
        Ok(text)
    }
}

fn words(bits: usize) -> usize {
    bits.div_ceil(32).max(1)
}

fn size_limit() -> JsError {
    JsError::new(
        ErrorKind::ExecutionLimit,
        "BigInt bit-width budget exhausted",
    )
}
