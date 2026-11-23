use crate::{JsBigInt, JsError, Value, Vm, vm::to_u32};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Element {
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float16,
    Float32,
    Float64,
    BigInt64,
    BigUint64,
}

impl Element {
    pub(super) const ALL: [Self; 12] = [
        Self::Int8,
        Self::Uint8,
        Self::Uint8Clamped,
        Self::Int16,
        Self::Uint16,
        Self::Int32,
        Self::Uint32,
        Self::Float16,
        Self::Float32,
        Self::Float64,
        Self::BigInt64,
        Self::BigUint64,
    ];
    pub(super) const COUNT: usize = Self::ALL.len();

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Int8 => "Int8Array",
            Self::Uint8 => "Uint8Array",
            Self::Uint8Clamped => "Uint8ClampedArray",
            Self::Int16 => "Int16Array",
            Self::Uint16 => "Uint16Array",
            Self::Int32 => "Int32Array",
            Self::Uint32 => "Uint32Array",
            Self::Float16 => "Float16Array",
            Self::Float32 => "Float32Array",
            Self::Float64 => "Float64Array",
            Self::BigInt64 => "BigInt64Array",
            Self::BigUint64 => "BigUint64Array",
        }
    }

    pub(super) fn width(self) -> usize {
        match self {
            Self::Int8 | Self::Uint8 | Self::Uint8Clamped => 1,
            Self::Int16 | Self::Uint16 | Self::Float16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float32 => 4,
            Self::Float64 | Self::BigInt64 | Self::BigUint64 => 8,
        }
    }

    fn is_bigint(self) -> bool {
        matches!(self, Self::BigInt64 | Self::BigUint64)
    }

    pub(super) fn check_content_type(self, other: Self) -> Result<(), JsError> {
        if self.is_bigint() != other.is_bigint() {
            return Err(JsError::type_error("TypedArray content types do not match"));
        }
        Ok(())
    }

    pub(super) fn same_encoding(self, other: Self) -> bool {
        self == other || self.is_bigint() && other.is_bigint()
    }

    pub(super) fn coerce_bits(self, vm: &mut Vm, value: &Value) -> Result<u64, JsError> {
        if self.is_bigint() {
            vm.to_bigint_low_u64(value)
        } else {
            Ok(self.number_bits(vm.to_number(value)?))
        }
    }

    pub(super) fn number_bits(self, number: f64) -> u64 {
        match self {
            Self::Int8 | Self::Uint8 | Self::Int16 | Self::Uint16 | Self::Int32 | Self::Uint32 => {
                to_u32(number) as u64
            }
            Self::Uint8Clamped => number.clamp(0.0, 255.0).round_ties_even() as u64,
            Self::Float16 => encode_half(number) as u64,
            Self::Float32 => (number as f32).to_bits() as u64,
            Self::Float64 => number.to_bits(),
            Self::BigInt64 | Self::BigUint64 => unreachable!("expected a Number element type"),
        }
    }

    pub(super) fn store_bits(self, bits: u64, little: bool) -> [u8; 8] {
        let mut bytes = bits.to_le_bytes();
        if !little {
            bytes[..self.width()].reverse();
        }
        bytes
    }

    pub(super) fn load_bits(source: &[u8], little: bool) -> u64 {
        let mut bytes = [0; 8];
        bytes[..source.len()].copy_from_slice(source);
        if !little {
            bytes[..source.len()].reverse();
        }
        u64::from_le_bytes(bytes)
    }

    pub(super) fn value(self, bits: u64) -> Value {
        match self {
            Self::BigInt64 => Value::BigInt(JsBigInt::from_i64(bits as i64)),
            Self::BigUint64 => Value::BigInt(JsBigInt::from_u64(bits)),
            _ => self.number_value(bits).into(),
        }
    }

    pub(super) fn number_value(self, bits: u64) -> f64 {
        match self {
            Self::Int8 => bits as i8 as f64,
            Self::Uint8 | Self::Uint8Clamped => bits as u8 as f64,
            Self::Int16 => bits as i16 as f64,
            Self::Uint16 => bits as u16 as f64,
            Self::Int32 => bits as i32 as f64,
            Self::Uint32 => bits as u32 as f64,
            Self::Float16 => decode_half(bits as u16),
            Self::Float32 => f32::from_bits(bits as u32) as f64,
            Self::Float64 => f64::from_bits(bits),
            Self::BigInt64 | Self::BigUint64 => unreachable!("expected a Number element type"),
        }
    }
}

fn encode_half(number: f64) -> u16 {
    let bits = number.to_bits();
    let sign = (bits >> 48) as u16 & 0x8000;
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let fraction = bits & ((1 << 52) - 1);
    if exponent == 1024 {
        return sign | if fraction == 0 { 0x7c00 } else { 0x7e00 };
    }
    if exponent > 15 {
        return sign | 0x7c00;
    }
    if exponent < -25 {
        return sign;
    }
    let significand = (1_u64 << 52) | fraction;
    // Round directly from the Number mantissa to avoid a second rounding
    // step via float32, including at the subnormal and overflow boundaries.
    let shift = if exponent >= -14 { 42 } else { 28 - exponent };
    let truncated = significand >> shift;
    let halfway = 1_u64 << (shift - 1);
    let trailing = significand & ((halfway << 1) - 1);
    let rounded =
        truncated + u64::from(trailing > halfway || trailing == halfway && truncated & 1 != 0);
    let magnitude = if exponent >= -14 {
        ((exponent + 15) as u16) * 1024 + rounded as u16 - 1024
    } else {
        rounded as u16
    };
    sign | magnitude
}

fn decode_half(bits: u16) -> f64 {
    let magnitude = bits & 0x7fff;
    let exponent = (magnitude >> 10) as i32;
    let fraction = (magnitude & 0x03ff) as u64;
    let number = if exponent == 0 {
        fraction as f64 / 16_777_216.0
    } else if exponent == 31 {
        if fraction == 0 {
            f64::INFINITY
        } else {
            f64::NAN
        }
    } else {
        f64::from_bits(((exponent - 15 + 1023) as u64) << 52 | fraction << 42)
    };
    if bits & 0x8000 == 0 { number } else { -number }
}
