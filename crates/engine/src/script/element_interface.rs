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
    let (template_prototype, div_prototype, anchor_prototype) = {
        let state = state.borrow();
        (state.template_prototype.clone(), state.div_prototype.clone(), state.anchor_prototype.clone())
    };
    let custom_state = state.clone();
    let constructor =
        webidl::interface(vm, prototype, "HTMLElement", 0, move |vm, _, _, target| {
            super::custom_elements::html_constructor(vm, &custom_state, target)
        })?;
    let element = vm.get_global("Element")?;
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    vm.call(set_prototype.clone(), Value::Undefined, &[constructor.clone(), element])?;
    // Autonomous custom elements use HTMLElement. The registry currently
    // rejects customized built-ins, leaving this HTMLConstructor illegal.
    let template = webidl::interface(vm, template_prototype, "HTMLTemplateElement", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal HTMLTemplateElement constructor"))
    })?;
    vm.call(set_prototype.clone(), Value::Undefined, &[template, constructor.clone()])?;
    let div = webidl::interface(vm, div_prototype.clone(), "HTMLDivElement", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal HTMLDivElement constructor"))
    })?;
    vm.call(set_prototype.clone(), Value::Undefined, &[div, constructor.clone()])?;
    let iframe = vm.get_global("HTMLIFrameElement")?;
    vm.call(set_prototype.clone(), Value::Undefined, &[iframe, constructor.clone()])?;
    let anchor = install_anchor(vm, state, anchor_prototype)?;
    vm.call(set_prototype, Value::Undefined, &[anchor, constructor])?;
    let read_state = state.clone();
    let get = vm.new_native_function("get align", move |vm, this, _| {
        let id = div_receiver(&read_state, &this)?;
        let state = read_state.borrow();
        let raw = state.document.attr(id, "align").unwrap_or("");
        vm.charge_buffer_work(raw.len())?;
        Ok(raw.into())
    });
    let write_state = state.clone();
    let set = vm.new_native_function("set align", move |vm, this, args| {
        let id = div_receiver(&write_state, &this)?;
        let value = super::text(vm, super::arg(args, 0))?;
        super::set_attribute(vm, &write_state, id, "align", Some(value))?;
        Ok(Value::Undefined)
    });
    vm.define_accessor(div_prototype, "align", Some(get), Some(set))?;
    Ok(())
}

fn anchor_id(state: &Shared, receiver: &Value) -> Result<crate::dom::NodeId, JsError> {
    let id = super::element(state, receiver)?;
    let state = state.borrow();
    if state.document.namespace(id) == crate::dom::Namespace::Html && state.document.tag(id) == "a" {
        return Ok(id);
    }
    Err(JsError::type_error("Expected an HTMLAnchorElement"))
}

fn anchor_value(vm: &mut Vm, host: &Shared, receiver: &Value, name: &str) -> Result<Value, JsError> {
    let id = anchor_id(host, receiver)?;
    let state = host.borrow();
    let Some(raw) = state.document.attr(id, name) else { return Ok("".into()) };
    vm.charge_buffer_work(raw.len())?;
    if name == "href" && let Ok(url) = super::document_base(&state).join(raw) {
        return Ok(url.to_string().into());
    }
    Ok(raw.into())
}

fn install_anchor(vm: &mut Vm, state: &Shared, prototype: Value) -> Result<Value, JsError> {
    let constructor = webidl::interface(vm, prototype.clone(), "HTMLAnchorElement", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal HTMLAnchorElement constructor"))
    })?;
    for name in ["href", "target", "download", "rel", "hreflang", "type"] {
        let read = state.clone();
        let get = vm.new_native_function(&format!("get {name}"), move |vm, this, _| {
            anchor_value(vm, &read, &this, name)
        });
        let write = state.clone();
        let set = vm.new_native_function(&format!("set {name}"), move |vm, this, args| {
            let id = anchor_id(&write, &this)?;
            let value = super::text(vm, super::arg(args, 0))?;
            super::set_attribute(vm, &write, id, name, Some(value))?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype.clone(), name, Some(get), Some(set))?;
    }
    let host = state.clone();
    webidl::operation(vm, prototype, "toString", 0, move |vm, this, _| {
        anchor_value(vm, &host, &this, "href")
    })?;
    Ok(constructor)
}

fn div_receiver(state: &Shared, this: &Value) -> Result<crate::dom::NodeId, JsError> {
    let id = super::element(state, this)?;
    let state = state.borrow();
    if state.document.namespace(id) == crate::dom::Namespace::Html && state.document.tag(id) == "div" {
        return Ok(id);
    }
    Err(JsError::type_error("Expected an HTMLDivElement"))
}
