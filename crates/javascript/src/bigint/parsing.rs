use super::{Integer, JsBigInt, MAX_BITS, size_limit, words};
use crate::{JsError, JsString, Vm, builtins::js_whitespace};

fn significant(digits: &[u8]) -> &[u8] {
    let first = digits
        .iter()
        .position(|digit| *digit != b'0')
        .unwrap_or(digits.len());
    &digits[first..]
}

fn estimated_bits(digits: usize, radix: u32) -> usize {
    if radix == 10 {
        digits.saturating_mul(3322).div_ceil(1000)
    } else {
        digits.saturating_mul(radix.ilog2() as usize)
    }
}

fn parse_work(len: usize, bits: usize, radix: u32) -> usize {
    let words = words(bits);
    len.saturating_add(if radix == 10 {
        words.saturating_mul(words)
    } else {
        words
    })
}

pub(crate) fn literal_work(digits: &[u8], radix: u32) -> usize {
    parse_work(
        digits.len(),
        estimated_bits(significant(digits).len(), radix),
        radix,
    )
}

pub(crate) fn literal(digits: &[u8], radix: u32) -> Result<JsBigInt, JsError> {
    from_digits(significant(digits), radix, false)
}

fn from_digits(digits: &[u8], radix: u32, negative: bool) -> Result<JsBigInt, JsError> {
    if digits.is_empty() {
        return Ok(JsBigInt::from_i64(0));
    }
    // The digit estimate includes the unused high bits of the leading digit.
    if estimated_bits(digits.len(), radix) > MAX_BITS + 8 {
        return Err(size_limit());
    }
    let value = Integer::parse_bytes(digits, radix).expect("validated BigInt digits");
    JsBigInt::from_integer(if negative { -value } else { value })
}

impl Vm {
    pub(crate) fn parse_bigint_string(
        &mut self,
        input: &JsString,
    ) -> Result<Option<JsBigInt>, JsError> {
        self.consume_work(input.len())?;
        let units = input.as_utf16();
        let first = units
            .iter()
            .position(|u| !js_whitespace(*u))
            .unwrap_or(units.len());
        let last = units
            .iter()
            .rposition(|u| !js_whitespace(*u))
            .map_or(first, |i| i + 1);
        let units = &units[first..last];
        if units.is_empty() {
            return Ok(Some(JsBigInt::from_i64(0)));
        }
        let (negative, radix, digits) = match units {
            [45, rest @ ..] => (true, 10, rest),
            [43, rest @ ..] => (false, 10, rest),
            [48, 66 | 98, rest @ ..] => (false, 2, rest),
            [48, 79 | 111, rest @ ..] => (false, 8, rest),
            [48, 88 | 120, rest @ ..] => (false, 16, rest),
            _ => (false, 10, units),
        };
        if digits.is_empty()
            || digits.iter().any(|unit| {
                u8::try_from(*unit)
                    .ok()
                    .and_then(|byte| char::from(byte).to_digit(radix))
                    .is_none()
            })
        {
            return Ok(None);
        }
        let first = digits.iter().position(|d| *d != 48).unwrap_or(digits.len());
        let digits = &digits[first..];
        let bits = estimated_bits(digits.len(), radix);
        if bits > MAX_BITS + 8 {
            return Err(size_limit());
        }
        self.ensure_allocation(digits.len().saturating_add(64))?;
        self.prepare_bigint_work(bits, parse_work(digits.len(), bits, radix))?;
        let digits: Vec<u8> = digits.iter().map(|unit| *unit as u8).collect();
        from_digits(&digits, radix, negative).map(Some)
    }
}
