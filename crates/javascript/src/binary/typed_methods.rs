use super::{
    TypedArray, absolute_index, allocation_error, arg, clamped_index, element::Element, kind_id,
    range_error, species_constructor, to_length, typed_array::LITTLE_ENDIAN,
};
use crate::{
    JsError, JsString, Value, Vm,
    builtins::{join_indexed_properties, method},
    heap::ObjectKind,
};
use std::{cmp::Ordering, mem::size_of, ops::Range};

fn receiver(vm: &Vm, value: &Value) -> Result<TypedArray, JsError> {
    vm.array_data(kind_id(vm, value, "TypedArray receiver must be an object")?)
}

impl TypedArray {
    fn range(self, vm: &Vm) -> Result<Range<usize>, JsError> {
        vm.require_view_range(self.view, self.element.width())
    }

    fn length(self, vm: &Vm) -> Result<usize, JsError> {
        Ok(self.range(vm)?.len() / self.element.width())
    }

    fn relative_bytes(self, start: usize, count: usize) -> Range<usize> {
        let width = self.element.width();
        let start = self.view.offset + start * width;
        start..start + count * width
    }
}

fn construct(
    vm: &mut Vm,
    ctor: Value,
    args: &[Value],
    minimum: usize,
) -> Result<(Value, TypedArray), JsError> {
    let result = vm.construct(ctor, args)?;
    let array = receiver(vm, &result)?;
    if array.length(vm)? < minimum {
        return Err(JsError::type_error(
            "TypedArray constructor result is too short",
        ));
    }
    Ok((result, array))
}

fn species(
    vm: &mut Vm,
    this: Value,
    source: TypedArray,
    args: &[Value],
    minimum: usize,
) -> Result<(Value, TypedArray), JsError> {
    let fallback = vm.binary.typed_constructors[source.element as usize].unwrap();
    let ctor = species_constructor(vm, this, fallback)?;
    let (result, array) = construct(vm, ctor, args, minimum)?;
    source.element.check_content_type(array.element)?;
    Ok((result, array))
}

fn same_type(
    vm: &mut Vm,
    source: TypedArray,
    length: usize,
) -> Result<(Value, TypedArray), JsError> {
    let prototype = vm.binary.typed[source.element as usize].unwrap();
    let value = vm.allocate_typed_array(source.element, length, prototype)?;
    let array = receiver(vm, &value)?;
    Ok((value, array))
}

fn end_index(vm: &mut Vm, value: &Value, length: usize) -> Result<usize, JsError> {
    if value.is_undefined() {
        Ok(length)
    } else {
        clamped_index(vm, value, length)
    }
}

fn at(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    let array = receiver(vm, &this)?;
    let length = array.length(vm)?;
    let index = absolute_index(vm, &arg(args, 0), length)?;
    if (0.0..length as f64).contains(&index) {
        vm.read_typed_element(array, index as usize)
    } else {
        Ok(Value::Undefined)
    }
}

fn subarray(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    let array = receiver(vm, &this)?;
    let width = array.element.width();
    let length = vm
        .view_range(array.view, width)?
        .map_or(0, |range| range.len() / width);
    let start = clamped_index(vm, &arg(args, 0), length)?;
    let end = arg(args, 1);
    let offset = array.view.offset + start * width;
    let params = [
        Value::Object(array.view.buffer),
        offset.into(),
        Value::Undefined,
    ];
    if array.view.bytes.is_none() && end.is_undefined() {
        return Ok(species(vm, this, array, &params[..2], 0)?.0);
    }
    let length = end_index(vm, &end, length)?.saturating_sub(start);
    let params = [params[0].clone(), params[1].clone(), length.into()];
    Ok(species(vm, this, array, &params, 0)?.0)
}

