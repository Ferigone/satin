use crate::script::{arg, event_target, getter, webidl};
use rust_browser_javascript::{JsError, JsString, Value, Vm};

const FIELDS: [(u32, &str); 3] = [(20, "lengthComputable"), (21, "loaded"), (22, "total")];

pub(super) fn install(vm: &mut Vm, event_proto: &Value) -> Result<Value, JsError> {
    let prototype = vm.new_object_with_prototype(event_proto.clone())?;
    let intrinsic = prototype.clone();
    webidl::interface(
        vm,
        prototype.clone(),
        "ProgressEvent",
        1,
        move |vm, _, args, target| {
            let target = target.ok_or_else(|| JsError::type_error("ProgressEvent requires new"))?;
            webidl::required(args, 1, "ProgressEvent")?;
            let kind = event_target::kind(vm, &arg(args, 0))?;
            let options = arg(args, 1);
            let flags = event_target::flags(vm, &options)?;
            let computable = event_target::option(vm, &options, "lengthComputable")?.is_truthy();
            let mut amounts = [0.0; 2];
            for (name, amount) in ["loaded", "total"].into_iter().zip(&mut amounts) {
                let value = event_target::option(vm, &options, name)?;
                if !value.is_undefined() {
                    *amount = vm.to_number(&value)?;
                    if !amount.is_finite() {
                        return Err(JsError::type_error("ProgressEvent amounts must be finite"));
                    }
                }
            }
            let object = webidl::instance(vm, target, &intrinsic)?;
            init(
                vm,
                object,
                kind,
                flags,
                false,
                [
                    Value::Bool(computable),
                    Value::Number(amounts[0]),
                    Value::Number(amounts[1]),
                ],
            )
        },
    )?;
    for (slot, name) in FIELDS {
        getter(vm, prototype.clone(), name, move |vm, event, _| {
            vm.host_slot(&event, slot)
                .ok_or_else(|| JsError::type_error("Illegal ProgressEvent receiver"))
        })?;
    }
    Ok(prototype)
}

pub(super) fn init(
    vm: &mut Vm,
    event: Value,
    kind: JsString,
    flags: [bool; 3],
    trusted: bool,
    values: [Value; 3],
) -> Result<Value, JsError> {
    let event = event_target::init_event(vm, event, kind, flags, trusted)?;
    for ((slot, _), value) in FIELDS.into_iter().zip(values) {
        vm.set_host_slot(event.clone(), slot, value)?;
    }
    Ok(event)
}
