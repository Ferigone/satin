//! Resumable destructuring assignments. A property or iterator value is read
//! after the assignment target is evaluated, even when that target can yield.
use super::{Emit, Op, Pattern, Value, anonymous};

#[derive(Clone, Copy, Debug)]
pub(crate) enum PatternStep {
    OpenArray,
    OpenObject,
    IteratorNext,
    IteratorSkip,
    IteratorRest,
    ObjectKey,
    ObjectValue,
    ObjectRest,
    Close,
}

pub(super) fn reference<'a>(pattern: &'a Pattern, pending: &mut Vec<Emit<'a>>, out: &mut Vec<Op>) {
    match pattern {
        Pattern::Name(name) => out.push(Op::Name(name.clone())),
        Pattern::Target(expr) => pending.push(Emit::Reference(expr, true)),
        Pattern::Default(target, _) => pending.push(Emit::PatternReference(target)),
        Pattern::Array(..) | Pattern::Object(..) => {}
    }
}

pub(super) fn assign<'a>(pattern: &'a Pattern, pending: &mut Vec<Emit<'a>>, out: &mut Vec<Op>) {
    use Emit::{Expression, Instruction, PatternAssign, PatternReference};
    let step = |op| Instruction(Op::Pattern(op));
    match pattern {
        Pattern::Name(_) | Pattern::Target(_) => out.extend([Op::Set, Op::Pop]),
        Pattern::Default(target, value) => {
            out.extend([Op::Dup, Op::Push(Value::Undefined), Op::Binary("===")]);
            let jump = out.len();
            out.extend([Op::JumpFalse(0), Op::Pop]);
            pending.extend([PatternAssign(target), Emit::Patch(jump, "&&")]);
            if let Pattern::Name(name) = target.as_ref()
                && anonymous(value)
            {
                pending.push(Emit::Named(value, name));
            } else {
                pending.push(Expression(value));
            }
        }
        Pattern::Array(items, rest) => {
            out.push(Op::Pattern(PatternStep::OpenArray));
            pending.push(step(PatternStep::Close));
            if let Some(rest) = rest {
                pending.extend([
                    PatternAssign(rest),
                    step(PatternStep::IteratorRest),
                    PatternReference(rest),
                ]);
            }
            for item in items.iter().rev() {
                if let Some(item) = item {
                    pending.extend([
                        PatternAssign(item),
                        step(PatternStep::IteratorNext),
                        PatternReference(item),
                    ]);
                } else {
                    pending.push(step(PatternStep::IteratorSkip));
                }
            }
        }
        Pattern::Object(properties, rest) => {
            out.push(Op::Pattern(PatternStep::OpenObject));
            pending.push(step(PatternStep::Close));
            if let Some(rest) = rest {
                pending.extend([
                    PatternAssign(rest),
                    step(PatternStep::ObjectRest),
                    PatternReference(rest),
                ]);
            }
            for (key, item, _) in properties.iter().rev() {
                pending.extend([
                    PatternAssign(item),
                    step(PatternStep::ObjectValue),
                    PatternReference(item),
                    step(PatternStep::ObjectKey),
                    Expression(key),
                ]);
            }
        }
    }
}
