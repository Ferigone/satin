//! Iterator records and object rest exclusions survive yield and await with the
//! operand stack. Abrupt completion closes active iterators from the inside out.
use super::{Flow, Frame, reserve_one};
use crate::{
    ErrorKind, JsError, PropertyKey, Value, Vm, bytecode::PatternStep, iterator::IteratorRecord,
    vm::Item,
};
use std::mem::size_of;

pub(super) enum ActivePattern {
    Array(IteratorRecord),
    Object {
        source: Value,
        keys: Vec<PropertyKey>,
    },
}

impl ActivePattern {
    pub(super) fn visit(&self, visit: &mut impl FnMut(&Value)) {
        match self {
            Self::Array(record) => {
                visit(&record.iterator);
                visit(&record.next);
            }
            Self::Object { source, keys } => {
                visit(source);
                for key in keys {
                    match key {
                        PropertyKey::String(s) => visit(&Value::String(s.clone())),
                        PropertyKey::Symbol(s) => visit(&Value::Symbol(s.clone())),
                    }
                }
            }
        }
    }

    pub(super) fn extra_bytes(&self) -> usize {
        match self {
            Self::Object { keys, .. } => keys.capacity() * size_of::<PropertyKey>(),
            Self::Array(_) => 0,
        }
    }
}

pub(super) fn close_patterns(
    vm: &mut Vm,
    patterns: &mut Vec<ActivePattern>,
    mut flow: Flow,
) -> Flow {
    while let Some(pattern) = patterns.pop() {
        if let ActivePattern::Array(mut record) = pattern {
            flow = match flow {
                Ok(completion) => vm
                    .iterator_close(&mut record, Ok(Value::Undefined))
                    .map(|_| completion),
                Err(error) => Err(vm.iterator_close(&mut record, Err(error)).unwrap_err()),
            };
        }
    }
    flow
}

pub(super) fn step(vm: &mut Vm, frame: &mut Frame, step: PatternStep) -> Result<(), JsError> {
    use PatternStep::*;
    if matches!(step, OpenArray | OpenObject) {
        if frame.patterns.len() >= 16 {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Destructuring depth budget exhausted",
            ));
        }
        let source = vm.pop_value(&mut frame.operands)?;
        reserve_one(vm, &mut frame.patterns)?;
        frame.patterns.push(if matches!(step, OpenArray) {
            ActivePattern::Array(vm.get_iterator(source)?)
        } else if !source.is_nullish() {
            ActivePattern::Object {
                source,
                keys: Vec::new(),
            }
        } else {
            return Err(JsError::type_error("Cannot destructure null or undefined"));
        });
        return Ok(());
    }
    if matches!(step, Close) {
        if let ActivePattern::Array(mut record) =
            frame.patterns.pop().expect("open destructuring pattern")
        {
            vm.iterator_close(&mut record, Ok(Value::Undefined))?;
        }
        return Ok(());
    }
    reserve_one(vm, &mut frame.operands)?;
    let active = frame
        .patterns
        .last_mut()
        .expect("open destructuring pattern");
    let result = match (step, active) {
        (IteratorNext, ActivePattern::Array(record)) => {
            vm.iterator_step(record)?.unwrap_or_default()
        }
        (IteratorSkip, ActivePattern::Array(record)) => return vm.iterator_skip(record),
        (IteratorRest, ActivePattern::Array(record)) => vm.collect_pattern_rest(record)?,
        (ObjectKey, ActivePattern::Object { keys, .. }) => {
            let key = vm.pop_value(&mut frame.operands)?;
            let key = vm.to_property_key(&key)?;
            reserve_one(vm, keys)?;
            keys.push(key);
            return Ok(());
        }
        (ObjectValue, ActivePattern::Object { source, keys }) => vm.get_property_key(
            source.clone(),
            keys.last().expect("destructuring key").clone(),
        )?,
        (ObjectRest, ActivePattern::Object { source, keys }) => {
            vm.ensure_allocation(256)?;
            let object = vm.new_object();
            vm.copy_data_properties(object.clone(), source.clone(), keys)?;
            object
        }
        _ => unreachable!("pattern opcode and record must agree"),
    };
    frame.operands.push(Item::Value(result));
    Ok(())
}