fn slice(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    let source = receiver(vm, &this)?;
    let length = source.length(vm)?;
    let start = clamped_index(vm, &arg(args, 0), length)?;
    let end = end_index(vm, &arg(args, 1), length)?;
    let count = end.saturating_sub(start);
    let (result, target) = species(vm, this, source, &[count.into()], count)?;
    if count == 0 {
        return Ok(result);
    }
    let count = end.min(source.length(vm)?).saturating_sub(start);
    if count == 0 {
        return Ok(result);
    }
    if source.element.same_encoding(target.element) {
        let from = source.relative_bytes(start, count);
        let offset = target.view.offset;
        if source.view.buffer == target.view.buffer && from.start < offset && offset < from.end {
            // A species constructor can return an overlapping view. Slice reads
            // forward, so a destination byte can become the next source byte.
            vm.consume_work(from.len())?;
            let bytes = vm
                .buffer_data_mut(source.view.buffer)?
                .bytes
                .as_mut()
                .unwrap();
            for (index, read) in from.enumerate() {
                bytes[offset + index] = bytes[read];
            }
        } else {
            vm.copy_binary_between(source.view.buffer, from, target.view.buffer, offset)?;
        }
    } else {
        vm.consume_work(count)?;
        for index in 0..count {
            let bits = vm.read_typed_bits(source, start + index)?.unwrap();
            let number = source.element.number_value(bits);
            vm.write_typed_bits(target, index, target.element.number_bits(number))?;
        }
    }
    Ok(result)
}

fn set(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    let target = receiver(vm, &this)?;
    let offset = vm.to_number(&arg(args, 1))?.trunc();
    if offset < 0.0 {
        return Err(range_error("TypedArray.set offset is negative"));
    }
    let offset = offset.max(0.0);
    let source = arg(args, 0);
    let target_length = target.length(vm)?;
    if let Some(id) = source.as_object()
        && let ObjectKind::TypedArray(input) = vm.heap.object(id)?.kind
    {
        let range = input.range(vm)?;
        let count = range.len() / input.element.width();
        if offset + count as f64 > target_length as f64 {
            return Err(range_error("TypedArray.set source exceeds target length"));
        }
        target.element.check_content_type(input.element)?;
        let offset = offset as usize;
        if input.element.same_encoding(target.element) {
            vm.copy_binary_between(
                input.view.buffer,
                range,
                target.view.buffer,
                target.relative_bytes(offset, 0).start,
            )?;
        } else {
            copy_converted(vm, input, range, target, offset, count)?;
        }
    } else {
        let source = vm.coerce_object(source)?;
        let length = vm.get_property(source.clone(), "length")?;
        let count = to_length(vm, &length)?;
        if offset + count as f64 > target_length as f64 {
            return Err(range_error("TypedArray.set source exceeds target length"));
        }
        for index in 0..count {
            vm.tick()?;
            let value = vm.get_property(source.clone(), &index.to_string())?;
            vm.write_typed_value(target, offset as usize + index, &value)?;
        }
    }
    Ok(Value::Undefined)
}

fn copy_converted(
    vm: &mut Vm,
    input: TypedArray,
    range: Range<usize>,
    target: TypedArray,
    offset: usize,
    count: usize,
) -> Result<(), JsError> {
    vm.consume_work(count)?;
    if input.view.buffer == target.view.buffer {
        let bytes = vm.copy_binary_range(input.view.buffer, range)?;
        for (index, chunk) in bytes.chunks_exact(input.element.width()).enumerate() {
            let number = input
                .element
                .number_value(Element::load_bits(chunk, LITTLE_ENDIAN));
            vm.write_typed_bits(target, offset + index, target.element.number_bits(number))?;
        }
    } else {
        for index in 0..count {
            let bits = vm.read_typed_bits(input, index)?.unwrap();
            let number = input.element.number_value(bits);
            vm.write_typed_bits(target, offset + index, target.element.number_bits(number))?;
        }
    }
    Ok(())
}

fn copy_within(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    let array = receiver(vm, &this)?;
    let length = array.length(vm)?;
    let target = clamped_index(vm, &arg(args, 0), length)?;
    let start = clamped_index(vm, &arg(args, 1), length)?;
    let end = end_index(vm, &arg(args, 2), length)?;
    let count = end.saturating_sub(start).min(length - target);
    if count > 0 {
        let length = array.length(vm)?;
        let count = count
            .min(length.saturating_sub(start))
            .min(length.saturating_sub(target));
        if count > 0 {
            vm.copy_binary_between(
                array.view.buffer,
                array.relative_bytes(start, count),
                array.view.buffer,
                array.relative_bytes(target, 0).start,
            )?;
        }
    }
    Ok(this)
}

