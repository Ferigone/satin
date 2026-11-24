//! Uint8Array hexadecimal and Base64 codecs. Decoders commit complete chunks
//! immediately so an invalid suffix preserves bytes already written.
use super::{TypedArray, allocation_error, arg, element::Element};
use crate::{ErrorKind, JsError, JsString, Value, Vm, builtins::method};

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Clone, Copy, PartialEq)]
enum LastChunk {
    Loose,
    Strict,
    Stop,
}
#[derive(Clone, Copy)]
enum Codec {
    Hex,
    Base64 {
        url: bool,
        last: LastChunk,
        omit_padding: bool,
    },
}

fn config(vm: &mut Vm, options: Value, encode: bool) -> Result<Codec, JsError> {
    let read = |vm: &mut Vm, key: &str| {
        if options.is_undefined() {
            Ok(Value::Undefined)
        } else if options.as_object().is_none() {
            Err(JsError::type_error("Codec options must be an object"))
        } else {
            vm.get_property(options.clone(), key)
        }
    };
    let alphabet = read(vm, "alphabet")?;
    let url = if alphabet.is_undefined() || alphabet == Value::from("base64") {
        false
    } else if alphabet == Value::from("base64url") {
        true
    } else {
        return Err(JsError::type_error("Unknown Base64 alphabet"));
    };
    let (last, omit_padding) = if encode {
        (LastChunk::Loose, read(vm, "omitPadding")?.is_truthy())
    } else {
        let handling = read(vm, "lastChunkHandling")?;
        let last = if handling.is_undefined() || handling == Value::from("loose") {
            LastChunk::Loose
        } else if handling == Value::from("strict") {
            LastChunk::Strict
        } else if handling == Value::from("stop-before-partial") {
            LastChunk::Stop
        } else {
            return Err(JsError::type_error("Unknown Base64 final chunk mode"));
        };
        (last, false)
    };
    Ok(Codec::Base64 {
        url,
        last,
        omit_padding,
    })
}

fn invalid() -> JsError {
    JsError::new(ErrorKind::SyntaxError, "Invalid encoded byte string")
}
fn hex(c: u16) -> Result<u8, JsError> {
    Ok(match c {
        48..=57 => c - 48,
        65..=70 => c - 55,
        97..=102 => c - 87,
        _ => return Err(invalid()),
    } as u8)
}
fn sextet(c: u16, url: bool) -> Result<u32, JsError> {
    Ok(match c {
        65..=90 => c - 65,
        97..=122 => c - 71,
        48..=57 => c + 4,
        43 if !url => 62,
        47 if !url => 63,
        45 if url => 62,
        95 if url => 63,
        _ => return Err(invalid()),
    } as u32)
}
fn skip_whitespace(input: &[u16], mut at: usize) -> usize {
    while matches!(input.get(at), Some(9 | 10 | 12 | 13 | 32)) {
        at += 1;
    }
    at
}
/// Calls emit only for complete, validated chunks. The same decoder is used to
/// size new array buffers and to decode into bounded, possibly shared views.
fn decode(
    codec: Codec,
    input: &[u16],
    max: usize,
    mut emit: impl FnMut(usize, &[u8]),
) -> Result<(usize, usize), JsError> {
    let Codec::Base64 { url, last, .. } = codec else {
        if input.len() % 2 != 0 {
            return Err(invalid());
        }
        let count = max.min(input.len() / 2);
        for (at, pair) in input.chunks_exact(2).take(count).enumerate() {
            emit(at, &[(hex(pair[0])? << 4) | hex(pair[1])?]);
        }
        return Ok((count * 2, count));
    };
    let (mut at, mut read, mut written) = (0, 0, 0);
    let (mut bits, mut digits) = (0u32, 0);
    while written < max {
        at = skip_whitespace(input, at);
        let c = input.get(at).copied();
        if c.is_none() || c == Some(61) {
            if c == Some(61) {
                if digits < 2 {
                    return Err(invalid());
                }
                at = skip_whitespace(input, at + 1);
                if digits == 2 {
                    if at == input.len() && last == LastChunk::Stop {
                        return Ok((read, written));
                    }
                    if input.get(at) != Some(&61) {
                        return Err(invalid());
                    }
                    at = skip_whitespace(input, at + 1);
                }
                if at != input.len() {
                    return Err(invalid());
                }
            } else if digits != 0 {
                if last == LastChunk::Stop {
                    return Ok((read, written));
                }
                if last == LastChunk::Strict || digits == 1 {
                    return Err(invalid());
                }
            }
            if digits >= 2 {
                let extra = if digits == 2 { 4 } else { 2 };
                if last == LastChunk::Strict && bits & ((1 << extra) - 1) != 0 {
                    return Err(invalid());
                }
                let bytes = (bits << (24 - 6 * digits)).to_be_bytes();
                emit(written, &bytes[1..digits]);
                written += digits - 1;
            }
            return Ok((input.len(), written));
        }
        let value = sextet(c.unwrap(), url)?;
        let room = max - written;
        if (room == 1 && digits == 2) || (room == 2 && digits == 3) {
            break;
        }
        bits = (bits << 6) | value;
        digits += 1;
        at += 1;
        if digits == 4 {
            emit(written, &bits.to_be_bytes()[1..]);
            written += 3;
            read = at;
            digits = 0;
            bits = 0;
        }
    }
    Ok((read, written))
}

