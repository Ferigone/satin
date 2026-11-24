//! A bounded, read-only XML response DOM. These nodes are never placed in the
//! live document, script loader, layout, or resource-discovery pipeline.
mod source;

use crate::script::{arg, getter, webidl};
use roxmltree::{Node, NodeType};
use rust_browser_javascript::{HostIndexedProperties, JsError, Value, Vm};
use std::rc::Rc;

pub(super) use source::{Source, fallback_text_encoding};

const NAME: u32 = 0;
const KIND: u32 = 1;
const DATA: u32 = 2;
const PARENT: u32 = 3;
const OWNER: u32 = 4;
const FIRST: u32 = 5;
const LAST: u32 = 6;
const NEXT: u32 = 7;
const PREVIOUS: u32 = 8;
const CHILDREN: u32 = 9;
const NAMESPACE: u32 = 10;
const LOCAL: u32 = 11;
const PREFIX: u32 = 12;
const ATTRIBUTES: u32 = 13;
const ATTRIBUTES_NS: u32 = 14;
const DOCUMENT_ELEMENT: u32 = 15;
const CONTENT_TYPE: u32 = 16;
const URL: u32 = 17;
const ENCODING: u32 = 18;
const XMLNS: &str = "http://www.w3.org/2000/xmlns/";

struct XmlNode;

struct Children(Vec<Value>);

impl HostIndexedProperties for Children {
    fn indexed_len(&self) -> Result<usize, JsError> {
        Ok(self.0.len())
    }

    fn indexed_get(&self, _: &mut Vm, index: u32) -> Result<Option<Value>, JsError> {
        Ok(self.0.get(index as usize).cloned())
    }
}

pub(super) struct Host {
    document: Value,
    element: Value,
    character: Value,
}

impl Host {
    pub(super) fn install(vm: &mut Vm) -> Result<Self, JsError> {
        let node = prototype(vm, "Node")?;
        let element = prototype(vm, "Element")?;
        let children = install_child_nodes(vm)?;
        let document = vm.new_object_with_prototype(node.clone())?;
        let character = vm.new_object_with_prototype(node)?;
        let element = vm.new_object_with_prototype(element)?;
        for prototype in [&document, &character, &element] {
            vm.root(prototype.clone());
            install_node(vm, prototype, &children)?;
        }
        webidl::tag(vm, document.clone(), "XMLDocument")?;
        for (name, slot) in [
            ("documentElement", DOCUMENT_ELEMENT),
            ("contentType", CONTENT_TYPE),
            ("URL", URL),
            ("documentURI", URL),
            ("characterSet", ENCODING),
        ] {
            install_getter(vm, &document, name, slot)?;
        }
        getter(vm, document.clone(), "defaultView", |vm, this, _| {
            require_node(vm, &this)?;
            Ok(Value::Null)
        })?;
        for (name, slot) in [
            ("tagName", NAME),
            ("namespaceURI", NAMESPACE),
            ("localName", LOCAL),
            ("prefix", PREFIX),
        ] {
            install_getter(vm, &element, name, slot)?;
        }
        webidl::operation(vm, element.clone(), "getAttribute", 1, |vm, this, args| {
            require_node(vm, &this)?;
            webidl::required(args, 1, "Element.getAttribute")?;
            let Some(name) = string_arg(vm, args, 0)? else {
                return Ok(Value::Null);
            };
            get_attribute(vm, &this, ATTRIBUTES, &name)
        })?;
        webidl::operation(
            vm,
            element.clone(),
            "getAttributeNS",
            2,
            |vm, this, args| {
                require_node(vm, &this)?;
                webidl::required(args, 2, "Element.getAttributeNS")?;
                let ns = if arg(args, 0).is_nullish() {
                    Some(String::new())
                } else {
                    string_arg(vm, args, 0)?
                };
                let (Some(ns), Some(name)) = (ns, string_arg(vm, args, 1)?) else {
                    return Ok(Value::Null);
                };
                if ns.contains('\0') || name.contains('\0') {
                    return Ok(Value::Null);
                }
                get_attribute(vm, &this, ATTRIBUTES_NS, &ns_key(&ns, &name))
            },
        )?;
        install_getter(vm, &character, "data", DATA)?;
        Ok(Self {
            document,
            element,
            character,
        })
    }

