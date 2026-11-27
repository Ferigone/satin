//! HTML's binary DOMString Base64 methods for windows and workers.
use super::{abort::dom_exception_error, webidl};
use crate::data_url::{DataUrlErrorKind, forgiving_base64};
use base64::{Engine, engine::general_purpose::STANDARD};
use rust_browser_javascript::{ErrorKind, JsError, JsString, Value, Vm};

pub(super) fn install(vm: &mut Vm) -> Result<(), JsError> {
    let class = vm.get_global("DOMException")?;
    let root = vm.root(class);
    for (name, encode) in [("btoa", true), ("atob", false)] {
        webidl::operation(vm, vm.global(), name, 1, move |vm, _, args| {
            webidl::required(args, 1, name)?;
            let source = vm.to_string(&args[0])?;
            let units = source.as_utf16();
            vm.charge_buffer_work(units.len())?;
            let invalid = |vm: &mut Vm| {
                let class = vm.root_value(root).expect("rooted DOMException");
                dom_exception_error(vm, class, "InvalidCharacterError", "Invalid Base64 input")
                    .unwrap_or_else(|error| error)
            };
            if encode {
                vm.ensure_string_allocation(units.len().div_ceil(3).saturating_mul(4))?;
                let bytes = units
                    .iter()
                    .copied()
                    .map(u8::try_from)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| invalid(vm))?;
                return Ok(STANDARD.encode(bytes).into());
            }
            let bytes = forgiving_base64(
                units
                    .iter()
                    .copied()
                    .map(|unit| u8::try_from(unit).unwrap_or(0xff)),
                vm.limits().max_string_units,
            )
            .map_err(|error| match error.kind {
                DataUrlErrorKind::Invalid => invalid(vm),
                DataUrlErrorKind::Limit => JsError::new(
                    ErrorKind::HeapLimit,
                    "Base64 output exceeds the string budget",
                ),
            })?;
            vm.ensure_string_allocation(bytes.len())?;
            Ok(Value::String(JsString::from_utf16(
                bytes.into_iter().map(u16::from).collect::<Vec<_>>(),
            )))
        })?;
    }
    Ok(())
}
