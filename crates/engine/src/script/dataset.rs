//! Live reflection between DOMStringMap and data-* attributes.
use super::{Shared, State, dom_api::Errors, element, getter, set_attribute, text, webidl};
use crate::dom::{Namespace, NodeId, NodeKind};
use rust_browser_javascript::{HostNamedProperties, JsError, JsString, Value, Vm};
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

const DATASET_SLOT: u32 = 677;

fn to_property(attribute: &str) -> Option<JsString> {
    let suffix = attribute.strip_prefix("data-")?;
    if suffix.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    let mut chars = suffix.chars().peekable();
    let mut name = String::with_capacity(suffix.len());
    while let Some(ch) = chars.next() {
        if ch == '-' && chars.peek().is_some_and(char::is_ascii_lowercase) {
            name.push(chars.next().unwrap().to_ascii_uppercase());
        } else {
            name.push(ch);
        }
    }
    Some(name.into())
}

fn to_attribute(name: &JsString) -> Option<String> {
    let units = name.as_utf16();
    if units
        .windows(2)
        .any(|pair| pair[0] == b'-' as u16 && (97..=122).contains(&pair[1]))
    {
        return None;
    }
    let mut attribute = String::from("data-");
    for ch in name.to_string_lossy().chars() {
        if ch.is_ascii_uppercase() {
            attribute.push('-');
            attribute.push(ch.to_ascii_lowercase());
        } else {
            attribute.push(ch);
        }
    }
    Some(attribute)
}

struct Dataset {
    owner: Weak<RefCell<State>>,
    node: NodeId,
    errors: Errors,
}

impl HostNamedProperties for Dataset {
    fn overrides_prototype(&self) -> bool {
        true
    }

    fn names(&self) -> Vec<JsString> {
        let Some(owner) = self.owner.upgrade() else {
            return Vec::new();
        };
        let owner = owner.borrow();
        let NodeKind::Element { attrs, .. } = &owner.document.nodes[self.node.0].kind else {
            return Vec::new();
        };
        attrs
            .keys()
            .filter(|name| {
                owner
                    .document
                    .attribute_namespace(self.node, name)
                    .is_none()
            })
            .filter_map(|name| to_property(name))
            .collect()
    }

    fn get(&self, name: &JsString) -> Option<Value> {
        let attribute = to_attribute(name)?;
        let owner = self.owner.upgrade()?;
        let owner = owner.borrow();
        if owner
            .document
            .attribute_namespace(self.node, &attribute)
            .is_some()
        {
            return None;
        }
        owner.document.attr(self.node, &attribute).map(Value::from)
    }

    fn set(&self, vm: &mut Vm, name: JsString, value: Value) -> Result<(), JsError> {
        let Some(owner) = self.owner.upgrade() else {
            return Ok(());
        };
        vm.charge_buffer_work(name.len())?;
        let Some(attribute) = to_attribute(&name) else {
            return Err(self.errors.exception(
                vm,
                "SyntaxError",
                "A dataset key cannot contain a hyphen followed by a lowercase ASCII letter",
            ));
        };
        if !attribute.chars().all(crate::dom::xml_names::name_char) {
            return Err(self.errors.exception(
                vm,
                "InvalidCharacterError",
                "Invalid dataset attribute name",
            ));
        }
        let value = text(vm, value)?;
        set_attribute(vm, &owner, self.node, &attribute, Some(value))
    }

    fn delete(&self, vm: &mut Vm, name: &JsString) -> Result<(), JsError> {
        let (Some(owner), Some(attribute)) = (self.owner.upgrade(), to_attribute(name)) else {
            return Ok(());
        };
        let present = {
            let doc = &owner.borrow().document;
            doc.attribute_namespace(self.node, &attribute).is_none()
                && doc.attr(self.node, &attribute).is_some()
        };
        if present {
            set_attribute(vm, &owner, self.node, &attribute, None)?;
        }
        Ok(())
    }
}

pub(super) fn install(vm: &mut Vm, owner: &Shared, errors: &Errors) -> Result<(), JsError> {
    let prototype = vm.new_object();
    webidl::interface(vm, prototype.clone(), "DOMStringMap", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal DOMStringMap constructor"))
    })?;
    let prototypes = {
        let state = owner.borrow();
        [
            state.html_element_prototype.clone(),
            state.svg_element_prototype.clone(),
        ]
    };
    for element_prototype in prototypes {
        let owner = owner.clone();
        let errors = errors.clone();
        let prototype = prototype.clone();
        getter(vm, element_prototype, "dataset", move |vm, this, _| {
            let node = element(&owner, &this)?;
            if !matches!(
                owner.borrow().document.namespace(node),
                Namespace::Html | Namespace::Svg
            ) {
                return Err(JsError::type_error("Illegal dataset receiver"));
            }
            if let Some(ready) = vm.host_slot(&this, DATASET_SLOT) {
                return Ok(ready);
            }
            let map = vm.new_object_with_prototype(prototype.clone())?;
            vm.mark_platform_object(&map)?;
            vm.set_host_named_properties(
                map.clone(),
                Rc::new(Dataset {
                    owner: Rc::downgrade(&owner),
                    node,
                    errors: errors.clone(),
                }),
            )?;
            vm.set_host_slot(this, DATASET_SLOT, map.clone())?;
            Ok(map)
        })?;
    }
    Ok(())
}
