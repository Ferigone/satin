//! Encoding Standard interfaces over the VM's native byte storage.
use super::{arg, webidl};
use encoding_rs::{CoderResult, Decoder, DecoderResult, Encoding, UTF_8};
use rust_browser_javascript::{ErrorKind, JsError, JsString, Value, Vm};
use std::{cell::RefCell, rc::Rc};

struct Encoder;

struct TextDecoder {
    encoding: &'static Encoding,
    fatal: bool,
    ignore_bom: bool,
    pending: RefCell<Option<Decoder>>,
}

impl TextDecoder {
    fn fresh_decoder(&self) -> Decoder {
        if self.ignore_bom {
            self.encoding.new_decoder_without_bom_handling()
        } else {
            self.encoding.new_decoder_with_bom_removal()
        }
    }

    fn decode(&self, vm: &mut Vm, input: Value, stream: bool) -> Result<Value, JsError> {
        let size = if input.is_undefined() {
            0
        } else {
            vm.fixed_buffer_source_bytes(&input)?.len()
        };
        vm.charge_work(size.div_ceil(4))?;
        let mut pending = self.pending.borrow_mut();
        let decoder = pending.get_or_insert_with(|| self.fresh_decoder());
        // Cap the codec's worst-case estimate before allocation, then examine
        // the actual result. Multi-byte encodings can fit under the string limit.
        let capacity = decoder
            .max_utf16_buffer_length(size)
            .ok_or_else(output_limit)?
            .min(vm.limits().max_string_units);
        vm.ensure_string_allocation(capacity)?;
        let mut output = Vec::new();
        output.try_reserve_exact(capacity).map_err(|_| {
            JsError::new(ErrorKind::HeapLimit, "TextDecoder output allocation failed")
        })?;
        output.resize(capacity, 0);
        let bytes = if input.is_undefined() {
            rust_browser_javascript::BufferRead::Owned(&[])
        } else {
            vm.fixed_buffer_source_bytes(&input)?
        };
        let decoded = if self.fatal {
            let (result, _, written) =
                decoder.decode_to_utf16_without_replacement(&bytes, &mut output, !stream);
            match result {
                DecoderResult::InputEmpty => Ok(written),
                DecoderResult::OutputFull => Err(output_limit()),
                DecoderResult::Malformed(_, _) => Err(JsError::type_error(
                    "TextDecoder encountered malformed bytes",
                )),
            }
        } else {
            let (result, _, written, _) = decoder.decode_to_utf16(&bytes, &mut output, !stream);
            match result {
                CoderResult::InputEmpty => Ok(written),
                CoderResult::OutputFull => Err(output_limit()),
            }
        };
        drop(bytes);
        if !stream
            || decoded
                .as_ref()
                .is_err_and(|error| error.kind == ErrorKind::ExecutionLimit)
        {
            pending.take();
        }
        output.truncate(decoded?);
        vm.finish_string(output)
    }
}

fn output_limit() -> JsError {
    JsError::new(
        ErrorKind::ExecutionLimit,
        "TextDecoder output exceeds the string limit",
    )
}

fn scalars(text: &JsString) -> impl Iterator<Item = char> + '_ {
    char::decode_utf16(text.as_utf16().iter().copied())
        .map(|point| point.unwrap_or(char::REPLACEMENT_CHARACTER))
}

fn encode_into(text: &JsString, destination: &mut [u8]) -> (usize, usize) {
    let (mut read, mut written) = (0, 0);
    for point in scalars(text) {
        let count = point.len_utf8();
        if destination.len() - written < count {
            break;
        }
        point.encode_utf8(&mut destination[written..written + count]);
        written += count;
        read += point.len_utf16();
    }
    (read, written)
}

fn dictionary(value: Value) -> Result<Option<Value>, JsError> {
    if value.is_nullish() {
        return Ok(None);
    }
    if value.as_object().is_none() {
        return Err(JsError::type_error(
            "Text decoding options must be an object",
        ));
    }
    Ok(Some(value))
}

fn option(vm: &mut Vm, options: &Option<Value>, name: &str) -> Result<bool, JsError> {
    if let Some(options) = options {
        Ok(vm.get_property(options.clone(), name)?.is_truthy())
    } else {
        Ok(false)
    }
}

fn label(text: &JsString) -> Option<&'static Encoding> {
    let units = text.as_utf16();
    let space = |unit| matches!(unit, 9 | 10 | 12 | 13 | 32);
    let start = units.iter().position(|&unit| !space(unit))?;
    let end = units.iter().rposition(|&unit| !space(unit))? + 1;
    let units = &units[start..end];
    let mut ascii = [0; 64];
    if units.len() > ascii.len() || units.iter().any(|&unit| unit > 127) {
        return None;
    }
    for (byte, &unit) in ascii.iter_mut().zip(units) {
        *byte = unit as u8;
    }
    Encoding::for_label_no_replacement(&ascii[..units.len()])
}

fn encoder(vm: &Vm, value: &Value) -> Result<Rc<Encoder>, JsError> {
    vm.host_data(value)
        .ok_or_else(|| JsError::type_error("Receiver is not a TextEncoder"))
}

fn decoder(vm: &Vm, value: &Value) -> Result<Rc<TextDecoder>, JsError> {
    vm.host_data(value)
        .ok_or_else(|| JsError::type_error("Receiver is not a TextDecoder"))
}

