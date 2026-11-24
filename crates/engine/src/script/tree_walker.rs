//! Live forward tree traversal, including detached template contents used by Lit.
use super::{Shared, arg, dom_api::Errors, getter, node, webidl, wrap};
use crate::dom::{Document, NodeId, NodeKind};
use rust_browser_javascript::{JsError, Value, Vm};
use std::{cell::Cell, rc::Rc};

const FILTER_SLOT: u32 = 0x5746_4c54;
const ACCEPT: u16 = 1;
const REJECT: u16 = 2;
const SKIP: u16 = 3;

struct Walker {
    state: Shared,
    root: NodeId,
    current: Cell<NodeId>,
    mask: u32,
    active: Cell<bool>,
    // Sequential visits to a wide list should not rescan all preceding siblings.
    // Validate the hint against the live tree after any author mutation.
    sibling_hint: Cell<Option<usize>>,
}

fn receiver(vm: &Vm, object: &Value) -> Result<Rc<Walker>, JsError> {
    vm.host_data(object)
        .ok_or_else(|| JsError::type_error("Illegal TreeWalker receiver"))
}

fn node_mask(document: &Document, id: NodeId) -> u32 {
    match document.nodes[id.0].kind {
        NodeKind::Element { .. } => 0x1,
        NodeKind::Text(_) => 0x4,
        NodeKind::Comment(_) => 0x80,
        NodeKind::Document if document.is_document(id) => 0x100,
        NodeKind::Doctype { .. } => 0x200,
        NodeKind::Document => 0x400,
    }
}

impl Walker {
    fn filter(
        &self,
        vm: &mut Vm,
        id: NodeId,
        filter: &Value,
        errors: &Errors,
    ) -> Result<u16, JsError> {
        if self.active.get() {
            return Err(errors.exception(
                vm,
                "InvalidStateError",
                "The node filter is already active",
            ));
        }
        if node_mask(&self.state.borrow().document, id) & self.mask == 0 {
            return Ok(SKIP);
        }
        if filter.is_nullish() {
            return Ok(ACCEPT);
        }
        self.active.set(true);
        let result = (|| {
            let value = wrap(vm, &self.state, Some(id))?;
            let (callable, this) = if vm.is_callable(filter) {
                (filter.clone(), Value::Undefined)
            } else {
                (
                    vm.get_property(filter.clone(), "acceptNode")?,
                    filter.clone(),
                )
            };
            let result = vm.call(callable, this, &[value])?;
            Ok(vm.to_number(&result)?.trunc().rem_euclid(65536.) as u16)
        })();
        self.active.set(false);
        result
    }

    fn following_sibling_or_ancestor_sibling(
        &self,
        vm: &mut Vm,
        mut id: NodeId,
    ) -> Result<Option<NodeId>, JsError> {
        let state = self.state.borrow();
        while id != self.root {
            vm.charge_work(1)?;
            let Some(parent) = state.document.nodes[id.0].parent else {
                break;
            };
            let children = &state.document.nodes[parent.0].children;
            if children.last() != Some(&id) {
                let index = if let Some(hint) = self.sibling_hint.get()
                    && children.get(hint) == Some(&id)
                {
                    hint
                } else {
                    let Some(index) = children.iter().position(|&child| child == id) else {
                        return Ok(None);
                    };
                    vm.charge_work(index + 1)?;
                    index
                };
                self.sibling_hint.set(Some(index + 1));
                return Ok(children.get(index + 1).copied());
            }
            id = parent;
        }
        Ok(None)
    }

