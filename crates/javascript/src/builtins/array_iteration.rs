use super::{arg, callback_iteration::CallbackIteration, method};
use crate::{
    JsError, JsString, Value, Vm,
    heap::{ObjectKind, Property, array_index},
};
use std::ops::Range;

#[derive(Clone, Copy)]
enum Visit {
    Every,
    Some,
    Filter,
}

pub(super) fn install(vm: &mut Vm, proto: &Value) {
    for (name, intrinsic, map) in [
        ("forEach", crate::heap::NativeCall::ArrayForEach, false),
        ("map", crate::heap::NativeCall::ArrayMap, true),
    ] {
        let function = method(vm, proto.clone(), name, 1, move |vm, this, args| {
            CallbackIteration::array(vm, this, args, map)?.run(vm)
        });
        vm.mark_owned_native_call(&function, intrinsic);
    }
    for (name, kind) in [
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
    for (name, right, return_index) in [
        ("find", false, false),
        ("findIndex", false, true),
        ("findLast", true, false),
        ("findLastIndex", true, true),
    ] {
        method(vm, proto.clone(), name, 1, move |vm, this, args| {
            find(vm, this, args, right, return_index)
        });
    }
}

pub(super) fn prepare(vm: &mut Vm, this: Value, args: &[Value]) -> Result<(Value, u64, Value), JsError> {
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

/// Each candidate is looked up again after the last callback, indexed getter
/// or species result write. Only an uninterrupted run of holes may be skipped.
pub(super) struct PresentIndices {
    range: Range<u64>,
    reverse: bool,
}

impl PresentIndices {
    pub(super) fn new(range: Range<u64>, reverse: bool) -> Self {
        Self { range, reverse }
    }

    pub(super) fn next(
        &mut self,
        vm: &mut Vm,
        object: &Value,
    ) -> Result<Option<(u64, JsString)>, JsError> {
        let mut misses = 0usize;
        loop {
            let index = if self.reverse {
                self.range.next_back()
            } else {
                self.range.next()
            };
            let Some(index) = index else { return Ok(None) };
            vm.tick()?;
            let key = JsString::from(index.to_string());
            if vm.has(object, &key)? {
                return Ok(Some((index, key)));
            }
            misses += 1;
            if misses >= 64
                && misses.is_power_of_two()
                && self.range.end - self.range.start > misses as u64
            {
                self.skip_ordinary_holes(vm, object, misses)?;
            }
        }
    }

    fn skip_ordinary_holes(
        &mut self,
        vm: &mut Vm,
        object: &Value,
        mut scan_budget: usize,
    ) -> Result<(), JsError> {
        let Some(mut id) = object.as_object() else {
            return Ok(());
        };
        if self.range.end > u64::from(u32::MAX)
            || !matches!(vm.heap.object(id)?.kind, ObjectKind::Array { .. })
        {
            return Ok(());
        }
        let mut nearest = None;
        loop {
            let entry = vm.heap.object(id)?;
            if !matches!(entry.kind, ObjectKind::Ordinary | ObjectKind::Array { .. })
                || entry.is_platform_object()
            {
                return Ok(());
            }
            // A key scan must cost no more than the sequential misses that
            // justified it. This bounds repeated scans on partly dense inputs.
            let cost = entry.properties.len() + 1;
            let Some(remaining) = scan_budget.checked_sub(cost) else {
                return Ok(());
            };
            scan_budget = remaining;
            vm.consume_work(cost)?;
            let entry = vm.heap.object(id)?;
            for key in entry.properties.keys().filter(|key| key.len() <= 10) {
                if let Some(index) = array_index(key).map(u64::from)
                    && self.range.contains(&index)
                {
                    nearest = Some(match nearest {
                        Some(found) if self.reverse => index.max(found),
                        Some(found) => index.min(found),
                        None => index,
                    });
                }
            }
            let Some(parent) = entry.prototype else { break };
            id = parent;
        }
        if self.reverse {
            self.range.end = nearest.map_or(self.range.start, |i| i + 1);
        } else {
            self.range.start = nearest.unwrap_or(self.range.end);
        }
        Ok(())
    }
}

fn visit(vm: &mut Vm, this: Value, args: &[Value], kind: Visit) -> Result<Value, JsError> {
    let (object, length, callback) = prepare(vm, this, args)?;
    let result = match kind {
        Visit::Filter => vm.array_species_create(object.clone(), 0)?,
        Visit::Every => Value::Bool(true),
        Visit::Some => Value::Bool(false),
    };
    let this_arg = arg(args, 1);
    let mut count = 0u64;
    let mut indices = PresentIndices::new(0..length, false);
    while let Some((index, key)) = indices.next(vm, &object)? {
        let value = vm.get(object.clone(), &key)?;
        let mapped = vm.invoke(
            callback.clone(),
            this_arg.clone(),
            &[value.clone(), Value::from(index as f64), object.clone()],
        )?;
        match kind {
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
    let mut indices = PresentIndices::new(0..length, right);
    while let Some((index, key)) = indices.next(vm, &object)? {
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

fn find(
    vm: &mut Vm,
    this: Value,
    args: &[Value],
    right: bool,
    return_index: bool,
) -> Result<Value, JsError> {
    let (object, length, callback) = prepare(vm, this, args)?;
    let this_arg = arg(args, 1);
    // FindViaPredicate uses Get, not HasProperty: holes still call the
    // predicate, and every indexed lookup is observable on exotic receivers.
    for offset in 0..length {
        vm.tick()?;
        let index = if right { length - 1 - offset } else { offset };
        let key = JsString::from(index.to_string());
        let value = vm.get(object.clone(), &key)?;
        let index = Value::from(index as f64);
        let matched = vm.invoke(
            callback.clone(),
            this_arg.clone(),
            &[value.clone(), index.clone(), object.clone()],
        )?;
        if matched.is_truthy() {
            return Ok(if return_index { index } else { value });
        }
    }
    Ok(if return_index {
        Value::from(-1)
    } else {
        Value::Undefined
    })
}
