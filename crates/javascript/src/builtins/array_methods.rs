//! Generic indexed collection methods and change-array-by-copy operations.
mod from_async;
use super::{
    arg, compare_array_elements, integer, method, move_indexed_property, relative,
    reserve_sort_items, stable_merge_sort,
};
use crate::{ErrorKind, JsError, JsString, Value, Vm};

const MAX_SAFE: usize = if usize::BITS >= 53 {
    9_007_199_254_740_991u64 as usize
} else {
    usize::MAX
};

fn array_like(vm: &mut Vm, this: Value) -> Result<(Value, usize), JsError> {
    let object = vm.coerce_object(this)?;
    let length = vm.get_property(object.clone(), "length")?;
    let length = integer(vm, length)?.clamp(0., MAX_SAFE as f64) as usize;
    Ok((object, length))
}

fn end_index(vm: &mut Vm, value: Value, len: usize) -> Result<usize, JsError> {
    if value.is_undefined() {
        Ok(len)
    } else {
        Ok(relative(integer(vm, value)?, len))
    }
}

fn define(vm: &mut Vm, target: &Value, index: usize, value: Value) -> Result<(), JsError> {
    vm.define_data_property(target.clone(), &index.to_string(), value, true, true, true)
}

fn new_array(vm: &mut Vm, len: usize) -> Result<Value, JsError> {
    if len > u32::MAX as usize {
        return Err(JsError::new(ErrorKind::RangeError, "Invalid array length"));
    }
    let array = vm.try_new_array(&[])?;
    vm.set_property(array.clone(), "length", Value::from(len))?;
    Ok(array)
}

pub(super) fn array_of(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    if !vm.is_constructor(&this) {
        return vm.try_new_array(args);
    }
    let result = vm.construct(this, &[Value::from(args.len())])?;
    for (i, item) in args.iter().enumerate() {
        vm.tick()?;
        define(vm, &result, i, item.clone())?;
    }
    vm.set_property(result.clone(), "length", Value::from(args.len()))?;
    Ok(result)
}

pub(super) fn install_unscopables(vm: &mut Vm) {
    let blocked = vm.new_object_with_prototype(Value::Null).unwrap();
    for key in [
        "at",
        "copyWithin",
        "entries",
        "fill",
        "find",
        "findIndex",
        "findLast",
        "findLastIndex",
        "flat",
        "flatMap",
        "includes",
        "keys",
        "toReversed",
        "toSorted",
        "toSpliced",
        "values",
    ] {
        vm.define_data_property(blocked.clone(), key, true.into(), true, true, true)
            .unwrap();
    }
    vm.define_data_property_key(
        Value::Object(vm.realm.array_prototype),
        vm.well_known_key("unscopables"),
        blocked,
        false,
        false,
        true,
    )
    .unwrap();
}

pub(super) fn install(vm: &mut Vm, proto: &Value) {
    from_async::install(vm);
    method(vm, proto.clone(), "lastIndexOf", 1, |vm, this, args| {
        let (obj, len) = array_like(vm, this)?;
        if len == 0 {
            return Ok((-1).into());
        }
        let from = if args.len() > 1 {
            integer(vm, arg(args, 1))?
        } else {
            len as f64 - 1.
        };
        let index = if from < 0. {
            len as f64 + from
        } else {
            from.min(len as f64 - 1.)
        };
        if index < 0. {
            return Ok((-1).into());
        }
        let needle = arg(args, 0);
        for i in (0..=index as usize).rev() {
            vm.tick()?;
            let key = JsString::from(i.to_string());
            if vm.has(&obj, &key)? {
                let value = vm.get(obj.clone(), &key)?;
                vm.charge_bigint_comparison(&value, &needle)?;
                if value == needle {
                    return Ok(Value::from(i));
                }
            }
        }
        Ok((-1).into())
    });
    method(vm, proto.clone(), "copyWithin", 2, |vm, this, args| {
        let (obj, len) = array_like(vm, this)?;
        let to = relative(integer(vm, arg(args, 0))?, len);
        let from = relative(integer(vm, arg(args, 1))?, len);
        let end = end_index(vm, arg(args, 2), len)?;
        let count = end.saturating_sub(from).min(len - to);
        let backward = from < to && to < from + count;
        for i in 0..count {
            let offset = if backward { count - i - 1 } else { i };
            move_indexed_property(vm, &obj, from + offset, to + offset)?;
        }
        Ok(obj)
    });
    method(vm, proto.clone(), "fill", 1, |vm, this, args| {
        let (obj, len) = array_like(vm, this)?;
        let start = relative(integer(vm, arg(args, 1))?, len);
        let end = end_index(vm, arg(args, 2), len)?;
        let value = arg(args, 0);
        for i in start..end {
            vm.tick()?;
            vm.set_property(obj.clone(), &i.to_string(), value.clone())?;
        }
        Ok(obj)
    });
    for (name, which) in [
        ("toReversed", Copy::Reverse),
        ("toSpliced", Copy::Splice),
        ("with", Copy::With),
    ] {
        let length = if matches!(which, Copy::Reverse) { 0 } else { 2 };
        method(vm, proto.clone(), name, length, move |vm, this, args| {
            copy(vm, this, args, which)
        });
    }
    method(vm, proto.clone(), "toSorted", 1, |vm, this, args| {
        let comparator = arg(args, 0);
        if !comparator.is_undefined() && !vm.is_callable(&comparator) {
            return Err(JsError::type_error("Array sort comparator is not callable"));
        }
        let (obj, len) = array_like(vm, this)?;
        let result = new_array(vm, len)?;
        let mut items = Vec::new();
        reserve_sort_items(vm, &mut items, len)?;
        for i in 0..len {
            vm.tick()?;
            items.push(vm.get_property(obj.clone(), &i.to_string())?);
        }
        let mut scratch = Vec::new();
        reserve_sort_items(vm, &mut scratch, len)?;
        let items = stable_merge_sort(vm, items, scratch, |vm, a, b| {
            compare_array_elements(vm, &comparator, a, b)
        })?;
        for (i, value) in items.into_iter().enumerate() {
            vm.tick()?;
            define(vm, &result, i, value)?;
        }
        Ok(result)
    });
    for (name, map) in [("flat", false), ("flatMap", true)] {
        method(
            vm,
            proto.clone(),
            name,
            if map { 1 } else { 0 },
            move |vm, this, args| flatten(vm, this, args, map),
        );
    }
}
#[derive(Clone, Copy)]
enum Copy {
    Reverse,
    Splice,
    With,
}