    fn next(&self, vm: &mut Vm, filter: &Value, errors: &Errors) -> Result<Value, JsError> {
        let mut id = self.current.get();
        let mut verdict = ACCEPT;
        loop {
            vm.charge_work(1)?;
            // Re-read parent and child relationships after every filter callback.
            let child = if verdict == REJECT {
                None
            } else {
                self.state.borrow().document.nodes[id.0]
                    .children
                    .first()
                    .copied()
            };
            id = if let Some(child) = child {
                child
            } else if let Some(sibling) = self.following_sibling_or_ancestor_sibling(vm, id)? {
                sibling
            } else {
                return Ok(Value::Null);
            };
            verdict = self.filter(vm, id, filter, errors)?;
            if verdict == ACCEPT {
                let value = wrap(vm, &self.state, Some(id))?;
                self.current.set(id);
                return Ok(value);
            }
        }
    }
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    document: &Value,
    errors: &Errors,
) -> Result<(), JsError> {
    let constants = vm.new_native_function("NodeFilter", |_, _, _| {
        Err(JsError::type_error("Illegal NodeFilter invocation"))
    });
    for (name, value) in [
        ("FILTER_ACCEPT", 1),
        ("FILTER_REJECT", 2),
        ("FILTER_SKIP", 3),
        ("SHOW_ALL", u32::MAX),
        ("SHOW_ELEMENT", 1),
        ("SHOW_ATTRIBUTE", 2),
        ("SHOW_TEXT", 4),
        ("SHOW_CDATA_SECTION", 8),
        ("SHOW_ENTITY_REFERENCE", 16),
        ("SHOW_ENTITY", 32),
        ("SHOW_PROCESSING_INSTRUCTION", 64),
        ("SHOW_COMMENT", 128),
        ("SHOW_DOCUMENT", 256),
        ("SHOW_DOCUMENT_TYPE", 512),
        ("SHOW_DOCUMENT_FRAGMENT", 1024),
        ("SHOW_NOTATION", 2048),
    ] {
        vm.define_data_property(
            constants.clone(),
            name,
            Value::Number(value as f64),
            false,
            true,
            false,
        )?;
    }
    vm.set_global("NodeFilter", constants)?;
    let proto = vm.new_object();
    webidl::interface(vm, proto.clone(), "TreeWalker", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal TreeWalker constructor"))
    })?;
    vm.root(proto.clone());
    getter(vm, proto.clone(), "root", |vm, object, _| {
        let walker = receiver(vm, &object)?;
        wrap(vm, &walker.state, Some(walker.root))
    })?;
    getter(vm, proto.clone(), "whatToShow", |vm, object, _| {
        Ok(Value::Number(receiver(vm, &object)?.mask as f64))
    })?;
    getter(vm, proto.clone(), "filter", |vm, object, _| {
        receiver(vm, &object)?;
        Ok(vm.host_slot(&object, FILTER_SLOT).unwrap_or(Value::Null))
    })?;
    let get = vm.new_native_function("get currentNode", |vm, object, _| {
        let walker = receiver(vm, &object)?;
        wrap(vm, &walker.state, Some(walker.current.get()))
    });
    let set = vm.new_native_function("set currentNode", |vm, object, args| {
        let walker = receiver(vm, &object)?;
        walker.current.set(node(&walker.state, &arg(args, 0))?);
        walker.sibling_hint.set(None);
        Ok(Value::Undefined)
    });
    vm.define_data_property(set.clone(), "length", 1.into(), false, false, true)?;
    vm.define_accessor(proto.clone(), "currentNode", Some(get), Some(set))?;
    let errors = errors.clone();
    webidl::operation(vm, proto.clone(), "nextNode", 0, move |vm, object, _| {
        let walker = receiver(vm, &object)?;
        let filter = vm.host_slot(&object, FILTER_SLOT).unwrap_or(Value::Null);
        walker.next(vm, &filter, &errors)
    })?;
    let state = state.clone();
    webidl::operation(
        vm,
        document.clone(),
        "createTreeWalker",
        1,
        move |vm, this, args| {
            super::document_node(&state, &this)?;
            webidl::required(args, 1, "createTreeWalker")?;
            let root = node(&state, &args[0])?;
            let mask = match args.get(1) {
                Some(value) if !value.is_undefined() => {
                    vm.to_number(value)?.trunc().rem_euclid(4294967296.) as u32
                }
                _ => u32::MAX,
            };
            let filter = arg(args, 2);
            if !filter.is_nullish() && filter.as_object().is_none() {
                return Err(JsError::type_error("NodeFilter must be an object"));
            }
            let object = vm.new_object_with_prototype(proto.clone())?;
            if !filter.is_nullish() {
                vm.set_host_slot(object.clone(), FILTER_SLOT, filter)?;
            }
            vm.set_host_data(
                object.clone(),
                Rc::new(Walker {
                    state: state.clone(),
                    root,
                    current: Cell::new(root),
                    mask,
                    active: Cell::new(false),
                    sibling_hint: Cell::new(None),
                }),
            )?;
            Ok(object)
        },
    )?;
    Ok(())
}
