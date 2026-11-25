//! DOM operations shared by the active page and independent documents.
use super::{
    Shared, arg, descendants, document_base, document_node, document_part_in, dom_api::Errors,
    getter, method, new_detached, text, webidl, wrap,
};
use crate::dom::{NodeId, NodeKind};
use rust_browser_javascript::{JsError, JsString, Value, Vm};

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    document_prototype: Value,
    errors: &Errors,
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
    let cdata_state = state.clone();
    let cdata_errors = errors.clone();
    webidl::operation(
        vm,
        document_prototype.clone(),
        "createCDATASection",
        1,
        move |vm, this, args| {
            let document = document_node(&cdata_state, &this)?;
            webidl::required(args, 1, "Document.createCDATASection")?;
            let data = text(vm, args[0].clone())?;
            if cdata_state.borrow().document.is_html_document(document) {
                return Err(cdata_errors.exception(
                    vm,
                    "NotSupportedError",
                    "Cannot create a CDATA section in an HTML document",
                ));
            }
            vm.charge_work(data.len())?;
            if data.contains("]]>") {
                return Err(cdata_errors.exception(
                    vm,
                    "InvalidCharacterError",
                    "CDATA section data contains ]]>",
                ));
            }
            let id = new_detached(&cdata_state, NodeKind::Cdata(data))?;
            cdata_state.borrow_mut().document.adopt(id, document);
            wrap(vm, &cdata_state, Some(id))
        },
    )?;
    let pi_state = state.clone();
    let pi_errors = errors.clone();
    webidl::operation(
        vm,
        document_prototype.clone(),
        "createProcessingInstruction",
        2,
        move |vm, this, args| {
            let document = document_node(&pi_state, &this)?;
            webidl::required(args, 2, "Document.createProcessingInstruction")?;
            let target = vm.to_string(&args[0])?;
            let data = text(vm, args[1].clone())?;
            let id = processing_instruction(vm, &pi_state, &pi_errors, target, data)?;
            pi_state.borrow_mut().document.adopt(id, document);
            wrap(vm, &pi_state, Some(id))
        },
    )?;
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

/// XML 1.0 (Fifth Edition), productions 4, 4a and 5. A PI target is a Name,
/// so colons are permitted independently of namespace prefix handling.
fn valid_xml_name(name: &str) -> bool {
    use crate::dom::xml_names::{name_char, name_start};
    let mut chars = name.chars();
    chars.next().is_some_and(name_start) && chars.all(name_char)
}

pub(super) fn processing_instruction(
    vm: &mut Vm,
    state: &Shared,
    errors: &Errors,
    target: JsString,
    data: String,
) -> Result<NodeId, JsError> {
    vm.charge_work(target.len() + data.len())?;
    let target = String::from_utf16(target.as_utf16())
        .ok()
        .filter(|name| valid_xml_name(name))
        .ok_or_else(|| {
            errors.exception(
                vm,
                "InvalidCharacterError",
                "Invalid processing instruction target",
            )
        })?;
    if data.contains("?>") {
        return Err(errors.exception(
            vm,
            "InvalidCharacterError",
            "Processing instruction data contains ?>",
        ));
    }
    new_detached(state, NodeKind::ProcessingInstruction { target, data })
}