    pub(super) fn parse(&self, vm: &mut Vm, source: &Source, url: &str) -> Result<Value, JsError> {
        vm.charge_work(source.bytes_len())?;
        let Some((xml, encoding)) = source.decode() else {
            return Ok(Value::Null);
        };
        vm.charge_work(xml.len())?;
        let Some(parsed) = source::parse(&xml) else {
            return Ok(Value::Null);
        };
        let mut objects = Vec::with_capacity(parsed.descendants().len());
        let brand = Rc::new(XmlNode);
        for node in parsed.descendants() {
            let proto = match node.node_type() {
                NodeType::Root => &self.document,
                NodeType::Element => &self.element,
                _ => &self.character,
            };
            let object = vm.new_object_with_prototype(proto.clone())?;
            vm.mark_platform_object(&object)?;
            vm.set_host_data(object.clone(), brand.clone())?;
            objects.push(object);
        }
        let root = &objects[parsed.root().id().get_usize()];
        put_text(vm, root, CONTENT_TYPE, source.content_type())?;
        put_text(vm, root, URL, url)?;
        put_text(vm, root, ENCODING, encoding.name())?;
        vm.set_host_slot(
            root.clone(),
            DOCUMENT_ELEMENT,
            objects[parsed.root_element().id().get_usize()].clone(),
        )?;
        for node in parsed.descendants() {
            vm.charge_work(1)?;
            let object = &objects[node.id().get_usize()];
            if !node.is_root() {
                vm.set_host_slot(object.clone(), OWNER, root.clone())?;
            }
            for (slot, other) in [
                (PARENT, node.parent()),
                (FIRST, node.first_child()),
                (LAST, node.last_child()),
                (NEXT, node.next_sibling()),
                (PREVIOUS, node.prev_sibling()),
            ] {
                if let Some(other) = other {
                    vm.set_host_slot(
                        object.clone(),
                        slot,
                        objects[other.id().get_usize()].clone(),
                    )?;
                }
            }
            let (name, kind, data) = match node.node_type() {
                NodeType::Root => ("#document", 9, None),
                NodeType::Element => (install_element(vm, object, node, &xml)?, 1, None),
                NodeType::PI => {
                    let pi = node.pi().expect("PI node");
                    (pi.target, 7, Some(pi.value.unwrap_or("")))
                }
                NodeType::Comment => ("#comment", 8, node.text()),
                NodeType::Text => ("#text", 3, node.text()),
            };
            put_text(vm, object, NAME, name)?;
            vm.set_host_slot(object.clone(), KIND, Value::from(kind as u32))?;
            if let Some(data) = data {
                put_text(vm, object, DATA, data)?;
            }
        }
        Ok(root.clone())
    }
}

fn install_element<'a>(
    vm: &mut Vm,
    object: &Value,
    node: Node<'_, 'a>,
    xml: &'a str,
) -> Result<&'a str, JsError> {
    let tag = node.tag_name();
    let start = xml
        .get(node.range().start..)
        .and_then(|tail| tail.strip_prefix('<'))
        .ok_or_else(position_error)?;
    let end = start
        .find(|c| source::xml_space(c) || matches!(c, '/' | '>'))
        .ok_or_else(position_error)?;
    let name = &start[..end];
    put_text(vm, object, LOCAL, tag.name())?;
    if let Some(uri) = tag.namespace() {
        put_text(vm, object, NAMESPACE, uri)?;
    }
    if let Some((prefix, _)) = name.split_once(':') {
        put_text(vm, object, PREFIX, prefix)?;
    }
    let qualified = vm.new_object_with_prototype(Value::Null)?;
    let namespaced = vm.new_object_with_prototype(Value::Null)?;
    for attr in node.attributes() {
        // roxmltree 0.20 saturates range_qname's length at u16::MAX,
        // which may truncate the name or cut through a UTF-8 code point.
        // The attribute's start offset is not subject to that limit.
        let qname = xml
            .get(attr.range().start..)
            .and_then(|tail| tail.split(|c| source::xml_space(c) || c == '=').next())
            .filter(|name| !name.is_empty())
            .ok_or_else(position_error)?;
        put_attribute(
            vm,
            &qualified,
            &namespaced,
            qname,
            attr.namespace().unwrap_or(""),
            attr.name(),
            attr.value(),
        )?;
    }
    // roxmltree exposes namespace bindings separately from attributes. Take
    // only declarations written on this element, not inherited bindings.
    let mut start = start[end..].trim_start_matches(source::xml_space);
    while let Some((name, _, rest)) = source::attribute(start) {
        if let Some(local) = name
            .strip_prefix("xmlns:")
            .or((name == "xmlns").then_some("xmlns"))
        {
            let prefix = (name != "xmlns").then_some(local);
            let value = node.lookup_namespace_uri(prefix).unwrap_or("");
            put_attribute(vm, &qualified, &namespaced, name, XMLNS, local, value)?;
        }
        start = rest.trim_start_matches(source::xml_space);
        if start.starts_with(['/', '>']) {
            break;
        }
    }
    vm.set_host_slot(object.clone(), ATTRIBUTES, qualified)?;
    vm.set_host_slot(object.clone(), ATTRIBUTES_NS, namespaced)?;
    Ok(name)
}