fn fill(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    let array = receiver(vm, &this)?;
    let length = array.length(vm)?;
    let bits = array.element.coerce_bits(vm, &arg(args, 0))?;
    let start = clamped_index(vm, &arg(args, 1), length)?;
    let end = end_index(vm, &arg(args, 2), length)?.min(array.length(vm)?);
    if start < end {
        let range = array.relative_bytes(start, end - start);
        let width = array.element.width();
        let pattern = array.element.store_bits(bits, LITTLE_ENDIAN);
        vm.binary_work(range.len())?;
        let bytes = &mut vm
            .buffer_data_mut(array.view.buffer)?
            .bytes
            .as_mut()
            .unwrap()[range];
        if width == 1 {
            bytes.fill(pattern[0]);
        } else {
            for chunk in bytes.chunks_exact_mut(width) {
                chunk.copy_from_slice(&pattern[..width]);
            }
        }
    }
    Ok(this)
}

fn reverse(vm: &mut Vm, this: Value, copy: bool) -> Result<Value, JsError> {
    let array = receiver(vm, &this)?;
    let range = array.range(vm)?;
    let width = array.element.width();
    let length = range.len() / width;
    let (result, destination) = if copy {
        let (result, target) = same_type(vm, array, length)?;
        vm.copy_binary_between(
            array.view.buffer,
            range,
            target.view.buffer,
            target.view.offset,
        )?;
        (result, target)
    } else {
        (this, array)
    };
    vm.consume_work(length.div_ceil(4))?;
    let bytes = &mut vm
        .buffer_data_mut(destination.view.buffer)?
        .bytes
        .as_mut()
        .unwrap()[destination.relative_bytes(0, length)];
    for index in 0..length / 2 {
        for byte in 0..width {
            bytes.swap(index * width + byte, (length - index - 1) * width + byte);
        }
    }
    Ok(result)
}

fn with(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    let source = receiver(vm, &this)?;
    let length = source.length(vm)?;
    let index = absolute_index(vm, &arg(args, 0), length)?;
    let replacement = source.element.coerce_bits(vm, &arg(args, 1))?;
    let current = vm
        .view_range(source.view, source.element.width())?
        .map_or(0, |range| range.len() / source.element.width());
    if !(0.0..current as f64).contains(&index) {
        return Err(range_error("TypedArray.with index exceeds array length"));
    }
    let (result, target) = same_type(vm, source, length)?;
    vm.consume_work(length)?;
    for k in 0..length {
        let bits = if k == index as usize {
            replacement
        } else if let Some(bits) = vm.read_typed_bits(source, k)? {
            bits
        } else {
            source.element.coerce_bits(vm, &Value::Undefined)?
        };
        vm.write_typed_bits(target, k, bits)?;
    }
    Ok(result)
}

fn join(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    let array = receiver(vm, &this)?;
    let length = array.length(vm)?;
    let separator = arg(args, 0);
    let separator = if separator.is_undefined() {
        JsString::from(",")
    } else {
        vm.to_string(&separator)?
    };
    let fixed = (length.saturating_sub(1)).saturating_mul(separator.len());
    vm.ensure_string_allocation(fixed)?;
    join_indexed_properties(vm, length, &separator, None, |vm, index| {
        vm.read_typed_element(array, index)
    })
}

fn to_locale_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value, JsError> {
    let array = receiver(vm, &this)?;
    let length = array.length(vm)?;
    join_indexed_properties(
        vm,
        length,
        &",".into(),
        Some("toLocaleString"),
        |vm, index| vm.read_typed_element(array, index),
    )
}

#[derive(Clone, Copy)]
enum Search {
    Includes,
    IndexOf,
    LastIndexOf,
}

