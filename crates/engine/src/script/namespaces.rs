use super::*;
use crate::dom::{AttributeNamespace, Namespace, xml_names};

fn attribute_namespace(uri: &str) -> Option<AttributeNamespace> {
    [AttributeNamespace::XLink, AttributeNamespace::Xml, AttributeNamespace::Xmlns]
        .into_iter().find(|namespace| namespace.uri() == uri)
}

fn attribute_key(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    namespace: Option<AttributeNamespace>,
    local: &str,
) -> Result<Option<String>, JsError> {
    let state = state.borrow();
    let NodeKind::Element { attrs, .. } = &state.document.nodes[id.0].kind else {
        unreachable!("Element receiver already checked")
    };
    if namespace.is_none() {
        let present = attrs.contains_key(local) && state.document.attribute_namespace(id, local).is_none();
        return Ok(present.then(|| local.to_owned()));
    }
    for key in attrs.keys() {
        vm.charge_work(key.len().saturating_add(1))?;
        if state.document.attribute_namespace(id, key) == namespace
            && state.document.attribute_local_name(id, key) == local
        {
            return Ok(Some(key.clone()));
        }
    }
    Ok(None)
}

pub(super) fn install_attributes(
    vm: &mut Vm,
    state: &Shared,
    prototype: &Value,
    errors: &dom_api::Errors,
) -> Result<(), JsError> {
    for (name, arity) in [
        ("setAttributeNS", 3), ("getAttributeNS", 2), ("hasAttributeNS", 2), ("removeAttributeNS", 2),
    ] {
        let state = state.clone();
        let errors = errors.clone();
        webidl::operation(vm, prototype.clone(), name, arity, move |vm, receiver, args| {
            let id = element(&state, &receiver)?;
            webidl::required(args, arity as usize, name)?;
            let uri = if args[0].is_nullish() { String::new() } else { text(vm, args[0].clone())? };
            let name_arg = text(vm, args[1].clone())?;
            let namespace = attribute_namespace(&uri);
            if name == "setAttributeNS" {
                let value = text(vm, args[2].clone())?;
                let names = name_arg.split_once(':');
                let (prefix, local) = names.unwrap_or(("", name_arg.as_str()));
                let valid = |part: &str| {
                    let mut chars = part.chars();
                    chars.next().is_some_and(|ch| ch != ':' && xml_names::name_start(ch))
                        && chars.all(|ch| ch != ':' && xml_names::name_char(ch))
                };
                if !valid(local) || (names.is_some() && !valid(prefix)) {
                    return Err(errors.exception(vm, "InvalidCharacterError", "Invalid attribute qualified name"));
                }
                if (!prefix.is_empty() && uri.is_empty())
                    || (prefix == "xml" && namespace != Some(AttributeNamespace::Xml))
                    || ((name_arg == "xmlns" || prefix == "xmlns") != (namespace == Some(AttributeNamespace::Xmlns)))
                {
                    return Err(errors.exception(vm, "NamespaceError", "Invalid attribute namespace and prefix"));
                }
                if namespace.is_none() && !uri.is_empty() {
                    return Err(unsupported("Additional XML attribute namespaces are not implemented"));
                }
                let key = attribute_key(vm, &state, id, namespace, local)?.unwrap_or(name_arg);
                mutate_attribute(vm, &state, id, &key, Some(value), Some(namespace))?;
                return Ok(Value::Undefined);
            }
            let key = if namespace.is_some() || uri.is_empty() {
                attribute_key(vm, &state, id, namespace, &name_arg)?
            } else {
                None
            };
            match (name, key) {
                ("hasAttributeNS", key) => Ok(key.is_some().into()),
                ("getAttributeNS", Some(key)) => {
                    let state = state.borrow();
                    let value = state.document.attr(id, &key).expect("found attribute");
                    vm.charge_work(value.len())?;
                    Ok(Value::from(value))
                }
                ("getAttributeNS", None) => Ok(Value::Null),
                (_, Some(key)) => {
                    mutate_attribute(vm, &state, id, &key, None, Some(namespace))?;
                    Ok(Value::Undefined)
                }
                _ => Ok(Value::Undefined),
            }
        })?;
    }
    Ok(())
}

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