fn ns_key(namespace: &str, local: &str) -> String {
    // XML names/namespace URIs cannot contain NUL.
    format!("{namespace}\0{local}")
}

fn put_attribute(
    vm: &mut Vm,
    qualified: &Value,
    namespaced: &Value,
    name: &str,
    namespace: &str,
    local: &str,
    value: &str,
) -> Result<(), JsError> {
    vm.charge_work(name.len() + namespace.len() + local.len())?;
    let value = vm.finish_string(value.encode_utf16().collect())?;
    vm.define_data_property(qualified.clone(), name, value.clone(), false, false, false)?;
    vm.define_data_property(
        namespaced.clone(),
        &ns_key(namespace, local),
        value,
        false,
        false,
        false,
    )
}

fn require_node(vm: &Vm, node: &Value) -> Result<(), JsError> {
    vm.host_data::<XmlNode>(node)
        .map(|_| ())
        .ok_or_else(|| JsError::type_error("Illegal XML node receiver"))
}

fn prototype(vm: &mut Vm, name: &str) -> Result<Value, JsError> {
    let constructor = vm.get_global(name)?;
    vm.get_property(constructor, "prototype")
}

fn slot(vm: &Vm, object: &Value, slot: u32) -> Value {
    vm.host_slot(object, slot).unwrap_or(Value::Null)
}

fn put_text(vm: &mut Vm, node: &Value, slot: u32, text: &str) -> Result<(), JsError> {
    let text = vm.finish_string(text.encode_utf16().collect())?;
    vm.set_host_slot(node.clone(), slot, text)
}

fn install_getter(vm: &mut Vm, proto: &Value, name: &str, id: u32) -> Result<(), JsError> {
    getter(vm, proto.clone(), name, move |vm, node, _| {
        require_node(vm, &node)?;
        Ok(slot(vm, &node, id))
    })
}

fn get_attribute(vm: &mut Vm, node: &Value, id: u32, name: &str) -> Result<Value, JsError> {
    let table = slot(vm, node, id);
    if table == Value::Null {
        return Err(JsError::type_error("Illegal XML element receiver"));
    }
    let value = vm.get_property(table, name)?;
    Ok(if value.is_undefined() {
        Value::Null
    } else {
        value
    })
}

fn position_error() -> JsError {
    JsError::type_error("Invalid XML source position")
}

fn string_arg(vm: &mut Vm, args: &[Value], index: usize) -> Result<Option<String>, JsError> {
    let value = vm.to_string(&arg(args, index))?;
    vm.charge_work(value.len())?;
    // XML names cannot contain a lone surrogate. A DOMString with one
    // must not accidentally match an actual U+FFFD in the parsed XML.
    Ok(String::from_utf16(value.as_utf16()).ok())
}

