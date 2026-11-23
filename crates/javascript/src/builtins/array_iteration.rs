use super::{arg, method};
use crate::{JsError, JsString, Value, Vm, heap::Property};

#[derive(Clone, Copy)]
enum Visit {
    Every,
    Some,
    ForEach,
    Map,
    Filter,
}

pub(super) fn install(vm: &mut Vm, proto: &Value) {
    for (name, kind) in [
        ("forEach", Visit::ForEach),
        ("map", Visit::Map),
        ("filter", Visit::Filter),
        ("some", Visit::Some),
        ("every", Visit::Every),
    ] {
        method(vm, proto.clone(), name, 1, move |vm, this, args| {
            visit(vm, this, args, kind)
        });
    }
    for (name, right) in [("reduce", false), ("reduceRight", true)] {
        method(vm, proto.clone(), name, 1, move |vm, this, args| {
            reduce(vm, this, args, right)
        });
    }
}

fn prepare(vm: &mut Vm, this: Value, args: &[Value]) -> Result<(Value, u64, Value), JsError> {
    let object = vm.coerce_object(this)?;
    let length = vm.get_property(object.clone(), "length")?;
    // LengthOfArrayLike: a large input need not be visited in full or even be
    // an Array. Apply work limits as we visit, not while coercing the length.
    let length = vm
        .to_number(&length)?
        .trunc()
        .clamp(0.0, 9_007_199_254_740_991.0) as u64;
    let callback = arg(args, 0);
    if !vm.is_callable(&callback) {
        return Err(JsError::type_error("Array callback is not callable"));
    }
    Ok((object, length, callback))
}

fn visit(vm: &mut Vm, this: Value, args: &[Value], kind: Visit) -> Result<Value, JsError> {
    let (object, length, callback) = prepare(vm, this, args)?;
    let result = match kind {
        Visit::Map => vm.array_species_create(object.clone(), length)?,
        Visit::Filter => vm.array_species_create(object.clone(), 0)?,
        Visit::Every => Value::Bool(true),
        Visit::Some => Value::Bool(false),
        Visit::ForEach => Value::Undefined,
    };
    let this_arg = arg(args, 1);
    let mut count = 0u64;
    for index in 0..length {
        vm.tick()?;
        let key = JsString::from(index.to_string());
        if !vm.has(&object, &key)? {
            continue;
        }
        let value = vm.get(object.clone(), &key)?;
        let mapped = vm.invoke(
            callback.clone(),
            this_arg.clone(),
            &[value.clone(), Value::from(index as f64), object.clone()],
        )?;
        match kind {
            Visit::Map => vm.define_property(result.clone(), key, Property::data(mapped))?,
            Visit::Filter if mapped.is_truthy() => {
                vm.define_property(
                    result.clone(),
                    JsString::from(count.to_string()),
                    Property::data(value),
                )?;
                count += 1;
            }
            Visit::Every if !mapped.is_truthy() => return Ok(Value::Bool(false)),
            Visit::Some if mapped.is_truthy() => return Ok(Value::Bool(true)),
            _ => {}
        }
    }
    Ok(result)
}

fn reduce(vm: &mut Vm, this: Value, args: &[Value], right: bool) -> Result<Value, JsError> {
    let (object, length, callback) = prepare(vm, this, args)?;
    let mut accumulator = args.get(1).cloned();
    for offset in 0..length {
        vm.tick()?;
        let index = if right { length - 1 - offset } else { offset };
        let key = JsString::from(index.to_string());
        if !vm.has(&object, &key)? {
            continue;
        }
        let value = vm.get(object.clone(), &key)?;
        accumulator = Some(match accumulator {
            Some(accumulator) => vm.invoke(
                callback.clone(),
                Value::Undefined,
                &[
                    accumulator,
                    value,
                    Value::from(index as f64),
                    object.clone(),
                ],
            )?,
            None => value,
        });
    }
    accumulator.ok_or_else(|| JsError::type_error("Reduce of empty array without initial value"))
}
