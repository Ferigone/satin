//! Default String collation without the optional ECMA-402 API.
use crate::{JsError, Vm};
use icu_normalizer::ComposingNormalizerBorrowed;
use std::{borrow::Cow, cmp::Ordering};

pub(crate) fn compare(vm: &mut Vm, lhs: &[u16], rhs: &[u16]) -> Result<Ordering, JsError> {
    let units = lhs.len().saturating_add(rhs.len());
    vm.consume_work(units)?;
    if lhs == rhs {
        return Ok(Ordering::Equal);
    }

    // Bound normalization and its scratch buffers before native work. Combining
    // marks may have to be reordered, so charge more than one pass.
    vm.consume_work(units.saturating_mul(8 + units.max(1).ilog2() as usize))?;
    vm.ensure_allocation(units.saturating_mul(64))?;
    let (lhs_unpaired, lhs) = canonical_units(lhs);
    let (rhs_unpaired, rhs) = canonical_units(rhs);
    if lhs_unpaired || rhs_unpaired {
        // Use a separate ordered group for ill-formed UTF-16. Falling back to
        // ordinal comparison for just one operand could make ordering cyclic.
        return Ok(lhs_unpaired.cmp(&rhs_unpaired).then_with(|| lhs.cmp(&rhs)));
    }
    if lhs == rhs {
        return Ok(Ordering::Equal);
    }
    compare_default_locale(&lhs, &rhs)
}

fn canonical_units(input: &[u16]) -> (bool, Cow<'_, [u16]>) {
    if input.iter().all(|&unit| unit <= 0x7f) {
        return (false, Cow::Borrowed(input));
    }
    let mut output = Vec::with_capacity(input.len());
    let mut start = 0;
    let mut offset = 0;
    for scalar in char::decode_utf16(input.iter().copied()) {
        match scalar {
            Ok(ch) => offset += ch.len_utf16(),
            Err(err) => {
                normalize_run(&input[start..offset], &mut output);
                // Do not conflate an isolated surrogate with U+FFFD or with
                // another surrogate. It also bounds a normalization run.
                output.push(err.unpaired_surrogate());
                offset += 1;
                start = offset;
            }
        }
    }
    normalize_run(&input[start..], &mut output);
    (start != 0, Cow::Owned(output))
}

fn normalize_run(input: &[u16], output: &mut Vec<u16>) {
    let normalizer = ComposingNormalizerBorrowed::new_nfc();
    let scalars = char::decode_utf16(input.iter().copied())
        .map(|ch| ch.expect("collation normalization run contains only Unicode scalars"));
    for ch in normalizer.normalize_iter(scalars) {
        output.extend_from_slice(ch.encode_utf16(&mut [0; 2]));
    }
}

#[cfg(windows)]
fn compare_default_locale(lhs: &[u16], rhs: &[u16]) -> Result<Ordering, JsError> {
    use crate::ErrorKind;
    use std::ptr::null;
    use windows_sys::Win32::Globalization::{
        CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringEx,
    };

    static EMPTY: [u16; 1] = [0];
    fn argument(units: &[u16]) -> Result<(*const u16, i32), JsError> {
        if units.is_empty() {
            return Ok((EMPTY.as_ptr(), -1));
        }
        let length = i32::try_from(units.len()).map_err(|_| {
            JsError::new(ErrorKind::ExecutionLimit, "String collation input too long")
        })?;
        Ok((units.as_ptr(), length))
    }

    let (lhs, lhs_len) = argument(lhs)?;
    let (rhs, rhs_len) = argument(rhs)?;
    // SAFETY: null selects the user's default locale. Both buffers remain
    // valid for their explicit lengths, or are a NUL-terminated empty string.
    // Unused version information, reserved data and sort handle are zero.
    match unsafe { CompareStringEx(null(), 0, lhs, lhs_len, rhs, rhs_len, null(), null(), 0) } {
        CSTR_LESS_THAN => Ok(Ordering::Less),
        CSTR_EQUAL => Ok(Ordering::Equal),
        CSTR_GREATER_THAN => Ok(Ordering::Greater),
        _ => Err(JsError::new(
            ErrorKind::InternalError,
            "Windows string collation failed",
        )),
    }
}

#[cfg(not(windows))]
fn compare_default_locale(lhs: &[u16], rhs: &[u16]) -> Result<Ordering, JsError> {
    Ok(lhs.cmp(rhs))
}