fn install_node(vm: &mut Vm, proto: &Value, child_list: &Value) -> Result<(), JsError> {
    for (name, id) in [
        ("nodeName", NAME),
        ("nodeType", KIND),
        ("nodeValue", DATA),
        ("parentNode", PARENT),
        ("ownerDocument", OWNER),
        ("firstChild", FIRST),
        ("lastChild", LAST),
        ("nextSibling", NEXT),
        ("previousSibling", PREVIOUS),
    ] {
        install_getter(vm, proto, name, id)?;
    }
    getter(vm, proto.clone(), "parentElement", |vm, node, _| {
        require_node(vm, &node)?;
        let parent = slot(vm, &node, PARENT);
        Ok(if slot(vm, &parent, KIND).as_number() == Some(1.0) {
            parent
        } else {
            Value::Null
        })
    })?;
    getter(vm, proto.clone(), "textContent", |vm, node, _| {
        require_node(vm, &node)?;
        match slot(vm, &node, KIND).as_number() {
            Some(1.0) => {
                let mut units = Vec::new();
                let mut pending = vec![slot(vm, &node, FIRST)];
                while let Some(child) = pending.pop() {
                    if child == Value::Null {
                        continue;
                    }
                    vm.charge_work(1)?;
                    pending.push(slot(vm, &child, NEXT));
                    match slot(vm, &child, KIND).as_number() {
                        Some(1.0) => pending.push(slot(vm, &child, FIRST)),
                        Some(3.0) => {
                            if let Value::String(text) = slot(vm, &child, DATA) {
                                vm.charge_work(text.len())?;
                                units.extend_from_slice(text.as_utf16());
                            }
                        }
                        _ => {}
                    }
                }
                vm.finish_string(units)
            }
            _ => Ok(slot(vm, &node, DATA)),
        }
    })?;
    let child_list = child_list.clone();
    getter(vm, proto.clone(), "childNodes", move |vm, node, _| {
        require_node(vm, &node)?;
        if let Some(list) = vm.host_slot(&node, CHILDREN) {
            return Ok(list);
        }
        let mut children = Vec::new();
        let mut child = slot(vm, &node, FIRST);
        while child != Value::Null {
            vm.charge_work(1)?;
            children.push(child.clone());
            child = slot(vm, &child, NEXT);
        }
        let list = vm.new_object_with_prototype(child_list.clone())?;
        vm.mark_platform_object(&list)?;
        // The native index provider may not run JS. A hidden array traces
        // every retained node and accounts for the backing in the VM heap.
        let trace = vm.try_new_array(&children)?;
        vm.set_host_slot(list.clone(), 0, trace)?;
        let children = Rc::new(Children(children));
        vm.set_host_data(list.clone(), children.clone())?;
        vm.set_host_indexed_properties(list.clone(), children)?;
        vm.set_host_slot(node, CHILDREN, list.clone())?;
        Ok(list)
    })
}

fn install_child_nodes(vm: &mut Vm) -> Result<Value, JsError> {
    let proto = prototype(vm, "NodeList")?;
    let proto = vm.new_object_with_prototype(proto)?;
    vm.root(proto.clone());
    let children = |vm: &Vm, list: &Value| {
        vm.host_data::<Children>(list)
            .ok_or_else(|| JsError::type_error("Illegal XML NodeList receiver"))
    };
    getter(vm, proto.clone(), "length", move |vm, list, _| {
        Ok(children(vm, &list)?.0.len().into())
    })?;
    webidl::operation(vm, proto.clone(), "item", 1, move |vm, list, args| {
        let children = children(vm, &list)?;
        webidl::required(args, 1, "NodeList.item")?;
        let n = vm.to_number(&arg(args, 0))?;
        let index = n.trunc().rem_euclid(4294967296.0) as usize;
        Ok(children.0.get(index).cloned().unwrap_or(Value::Null))
    })?;
    webidl::operation(vm, proto.clone(), "forEach", 1, move |vm, list, args| {
        let children = children(vm, &list)?;
        let callback = arg(args, 0);
        if !vm.is_callable(&callback) {
            return Err(JsError::type_error(
                "NodeList.forEach requires a callable callback",
            ));
        }
        for (index, node) in children.0.iter().enumerate() {
            vm.charge_work(1)?;
            vm.call(
                callback.clone(),
                arg(args, 1),
                &[node.clone(), index.into(), list.clone()],
            )?;
        }
        Ok(Value::Undefined)
    })?;
    Ok(proto)
}