fn search(vm: &mut Vm, this: Value, args: &[Value], mode: Search) -> Result<Value, JsError> {
    let array = receiver(vm, &this)?;
    let length = array.length(vm)?;
    let includes = matches!(mode, Search::Includes);
    let absent = if includes { false.into() } else { (-1).into() };
    if length == 0 {
        return Ok(absent);
    }
    let last = matches!(mode, Search::LastIndexOf);
    let offset = if last && args.len() < 2 {
        (length - 1) as f64
    } else {
        absolute_index(vm, &arg(args, 1), length)?
    };
    if (last && offset < 0.0) || (!last && offset >= length as f64) {
        return Ok(absent);
    }
    let needle = arg(args, 0);
    let start = offset.min((length - 1) as f64).max(0.0) as usize;
    let (count, direction) = if last {
        (start + 1, -1)
    } else {
        (length - start, 1)
    };
    for step in 0..count {
        vm.tick()?;
        let index = (start as isize + step as isize * direction) as usize;
        let value = vm.read_typed_element(array, index)?;
        if (includes || !value.is_undefined())
            && (value == needle || (includes && is_nan(&value) && is_nan(&needle)))
        {
            return Ok(if includes { true.into() } else { index.into() });
        }
    }
    Ok(absent)
}

fn is_nan(value: &Value) -> bool {
    value.as_number().is_some_and(f64::is_nan)
}

#[derive(Clone, Copy)]
enum Visit {
    Every,
    Some,
    ForEach,
    Find { last: bool, index: bool },
    Map,
    Filter,
}

fn visit(vm: &mut Vm, this: Value, args: &[Value], mode: Visit) -> Result<Value, JsError> {
    let source = receiver(vm, &this)?;
    let length = source.length(vm)?;
    let callback = require_callback(vm, arg(args, 0))?;
    let context = arg(args, 1);
    let output = if matches!(mode, Visit::Map) {
        Some(species(vm, this.clone(), source, &[length.into()], length)?)
    } else {
        None
    };
    let mut kept = Vec::new();
    for step in 0..length {
        vm.tick()?;
        let index = if matches!(mode, Visit::Find { last: true, .. }) {
            length - 1 - step
        } else {
            step
        };
        let value = vm.read_typed_element(source, index)?;
        let selected = vm.invoke(
            callback.clone(),
            context.clone(),
            &[value.clone(), index.into(), this.clone()],
        )?;
        match mode {
            Visit::Every if !selected.is_truthy() => return Ok(false.into()),
            Visit::Some if selected.is_truthy() => return Ok(true.into()),
            Visit::Find {
                index: as_index, ..
            } if selected.is_truthy() => {
                return Ok(if as_index { index.into() } else { value });
            }
            Visit::Map => vm.write_typed_value(output.as_ref().unwrap().1, index, &selected)?,
            Visit::Filter if selected.is_truthy() => {
                vm.reserve_allocation(size_of::<Value>() * 2)?;
                kept.push(value);
            }
            _ => {}
        }
    }
    match mode {
        Visit::Every => Ok(true.into()),
        Visit::Some => Ok(false.into()),
        Visit::Find { index: true, .. } => Ok((-1).into()),
        Visit::Map => Ok(output.unwrap().0),
        Visit::Filter => {
            let (value, target) = species(vm, this, source, &[kept.len().into()], kept.len())?;
            for (index, source) in kept.iter().enumerate() {
                vm.tick()?;
                vm.write_typed_value(target, index, source)?;
            }
            Ok(value)
        }
        _ => Ok(Value::Undefined),
    }
}

fn require_callback(vm: &Vm, callback: Value) -> Result<Value, JsError> {
    if !vm.is_callable(&callback) {
        return Err(JsError::type_error("TypedArray callback is not callable"));
    }
    Ok(callback)
}

fn reduce(vm: &mut Vm, this: Value, args: &[Value], right: bool) -> Result<Value, JsError> {
    let array = receiver(vm, &this)?;
    let length = array.length(vm)?;
    let callback = require_callback(vm, arg(args, 0))?;
    let initial = args.get(1).cloned();
    let (mut acc, start) = if let Some(initial) = initial {
        (initial, 0)
    } else {
        if length == 0 {
            return Err(JsError::type_error(
                "Reduce of empty TypedArray without initial value",
            ));
        }
        (
            vm.read_typed_element(array, if right { length - 1 } else { 0 })?,
            1,
        )
    };
    for step in start..length {
        vm.tick()?;
        let index = if right { length - 1 - step } else { step };
        let value = vm.read_typed_element(array, index)?;
        acc = vm.invoke(
            callback.clone(),
            Value::Undefined,
            &[acc, value, index.into(), this.clone()],
        )?;
    }
    Ok(acc)
}

