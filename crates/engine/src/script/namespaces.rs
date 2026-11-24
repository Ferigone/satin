use super::*;
use crate::dom::Namespace;

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    prototype: Value,
    document: Value,
) -> Result<(), JsError> {
    for name in ["namespaceURI", "localName", "prefix"] {
        let state = state.clone();
        getter(vm, prototype.clone(), name, move |_, this, _| {
            let id = node(&state, &this)?;
            let state = state.borrow();
            if !matches!(state.document.nodes[id.0].kind, NodeKind::Element { .. }) {
                return Ok(Value::Null);
            }
            Ok(match name {
                "namespaceURI" => state
                    .document
                    .namespace_uri(id)
                    .map(Value::from)
                    .unwrap_or(Value::Null),
                "localName" => state.document.tag(id).into(),
                _ => Value::Null,
            })
        })?;
    }
    let state = state.clone();
    method(vm, document, "createElementNS", move |vm, this, args| {
        let receiver = super::document_node(&state, &this)?;
        webidl::required(args, 2, "Document.createElementNS")?;
        let value = arg(args, 0);
        let namespace = if value.is_nullish() {
            String::new()
        } else {
            text(vm, value)?
        };
        let namespace = match namespace.as_str() {
            "" => Namespace::Null,
            "http://www.w3.org/1999/xhtml" => Namespace::Html,
            "http://www.w3.org/2000/svg" => Namespace::Svg,
            "http://www.w3.org/1998/Math/MathML" => Namespace::MathMl,
            _ => {
                return Err(unsupported("Additional XML namespaces are not implemented"));
            }
        };
        let tag = text(vm, arg(args, 1))?;
        if tag.contains(':') {
            return Err(unsupported(
                "Element namespace prefixes need an explicit qualified-name model",
            ));
        }
        if tag.is_empty()
            || !tag
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(JsError::type_error("Invalid element qualified name"));
        }
        let id = new_detached(
            &state,
            NodeKind::Element {
                tag,
                attrs: Default::default(),
            },
        )?;
        state.borrow_mut().document.adopt(id, receiver);
        if namespace != Namespace::Html {
            state.borrow_mut().document.namespaces.insert(id, namespace);
            return wrap(vm, &state, Some(id));
        }
        if receiver == state.borrow().document.root {
            custom_elements::created(vm, &state, id)
        } else {
            wrap(vm, &state, Some(id))
        }
    })
}
