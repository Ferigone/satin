//! Expose the shared prototypes of native DOM node and element wrappers.
use super::webidl;
use rust_browser_javascript::{JsError, Value, Vm};

pub(super) fn install(vm: &mut Vm, node: Value, element: Value) -> Result<(), JsError> {
    let node_constructor = webidl::interface(vm, node.clone(), "Node", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal Node constructor"))
    })?;
    let element_constructor = webidl::interface(vm, element, "Element", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal Element constructor"))
    })?;

    // EventTarget has already linked the node instance prototype to its own.
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    let target_constructor = vm.get_global("EventTarget")?;
    vm.call(
        set_prototype.clone(),
        Value::Undefined,
        &[node_constructor.clone(), target_constructor],
    )?;
    vm.call(
        set_prototype,
        Value::Undefined,
        &[element_constructor, node_constructor.clone()],
    )?;

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
