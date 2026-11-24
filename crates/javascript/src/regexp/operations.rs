//! RegExp / String protocol methods. Each lookup and coercion uses ordinary VM
//! operations, including overridden exec, symbol methods, and species constructors.
use super::{
    Flags, limit,
    program::advance,
    runtime::{
        Output, arg, create, exec, is_regexp, last_index, length, require_object, set_last_index,
    },
};
use crate::{
    JsError, JsString, Value, Vm,
    builtins::{find_units, method},
    heap::ObjectKind,
    vm::{same_value, to_u32},
};
use std::{mem::size_of, rc::Rc};

pub(crate) fn install(vm: &mut Vm) {
    let regexp = Value::Object(vm.realm.regexp_prototype.unwrap());
    for (name, length, callback) in [
        ("match", 1, match_string as Protocol),
        ("matchAll", 1, match_all as Protocol),
        ("search", 1, search as Protocol),
        ("replace", 2, replace as Protocol),
        ("split", 2, split as Protocol),
    ] {
        let function = vm.native(
            &format!("[Symbol.{name}]"),
            length,
            false,
            Rc::new(callback),
        );
        vm.define_data_property_key(
            regexp.clone(),
            vm.well_known_key(name),
            function,
            true,
            false,
            true,
        )
        .unwrap();
    }
    let iterator_prototype = vm
        .new_object_with_prototype(Value::Object(vm.realm.iterator_prototype.unwrap()))
        .unwrap();
    vm.realm.regexp_iterator_prototype = iterator_prototype.as_object();
    vm.root(iterator_prototype.clone());
    vm.define_data_property_key(
        iterator_prototype.clone(),
        vm.well_known_key("toStringTag"),
        Value::from("RegExp String Iterator"),
        false,
        false,
        true,
    )
    .unwrap();
    method(vm, iterator_prototype, "next", 0, |vm, this, _| {
        next(vm, this)
    });
    let string = Value::Object(vm.realm.string_prototype);
    for name in ["match", "matchAll", "search"] {
        method(vm, string.clone(), name, 1, move |vm, this, args| {
            let regexp = arg(args, 0);
            let all = name == "matchAll";
            if let Some(value) = string_hook(vm, &this, &regexp, name, None, all)? {
                return Ok(value);
            }
            let input = vm.to_string(&this)?;
            let flags = if all {
                Value::from("g")
            } else {
                Value::Undefined
            };
            let regexp = create(vm, regexp, flags)?;
            let function = vm.get_property_key(regexp.clone(), vm.well_known_key(name))?;
            vm.invoke(function, regexp, &[Value::String(input)])
        });
    }
    for (name, all) in [("replace", false), ("replaceAll", true)] {
        method(vm, string.clone(), name, 2, move |vm, this, args| {
            let pattern = arg(args, 0);
            let replacement = arg(args, 1);
            if let Some(value) =
                string_hook(vm, &this, &pattern, "replace", Some(&replacement), all)?
            {
                return Ok(value);
            }
            replace_string(vm, this, pattern, replacement, all)
        });
    }
}

type Protocol = fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError>;

pub(crate) fn string_hook(
    vm: &mut Vm,
    this: &Value,
    pattern: &Value,
    name: &str,
    extra: Option<&Value>,
    global_required: bool,
) -> Result<Option<Value>, JsError> {
    if this.is_nullish() {
        return Err(JsError::type_error(
            "String method called on null or undefined",
        ));
    }
    if pattern.as_object().is_none() {
        return Ok(None);
    }
    if global_required && is_regexp(vm, pattern)? {
        let flags = vm.get_property(pattern.clone(), "flags")?;
        if flags.is_nullish() {
            return Err(JsError::type_error("RegExp flags are null or undefined"));
        }
        let flags = vm.to_string(&flags)?;
        if !observed_flags(vm, &flags)?.has(Flags::GLOBAL) {
            return Err(JsError::type_error(
                "String method requires a global RegExp",
            ));
        }
    }
    let method = vm.get_property_key(pattern.clone(), vm.well_known_key(name))?;
    if method.is_nullish() {
        return Ok(None);
    }
    if let Some(extra) = extra {
        vm.invoke(method, pattern.clone(), &[this.clone(), extra.clone()])
            .map(Some)
    } else {
        vm.invoke(method, pattern.clone(), std::slice::from_ref(this))
            .map(Some)
    }
}

