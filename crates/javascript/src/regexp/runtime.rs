use super::{Flags, Program, compile, program};
use crate::{
    JsError, JsString, PropertyKey, Value, Vm,
    builtins::{constructor, method, replace_construct},
    heap::{ObjectKind, Property, PropertyKind},
};
use std::{mem::size_of, rc::Rc};

const FLAG_GETTERS: [(&str, u8, u8); 8] = [
    ("hasIndices", b'd', Flags::INDICES),
    ("global", b'g', Flags::GLOBAL),
    ("ignoreCase", b'i', Flags::IGNORE_CASE),
    ("multiline", b'm', Flags::MULTILINE),
    ("dotAll", b's', Flags::DOT_ALL),
    ("unicode", b'u', Flags::UNICODE),
    ("unicodeSets", b'v', Flags::UNICODE_SETS),
    ("sticky", b'y', Flags::STICKY),
];

pub(crate) fn install(vm: &mut Vm) {
    let prototype = vm.new_object();
    vm.realm.regexp_prototype = prototype.as_object();
    vm.root(prototype.clone());
    let regexp = constructor(vm, "RegExp", 2, prototype.clone(), |vm, _, args| {
        create_from_args(vm, args, None)
    });
    vm.realm.regexp_constructor = regexp.as_object();
    vm.root(regexp.clone());
    replace_construct(vm, regexp.clone(), |vm, _, args, new_target| {
        create_from_args(vm, args, Some(new_target))
    });
    getter(
        vm,
        regexp.clone(),
        vm.well_known_key("species"),
        "get [Symbol.species]",
        |_, this, _| Ok(this),
    );
    super::legacy::install(vm, &regexp);
    method(vm, regexp, "escape", 1, |vm, _, args| {
        escape(vm, arg(args, 0))
    });
    for (name, _, flag) in FLAG_GETTERS {
        getter(
            vm,
            prototype.clone(),
            name.into(),
            &format!("get {name}"),
            move |vm, this, _| {
                if let Some(program) = native_program(vm, &this) {
                    Ok(Value::Bool(program.flags.has(flag)))
                } else if this.as_object().is_some()
                    && this.as_object() == vm.realm.regexp_prototype
                {
                    Ok(Value::Undefined)
                } else {
                    Err(JsError::type_error(
                        "RegExp flag getter requires a RegExp receiver",
                    ))
                }
            },
        );
    }
    getter(
        vm,
        prototype.clone(),
        "flags".into(),
        "get flags",
        |vm, this, _| {
            require_object(&this)?;
            let mut units = Vec::with_capacity(FLAG_GETTERS.len());
            for (name, ch, _) in FLAG_GETTERS {
                if vm.get_property(this.clone(), name)?.is_truthy() {
                    units.push(u16::from(ch));
                }
            }
            vm.finish_string(units)
        },
    );
    getter(
        vm,
        prototype.clone(),
        "source".into(),
        "get source",
        |vm, this, _| {
            if let Some(program) = native_program(vm, &this) {
                source(vm, &program.source)
            } else if this.as_object().is_some() && this.as_object() == vm.realm.regexp_prototype {
                vm.account_value(Value::from("(?:)"))
            } else {
                Err(JsError::type_error(
                    "RegExp source getter requires a RegExp receiver",
                ))
            }
        },
    );
    let intrinsic_exec = method(vm, prototype.clone(), "exec", 1, |vm, this, args| {
        require_program(vm, &this)?;
        let input = vm.to_string(&arg(args, 0))?;
        builtin_exec(vm, this, input)
    });
    vm.realm.regexp_exec = intrinsic_exec.as_object();
    vm.root(intrinsic_exec);
    method(vm, prototype.clone(), "test", 1, |vm, this, args| {
        require_object(&this)?;
        let input = vm.to_string(&arg(args, 0))?;
        if super::bulk::Search::eligible(vm, &input)
            && let Some(program) = plain_exec_program(vm, &this)
            && !program.flags.has(Flags::GLOBAL | Flags::STICKY)
            && let Some(search) = super::bulk::Search::new(vm, &program)?
            && let Some(matched) = search.first_match(vm, input.as_utf16())?
        {
            if let Some(span) = matched {
                super::legacy::record_bulk(vm, &this, &input, &program, span)?;
            }
            return Ok(Value::Bool(matched.is_some()));
        }
        Ok(Value::Bool(exec(vm, this, input)? != Value::Null))
    });
    method(vm, prototype.clone(), "toString", 0, |vm, this, _| {
        require_object(&this)?;
        let source = vm.get_property(this.clone(), "source")?;
        let source = vm.to_string(&source)?;
        let flags = vm.get_property(this, "flags")?;
        let flags = vm.to_string(&flags)?;
        let mut out = Output::default();
        out.add(vm, &[47])?;
        out.add(vm, source.as_utf16())?;
        out.add(vm, &[47])?;
        out.add(vm, flags.as_utf16())?;
        out.complete(vm)
    });
    method(vm, prototype, "compile", 2, |vm, this, args| {
        let origin = super::legacy::origin(vm, &this)
            .filter(|o| o.enabled && o.realm == vm.realm.module_realm)
            .ok_or_else(|| {
                JsError::type_error("RegExp.compile requires a local non-subclassed RegExp")
            })?;
        let pattern = arg(args, 0);
        let flags = arg(args, 1);
        let program = if let Some(program) = native_program(vm, &pattern) {
            if !flags.is_undefined() {
                return Err(JsError::type_error(
                    "RegExp.compile received flags with a RegExp pattern",
                ));
            }
            program
        } else {
            compile_args(vm, pattern, flags)?
        };
        vm.reserve_allocation(program.retained_bytes())?;
        vm.heap.object_mut(this.as_object().unwrap())?.kind = ObjectKind::RegExp(program, origin);
        vm.set_property(this.clone(), "lastIndex", Value::Number(0.0))?;
        Ok(this)
    });
}

