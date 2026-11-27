use super::{Expr, Function, FunctionKind, JsError, JsString, Stmt, Value, ungroup};
use crate::Vm;

/// A return using only positional arguments and primitive constants cannot
/// observe a lexical environment unless coercion calls user code.
#[derive(Clone, Debug)]
pub(crate) struct LeafReturn {
    operator: &'static str,
    left: Operand,
    right: Operand,
    select: Option<(Operand, Operand)>,
}

#[derive(Clone, Debug)]
enum Operand {
    Argument(u8),
    Number(f64),
    Bool(bool),
    Null,
}

impl Operand {
    fn compile(expr: &Expr, params: &[JsString]) -> Option<Self> {
        Some(match ungroup(expr) {
            Expr::Identifier(name) => Self::Argument(params.iter().position(|p| p == name)? as u8),
            Expr::Literal(Value::Number(n)) => Self::Number(*n),
            Expr::Unary("-", value) => match ungroup(value) {
                Expr::Literal(Value::Number(n)) => Self::Number(-n),
                _ => return None,
            },
            Expr::Literal(Value::Bool(b)) => Self::Bool(*b),
            Expr::Literal(Value::Null) => Self::Null,
            _ => return None,
        })
    }

    fn value(&self, args: &[Value]) -> Value {
        match self {
            Self::Argument(i) => args.get(usize::from(*i)).cloned().unwrap_or_default(),
            Self::Number(n) => Value::Number(*n),
            Self::Bool(b) => Value::Bool(*b),
            Self::Null => Value::Null,
        }
    }
}

impl LeafReturn {
    pub(super) fn compile(f: &Function, params: &[JsString], simple: bool) -> Option<Box<Self>> {
        let [Stmt::Return(Some(expr))] = f.body.as_slice() else { return None; };
        if f.kind != FunctionKind::Ordinary || !simple || params.len() > 8
            || params.iter().enumerate().any(|(i,p)| params[..i].contains(p))
        { return None; }
        let (expr, select) = match ungroup(expr) {
            Expr::Conditional(arms, fallback) => {
                let [(test, value)] = arms.as_slice() else { return None; };
                (ungroup(test), Some((Operand::compile(value,params)?, Operand::compile(fallback,params)?)))
            }
            expr => (expr, None),
        };
        let Expr::Binary(operator,left,right) = expr else { return None; };
        if !matches!(*operator, "<" | ">" | "<=" | ">=" | "===" | "!==" | "==" | "!="
            | "+" | "-" | "*" | "/" | "%" | "|" | "&" | "^" | "<<" | ">>" | ">>>")
        { return None; }
        Some(Box::new(Self { operator, left:Operand::compile(left,params)?,
            right:Operand::compile(right,params)?, select }))
    }

    pub(crate) fn evaluate(&self, vm: &mut Vm, args: &[Value]) -> Option<Result<Value, JsError>> {
        let left = self.left.value(args);
        let right = self.right.value(args);
        if left.as_object().is_some() || right.as_object().is_some() { return None; }
        Some(vm.tick().and_then(|()| vm.binary(self.operator, left, right)).map(|result| {
            match &self.select {
                Some((yes,no)) => if result.is_truthy() { yes } else { no }.value(args),
                None => result,
            }
        }))
    }
}