fn flags(vm: &mut Vm, regexp: &Value) -> Result<JsString, JsError> {
    let flags = vm.get_property(regexp.clone(), "flags")?;
    vm.to_string(&flags)
}

fn observed_flags(vm: &mut Vm, text: &JsString) -> Result<Flags, JsError> {
    vm.consume_work(text.len())?;
    Ok(Flags(text.as_utf16().iter().fold(0, |bits, &unit| {
        bits | Flags::bit(unit).unwrap_or(0)
    })))
}

fn full_unicode(flags: Flags) -> bool {
    flags.has(Flags::UNICODE | Flags::UNICODE_SETS)
}

fn matched_string(vm: &mut Vm, result: &Value) -> Result<JsString, JsError> {
    let matched = vm.get_property(result.clone(), "0")?;
    vm.to_string(&matched)
}

fn step_empty(vm: &mut Vm, regexp: &Value, input: &JsString, unicode: bool) -> Result<(), JsError> {
    let index = last_index(vm, regexp)?;
    set_last_index(vm, regexp, advance(input.as_utf16(), index, unicode))
}

fn match_string(vm: &mut Vm, regexp: Value, args: &[Value]) -> Result<Value, JsError> {
    require_object(&regexp)?;
    let input = vm.to_string(&arg(args, 0))?;
    let flags = flags(vm, &regexp)?;
    let flags = observed_flags(vm, &flags)?;
    if !flags.has(Flags::GLOBAL) {
        return exec(vm, regexp, input);
    }
    set_last_index(vm, &regexp, 0)?;
    let array = vm.try_new_array(&[])?;
    let mut count = 0;
    loop {
        vm.consume_work(1)?;
        let result = exec(vm, regexp.clone(), input.clone())?;
        if result == Value::Null {
            return Ok(if count == 0 { Value::Null } else { array });
        }
        let matched = matched_string(vm, &result)?;
        let empty = matched.is_empty();
        vm.append_array_literal(array.clone(), Some(Value::String(matched)))?;
        if empty {
            step_empty(vm, &regexp, &input, full_unicode(flags))?;
        }
        count += 1;
    }
}

fn search(vm: &mut Vm, regexp: Value, args: &[Value]) -> Result<Value, JsError> {
    require_object(&regexp)?;
    let input = vm.to_string(&arg(args, 0))?;
    let previous = vm.get_property(regexp.clone(), "lastIndex")?;
    if !same_value(&previous, &Value::Number(0.0)) {
        set_last_index(vm, &regexp, 0)?;
    }
    let result = exec(vm, regexp.clone(), input)?;
    let current = vm.get_property(regexp.clone(), "lastIndex")?;
    vm.charge_bigint_comparison(&current, &previous)?;
    if !same_value(&current, &previous) {
        vm.set_property(regexp, "lastIndex", previous)?;
    }
    if result == Value::Null {
        Ok(Value::Number(-1.0))
    } else {
        vm.get_property(result, "index")
    }
}

fn species(vm: &mut Vm, regexp: &Value) -> Result<Value, JsError> {
    let intrinsic = Value::Object(vm.realm.regexp_constructor.unwrap());
    let constructor = vm.get_property(regexp.clone(), "constructor")?;
    if constructor.is_undefined() {
        return Ok(intrinsic);
    }
    require_object(&constructor)?;
    let derived = vm.get_property_key(constructor, vm.well_known_key("species"))?;
    if derived.is_nullish() {
        return Ok(intrinsic);
    }
    if !vm.is_constructor(&derived) {
        return Err(JsError::type_error("RegExp species is not a constructor"));
    }
    Ok(derived)
}

#[derive(Clone)]
pub(crate) enum StringIterator {
    Active {
        matcher: Value,
        input: JsString,
        global: bool,
        unicode: bool,
    },
    Done,
}

