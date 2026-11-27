//! Web Crypto entropy and digest operations for document and worker globals.
use super::{arg, dom_api::Errors, fatal, getter, webidl};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use rust_browser_javascript::{JsError, Value, Vm};
use std::{fmt::Write, rc::Rc};
use url::Url;

const MAX_RANDOM_BYTES: usize = 65_536;

struct Crypto;
struct SubtleCrypto;

fn secure_context(url: &Url) -> bool {
    match url.scheme() {
        "https" | "http" => crate::network::trustworthy(url),
        "blob" => Url::parse(&url.origin().ascii_serialization())
            .is_ok_and(|origin| secure_context(&origin)),
        _ => false,
    }
}

pub(super) fn install(
    vm: &mut Vm,
    global: &Value,
    address: &Url,
    errors: &Errors,
) -> Result<(), JsError> {
    let secure = secure_context(address);
    getter(vm, global.clone(), "isSecureContext", move |_, _, _| {
        Ok(secure.into())
    })?;

    let prototype = vm.new_object();
    webidl::interface(vm, prototype.clone(), "Crypto", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal Crypto constructor"))
    })?;
    let object = vm.new_object_with_prototype(prototype.clone())?;
    vm.set_host_data(object.clone(), Rc::new(Crypto))?;
    vm.mark_platform_object(&object)?;
    vm.root(object.clone());
    getter(vm, global.clone(), "crypto", move |_, _, _| {
        Ok(object.clone())
    })?;

    let random_errors = errors.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "getRandomValues",
        1,
        move |vm, this, args| {
            brand::<Crypto>(vm, &this)?;
            webidl::required(args, 1, "Crypto.getRandomValues")?;
            let target = arg(args, 0);
            let kind = vm.array_buffer_view_kind(&target)?;
            if !kind.is_integer_typed_array() {
                return Err(random_errors.exception(
                    vm,
                    "TypeMismatchError",
                    "Expected an integer typed array",
                ));
            }
            let length = vm.fixed_buffer_source_bytes(&target)?.len();
            if length > MAX_RANDOM_BYTES {
                return Err(random_errors.exception(
                    vm,
                    "QuotaExceededError",
                    "Random byte quota exceeded",
                ));
            }
            if length != 0 {
                let view = vm.fixed_array_buffer_view(&target)?;
                let mut bytes = vec![0; length];
                fill(vm, &random_errors, &mut bytes)?;
                vm.write_array_buffer(&view.buffer, view.byte_range.start, &bytes)?;
            }
            Ok(target)
        },
    )?;

    if secure {
        let uuid_errors = errors.clone();
        webidl::operation(
            vm,
            prototype.clone(),
            "randomUUID",
            0,
            move |vm, this, _| {
                brand::<Crypto>(vm, &this)?;
                let mut bytes = [0; 16];
                fill(vm, &uuid_errors, &mut bytes)?;
                bytes[6] = (bytes[6] & 0x0f) | 0x40;
                bytes[8] = (bytes[8] & 0x3f) | 0x80;
                let mut uuid = String::with_capacity(36);
                for (index, byte) in bytes.into_iter().enumerate() {
                    if matches!(index, 4 | 6 | 8 | 10) {
                        uuid.push('-');
                    }
                    write!(uuid, "{byte:02x}").expect("writing to a String");
                }
                Ok(uuid.into())
            },
        )?;
        let subtle = install_subtle(vm, errors.clone())?;
        getter(vm, prototype, "subtle", move |vm, this, _| {
            brand::<Crypto>(vm, &this)?;
            Ok(subtle.clone())
        })?;
    }
    Ok(())
}

fn brand<T: 'static>(vm: &Vm, this: &Value) -> Result<(), JsError> {
    if vm.host_data::<T>(this).is_none() {
        return Err(JsError::type_error("Illegal Web Crypto receiver"));
    }
    Ok(())
}

fn fill(vm: &mut Vm, errors: &Errors, output: &mut [u8]) -> Result<(), JsError> {
    SystemRandom::new()
        .fill(output)
        .map_err(|_| errors.exception(vm, "OperationError", "System random generator failed"))
}

fn install_subtle(vm: &mut Vm, errors: Errors) -> Result<Value, JsError> {
    let prototype = vm.new_object();
    webidl::interface(vm, prototype.clone(), "SubtleCrypto", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal SubtleCrypto constructor"))
    })?;
    let object = vm.new_object_with_prototype(prototype.clone())?;
    vm.mark_platform_object(&object)?;
    vm.set_host_data(object.clone(), Rc::new(SubtleCrypto))?;
    vm.root(object.clone());
    // Reuse the native task scheduler in both globals. Capture the original
    // operation so an author-defined setTimeout has no effect on digest.
    let timer = vm.get_global("setTimeout")?;
    let schedule = timer.clone();
    let operation = webidl::operation(vm, prototype, "digest", 2, move |vm, this, args| {
        let result = vm.new_promise_capability()?;
        let preparation = (|| {
            brand::<SubtleCrypto>(vm, &this)?;
            webidl::required(args, 2, "SubtleCrypto.digest")?;
            let bytes = digest_data(vm, &errors, arg(args, 0), arg(args, 1))?;
            vm.call(
                schedule.clone(),
                vm.global(),
                &[result.resolve, 0.into(), bytes],
            )?;
            Ok(())
        })();
        if let Err(error) = preparation {
            if fatal(&error) {
                return Err(error);
            }
            let reason = match &error.thrown {
                Some(reason) => reason.clone(),
                None => vm.error_value(&error)?,
            };
            vm.call(result.reject, Value::Undefined, &[reason])?;
        }
        Ok(result.promise)
    })?;
    vm.set_host_slot(operation, 0, timer)?;
    Ok(object)
}

fn digest_data(
    vm: &mut Vm,
    errors: &Errors,
    algorithm: Value,
    data: Value,
) -> Result<Value, JsError> {
    let length = vm.fixed_buffer_source_bytes(&data)?.len();
    vm.charge_work(length)?;
    let data = vm.fixed_buffer_source_bytes(&data)?.to_vec();
    let name = if algorithm.as_object().is_some() {
        let name = vm.get_property(algorithm, "name")?;
        if name.is_undefined() {
            return Err(JsError::type_error("Digest algorithm requires a name"));
        }
        name
    } else {
        algorithm
    };
    let name = vm.to_string(&name)?;
    if name.len() > "SHA-512".len() {
        return Err(errors.exception(vm, "NotSupportedError", "Unsupported digest algorithm"));
    }
    let name = name.to_string_lossy();
    let algorithm = match name.to_ascii_uppercase().as_str() {
        "SHA-1" => &digest::SHA1_FOR_LEGACY_USE_ONLY,
        "SHA-256" => &digest::SHA256,
        "SHA-384" => &digest::SHA384,
        "SHA-512" => &digest::SHA512,
        _ => return Err(errors.exception(vm, "NotSupportedError", "Unsupported digest algorithm")),
    };
    vm.new_array_buffer(digest::digest(algorithm, &data).as_ref().to_vec())
}
