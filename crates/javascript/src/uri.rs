use crate::{ErrorKind, JsError, Value, Vm, builtins::method};

const HEX: &[u8; 16] = b"0123456789ABCDEF";
const PERCENT: u16 = b'%' as u16;
const LOWER_U: u16 = b'u' as u16;

type Codec = fn(&mut Vm, &[u16]) -> Result<Vec<u16>, JsError>;

pub(crate) fn install(vm: &mut Vm) {
    let codecs: [(&str, Codec); 6] = [
        ("encodeURI", |vm, input| encode_uri(vm, input, true)),
        ("encodeURIComponent", |vm, input| {
            encode_uri(vm, input, false)
        }),
        ("decodeURI", |vm, input| decode_uri(vm, input, true)),
        ("decodeURIComponent", |vm, input| {
            decode_uri(vm, input, false)
        }),
        ("escape", escape),
        ("unescape", unescape),
    ];
    let global = vm.global();
    for (name, codec) in codecs {
        method(vm, global.clone(), name, 1, move |vm, _, args| {
            let text = vm.to_string(args.first().unwrap_or(&Value::Undefined))?;
            vm.consume_work(text.len())?;
            let output = codec(vm, text.as_utf16())?;
            vm.finish_string(output)
        });
    }
}

fn malformed_uri() -> JsError {
    JsError::new(ErrorKind::UriError, "Malformed URI")
}

fn uri_reserved(ch: u8) -> bool {
    b";/?:@&=+$,#".contains(&ch)
}

fn uri_unescaped(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&ch)
}

fn append(vm: &mut Vm, output: &mut Vec<u16>, units: &[u16]) -> Result<(), JsError> {
    vm.ensure_string_allocation(output.len().saturating_add(units.len()))?;
    vm.consume_work(units.len())?;
    output.extend_from_slice(units);
    Ok(())
}

fn hex_pair(byte: u8) -> [u16; 2] {
    [
        HEX[(byte >> 4) as usize] as u16,
        HEX[(byte & 15) as usize] as u16,
    ]
}

fn escaped_byte(byte: u8) -> [u16; 3] {
    let [high, low] = hex_pair(byte);
    [PERCENT, high, low]
}

fn read_hex(digits: &[u16]) -> Option<u16> {
    digits.iter().try_fold(0, |value, &digit| {
        let digit = match digit {
            0x30..=0x39 => digit - 0x30,
            0x41..=0x46 => digit - 0x41 + 10,
            0x61..=0x66 => digit - 0x61 + 10,
            _ => return None,
        };
        Some((value << 4) | digit)
    })
}

// ES5.1 §15.1.3 / Encode and Decode: only percent escapes carry UTF-8.
fn encode_uri(vm: &mut Vm, input: &[u16], keep_reserved: bool) -> Result<Vec<u16>, JsError> {
    let mut output = Vec::new();
    for scalar in char::decode_utf16(input.iter().copied()) {
        let scalar = scalar.map_err(|_| malformed_uri())?;
        if scalar.is_ascii()
            && (uri_unescaped(scalar as u8) || keep_reserved && uri_reserved(scalar as u8))
        {
            append(vm, &mut output, &[scalar as u16])?;
        } else {
            for &byte in scalar.encode_utf8(&mut [0; 4]).as_bytes() {
                append(vm, &mut output, &escaped_byte(byte))?;
            }
        }
    }
    Ok(output)
}

fn percent_byte(input: &[u16], index: usize) -> Result<u8, JsError> {
    match input.get(index..index + 3) {
        Some([PERCENT, digits @ ..]) => read_hex(digits).map(|b| b as u8).ok_or_else(malformed_uri),
        _ => Err(malformed_uri()),
    }
}

fn decode_uri(vm: &mut Vm, input: &[u16], keep_reserved: bool) -> Result<Vec<u16>, JsError> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < input.len() {
        if input[index] != PERCENT {
            // Raw UTF-16 code units, including lone surrogates, are copied verbatim.
            append(vm, &mut output, &input[index..index + 1])?;
            index += 1;
            continue;
        }
        let first = percent_byte(input, index)?;
        let width = match first {
            0..=0x7f => {
                if keep_reserved && uri_reserved(first) {
                    // Preserve original hex case for escaped reserved characters.
                    append(vm, &mut output, &input[index..index + 3])?;
                } else {
                    append(vm, &mut output, &[first as u16])?;
                }
                index += 3;
                continue;
            }
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Err(malformed_uri()),
        };

        let mut octets = [first, 0, 0, 0];
        for (offset, byte) in octets.iter_mut().enumerate().take(width).skip(1) {
            *byte = percent_byte(input, index + offset * 3)?;
        }
        // Strict UTF-8 rejects overlong forms, surrogate scalars and values above U+10FFFF.
        let text = std::str::from_utf8(&octets[..width]).map_err(|_| malformed_uri())?;
        let scalar = text.chars().next().unwrap();
        append(vm, &mut output, scalar.encode_utf16(&mut [0; 2]))?;
        index += width * 3;
    }
    Ok(output)
}

// Annex B operates on every UTF-16 code unit separately, not on Unicode scalars.
fn escape(vm: &mut Vm, input: &[u16]) -> Result<Vec<u16>, JsError> {
    let mut output = Vec::new();
    for &unit in input {
        if unit > 0xff {
            let [a, b] = hex_pair((unit >> 8) as u8);
            let [c, d] = hex_pair(unit as u8);
            append(vm, &mut output, &[PERCENT, LOWER_U, a, b, c, d])?;
        } else {
            let byte = unit as u8;
            if byte.is_ascii_alphanumeric() || b"@*_+-./".contains(&byte) {
                append(vm, &mut output, &[unit])?;
            } else {
                append(vm, &mut output, &escaped_byte(byte))?;
            }
        }
    }
    Ok(output)
}

fn unescape(vm: &mut Vm, input: &[u16]) -> Result<Vec<u16>, JsError> {
    let mut output = Vec::new();
    let mut index = 0;
    while let Some(&unit) = input.get(index) {
        let (decoded, consumed) = if unit == PERCENT {
            if input.get(index + 1) == Some(&LOWER_U)
                && let Some(value) = input.get(index + 2..index + 6).and_then(read_hex)
            {
                (value, 6)
            } else if let Some(value) = input.get(index + 1..index + 3).and_then(read_hex) {
                (value, 3)
            } else {
                (unit, 1)
            }
        } else {
            (unit, 1)
        };
        append(vm, &mut output, &[decoded])?;
        index += consumed;
    }
    Ok(output)
}