fn copy(vm: &mut Vm, this: Value, args: &[Value], op: Copy) -> Result<Value, JsError> {
    let (obj, len) = array_like(vm, this)?;
    let (start, removed, added): (usize, usize, &[Value]) = match op {
        Copy::Reverse => (0, 0, &[]),
        Copy::With => {
            let i = integer(vm, arg(args, 0))?;
            let i = if i < 0. { len as f64 + i } else { i };
            if i < 0. || i >= len as f64 {
                return Err(JsError::new(
                    ErrorKind::RangeError,
                    "Array index out of range",
                ));
            }
            (i as usize, 1, &[])
        }
        Copy::Splice => {
            let start = relative(integer(vm, arg(args, 0))?, len);
            let count = match args.len() {
                0 => 0,
                1 => len - start,
                _ => integer(vm, arg(args, 1))?.clamp(0., (len - start) as f64) as usize,
            };
            (start, count, args.get(2..).unwrap_or(&[]))
        }
    };
    let inserted = if matches!(op, Copy::With) {
        1
    } else {
        added.len()
    };
    let new_len = (len - removed)
        .checked_add(inserted)
        .filter(|n| *n <= MAX_SAFE)
        .ok_or_else(|| JsError::type_error("Result exceeds the maximum safe length"))?;
    let result = new_array(vm, new_len)?;
    for i in 0..new_len {
        vm.tick()?;
        let value = if matches!(op, Copy::With) && i == start {
            arg(args, 1)
        } else if i >= start && i < start + added.len() {
            added[i - start].clone()
        } else {
            let source = if matches!(op, Copy::Reverse) {
                len - i - 1
            } else if i < start {
                i
            } else {
                i + removed - inserted
            };
            vm.get_property(obj.clone(), &source.to_string())?
        };
        define(vm, &result, i, value)?;
    }
    Ok(result)
}

fn flatten(vm: &mut Vm, this: Value, args: &[Value], map: bool) -> Result<Value, JsError> {
    let (obj, len) = array_like(vm, this)?;
    let callback = arg(args, 0);
    if map && !vm.is_callable(&callback) {
        return Err(JsError::type_error("flatMap mapper is not callable"));
    }
    let depth = if map || callback.is_undefined() {
        1.
    } else {
        integer(vm, callback.clone())?.max(0.)
    };
    let target = vm.array_species_create(obj.clone(), 0)?;
    let mut stack = vec![(obj.clone(), len, 0usize, depth)];
    let mut out = 0usize;
    while let Some((source, length, index, depth)) = stack.last_mut() {
        vm.tick()?;
        if *index == *length {
            stack.pop();
            continue;
        }
        let i = *index;
        *index += 1;
        let key = JsString::from(i.to_string());
        if !vm.has(source, &key)? {
            continue;
        }
        let mut value = vm.get(source.clone(), &key)?;
        let depth = *depth;
        if map && stack.len() == 1 {
            value = vm.invoke(
                callback.clone(),
                arg(args, 1),
                &[value, Value::from(i), obj.clone()],
            )?;
        }
        if depth > 0. && vm.is_array_exotic(&value)? {
            let (nested, len) = array_like(vm, value)?;
            vm.reserve_allocation(std::mem::size_of::<(Value, usize, usize, f64)>())?;
            stack.push((nested, len, 0, depth - 1.));
        } else {
            if out >= MAX_SAFE {
                return Err(JsError::type_error(
                    "Flattened array exceeds the maximum safe length",
                ));
            }
            define(vm, &target, out, value)?;
            out += 1;
        }
    }
    Ok(target)
}