fn uint8(vm: &Vm, receiver: &Value) -> Result<TypedArray, JsError> {
    let id = receiver
        .as_object()
        .ok_or_else(|| JsError::type_error("Expected a Uint8Array"))?;
    let typed = vm.array_data(id)?;
    if typed.element != Element::Uint8 {
        return Err(JsError::type_error("Expected a Uint8Array"));
    }
    Ok(typed)
}

fn parse(
    vm: &mut Vm,
    target: Option<TypedArray>,
    args: &[Value],
    base64: bool,
) -> Result<Value, JsError> {
    if let Some(array) = target {
        vm.require_writable_buffer(array.view.buffer)?;
    }
    let Value::String(string) = arg(args, 0) else {
        return Err(JsError::type_error("Encoded input must be a string"));
    };
    let codec = if base64 {
        config(vm, arg(args, 1), false)?
    } else {
        Codec::Hex
    };
    let input = string.as_utf16();
    vm.binary_work(input.len())?;
    if let Some(target) = target {
        let range = vm.require_view_range(target.view, 1)?;
        let dst = &mut vm
            .buffer_data_mut(target.view.buffer)?
            .bytes
            .as_mut()
            .unwrap()[range];
        let (read, written) = decode(codec, input, dst.len(), |at, bytes| {
            dst[at..at + bytes.len()].copy_from_slice(bytes)
        })?;
        let result = vm.new_object();
        for (key, value) in [("read", read), ("written", written)] {
            vm.define_data_property(result.clone(), key, value.into(), true, true, true)?;
        }
        Ok(result)
    } else {
        let (_, len) = decode(codec, input, usize::MAX, |_, _| {})?;
        vm.new_uint8_array_with(len, |out| {
            // Validation is complete and no user JavaScript runs between passes.
            decode(codec, input, usize::MAX, |at, bytes| {
                out[at..at + bytes.len()].copy_from_slice(bytes)
            })
            .expect("validated coded input");
        })
    }
}

fn format(vm: &mut Vm, receiver: Value, args: &[Value], base64: bool) -> Result<Value, JsError> {
    let target = uint8(vm, &receiver)?;
    let codec = if base64 {
        config(vm, arg(args, 0), true)?
    } else {
        Codec::Hex
    };
    let range = vm.require_view_range(target.view, 1)?;
    let n = range.len();
    let n_out = match codec {
        Codec::Hex => n.saturating_mul(2),
        Codec::Base64 { omit_padding, .. } => n
            .div_ceil(3)
            .saturating_mul(4)
            .saturating_sub(if omit_padding { (3 - n % 3) % 3 } else { 0 }),
    };
    vm.ensure_string_allocation(n_out)?;
    vm.binary_work(n_out)?;
    let src = &vm.buffer_data(target.view.buffer)?.bytes.as_ref().unwrap()[range];
    let mut out = Vec::new();
    out.try_reserve_exact(n_out)
        .map_err(|_| allocation_error())?;
    match codec {
        Codec::Hex => {
            for &byte in src {
                out.extend([
                    HEX[(byte >> 4) as usize] as u16,
                    HEX[(byte & 15) as usize] as u16,
                ]);
            }
        }
        Codec::Base64 {
            url, omit_padding, ..
        } => {
            let alphabet = if url { URL } else { BASE64 };
            for chunk in src.chunks(3) {
                let bits = u32::from_be_bytes([
                    0,
                    chunk[0],
                    chunk.get(1).copied().unwrap_or(0),
                    chunk.get(2).copied().unwrap_or(0),
                ]);
                for i in 0..4 {
                    if i <= chunk.len() {
                        out.push(alphabet[((bits >> (18 - 6 * i)) & 63) as usize] as u16);
                    } else if !omit_padding {
                        out.push(61);
                    }
                }
            }
        }
    }
    Ok(Value::String(JsString::from_utf16(out)))
}

pub(super) fn install(vm: &mut Vm) {
    let idx = Element::Uint8 as usize;
    let ctor = Value::Object(vm.realm.binary.typed_constructors[idx].unwrap());
    let proto = Value::Object(vm.realm.binary.typed[idx].unwrap());
    for (from, set, to, base64) in [
        ("fromBase64", "setFromBase64", "toBase64", true),
        ("fromHex", "setFromHex", "toHex", false),
    ] {
        method(vm, ctor.clone(), from, 1, move |vm, _, args| {
            parse(vm, None, args, base64)
        });
        method(vm, proto.clone(), set, 1, move |vm, this, args| {
            let target = uint8(vm, &this)?;
            parse(vm, Some(target), args, base64)
        });
        method(vm, proto.clone(), to, 0, move |vm, this, args| {
            format(vm, this, args, base64)
        });
    }
}
