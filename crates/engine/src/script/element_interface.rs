//! Expose the shared prototypes of native DOM node and element wrappers.
use super::{Shared, dom_api, webidl};
use rust_browser_javascript::{JsError, Value, Vm};

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    node: Value,
    element: Value,
    errors: &dom_api::Errors,
) -> Result<(), JsError> {
    let node_constructor = webidl::interface(vm, node.clone(), "Node", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal Node constructor"))
    })?;
    let element_constructor = webidl::interface(vm, element, "Element", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal Element constructor"))
    })?;

    let document_state = state.clone();
    let document_prototype = state.borrow().document_prototype.clone();
    let document_constructor = webidl::interface(
        vm,
        document_prototype.clone(),
        "Document",
        0,
        move |vm, _, _, new_target| {
            let target = new_target.ok_or_else(|| JsError::type_error("Document requires new"))?;
            let prototype = vm.get_property(target, "prototype")?;
            let prototype = if prototype.as_object().is_some() {
                prototype
            } else {
                document_prototype.clone()
            };
            let id = super::new_detached(&document_state, crate::dom::NodeKind::Document)?;
            document_state
                .borrow_mut()
                .document
                .xml_documents
                .insert(id);
            super::wrap_new_node(vm, &document_state, id, prototype)
        },
    )?;

    let fragment_state = state.clone();
    let fragment_errors = errors.clone();
    let fragment_prototype = state.borrow().fragment_prototype.clone();
    let fragment_constructor = webidl::interface(
        vm,
        fragment_prototype.clone(),
        "DocumentFragment",
        0,
        move |vm, _, _, new_target| {
            let target =
                new_target.ok_or_else(|| JsError::type_error("DocumentFragment requires new"))?;
            let prototype = vm.get_property(target, "prototype")?;
            let prototype = if prototype.as_object().is_some() {
                prototype
            } else {
                fragment_prototype.clone()
            };
            dom_api::new_fragment(vm, &fragment_state, &fragment_errors, prototype)
        },
    )?;

    // EventTarget has already linked the node instance prototype to its own.
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    let target_constructor = vm.get_global("EventTarget")?;
    vm.call(
        set_prototype.clone(),
        Value::Undefined,
        &[node_constructor.clone(), target_constructor],
    )?;
    for constructor in [
        element_constructor,
        document_constructor,
        fragment_constructor,
    ] {
        vm.call(
            set_prototype.clone(),
            Value::Undefined,
            &[constructor, node_constructor.clone()],
        )?;
    }

    for (name, value) in [
        ("ELEMENT_NODE", 1u32),
        ("ATTRIBUTE_NODE", 2),
        ("TEXT_NODE", 3),
        ("CDATA_SECTION_NODE", 4),
        ("ENTITY_REFERENCE_NODE", 5),
        ("ENTITY_NODE", 6),
        ("PROCESSING_INSTRUCTION_NODE", 7),
        ("COMMENT_NODE", 8),
        ("DOCUMENT_NODE", 9),
        ("DOCUMENT_TYPE_NODE", 10),
        ("DOCUMENT_FRAGMENT_NODE", 11),
        ("NOTATION_NODE", 12),
        ("DOCUMENT_POSITION_DISCONNECTED", 0x01),
        ("DOCUMENT_POSITION_PRECEDING", 0x02),
        ("DOCUMENT_POSITION_FOLLOWING", 0x04),
        ("DOCUMENT_POSITION_CONTAINS", 0x08),
        ("DOCUMENT_POSITION_CONTAINED_BY", 0x10),
        ("DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC", 0x20),
    ] {
        for receiver in [&node_constructor, &node] {
            vm.define_data_property(receiver.clone(), name, value.into(), false, true, false)?;
        }
    }
    Ok(())
}

pub(super) fn install_html(vm: &mut Vm, state: &Shared, prototype: Value) -> Result<(), JsError> {
    let state = state.clone();
    let constructor =
        webidl::interface(vm, prototype, "HTMLElement", 0, move |vm, _, _, target| {
            super::custom_elements::html_constructor(vm, &state, target)
        })?;
    let element = vm.get_global("Element")?;
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    vm.call(set_prototype, Value::Undefined, &[constructor, element])?;
    Ok(())
}