fn match_all(vm: &mut Vm, regexp: Value, args: &[Value]) -> Result<Value, JsError> {
    require_object(&regexp)?;
    let input = vm.to_string(&arg(args, 0))?;
    let constructor = species(vm, &regexp)?;
    let flags = flags(vm, &regexp)?;
    let matcher = vm.construct_target(
        constructor.clone(),
        &[regexp.clone(), flags.clone().into()],
        constructor,
    )?;
    let index = last_index(vm, &regexp)?;
    set_last_index(vm, &matcher, index)?;
    let flags = observed_flags(vm, &flags)?;
    let data = StringIterator::Active {
        matcher,
        input,
        global: flags.has(Flags::GLOBAL),
        unicode: full_unicode(flags),
    };
    vm.allocate_constructed(
        ObjectKind::RegExpStringIterator(data),
        vm.realm.regexp_iterator_prototype.unwrap(),
    )
}

fn next(vm: &mut Vm, this: Value) -> Result<Value, JsError> {
    let id = this
        .as_object()
        .ok_or_else(|| JsError::type_error("RegExp iterator requires an iterator receiver"))?;
    let ObjectKind::RegExpStringIterator(state) = &vm.heap.object(id)?.kind else {
        return Err(JsError::type_error(
            "RegExp iterator requires an iterator receiver",
        ));
    };
    let StringIterator::Active {
        matcher,
        input,
        global,
        unicode,
    } = state.clone()
    else {
        return vm.iterator_result(Value::Undefined, true);
    };
    let match_value = exec(vm, matcher.clone(), input.clone())?;
    let done = match_value == Value::Null;
    if done || !global {
        vm.heap.object_mut(id)?.kind = ObjectKind::RegExpStringIterator(StringIterator::Done);
    } else if matched_string(vm, &match_value)?.is_empty() {
        step_empty(vm, &matcher, &input, unicode)?;
    }
    vm.iterator_result(if done { Value::Undefined } else { match_value }, done)
}

fn split(vm: &mut Vm, regexp: Value, args: &[Value]) -> Result<Value, JsError> {
    require_object(&regexp)?;
    let input = vm.to_string(&arg(args, 0))?;
    let constructor = species(vm, &regexp)?;
    let flags = flags(vm, &regexp)?;
    let bits = observed_flags(vm, &flags)?;
    let flags = if bits.has(Flags::STICKY) {
        Value::String(flags)
    } else {
        let mut out = Output::default();
        out.add(vm, flags.as_utf16())?;
        out.add(vm, &[u16::from(b'y')])?;
        out.complete(vm)?
    };
    let matcher = vm.construct_target(constructor.clone(), &[regexp, flags], constructor)?;
    let max = arg(args, 1);
    let max = if max.is_undefined() {
        u32::MAX
    } else {
        to_u32(vm.to_number(&max)?)
    } as usize;
    let result = vm.try_new_array(&[])?;
    if max == 0 {
        return Ok(result);
    }
    if input.is_empty() {
        if exec(vm, matcher, input.clone())? == Value::Null {
            vm.append_array_literal(result.clone(), Some(Value::String(input)))?;
        }
        return Ok(result);
    }
    let unicode = full_unicode(bits);
    let mut start = 0;
    let mut search = 0;
    let mut count = 0;
    while search < input.len() {
        vm.consume_work(1)?;
        set_last_index(vm, &matcher, search)?;
        let found = exec(vm, matcher.clone(), input.clone())?;
        if found == Value::Null {
            search = advance(input.as_utf16(), search, unicode);
            continue;
        }
        let end = last_index(vm, &matcher)?.min(input.len());
        if end == start {
            search = advance(input.as_utf16(), search, unicode);
            continue;
        }
        let segment = vm.copy_string(&input.as_utf16()[start..search])?;
        vm.append_array_literal(result.clone(), Some(segment))?;
        count += 1;
        if count == max {
            return Ok(result);
        }
        start = end;
        let len = vm.get_property(found.clone(), "length")?;
        let len = length(vm, &len)?;
        for index in 1..len {
            vm.consume_work(1)?;
            let capture = vm.get_property(found.clone(), &index.to_string())?;
            vm.append_array_literal(result.clone(), Some(capture))?;
            count += 1;
            if count == max {
                return Ok(result);
            }
        }
        search = start;
    }
    let segment = vm.copy_string(&input.as_utf16()[start..])?;
    vm.append_array_literal(result.clone(), Some(segment))?;
    Ok(result)
}

enum Replacer {
    Function(Value),
    Template(JsString),
}
impl Replacer {
    fn new(vm: &mut Vm, replacement: Value) -> Result<Self, JsError> {
        if vm.is_callable(&replacement) {
            Ok(Self::Function(replacement))
        } else {
            Ok(Self::Template(vm.to_string(&replacement)?))
        }
    }

