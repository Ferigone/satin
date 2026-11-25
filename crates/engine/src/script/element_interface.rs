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
    let attr_state = state.clone();
    let attr_errors = errors.clone();
    webidl::operation(
        vm,
        element.clone(),
        "toggleAttribute",
        1,
        move |vm, this, args| {
            let id = super::element(&attr_state, &this)?;
            webidl::required(args, 1, "Element.toggleAttribute")?;
            let mut name = super::text(vm, args[0].clone())?;
            vm.charge_work(name.len())?;
            if !super::valid_attribute_name(&name) {
                return Err(attr_errors.exception(
                    vm,
                    "InvalidCharacterError",
                    "Invalid attribute name",
                ));
            }
            let present = {
                let state = attr_state.borrow();
                if state.document.html_element_in_html_document(id) {
                    name.make_ascii_lowercase();
                }
                state.document.attr(id, &name).is_some()
            };
            let desired = args
                .get(1)
                .filter(|value| !value.is_undefined())
                .map_or(!present, Value::is_truthy);
            if present != desired {
                super::set_attribute(vm, &attr_state, id, &name, desired.then(String::new))?;
            }
            Ok(desired.into())
        },
    )?;
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
                .other_documents
                .insert(id, crate::dom::DocumentKind::Xml);
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
    install_character_interfaces(vm, state, &node_constructor, &set_prototype, errors)?;
    let svg_prototype = state.borrow().svg_element_prototype.clone();
    let svg = webidl::interface(vm, svg_prototype, "SVGElement", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal SVGElement constructor"))
    })?;
    vm.call(
        set_prototype.clone(),
        Value::Undefined,
        &[svg, element_constructor.clone()],
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

fn install_character_interfaces(
    vm: &mut Vm,
    state: &Shared,
    node: &Value,
    inherit: &Value,
    errors: &dom_api::Errors,
) -> Result<(), JsError> {
    let prototypes = state.borrow();
    let character_data = prototypes.character_data_prototype.clone();
    let text = prototypes.text_prototype.clone();
    let comment = prototypes.comment_prototype.clone();
    drop(prototypes);
    let character_data =
        webidl::interface(vm, character_data, "CharacterData", 0, |_, _, _, _| {
            Err(JsError::type_error("Illegal CharacterData constructor"))
        })?;
    vm.call(
        inherit.clone(),
        Value::Undefined,
        &[character_data.clone(), node.clone()],
    )?;
    for (name, prototype) in [("Text", text), ("Comment", comment)] {
        let state = state.clone();
        let fallback = prototype.clone();
        let constructor = webidl::interface(vm, prototype, name, 0, move |vm, _, args, target| {
            let target =
                target.ok_or_else(|| JsError::type_error(format!("{name} requires new")))?;
            let requested = vm.get_property(target, "prototype")?;
            let prototype = if requested.as_object().is_some() {
                requested
            } else {
                fallback.clone()
            };
            let value = if let Some(arg) = args.first().filter(|arg| !arg.is_undefined()) {
                super::text(vm, arg.clone())?
            } else {
                String::new()
            };
            let kind = if name == "Text" {
                crate::dom::NodeKind::Text(value)
            } else {
                crate::dom::NodeKind::Comment(value)
            };
            let id = super::new_detached(&state, kind)?;
            super::wrap_new_node(vm, &state, id, prototype)
        })?;
        vm.call(
            inherit.clone(),
            Value::Undefined,
            &[constructor, character_data.clone()],
        )?;
    }
    let text_constructor = vm.get_global("Text")?;
    let cdata = state.borrow().cdata_prototype.clone();
    let cdata = webidl::interface(vm, cdata, "CDATASection", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal CDATASection constructor"))
    })?;
    vm.call(
        inherit.clone(),
        Value::Undefined,
        &[cdata, text_constructor],
    )?;

    let pi_prototype = state.borrow().processing_instruction_prototype.clone();
    let getter_state = state.clone();
    super::getter(vm, pi_prototype.clone(), "target", move |vm, this, _| {
        let id = super::node(&getter_state, &this)?;
        let state = getter_state.borrow();
        let crate::dom::NodeKind::ProcessingInstruction { target, .. } =
            &state.document.nodes[id.0].kind
        else {
            return Err(JsError::type_error("Expected ProcessingInstruction"));
        };
        vm.charge_work(target.len())?;
        Ok(target.clone().into())
    })?;
    let state = state.clone();
    let errors = errors.clone();
    let pi = webidl::interface(
        vm,
        pi_prototype.clone(),
        "ProcessingInstruction",
        1,
        move |vm, _, args, new_target| {
            let new_target = new_target
                .ok_or_else(|| JsError::type_error("ProcessingInstruction requires new"))?;
            webidl::required(args, 1, "ProcessingInstruction")?;
            let requested = vm.get_property(new_target, "prototype")?;
            let prototype = if requested.as_object().is_some() {
                requested
            } else {
                pi_prototype.clone()
            };
            let target = vm.to_string(&args[0])?;
            let data = if let Some(arg) = args.get(1).filter(|arg| !arg.is_undefined()) {
                super::text(vm, arg.clone())?
            } else {
                String::new()
            };
            let id = super::document_interface::processing_instruction(
                vm, &state, &errors, target, data,
            )?;
            super::wrap_new_node(vm, &state, id, prototype)
        },
    )?;
    vm.call(inherit.clone(), Value::Undefined, &[pi, character_data])?;
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
