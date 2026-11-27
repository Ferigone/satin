//! Live indexed Element.attributes reflection over the DOM attribute list.
//! Attr identity is retained while attached; removing an attribute detaches
//! the existing object instead of retargeting it to a later insertion.
use super::*;
use crate::dom::AttributeNamespace;
use rust_browser_javascript::HostIndexedProperties;
use std::{cell::Cell, rc::Weak};

const MAP_SLOT: u32 = 0x4154_5400;
const DETACHED_VALUE_SLOT: u32 = 0x4154_5401;
const RETAINED_ATTR_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Cache {
    entries: HashMap<(NodeId, String), Entry>,
    resident: Rc<Cell<usize>>,
}
struct Entry {
    value: Value,
    root: RootId,
    attr: Rc<Attr>,
}
struct Attr {
    state: Weak<RefCell<State>>,
    owner: Cell<Option<NodeId>>,
    document: Cell<NodeId>,
    name: String,
    namespace: Option<AttributeNamespace>,
    resident: Rc<Cell<usize>>,
    bytes: usize,
}
impl Drop for Attr {
    fn drop(&mut self) {
        self.resident.set(self.resident.get().saturating_sub(self.bytes));
    }
}
struct Map {
    state: Weak<RefCell<State>>,
    owner: NodeId,
    attr_prototype: Value,
}
fn document_gone() -> JsError {
    JsError::type_error("Attribute document is gone")
}

fn attr_value(
    vm: &mut Vm,
    state: &Shared,
    owner: NodeId,
    name: &str,
    prototype: &Value,
) -> Result<Value, JsError> {
    vm.charge_work(name.len())?;
    let (namespace, document, resident, bytes) = {
        let state = state.borrow();
        if state.document.attr(owner, name).is_none() {
            return Ok(Value::Null);
        }
        if let Some(entry) = state.attribute_nodes.entries.get(&(owner, name.to_owned())) {
            return Ok(entry.value.clone());
        }
        let resident = state.attribute_nodes.resident.clone();
        let bytes = 256usize.saturating_add(name.len().saturating_mul(2));
        if resident.get().saturating_add(bytes) > RETAINED_ATTR_BYTES {
            return Err(JsError::new(ErrorKind::HeapLimit, "Retained Attr budget exceeded"));
        }
        (state.document.attribute_namespace(owner, name),
         state.document.node_document(owner), resident, bytes)
    };
    resident.set(resident.get() + bytes);
    let attr = Rc::new(Attr {
        state: Rc::downgrade(state),
        owner: Cell::new(Some(owner)),
        document: Cell::new(document),
        name: name.to_owned(),
        namespace,
        resident,
        bytes,
    });
    let value = vm.new_object_with_prototype(prototype.clone())?;
    vm.mark_platform_object(&value)?;
    vm.set_host_data(value.clone(), attr.clone())?;
    let root = vm.root(value.clone());
    state.borrow_mut().attribute_nodes.entries.insert(
        (owner, name.to_owned()),
        Entry { value: value.clone(), root, attr },
    );
    Ok(value)
}

// Called after DOM byte-budget preflight, before removing the attribute.
pub(super) fn detach(vm: &mut Vm, state: &mut State, owner: NodeId, name: &str) -> Result<(), JsError> {
    let key = (owner, name.to_owned());
    if let Some(entry) = state.attribute_nodes.entries.get(&key) {
        let old = state.document.attr(owner, name).unwrap_or("");
        vm.set_host_slot(entry.value.clone(), DETACHED_VALUE_SLOT, Value::from(old))?;
    }
    if let Some(entry) = state.attribute_nodes.entries.remove(&key) {
        entry.attr.document.set(state.document.node_document(owner));
        entry.attr.owner.set(None);
        vm.unroot(entry.root);
    }
    Ok(())
}