fn getter(
    vm: &mut Vm,
    prototype: Value,
    name: &str,
    read: impl Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
) -> Result<(), JsError> {
    let get = vm.new_native_function(&format!("get {name}"), read);
    vm.define_accessor(prototype, name, Some(get), None)
}

fn install_encoder(vm: &mut Vm) -> Result<(), JsError> {
    let prototype = vm.new_object();
    let root = vm.root(prototype.clone());
    webidl::interface(
        vm,
        prototype.clone(),
        "TextEncoder",
        0,
        move |vm, _, _, target| {
            let target = target.ok_or_else(|| JsError::type_error("TextEncoder requires new"))?;
            let prototype = vm
                .root_value(root)
                .expect("intrinsic TextEncoder prototype");
            let object = webidl::instance(vm, target, &prototype)?;
            vm.set_host_data(object.clone(), Rc::new(Encoder))?;
            Ok(object)
        },
    )?;
    getter(vm, prototype.clone(), "encoding", |vm, this, _| {
        encoder(vm, &this)?;
        Ok("utf-8".into())
    })?;
    webidl::operation(vm, prototype.clone(), "encode", 0, |vm, this, args| {
        encoder(vm, &this)?;
        let value = arg(args, 0);
        let text = if value.is_undefined() {
            JsString::default()
        } else {
            vm.to_string(&value)?
        };
        vm.charge_work(text.len().saturating_mul(2))?;
        let length = scalars(&text).map(char::len_utf8).sum();
        vm.new_uint8_array_with(length, |bytes| {
            encode_into(&text, bytes);
        })
    })?;
    webidl::operation(vm, prototype, "encodeInto", 2, |vm, this, args| {
        encoder(vm, &this)?;
        webidl::required(args, 2, "TextEncoder.encodeInto")?;
        let text = vm.to_string(&args[0])?;
        let destination = &args[1];
        if !vm.is_uint8_array(destination) {
            return Err(JsError::type_error(
                "TextEncoder destination must be a Uint8Array",
            ));
        }
        let capacity = vm.fixed_buffer_source_bytes(destination)?.len();
        vm.charge_work(text.len().min(capacity.saturating_add(2)))?;
        let (read, written) = encode_into(&text, &mut vm.fixed_uint8_array_bytes_mut(destination)?);
        let result = vm.new_object();
        vm.define_data_property(result.clone(), "read", read.into(), true, true, true)?;
        vm.define_data_property(result.clone(), "written", written.into(), true, true, true)?;
        Ok(result)
    })?;
    Ok(())
}

fn install_decoder(vm: &mut Vm) -> Result<(), JsError> {
    let prototype = vm.new_object();
    let root = vm.root(prototype.clone());
    webidl::interface(
        vm,
        prototype.clone(),
        "TextDecoder",
        0,
        move |vm, _, args, target| {
            let target = target.ok_or_else(|| JsError::type_error("TextDecoder requires new"))?;
            let value = arg(args, 0);
            let label = if value.is_undefined() {
                None
            } else {
                Some(vm.to_string(&value)?)
            };
            let options = dictionary(arg(args, 1))?;
            let fatal = option(vm, &options, "fatal")?;
            let ignore_bom = option(vm, &options, "ignoreBOM")?;
            let encoding = if let Some(text) = label {
                vm.charge_work(text.len())?;
                label_encoding(&text)?
            } else {
                UTF_8
            };
            let prototype = vm
                .root_value(root)
                .expect("intrinsic TextDecoder prototype");
            let object = webidl::instance(vm, target, &prototype)?;
            vm.set_host_data(
                object.clone(),
                Rc::new(TextDecoder {
                    encoding,
                    fatal,
                    ignore_bom,
                    pending: RefCell::new(None),
                }),
            )?;
            Ok(object)
        },
    )?;
    getter(vm, prototype.clone(), "encoding", |vm, this, _| {
        Ok(decoder(vm, &this)?
            .encoding
            .name()
            .to_ascii_lowercase()
            .into())
    })?;
    getter(vm, prototype.clone(), "fatal", |vm, this, _| {
        Ok(decoder(vm, &this)?.fatal.into())
    })?;
    getter(vm, prototype.clone(), "ignoreBOM", |vm, this, _| {
        Ok(decoder(vm, &this)?.ignore_bom.into())
    })?;
    webidl::operation(vm, prototype, "decode", 0, |vm, this, args| {
        let decoder = decoder(vm, &this)?;
        let input = arg(args, 0);
        if !input.is_undefined() {
            vm.fixed_buffer_source_bytes(&input)?;
        }
        let options = dictionary(arg(args, 1))?;
        let stream = option(vm, &options, "stream")?;
        // All conversions finish before borrowing native decoder state.
        decoder.decode(vm, input, stream)
    })?;
    Ok(())
}

fn label_encoding(text: &JsString) -> Result<&'static Encoding, JsError> {
    label(text).ok_or_else(|| {
        JsError::new(
            ErrorKind::RangeError,
            "Unsupported TextDecoder encoding label",
        )
    })
}

pub(super) fn install(vm: &mut Vm) -> Result<(), JsError> {
    install_encoder(vm)?;
    install_decoder(vm)
}