fn getter<F>(vm: &mut Vm, object: Value, key: PropertyKey, name: &str, callback: F)
where
    F: Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
{
    let get = vm.native(name, 0, false, Rc::new(callback));
    vm.define_key(
        object,
        key,
        Property {
            kind: PropertyKind::Accessor {
                get: Some(get),
                set: None,
            },
            enumerable: false,
            configurable: true,
        },
    )
    .unwrap();
}

pub(super) fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or_default()
}

pub(super) fn require_object(receiver: &Value) -> Result<(), JsError> {
    if receiver.as_object().is_some() {
        Ok(())
    } else {
        Err(JsError::type_error(
            "RegExp method requires an object receiver",
        ))
    }
}

fn native_program(vm: &Vm, receiver: &Value) -> Option<Rc<Program>> {
    if let ObjectKind::RegExp(program, _) = &vm.heap.object(receiver.as_object()?).ok()?.kind {
        Some(program.clone())
    } else {
        None
    }
}

// Skipping repeated Get("exec") and ToLength(lastIndex) is only observable
// when user accessors, conversions or a custom method participate.
pub(super) fn plain_exec_program(vm: &Vm, receiver: &Value) -> Option<Rc<Program>> {
    use crate::heap::PropertyKind;
    let program = native_program(vm, receiver)?;
    let id = receiver.as_object()?;
    let prototype = vm.realm.regexp_prototype?;
    if vm.heap.object(id).ok()?.prototype != Some(prototype) {
        return None;
    }
    let last_index = vm.own_descriptor(id, &"lastIndex".into()).ok()??;
    if !matches!(
        last_index.kind,
        PropertyKind::Data {
            value: Value::Number(_),
            ..
        }
    ) {
        return None;
    }
    let key = "exec".into();
    let descriptor = vm
        .own_descriptor(id, &key)
        .ok()?
        .or_else(|| vm.own_descriptor(prototype, &key).ok().flatten())?;
    match descriptor.kind {
        PropertyKind::Data {
            value: Value::Object(function),
            ..
        } if Some(function) == vm.realm.regexp_exec => Some(program),
        _ => None,
    }
}

