//! HTML ErrorEvent and the Window uncaught exception reporting algorithm.
use super::{Shared, arg, dispatch_prepared, event_target, fatal, getter, webidl};
use rust_browser_javascript::{ErrorKind, JsError, JsString, Value, Vm};

const FIELDS: [&str; 5] = ["message", "filename", "lineno", "colno", "error"];
const FIELD_SLOT: u32 = 16;

pub(super) fn install(vm: &mut Vm, event: Value, event_proto: Value) -> Result<Value, JsError> {
    let proto = vm.new_object_with_prototype(event_proto)?;
    let intrinsic = proto.clone();
    let constructor = webidl::interface(
        vm,
        proto.clone(),
        "ErrorEvent",
        1,
        move |vm, _, args, target| {
            let target = target.ok_or_else(|| JsError::type_error("ErrorEvent requires new"))?;
            webidl::required(args, 1, "ErrorEvent")?;
            let kind = event_target::kind(vm, &args[0])?;
            let init = arg(args, 1);
            let flags = event_target::flags(vm, &init)?;

            // Dictionary members are converted in lexicographic order after
            // the inherited EventInit fields; these reads can invoke getters.
            let colno = number_field(vm, &init, "colno")?;
            let error = match event_target::option(vm, &init, "error")? {
                Value::Undefined => Value::Null,
                value => value,
            };
            let filename = string_field(vm, &init, "filename")?;
            let lineno = number_field(vm, &init, "lineno")?;
            let message = string_field(vm, &init, "message")?;
            let object = webidl::instance(vm, target, &intrinsic)?;
            event_target::init_event(vm, object.clone(), kind, flags, false)?;
            set_fields(vm, &object, [message, filename, lineno, colno, error])?;
            Ok(object)
        },
    )?;
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    vm.call(set_prototype, Value::Undefined, &[constructor, event])?;
    for (index, name) in FIELDS.into_iter().enumerate() {
        getter(vm, proto.clone(), name, move |vm, this, _| {
            vm.host_slot(&this, FIELD_SLOT + index as u32)
                .ok_or_else(|| JsError::type_error("Illegal ErrorEvent receiver"))
        })?;
    }
    Ok(proto)
}

fn number_field(vm: &mut Vm, init: &Value, name: &str) -> Result<Value, JsError> {
    let value = event_target::option(vm, init, name)?;
    let number = vm.to_number(&value)?;
    Ok((number.trunc().rem_euclid(4_294_967_296.) as u32).into())
}

fn string_field(vm: &mut Vm, init: &Value, name: &str) -> Result<Value, JsError> {
    let value = event_target::option(vm, init, name)?;
    Ok(Value::String(if value.is_undefined() {
        JsString::default()
    } else {
        vm.to_string(&value)?
    }))
}

fn set_fields(vm: &mut Vm, object: &Value, fields: [Value; 5]) -> Result<(), JsError> {
    for (index, value) in fields.into_iter().enumerate() {
        vm.set_host_slot(object.clone(), FIELD_SLOT + index as u32, value)?;
    }
    Ok(())
}

pub(super) fn handler_args(vm: &Vm, event: &Value) -> Option<[Value; 5]> {
    vm.host_slot(event, FIELD_SLOT)?;
    Some(std::array::from_fn(|index| {
        vm.host_slot(event, FIELD_SLOT + index as u32)
            .expect("all ErrorEvent fields are initialized together")
    }))
}

/// Return whether normal diagnostic reporting should proceed.
pub(super) fn report(
    vm: &mut Vm,
    state: &Shared,
    failure: &JsError,
    script_url: Option<&str>,
) -> Result<bool, JsError> {
    if fatal(failure) || failure.kind == ErrorKind::UnsupportedFeature {
        return Ok(true);
    }
    let (prototype, filename, muted) = {
        let mut state = state.borrow_mut();
        if state.reporting_error {
            return Ok(true);
        }
        let muted = script_url.is_some_and(|source| {
            // Until the classic runner receives a CORS visibility bit, never
            // expose cross-origin error details (including thrown values).
            !url::Url::parse(source).is_ok_and(|source| source.origin() == state.url.origin())
        });
        let filename = script_url.unwrap_or(state.url.as_str()).to_owned();
        state.reporting_error = true;
        (state.error_event_prototype.clone(), filename, muted)
    };
    let result = (|| {
        let object = vm.new_object_with_prototype(prototype)?;
        let fields = if muted {
            [
                "Script error.".into(),
                "".into(),
                0.into(),
                0.into(),
                Value::Null,
            ]
        } else {
            let error = match &failure.thrown {
                Some(thrown) => thrown.clone(),
                None => vm.error_value(failure)?,
            };
            let message = if failure.kind == ErrorKind::Thrown {
                format!("Uncaught {}", failure.message)
            } else {
                format!("Uncaught {}: {}", failure.kind.name(), failure.message)
            };
            [
                message.into(),
                filename.into(),
                (failure.line.unwrap_or(0) as u32).into(),
                (failure.column.unwrap_or(0) as u32).into(),
                error,
            ]
        };
        set_fields(vm, &object, fields)?;
        let event =
            event_target::init_event(vm, object, "error".into(), [false, true, false], true)?;
        dispatch_prepared(vm, state, None, event, &[])
    })();
    state.borrow_mut().reporting_error = false;
    result
}
