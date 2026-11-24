//! Web-compatible RegExp static accessors. A constructor retains its input
//! through traced slots. The match state stores only offsets into that input.
use super::{
    Program,
    program::{Match, Span},
};
use crate::{
    JsError, JsString, Value, Vm,
    heap::{ObjectKind, Property, PropertyKind},
};
use std::rc::Rc;

const ORIGINAL_INPUT: u32 = 0;
const VISIBLE_INPUT: u32 = 1;

#[derive(Clone, Copy)]
pub(crate) struct Origin {
    pub(super) realm: u64,
    pub(super) enabled: bool,
}

#[derive(Clone, Copy)]
enum Field {
    Input,
    Match,
    LastParen,
    Left,
    Right,
    Group(usize),
}

struct Captures {
    whole: Span,
    groups: [Option<Span>; 9],
    last: Option<Span>,
}

fn receiver(vm: &Vm, this: &Value) -> Result<(), JsError> {
    if this.as_object() != vm.realm.regexp_constructor {
        return Err(JsError::type_error(
            "RegExp static accessor requires its defining constructor",
        ));
    }
    Ok(())
}

fn get(vm: &mut Vm, this: Value, field: Field) -> Result<Value, JsError> {
    receiver(vm, &this)?;
    let input = vm.host_slot(
        &this,
        if matches!(field, Field::Input) {
            VISIBLE_INPUT
        } else {
            ORIGINAL_INPUT
        },
    );
    let input = match input {
        None => return Ok("".into()),
        Some(Value::String(s)) => s,
        _ => {
            return Err(JsError::type_error(
                "RegExp legacy match state is unavailable",
            ));
        }
    };
    if matches!(field, Field::Input) {
        return Ok(input.into());
    }
    let state = vm
        .host_data::<Captures>(&this)
        .expect("input and captures installed together");
    let span = match field {
        Field::Match => Some(state.whole),
        Field::Left => Some((0, state.whole.0)),
        Field::Right => Some((state.whole.1, input.len())),
        Field::LastParen => state.last,
        Field::Group(index) => state.groups[index],
        Field::Input => unreachable!(),
    };
    match span {
        Some((start, end)) => vm.copy_string(&input.as_utf16()[start..end]),
        None => Ok("".into()),
    }
}

pub(super) fn install(vm: &mut Vm, ctor: &Value) {
    let fields = [
        ("input", Field::Input),
        ("$_", Field::Input),
        ("lastMatch", Field::Match),
        ("$&", Field::Match),
        ("lastParen", Field::LastParen),
        ("$+", Field::LastParen),
        ("leftContext", Field::Left),
        ("$`", Field::Left),
        ("rightContext", Field::Right),
        ("$'", Field::Right),
    ];
    for (name, field) in fields
        .into_iter()
        .map(|(n, f)| (n.to_owned(), f))
        .chain((0..9).map(|i| (format!("${}", i + 1), Field::Group(i))))
    {
        let getter = vm.native(
            &format!("get {name}"),
            0,
            false,
            Rc::new(move |vm, this, _| get(vm, this, field)),
        );
        let setter = matches!(field, Field::Input).then(|| {
            vm.native(
                &format!("set {name}"),
                1,
                false,
                Rc::new(|vm, this, args| {
                    receiver(vm, &this)?;
                    let value = vm.to_string(&super::runtime::arg(args, 0))?;
                    vm.set_host_slot(this, VISIBLE_INPUT, value.into())?;
                    Ok(Value::Undefined)
                }),
            )
        });
        vm.define_property(
            ctor.clone(),
            name.into(),
            Property {
                kind: PropertyKind::Accessor {
                    get: Some(getter),
                    set: setter,
                },
                enumerable: false,
                configurable: true,
            },
        )
        .expect("install RegExp accessors");
    }
}

pub(super) fn origin(vm: &Vm, re: &Value) -> Option<Origin> {
    match &vm.heap.object(re.as_object()?).ok()?.kind {
        ObjectKind::RegExp(_, origin) => Some(*origin),
        _ => None,
    }
}

pub(super) fn record(
    vm: &mut Vm,
    regexp: &Value,
    input: &JsString,
    matched: &Match,
) -> Result<(), JsError> {
    let origin = origin(vm, regexp).expect("native RegExp execution");
    if origin.realm != vm.realm.module_realm {
        return Ok(());
    }
    let ctor = Value::Object(vm.realm.regexp_constructor.unwrap());
    if !origin.enabled {
        vm.set_host_slot(ctor.clone(), ORIGINAL_INPUT, Value::Undefined)?;
        vm.set_host_slot(ctor, VISIBLE_INPUT, Value::Undefined)?;
        return Ok(());
    }
    let mut groups = [None; 9];
    for (slot, value) in groups.iter_mut().zip(&matched.captures) {
        *slot = *value;
    }
    let state = Captures {
        whole: matched.whole,
        groups,
        last: matched.captures.last().copied().flatten(),
    };
    vm.set_host_slot(ctor.clone(), ORIGINAL_INPUT, Value::String(input.clone()))?;
    vm.set_host_slot(ctor.clone(), VISIBLE_INPUT, Value::String(input.clone()))?;
    vm.set_host_data(ctor, Rc::new(state))
}

pub(super) fn record_bulk(
    vm: &mut Vm,
    re: &Value,
    input: &JsString,
    program: &Program,
    whole: Span,
) -> Result<(), JsError> {
    // The DFA reports only the match extent. Replay just the last successful
    // anchored match when the expression actually contains captures.
    let matched = if program.captures > 0 {
        let Some(m) = program.find(vm, input.as_utf16(), whole.0, true)? else {
            return Err(JsError::type_error("Inconsistent RegExp match extent"));
        };
        m
    } else {
        Match {
            whole,
            captures: Vec::new(),
        }
    };
    record(vm, re, input, &matched)
}