fn from(vm: &mut Vm, ctor: Value, args: &[Value]) -> Result<Value, JsError> {
    if !vm.is_constructor(&ctor) {
        return Err(JsError::type_error(
            "TypedArray.from receiver is not a constructor",
        ));
    }
    let source = arg(args, 0);
    let callback = arg(args, 1);
    if !callback.is_undefined() {
        require_callback(vm, callback.clone())?;
    }
    let context = arg(args, 2);
    let method = vm.get_property_key(source.clone(), vm.well_known_key("iterator"))?;
    if !method.is_nullish() {
        let iterator = vm.iterator_from_method(source, method)?;
        let values = vm.iterator_record_to_list(iterator)?;
        return create_from_iter(vm, ctor, values.into_iter(), callback, context);
    }
    let source = vm.coerce_object(source)?;
    let length = vm.get_property(source.clone(), "length")?;
    let length = to_length(vm, &length)?;
    let (result, target) = construct(vm, ctor, &[length.into()], length)?;
    for index in 0..length {
        vm.tick()?;
        let value = vm.get_property(source.clone(), &index.to_string())?;
        store_mapped(vm, target, index, value, &callback, &context)?;
    }
    Ok(result)
}

fn create_from_iter(
    vm: &mut Vm,
    ctor: Value,
    values: impl ExactSizeIterator<Item = Value>,
    callback: Value,
    context: Value,
) -> Result<Value, JsError> {
    let length = values.len();
    let (result, target) = construct(vm, ctor, &[length.into()], length)?;
    for (index, value) in values.enumerate() {
        vm.tick()?;
        store_mapped(vm, target, index, value, &callback, &context)?;
    }
    Ok(result)
}

fn store_mapped(
    vm: &mut Vm,
    target: TypedArray,
    index: usize,
    value: Value,
    callback: &Value,
    context: &Value,
) -> Result<(), JsError> {
    let value = if callback.is_undefined() {
        value
    } else {
        vm.invoke(callback.clone(), context.clone(), &[value, index.into()])?
    };
    vm.write_typed_value(target, index, &value)
}

fn sort(vm: &mut Vm, this: Value, args: &[Value], copy: bool) -> Result<Value, JsError> {
    let comparator = arg(args, 0);
    if !comparator.is_undefined() {
        require_callback(vm, comparator.clone())?;
    }
    let array = receiver(vm, &this)?;
    let length = array.length(vm)?;
    let (result, target) = if copy {
        same_type(vm, array, length)?
    } else {
        (this, array)
    };
    vm.reserve_allocation(length.saturating_mul(2 * size_of::<u64>()))?;
    vm.consume_work(length)?;
    let mut values = Vec::new();
    let mut scratch = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| allocation_error())?;
    scratch
        .try_reserve_exact(length)
        .map_err(|_| allocation_error())?;
    let range = array.range(vm)?;
    let bytes = &vm.buffer_data(array.view.buffer)?.bytes.as_ref().unwrap()[range];
    values.extend(
        bytes
            .chunks_exact(array.element.width())
            .map(|chunk| Element::load_bits(chunk, LITTLE_ENDIAN)),
    );
    // Stable merge, since an arbitrary JavaScript comparator can throw or be
    // inconsistent. Order the snapshot before writing back to the backing store.
    let mut width = 1;
    while width < length {
        scratch.clear();
        for start in (0..length).step_by(width * 2) {
            let end = (start + width * 2).min(length);
            let middle = (start + width).min(end);
            let mut left = start;
            let mut right = middle;
            while left < middle && right < end {
                if compare(vm, &comparator, array.element, values[left], values[right])?
                    != Ordering::Greater
                {
                    scratch.push(values[left]);
                    left += 1;
                } else {
                    scratch.push(values[right]);
                    right += 1;
                }
            }
            vm.consume_work(middle - left + end - right)?;
            scratch.extend_from_slice(&values[left..middle]);
            scratch.extend_from_slice(&values[right..end]);
        }
        std::mem::swap(&mut values, &mut scratch);
        width *= 2;
    }
    vm.consume_work(length)?;
    for (index, bits) in values.into_iter().enumerate() {
        vm.write_typed_bits(target, index, bits)?;
    }
    Ok(result)
}

