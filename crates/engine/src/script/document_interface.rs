//! DOM operations shared by the active page and independent documents.
use super::{
    Shared, arg, descendants, document_base, document_node, document_part_in, getter, method,
    new_detached, text, webidl, wrap,
};
use crate::dom::NodeKind;
use rust_browser_javascript::{JsError, Value, Vm};

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    document_prototype: Value,
) -> Result<(), JsError> {
    for name in ["body", "head", "documentElement"] {
        let state = state.clone();
        getter(vm, document_prototype.clone(), name, move |vm, this, _| {
            let root = document_node(&state, &this)?;
            let id = {
                let state = state.borrow();
                document_part_in(&state.document, root, name)
            };
            wrap(vm, &state, id)
        })?;
    }
    for name in [
        "readyState",
        "defaultView",
        "contentType",
        "URL",
        "documentURI",
        "baseURI",
        "compatMode",
    ] {
        let state = state.clone();
        getter(vm, document_prototype.clone(), name, move |vm, this, _| {
            let root = document_node(&state, &this)?;
            let state = state.borrow();
            let active = root == state.document.root;
            Ok(match name {
                "readyState" => if active {
                    state.ready_state
                } else {
                    "complete"
                }
                .into(),
                "defaultView" => {
                    if active {
                        state.window.clone()
                    } else {
                        Value::Null
                    }
                }
                "contentType" => if state.document.is_html_document(root) {
                    "text/html"
                } else {
                    "application/xml"
                }
                .into(),
                "compatMode" => {
                    if active && state.document.quirks_mode == crate::dom::QuirksMode::Quirks {
                        "BackCompat"
                    } else {
                        "CSS1Compat"
                    }
                    .into()
                }
                _ if !active => "about:blank".into(),
                "baseURI" => {
                    vm.charge_work(state.document.nodes.len())?;
                    document_base(&state).as_str().into()
                }
                _ => state.url.as_str().into(),
            })
        })?;
    }
    for name in ["createTextNode", "createComment"] {
        let state_create = state.clone();
        method(
            vm,
            document_prototype.clone(),
            name,
            move |vm, this, args| {
                let root = document_node(&state_create, &this)?;
                webidl::required(args, 1, name)?;
                let value = text(vm, arg(args, 0))?;
                let kind = if name == "createTextNode" {
                    NodeKind::Text(value)
                } else {
                    NodeKind::Comment(value)
                };
                let id = new_detached(&state_create, kind)?;
                state_create.borrow_mut().document.adopt(id, root);
                wrap(vm, &state_create, Some(id))
            },
        )?;
    }
    let state_id = state.clone();
    method(
        vm,
        document_prototype.clone(),
        "getElementById",
        move |vm, this, args| {
            let root = document_node(&state_id, &this)?;
            webidl::required(args, 1, "Document.getElementById")?;
            let name = text(vm, arg(args, 0))?;
            let id = if name.is_empty() {
                None
            } else {
                let state = state_id.borrow();
                descendants(&state.document, root)
                    .find(|&id| state.document.attr(id, "id") == Some(&name))
            };
            wrap(vm, &state_id, id)
        },
    )?;
    Ok(())
}