    fn apply(
        &self,
        vm: &mut Vm,
        out: &mut Output,
        matched: Replacement<'_>,
    ) -> Result<(), JsError> {
        match self {
            Self::Function(callback) => {
                let count = matched.captures.len() + 4;
                vm.ensure_allocation(count * size_of::<Value>())?;
                let mut args = Vec::with_capacity(count);
                args.push(Value::String(matched.text.clone()));
                args.extend_from_slice(matched.captures);
                args.push(Value::from(matched.position));
                args.push(Value::String(matched.input.clone()));
                if !matched.groups.is_undefined() {
                    args.push(matched.groups);
                }
                let value = vm.invoke(callback.clone(), Value::Undefined, &args)?;
                let text = vm.to_string(&value)?;
                out.add(vm, text.as_utf16())
            }
            Self::Template(template) => substitute(vm, out, matched, template),
        }
    }
}

struct Replacement<'a> {
    input: &'a JsString,
    text: &'a JsString,
    position: usize,
    captures: &'a [Value],
    groups: Value,
}
impl Replacement<'_> {
    fn end(&self) -> usize {
        self.position.saturating_add(self.text.len())
    }
}

fn replace(vm: &mut Vm, regexp: Value, args: &[Value]) -> Result<Value, JsError> {
    require_object(&regexp)?;
    let input = vm.to_string(&arg(args, 0))?;
    let replacer = Replacer::new(vm, arg(args, 1))?;
    let flags = flags(vm, &regexp)?;
    let flags = observed_flags(vm, &flags)?;
    let global = flags.has(Flags::GLOBAL);
    if global {
        set_last_index(vm, &regexp, 0)?;
    }
    if super::bulk::Search::eligible(vm, &input)
        && let Replacer::Template(template) = &replacer
        && template.len() <= 256
        && !template.as_utf16().contains(&u16::from(b'$'))
        && let Some(program) = super::runtime::plain_exec_program(vm, &regexp)
        && program.flags == flags
        && let Some(search) = super::bulk::Search::new(vm, &program)?
        && let Some((value, matched)) = search.replace(vm, &input, template, global)?
    {
        if let Some(span) = matched {
            super::legacy::record_bulk(vm, &regexp, &input, &program, span)?;
        }
        // A global intrinsic loop starts at zero and ends with a failed exec
        // resetting lastIndex to zero. No callback can observe its intermediate values.
        return Ok(value);
    }
    let mut matches = Vec::new();
    loop {
        vm.consume_work(1)?;
        let found = exec(vm, regexp.clone(), input.clone())?;
        if found == Value::Null {
            break;
        }
        push_match(vm, &mut matches, found.clone())?;
        if !global {
            break;
        }
        if matched_string(vm, &found)?.is_empty() {
            step_empty(vm, &regexp, &input, full_unicode(flags))?;
        }
    }
    if matches.is_empty() {
        return Ok(Value::String(input));
    }
    let mut out = Output::default();
    let mut offset = 0;
    for matched in matches {
        let length_value = vm.get_property(matched.clone(), "length")?;
        let count = length(vm, &length_value)?.saturating_sub(1);
        let text = matched_string(vm, &matched)?;
        let position = vm.get_property(matched.clone(), "index")?;
        let position = length(vm, &position)?.min(input.len());
        // Custom exec may return an arbitrary array-like object.
        vm.consume_work(count)?;
        vm.ensure_allocation(
            count
                .saturating_add(4)
                .saturating_mul(size_of::<Value>() * 2),
        )?;
        let mut captures = Vec::with_capacity(count);
        for index in 1..=count {
            let capture = vm.get_property(matched.clone(), &index.to_string())?;
            captures.push(if capture.is_undefined() {
                capture
            } else {
                Value::String(vm.to_string(&capture)?)
            });
        }
        let mut groups = vm.get_property(matched, "groups")?;
        if matches!(replacer, Replacer::Template(_)) && !groups.is_undefined() {
            groups = vm.coerce_object(groups)?;
        }
        let entry = Replacement {
            input: &input,
            text: &text,
            position,
            captures: &captures,
            groups,
        };
        if position >= offset {
            out.add(vm, &input.as_utf16()[offset..position])?;
            offset = entry.end();
            replacer.apply(vm, &mut out, entry)?;
        } else {
            // An overridden exec can move backwards. Its replacer still runs.
            replacer.apply(vm, &mut Output::discarded(), entry)?;
        }
    }
    if offset < input.len() {
        out.add(vm, &input.as_utf16()[offset..])?;
    }
    out.complete(vm)
}