fn compare(
    vm: &mut Vm,
    comparator: &Value,
    element: Element,
    a: u64,
    b: u64,
) -> Result<Ordering, JsError> {
    vm.tick()?;
    if !comparator.is_undefined() {
        let value = vm.invoke(
            comparator.clone(),
            Value::Undefined,
            &[element.value(a), element.value(b)],
        )?;
        let number = vm.to_number(&value)?;
        return Ok(number.partial_cmp(&0.0).unwrap_or(Ordering::Equal));
    }
    match element {
        Element::BigInt64 => return Ok((a as i64).cmp(&(b as i64))),
        Element::BigUint64 => return Ok(a.cmp(&b)),
        _ => {}
    }
    let a = element.number_value(a);
    let b = element.number_value(b);
    Ok(match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => a.total_cmp(&b),
    })
}

pub(super) fn install(vm: &mut Vm, prototype: Value, ctor: Value) {
    method(vm, ctor.clone(), "from", 1, from);
    method(vm, ctor, "of", 0, |vm, ctor, args| {
        create_from_iter(
            vm,
            ctor,
            args.iter().cloned(),
            Value::Undefined,
            Value::Undefined,
        )
    });
    for (name, length, implementation) in [
        (
            "at",
            1,
            at as fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError>,
        ),
        ("set", 1, set),
        ("subarray", 2, subarray),
        ("slice", 2, slice),
        ("copyWithin", 2, copy_within),
        ("fill", 1, fill),
        ("join", 1, join),
        ("toLocaleString", 0, to_locale_string),
        ("with", 2, with),
    ] {
        method(vm, prototype.clone(), name, length, implementation);
    }
    for (name, mode) in [
        ("includes", Search::Includes),
        ("indexOf", Search::IndexOf),
        ("lastIndexOf", Search::LastIndexOf),
    ] {
        method(vm, prototype.clone(), name, 1, move |vm, this, args| {
            search(vm, this, args, mode)
        });
    }
    for (name, mode) in [
        ("every", Visit::Every),
        ("some", Visit::Some),
        ("forEach", Visit::ForEach),
        ("map", Visit::Map),
        ("filter", Visit::Filter),
        (
            "find",
            Visit::Find {
                last: false,
                index: false,
            },
        ),
        (
            "findIndex",
            Visit::Find {
                last: false,
                index: true,
            },
        ),
        (
            "findLast",
            Visit::Find {
                last: true,
                index: false,
            },
        ),
        (
            "findLastIndex",
            Visit::Find {
                last: true,
                index: true,
            },
        ),
    ] {
        method(vm, prototype.clone(), name, 1, move |vm, this, args| {
            visit(vm, this, args, mode)
        });
    }
    for (name, right) in [("reduce", false), ("reduceRight", true)] {
        method(vm, prototype.clone(), name, 1, move |vm, this, args| {
            reduce(vm, this, args, right)
        });
    }
    for (name, copy) in [("reverse", false), ("toReversed", true)] {
        method(vm, prototype.clone(), name, 0, move |vm, this, _| {
            reverse(vm, this, copy)
        });
    }
    for (name, copy) in [("sort", false), ("toSorted", true)] {
        method(vm, prototype.clone(), name, 1, move |vm, this, args| {
            sort(vm, this, args, copy)
        });
    }
    let to_string = vm
        .get_property(Value::Object(vm.array_prototype), "toString")
        .unwrap();
    vm.define_data_property(prototype, "toString", to_string, true, false, true)
        .unwrap();
}