fn require_program(vm: &Vm, receiver: &Value) -> Result<Rc<Program>, JsError> {
    native_program(vm, receiver)
        .ok_or_else(|| JsError::type_error("RegExp method requires a RegExp receiver"))
}

pub(super) fn is_regexp(vm: &mut Vm, value: &Value) -> Result<bool, JsError> {
    if value.as_object().is_none() {
        return Ok(false);
    }
    let matcher = vm.get_property_key(value.clone(), vm.well_known_key("match"))?;
    if !matcher.is_undefined() {
        return Ok(matcher.is_truthy());
    }
    Ok(native_program(vm, value).is_some())
}

pub(crate) fn reject_regexp(vm: &mut Vm, value: &Value) -> Result<(), JsError> {
    if is_regexp(vm, value)? {
        Err(JsError::type_error(
            "String search argument cannot be a RegExp",
        ))
    } else {
        Ok(())
    }
}

fn create_from_args(
    vm: &mut Vm,
    args: &[Value],
    new_target: Option<Value>,
) -> Result<Value, JsError> {
    let pattern = arg(args, 0);
    let mut flags = arg(args, 1);
    let pattern_is_regexp = is_regexp(vm, &pattern)?;
    let intrinsic = Value::Object(vm.realm.regexp_constructor.unwrap());
    if new_target.is_none()
        && pattern_is_regexp
        && flags.is_undefined()
        && vm.get_property(pattern.clone(), "constructor")? == intrinsic
    {
        return Ok(pattern);
    }
    let native = native_program(vm, &pattern);
    let source = if let Some(program) = &native {
        Value::String(program.source.clone())
    } else if pattern_is_regexp {
        let source = vm.get_property(pattern.clone(), "source")?;
        if flags.is_undefined() {
            flags = vm.get_property(pattern, "flags")?;
        }
        source
    } else {
        pattern
    };
    let enabled = new_target.as_ref().is_none_or(|t| t == &intrinsic);
    let prototype = vm.prototype_from_constructor(
        new_target.unwrap_or(intrinsic),
        vm.realm.regexp_prototype.unwrap(),
    )?;
    let program = match native {
        Some(program) if flags.is_undefined() => program,
        _ => compile_args(vm, source, flags)?,
    };
    allocate(vm, program, prototype, enabled)
}

fn compile_args(vm: &mut Vm, source: Value, flags: Value) -> Result<Rc<Program>, JsError> {
    let source = if source.is_undefined() {
        JsString::default()
    } else {
        vm.to_string(&source)?
    };
    let flags = if flags.is_undefined() {
        JsString::default()
    } else {
        vm.to_string(&flags)?
    };
    vm.ensure_allocation(Vm::source_allocation(source.len()))?;
    vm.consume_work(
        source
            .len()
            .saturating_mul(4)
            .saturating_add(flags.len().min(9)),
    )?;
    compile(source, flags)
}

pub(crate) fn literal(vm: &mut Vm, program: Rc<Program>) -> Result<Value, JsError> {
    allocate(vm, program, vm.realm.regexp_prototype.unwrap(), true)
}

pub(super) fn create(vm: &mut Vm, pattern: Value, flags: Value) -> Result<Value, JsError> {
    let program = compile_args(vm, pattern, flags)?;
    literal(vm, program)
}

fn allocate(
    vm: &mut Vm,
    program: Rc<Program>,
    prototype: crate::ObjectId,
    enabled: bool,
) -> Result<Value, JsError> {
    vm.ensure_allocation(program.retained_bytes() + 1024)?;
    let origin = super::legacy::Origin {
        realm: vm.realm.module_realm,
        enabled,
    };
    let object = vm.allocate_constructed(ObjectKind::RegExp(program, origin), prototype)?;
    vm.define_data_property(
        object.clone(),
        "lastIndex",
        Value::Number(0.0),
        true,
        false,
        false,
    )?;
    Ok(object)
}

