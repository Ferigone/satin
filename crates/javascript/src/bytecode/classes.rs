use super::*;
use crate::parser::{Class, ClassElement, ClassKey, MethodKind};
#[derive(Clone, Debug)]
pub(crate) struct ClassCode {
    pub name: Option<JsString>,
    pub heritage: Option<Rc<Chunk>>,
    pub elements: Vec<ClassElementCode>,
    pub private_names: Vec<JsString>,
}
#[derive(Clone, Debug)]
pub(crate) enum ClassKeyCode {
    Public(Rc<Chunk>),
    Private(JsString),
}
#[derive(Clone, Debug)]
pub(crate) enum ClassElementCode {
    Method {
        key: ClassKeyCode,
        code: Rc<Code>,
        kind: MethodKind,
        is_static: bool,
        constructor: bool,
    },
    Field {
        key: ClassKeyCode,
        value: Option<Rc<Chunk>>,
        is_static: bool,
        infer_name: bool,
    },
    StaticBlock(Rc<Chunk>),
}
pub(super) fn compile(class: &Class) -> Result<Rc<ClassCode>, JsError> {
    if class.name.as_ref().is_some_and(invalid_strict_binding) {
        return Err(syntax("Invalid class binding name"));
    }
    let mut elements = Vec::new();
    for element in &class.elements {
        elements.push(match element {
            ClassElement::Method {
                key,
                function,
                kind,
                is_static,
                constructor,
            } => ClassElementCode::Method {
                key: compile_key(key)?,
                code: compile_function(function, true)?,
                kind: *kind,
                is_static: *is_static,
                constructor: *constructor,
            },
            ClassElement::Field {
                key,
                value,
                is_static,
            } => ClassElementCode::Field {
                key: compile_key(key)?,
                value: value.as_ref().map(|v| expression(v, true)).transpose()?,
                is_static: *is_static,
                infer_name: value.as_ref().is_some_and(anonymous),
            },
            ClassElement::StaticBlock(body) => {
                ClassElementCode::StaticBlock(block(body, true, true)?)
            }
        });
    }
    Ok(Rc::new(ClassCode {
        name: class.name.clone(),
        heritage: class
            .heritage
            .as_ref()
            .map(|e| require_non_suspending(expression(e, true)?, "class heritage"))
            .transpose()?,
        elements,
        private_names: class.private_names.clone(),
    }))
}
fn compile_key(key: &ClassKey) -> Result<ClassKeyCode, JsError> {
    Ok(match key {
        ClassKey::Public(key) => ClassKeyCode::Public(require_non_suspending(
            expression(key, true)?,
            "a class element name",
        )?),
        ClassKey::Private(name) => ClassKeyCode::Private(name.clone()),
    })
}
pub(super) fn retained_size(
    class: &ClassCode,
    functions: &mut Vec<Rc<Code>>,
    chunks: &mut Vec<Rc<Chunk>>,
) -> usize {
    chunks.extend(class.heritage.iter().cloned());
    let mut bytes = 256
        + class.name.as_ref().map_or(0, |n| n.len() * 2)
        + class.elements.len() * 128
        + class
            .private_names
            .iter()
            .map(|name| 32 + name.len() * 2)
            .sum::<usize>();
    for element in &class.elements {
        let key = match element {
            ClassElementCode::Method { key, code, .. } => {
                functions.push(code.clone());
                key
            }
            ClassElementCode::Field { key, value, .. } => {
                chunks.extend(value.iter().cloned());
                key
            }
            ClassElementCode::StaticBlock(body) => {
                chunks.push(body.clone());
                continue;
            }
        };
        match key {
            ClassKeyCode::Public(code) => chunks.push(code.clone()),
            ClassKeyCode::Private(name) => bytes += name.len() * 2,
        }
    }
    bytes
}