impl Map {
    fn state(&self) -> Result<Shared, JsError> {
        self.state.upgrade().ok_or_else(document_gone)
    }
    fn item(&self, vm: &mut Vm, index: usize) -> Result<Option<Value>, JsError> {
        let state = self.state()?;
        let name = {
            let state = state.borrow();
            let NodeKind::Element { attrs, .. } = &state.document.nodes[self.owner.0].kind else {
                return Ok(None);
            };
            attrs.get_index(index).map(|(name, _)| name.clone())
        };
        name.map(|name| attr_value(vm, &state, self.owner, &name, &self.attr_prototype)).transpose()
    }
    fn named(&self, vm: &mut Vm, name: String) -> Result<Value, JsError> {
        let state = self.state()?;
        let name = if state.borrow().document.html_element_in_html_document(self.owner) {
            name.to_ascii_lowercase()
        } else {
            name
        };
        attr_value(vm, &state, self.owner, &name, &self.attr_prototype)
    }
    fn named_ns(&self, vm: &mut Vm, namespace: Option<String>, local: String) -> Result<Value, JsError> {
        let state = self.state()?;
        let name = {
            let state = state.borrow();
            let doc = &state.document;
            let NodeKind::Element { attrs, .. } = &doc.nodes[self.owner.0].kind else {
                return Ok(Value::Null);
            };
            let mut result = None;
            for name in attrs.keys() {
                vm.charge_work(name.len().saturating_add(1))?;
                if doc.attribute_namespace(self.owner, name).map(AttributeNamespace::uri) == namespace.as_deref()
                    && doc.attribute_local_name(self.owner, name) == local {
                    result = Some(name.clone());
                    break;
                }
            }
            result
        };
        match name {
            Some(name) => attr_value(vm, &state, self.owner, &name, &self.attr_prototype),
            None => Ok(Value::Null),
        }
    }
}
impl HostIndexedProperties for Map {
    fn indexed_len(&self) -> Result<usize, JsError> {
        let state = self.state()?;
        let state = state.borrow();
        match &state.document.nodes[self.owner.0].kind {
            NodeKind::Element { attrs, .. } => Ok(attrs.len()),
            _ => Ok(0),
        }
    }
    fn indexed_get(&self, vm: &mut Vm, index: u32) -> Result<Option<Value>, JsError> {
        self.item(vm, index as usize)
    }
}
fn map_receiver(vm: &Vm, this: &Value) -> Result<Rc<Map>, JsError> {
    vm.host_data::<Map>(this).ok_or_else(|| JsError::type_error("Illegal NamedNodeMap receiver"))
}
fn attr_receiver(vm: &Vm, this: &Value) -> Result<Rc<Attr>, JsError> {
    vm.host_data::<Attr>(this).ok_or_else(|| JsError::type_error("Illegal Attr receiver"))
}
fn read_value(vm: &mut Vm, this: &Value, attr: &Attr) -> Result<Value, JsError> {
    if let Some(owner) = attr.owner.get() {
        let state = attr.state.upgrade().ok_or_else(document_gone)?;
        let state = state.borrow();
        let value = state.document.attr(owner, &attr.name).unwrap_or("");
        vm.charge_work(value.len())?;
        Ok(value.into())
    } else {
        Ok(vm.host_slot(this, DETACHED_VALUE_SLOT).unwrap_or_else(|| "".into()))
    }
}
fn write_value(vm: &mut Vm, this: Value, attr: &Attr, value: String) -> Result<Value, JsError> {
    if let Some(owner) = attr.owner.get() {
        let state = attr.state.upgrade().ok_or_else(document_gone)?;
        set_attribute(vm, &state, owner, &attr.name, Some(value))?;
    } else {
        vm.set_host_slot(this, DETACHED_VALUE_SLOT, value.into())?;
    }
    Ok(Value::Undefined)
}
fn namespace_arg(vm: &mut Vm, value: Value) -> Result<Option<String>, JsError> {
    if value.is_nullish() {
        return Ok(None);
    }
    let value = text(vm, value)?;
    Ok((!value.is_empty()).then_some(value))
}

