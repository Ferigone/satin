use super::{Expr, Stmt};
use crate::Value;

/// A directive prologue can only start a Script or FunctionBody. Keep the
/// original statement: dropping it would change the completion value.
pub(super) fn visit(statement: &Stmt, in_prologue: &mut bool) -> bool {
    if !*in_prologue {
        return false;
    }
    match statement {
        Stmt::EscapedStringStatement(_) => false,
        Stmt::Expression(Expr::Literal(Value::String(text))) => {
            const USE_STRICT: [u16; 10] = [117, 115, 101, 32, 115, 116, 114, 105, 99, 116];
            text.len() == USE_STRICT.len() && text.as_utf16() == USE_STRICT
        }
        _ => {
            *in_prologue = false;
            false
        }
    }
}