pub(super) fn exec(vm: &mut Vm, regexp: Value, input: JsString) -> Result<Value, JsError> {
    let method = vm.get_property(regexp.clone(), "exec")?;
    if vm.is_callable(&method) {
        let result = vm.invoke(method, regexp, &[Value::String(input)])?;
        if result != Value::Null && result.as_object().is_none() {
            return Err(JsError::type_error(
                "RegExp exec must return an object or null",
            ));
        }
        Ok(result)
    } else {
        builtin_exec(vm, regexp, input)
    }
}

pub(super) fn length(vm: &mut Vm, value: &Value) -> Result<usize, JsError> {
    let number = vm.to_number(value)?;
    Ok(number.floor().clamp(0.0, 9_007_199_254_740_991.0) as usize)
}

pub(super) fn last_index(vm: &mut Vm, regexp: &Value) -> Result<usize, JsError> {
    let value = vm.get_property(regexp.clone(), "lastIndex")?;
    length(vm, &value)
}

pub(super) fn set_last_index(vm: &mut Vm, regexp: &Value, index: usize) -> Result<(), JsError> {
    vm.set_property(regexp.clone(), "lastIndex", Value::from(index))
}

fn builtin_exec(vm: &mut Vm, regexp: Value, input: JsString) -> Result<Value, JsError> {
    require_program(vm, &regexp)?;
    let index = last_index(vm, &regexp)?;
    let program = require_program(vm, &regexp)?;
    let sticky = program.flags.has(Flags::STICKY);
    let use_index = sticky || program.flags.has(Flags::GLOBAL);
    let offset = if use_index { index } else { 0 };
    let matched = program.find(vm, input.as_utf16(), offset, sticky)?;
    if use_index {
        set_last_index(vm, &regexp, matched.as_ref().map_or(0, |m| m.whole.1))?;
    }
    if let Some(matched) = matched {
        let result = match_value(vm, input.clone(), &program, &matched)?;
        super::legacy::record(vm, &regexp, &input, &matched)?;
        Ok(result)
    } else {
        Ok(Value::Null)
    }
}

fn match_value(
    vm: &mut Vm,
    input: JsString,
    program: &Program,
    m: &program::Match,
) -> Result<Value, JsError> {
    let count = 1 + m.captures.len();
    vm.ensure_allocation(count * size_of::<Value>() * 2)?;
    let spans = || {
        [Some(m.whole)]
            .into_iter()
            .chain(m.captures.iter().copied())
    };
    let mut values = Vec::with_capacity(count);
    for span in spans() {
        vm.consume_work(1)?;
        values.push(if let Some((start, end)) = span {
            vm.copy_string(&input.as_utf16()[start..end])?
        } else {
            Value::Undefined
        });
    }
    let result = vm.try_new_array(&values)?;
    let groups = groups_object(vm, program, m, &values)?;
    let data = |vm: &mut Vm, key, value| {
        vm.define_data_property(result.clone(), key, value, true, true, true)
    };
    data(vm, "index", Value::from(m.whole.0))?;
    data(vm, "input", Value::String(input))?;
    data(vm, "groups", groups)?;
    if program.flags.has(Flags::INDICES) {
        let mut values = Vec::with_capacity(count);
        for span in spans() {
            vm.consume_work(1)?;
            values.push(if let Some((start, end)) = span {
                vm.try_new_array(&[Value::from(start), Value::from(end)])?
            } else {
                Value::Undefined
            });
        }
        let indices = vm.try_new_array(&values)?;
        let groups = groups_object(vm, program, m, &values)?;
        vm.define_data_property(indices.clone(), "groups", groups, true, true, true)?;
        data(vm, "indices", indices)?;
    }
    Ok(result)
}