pub(super) fn install(vm: &mut Vm, state: &Shared) -> Result<(), JsError> {
    let node_prototype = state.borrow().prototype.clone();
    let element_prototype = state.borrow().element_prototype.clone();
    let attr_prototype = vm.new_object_with_prototype(node_prototype)?;
    let attr_constructor = webidl::interface(vm, attr_prototype.clone(), "Attr", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal Attr constructor"))
    })?;
    let object = vm.get_global("Object")?;
    let inherit = vm.get_property(object, "setPrototypeOf")?;
    let node_constructor = vm.get_global("Node")?;
    vm.call(inherit, Value::Undefined, &[attr_constructor, node_constructor])?;
    vm.root(attr_prototype.clone());
    for property in ["name", "nodeName", "localName", "prefix", "namespaceURI", "ownerElement",
        "ownerDocument", "specified", "nodeType", "isConnected", "parentNode", "parentElement",
        "firstChild", "lastChild", "previousSibling", "nextSibling"] {
        getter(vm, attr_prototype.clone(), property, move |vm, this, _| {
            let attr = attr_receiver(vm, &this)?;
            let owner = attr.owner.get();
            let state = attr.state.upgrade().ok_or_else(document_gone)?;
            Ok(match property {
                "name" | "nodeName" => { vm.charge_work(attr.name.len())?; attr.name.as_str().into() }
                "localName" => {
                    let name = if attr.namespace.is_some() {
                        attr.name.rsplit_once(':').map_or(attr.name.as_str(), |(_, local)| local)
                    } else { attr.name.as_str() };
                    vm.charge_work(name.len())?;
                    name.into()
                }
                "prefix" => {
                    let prefix = attr.namespace.and_then(|_| attr.name.split_once(':').map(|(prefix, _)| prefix));
                    vm.charge_work(prefix.map_or(0, str::len))?;
                    prefix.map(Value::from).unwrap_or(Value::Null)
                }
                "namespaceURI" => attr.namespace.map(|ns| Value::from(ns.uri())).unwrap_or(Value::Null),
                "ownerElement" => return wrap(vm, &state, owner),
                "ownerDocument" => {
                    let document = owner.map_or(attr.document.get(), |id| state.borrow().document.node_document(id));
                    return wrap(vm, &state, Some(document));
                }
                "specified" => true.into(),
                "nodeType" => 2.into(),
                "isConnected" => false.into(),
                _ => Value::Null,
            })
        })?;
    }
    for property in ["value", "nodeValue", "textContent"] {
        let get = vm.new_native_function(&format!("get {property}"), |vm, this, _| {
            let attr = attr_receiver(vm, &this)?;
            read_value(vm, &this, &attr)
        });
        let set = vm.new_native_function(&format!("set {property}"), move |vm, this, args| {
            let attr = attr_receiver(vm, &this)?;
            let value = arg(args, 0);
            let value = if property != "value" && value.is_nullish() { String::new() } else { text(vm, value)? };
            write_value(vm, this, &attr, value)
        });
        vm.define_accessor(attr_prototype.clone(), property, Some(get), Some(set))?;
    }
    let map_prototype = vm.new_object();
    webidl::interface(vm, map_prototype.clone(), "NamedNodeMap", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal NamedNodeMap constructor"))
    })?;
    vm.root(map_prototype.clone());
    getter(vm, map_prototype.clone(), "length", |vm, this, _| {
        Ok((map_receiver(vm, &this)?.indexed_len()? as f64).into())
    })?;
    webidl::operation(vm, map_prototype.clone(), "item", 1, |vm, this, args| {
        webidl::required(args, 1, "NamedNodeMap.item")?;
        let map = map_receiver(vm, &this)?;
        let n = vm.to_number(&args[0])?.trunc();
        let index = if n.is_finite() { n.rem_euclid(4294967296.) as usize } else { 0 };
        Ok(map.item(vm, index)?.unwrap_or(Value::Null))
    })?;
    webidl::operation(vm, map_prototype.clone(), "getNamedItem", 1, |vm, this, args| {
        webidl::required(args, 1, "NamedNodeMap.getNamedItem")?;
        let map = map_receiver(vm, &this)?;
        let name = text(vm, args[0].clone())?;
        map.named(vm, name)
    })?;
    webidl::operation(vm, map_prototype.clone(), "getNamedItemNS", 2, |vm, this, args| {
        webidl::required(args, 2, "NamedNodeMap.getNamedItemNS")?;
        let map = map_receiver(vm, &this)?;
        let namespace = namespace_arg(vm, args[0].clone())?;
        let local = text(vm, args[1].clone())?;
        map.named_ns(vm, namespace, local)
    })?;
    let host = state.clone();
    let proto = attr_prototype.clone();
    getter(vm, element_prototype.clone(), "attributes", move |vm, this, _| {
        let owner = element(&host, &this)?;
        if let Some(value) = vm.host_slot(&this, MAP_SLOT) {
            return Ok(value);
        }
        let map = Rc::new(Map { state: Rc::downgrade(&host), owner, attr_prototype: proto.clone() });
        let value = vm.new_object_with_prototype(map_prototype.clone())?;
        vm.mark_platform_object(&value)?;
        vm.set_host_data(value.clone(), map.clone())?;
        vm.set_host_indexed_properties(value.clone(), map)?;
        vm.set_host_slot(this, MAP_SLOT, value.clone())?;
        Ok(value)
    })?;
    for name in ["getAttributeNode", "getAttributeNodeNS"] {
        let host = state.clone();
        let proto = attr_prototype.clone();
        webidl::operation(vm, element_prototype.clone(), name, if name == "getAttributeNode" { 1 } else { 2 },
            move |vm, this, args| {
                let owner = element(&host, &this)?;
                let map = Map { state: Rc::downgrade(&host), owner, attr_prototype: proto.clone() };
                if name == "getAttributeNode" {
                    webidl::required(args, 1, "Element.getAttributeNode")?;
                    let name = text(vm, args[0].clone())?;
                    map.named(vm, name)
                } else {
                    webidl::required(args, 2, "Element.getAttributeNodeNS")?;
                    let namespace = namespace_arg(vm, args[0].clone())?;
                    let local = text(vm, args[1].clone())?;
                    map.named_ns(vm, namespace, local)
                }
            })?;
    }
    Ok(())
}
