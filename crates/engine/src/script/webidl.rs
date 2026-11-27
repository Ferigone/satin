//! Shared conversions and property installation for native Web IDL interfaces.
use rust_browser_javascript::{ErrorKind, JsError, PropertyKey, Value, Vm};

pub(super) fn required(args: &[Value], count: usize, operation: &str) -> Result<(), JsError> {
    if args.len() >= count {
        Ok(())
    } else {
        Err(JsError::type_error(format!(
            "{operation} requires {count} argument(s)"
        )))
    }
}

pub(super) fn usv_string(vm: &mut Vm, input: &Value, byte_limit: usize) -> Result<String, JsError> {
    let text = vm.to_string(input)?;
    vm.charge_work(text.len())?;
    if text.len() > byte_limit {
        return Err(string_limit());
    }
    let bytes: usize = char::decode_utf16(text.as_utf16().iter().copied())
        .map(|scalar| scalar.unwrap_or(char::REPLACEMENT_CHARACTER).len_utf8())
        .sum();
    if bytes > byte_limit {
        return Err(string_limit());
    }
    Ok(text.to_string_lossy())
}

fn string_limit() -> JsError {
    JsError::new(
        ErrorKind::ExecutionLimit,
        "Web IDL string exceeds its byte budget",
    )
}

pub(super) fn well_known_key(vm: &Vm, name: &str) -> PropertyKey {
    let Some(Value::Symbol(symbol)) = vm.well_known_symbol(name) else {
        unreachable!("well-known symbols are installed by the VM");
    };
    PropertyKey::Symbol(symbol)
}

pub(super) fn iterator_method(vm: &mut Vm, input: &Value) -> Result<Value, JsError> {
    vm.get_property_key(input.clone(), well_known_key(vm, "iterator"))
}

/// Sequence item conversion is interleaved with iterator steps. It can invoke
/// author getters or string conversion, so callers must release host borrows.
pub(super) fn sequence<T>(
    vm: &mut Vm,
    input: Value,
    iterator: Value,
    limit: usize,
    mut convert: impl FnMut(&mut Vm, Value) -> Result<T, JsError>,
) -> Result<Vec<T>, JsError> {
    if input.as_object().is_none() {
        return Err(JsError::type_error("Expected an iterable object"));
    }
    let mut record = vm.iterator_from_method(input, iterator)?;
    let mut out = Vec::new();
    while let Some(item) = vm.iterator_step(&mut record)? {
        if out.len() >= limit {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                format!("Web IDL sequence exceeds its {limit}-item budget"),
            ));
        }
        match convert(vm, item) {
            Ok(value) => out.push(value),
            Err(error) => return Err(vm.iterator_close(&mut record, Err(error)).unwrap_err()),
        }
    }
    Ok(out)
}

pub(super) fn tag(vm: &mut Vm, object: Value, name: &str) -> Result<(), JsError> {
    vm.define_data_property_key(
        object,
        well_known_key(vm, "toStringTag"),
        name.into(),
        false,
        false,
        true,
    )
}

pub(super) fn operation<F>(
    vm: &mut Vm,
    object: Value,
    name: &str,
    arity: u32,
    callback: F,
) -> Result<Value, JsError>
where
    F: Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
{
    let function = vm.new_native_function(name, callback);
    vm.define_data_property(function.clone(), "length", arity.into(), false, false, true)?;
    vm.define_data_property(object, name, function.clone(), true, true, true)?;
    Ok(function)
}

pub(super) fn interface<F>(
    vm: &mut Vm,
    prototype: Value,
    name: &str,
    arity: u32,
    callback: F,
) -> Result<Value, JsError>
where
    F: Fn(&mut Vm, Value, &[Value], Option<Value>) -> Result<Value, JsError> + 'static,
{
    let constructor = vm.new_native_constructor_with_new_target(name, callback);
    vm.define_data_property(
        constructor.clone(),
        "prototype",
        prototype.clone(),
        false,
        false,
        false,
    )?;
    vm.define_data_property(
        constructor.clone(),
        "length",
        arity.into(),
        false,
        false,
        true,
    )?;
    vm.define_data_property(
        prototype.clone(),
        "constructor",
        constructor.clone(),
        true,
        false,
        true,
    )?;
    tag(vm, prototype, name)?;
    vm.set_global(name, constructor.clone())?;
    Ok(constructor)
}

pub(super) fn instance(
    vm: &mut Vm,
    new_target: Value,
    intrinsic: &Value,
) -> Result<Value, JsError> {
    let prototype = vm.get_property(new_target, "prototype")?;
    let object = vm.new_object_with_prototype(if prototype.as_object().is_some() {
        prototype
    } else {
        intrinsic.clone()
    })?;
    vm.mark_platform_object(&object)?;
    Ok(object)
}