fn groups_object(
    vm: &mut Vm,
    program: &Program,
    m: &program::Match,
    values: &[Value],
) -> Result<Value, JsError> {
    if program.names.is_empty() {
        return Ok(Value::Undefined);
    }
    let groups = vm.new_object_with_prototype(Value::Null)?;
    for (name, slots) in &program.names {
        vm.consume_work(slots.len())?;
        let value = slots
            .iter()
            .find(|&&slot| m.captures[slot].is_some())
            .map_or(Value::Undefined, |&slot| values[slot + 1].clone());
        vm.define_data_property_key(
            groups.clone(),
            PropertyKey::String(name.clone()),
            value,
            true,
            true,
            true,
        )?;
    }
    Ok(groups)
}

fn source(vm: &mut Vm, source: &JsString) -> Result<Value, JsError> {
    if source.is_empty() {
        return vm.account_value(Value::from("(?:)"));
    }
    let mut out = Output::default();
    let mut escaped = false;
    let mut in_class = false;
    for &unit in source.as_utf16() {
        if program::line_terminator(unit.into()) {
            if escaped {
                out.remove_last();
            }
            match unit {
                10 => out.add(vm, &[92, 110])?,
                13 => out.add(vm, &[92, 114])?,
                _ => out.hex(vm, unit, false)?,
            }
        } else {
            if !escaped {
                match unit {
                    91 => in_class = true,
                    93 => in_class = false,
                    47 if !in_class => out.add(vm, &[92])?,
                    _ => {}
                }
            }
            out.add(vm, &[unit])?;
        }
        escaped = !escaped && unit == 92;
    }
    out.complete(vm)
}

fn escape(vm: &mut Vm, string: Value) -> Result<Value, JsError> {
    let Value::String(string) = string else {
        return Err(JsError::type_error("RegExp.escape requires a string"));
    };
    let units = string.as_utf16();
    let mut out = Output::default();
    let mut at = 0;
    while at < units.len() {
        let next = program::advance(units, at, true);
        let unit = units[at];
        let ascii = u8::try_from(unit).unwrap_or(0);
        if next == at + 2 {
            out.add(vm, &units[at..next])?;
        } else if at == 0 && ascii.is_ascii_alphanumeric() {
            out.hex(vm, unit, true)?;
        } else if b"^$\\.*+?()[]{}|/".contains(&ascii) {
            out.add(vm, &[92, unit])?;
        } else if matches!(unit, 9..=13) {
            out.add(vm, &[92, b"tnvfr"[(unit - 9) as usize].into()])?;
        } else if b",-=<>#&!%:;@~'`\"".contains(&ascii)
            || program::space(unit.into())
            || (0xd800..=0xdfff).contains(&unit)
        {
            out.hex(vm, unit, unit <= 255)?;
        } else {
            out.add(vm, &[unit])?;
        }
        at = next;
    }
    out.complete(vm)
}

pub(super) struct Output {
    storage: Option<Vec<u16>>,
    length: usize,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            storage: Some(Vec::new()),
            length: 0,
        }
    }
}

impl Output {
    pub(super) fn discarded() -> Self {
        Self {
            storage: None,
            length: 0,
        }
    }

    pub(super) fn add(&mut self, vm: &mut Vm, units: &[u16]) -> Result<(), JsError> {
        let length = self.length.saturating_add(units.len());
        vm.ensure_string_allocation(length)?;
        vm.consume_work(units.len())?;
        if let Some(storage) = &mut self.storage {
            storage.extend_from_slice(units);
        }
        self.length = length;
        Ok(())
    }

    fn remove_last(&mut self) {
        self.storage.as_mut().unwrap().pop();
        self.length -= 1;
    }

    fn hex(&mut self, vm: &mut Vm, unit: u16, short: bool) -> Result<(), JsError> {
        let hex = |shift: u32| u16::from(b"0123456789abcdef"[((unit >> shift) & 15) as usize]);
        let digits = [hex(12), hex(8), hex(4), hex(0)];
        self.add(vm, &[92, if short { 120 } else { 117 }])?;
        self.add(vm, if short { &digits[2..] } else { &digits })
    }

    pub(super) fn complete(self, vm: &mut Vm) -> Result<Value, JsError> {
        vm.finish_string(self.storage.unwrap())
    }
}