fn replace_string(
    vm: &mut Vm,
    this: Value,
    pattern: Value,
    replacement: Value,
    all: bool,
) -> Result<Value, JsError> {
    let input = vm.to_string(&this)?;
    let pattern = vm.to_string(&pattern)?;
    let replacer = Replacer::new(vm, replacement)?;
    let mut positions = Vec::new();
    let mut offset = 0;
    while let Some(index) = find_units(vm, input.as_utf16(), pattern.as_utf16(), offset)? {
        vm.consume_work(1)?;
        push_match(vm, &mut positions, index)?;
        if !all {
            break;
        }
        offset = index + pattern.len().max(1);
    }
    if positions.is_empty() {
        return Ok(Value::String(input));
    }
    let mut out = Output::default();
    let mut offset = 0;
    for position in positions {
        out.add(vm, &input.as_utf16()[offset..position])?;
        let entry = Replacement {
            input: &input,
            text: &pattern,
            position,
            captures: &[],
            groups: Value::Undefined,
        };
        offset = entry.end();
        replacer.apply(vm, &mut out, entry)?;
    }
    out.add(vm, &input.as_utf16()[offset..])?;
    out.complete(vm)
}

fn push_match<T>(vm: &mut Vm, matches: &mut Vec<T>, value: T) -> Result<(), JsError> {
    if matches.len() == matches.capacity() {
        let capacity = matches.capacity().saturating_mul(2).max(4);
        vm.reserve_allocation(capacity.saturating_mul(size_of::<T>()))?;
        matches
            .try_reserve_exact(capacity - matches.len())
            .map_err(|_| limit("Regular expression result storage allocation failed"))?;
    }
    matches.push(value);
    Ok(())
}

fn substitute(
    vm: &mut Vm,
    out: &mut Output,
    matched: Replacement<'_>,
    template: &JsString,
) -> Result<(), JsError> {
    let units = template.as_utf16();
    let input = matched.input.as_utf16();
    let mut at = 0;
    while at < units.len() {
        vm.consume_work(1)?;
        if units[at] != 36 || at + 1 == units.len() {
            out.add(vm, &units[at..at + 1])?;
            at += 1;
            continue;
        }
        match units[at + 1] {
            36 => out.add(vm, &[36])?,
            38 => out.add(vm, matched.text.as_utf16())?,
            96 => out.add(vm, &input[..matched.position])?,
            39 => out.add(vm, &input[matched.end().min(input.len())..])?,
            48..=57 => {
                let first = usize::from(units[at + 1] - 48);
                let mut index = first;
                let mut digits = 1;
                if let Some(&digit @ 48..=57) = units.get(at + 2) {
                    let pair = first * 10 + usize::from(digit - 48);
                    if pair <= matched.captures.len() {
                        index = pair;
                        digits = 2;
                    }
                }
                if index > 0 && index <= matched.captures.len() {
                    if let Some(capture) = matched.captures[index - 1].as_string() {
                        out.add(vm, capture.as_utf16())?;
                    }
                } else {
                    out.add(vm, &units[at..at + digits + 1])?;
                }
                at += digits + 1;
                continue;
            }
            60 if !matched.groups.is_undefined() => {
                if let Some(end) = find_units(vm, units, &[62], at + 2)? {
                    let key = vm.copy_string(&units[at + 2..end])?;
                    let value = vm.get_property_key(
                        matched.groups.clone(),
                        crate::PropertyKey::String(key.as_string().unwrap().clone()),
                    )?;
                    if !value.is_undefined() {
                        let value = vm.to_string(&value)?;
                        out.add(vm, value.as_utf16())?;
                    }
                    at = end + 1;
                    continue;
                }
                out.add(vm, &[36, 60])?;
            }
            _ => {
                out.add(vm, &[36])?;
                at += 1;
                continue;
            }
        }
        at += 2;
    }
    Ok(())
}
