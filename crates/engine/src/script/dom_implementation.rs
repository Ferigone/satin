//! DOMImplementation objects and the independent documents they create.
use super::{
    NODE_LIMIT, Shared, arg, changed_bytes, document_node, getter, node, text, unsupported, webidl,
    wrap,
};
use crate::dom::{Document, DocumentKind, Node, NodeId, NodeKind};
use rust_browser_javascript::{JsError, Vm};
use std::rc::Rc;

const IMPLEMENTATION_SLOT: u32 = 0x4449_4d50;

struct Implementation;

fn element(doc: &mut Document, parent: NodeId, tag: &str) -> NodeId {
    doc.append(
        parent,
        NodeKind::Element {
            tag: tag.into(),
            attrs: Default::default(),
        },
    )
}

fn create_html(state: &Shared, title: Option<String>) -> Result<NodeId, JsError> {
    let mut state = state.borrow_mut();
    // Reserve the complete tree before mutating the shared arena.
    let count = if title.is_some() { 7 } else { 5 };
    if state.document.nodes.len().saturating_add(count) > NODE_LIMIT {
        return Err(unsupported("DOM node budget exceeded"));
    }
    let bytes = 2 * "html".len() + "head".len() + "body".len() + title.as_ref().map_or(0, |v| "title".len() + v.len());
    let next_bytes = changed_bytes(&state, 0, bytes)?;
    let doc = &mut state.document;
    let root = NodeId(doc.nodes.len());
    doc.nodes.push(Node {
        kind: NodeKind::Document,
        parent: None,
        children: vec![],
    });
    doc.other_documents.insert(root, DocumentKind::Html);
    doc.append(
        root,
        NodeKind::Doctype {
            name: "html".into(),
            public_id: None,
            system_id: None,
        },
    );
    let html = element(doc, root, "html");
    let head = element(doc, html, "head");
    if let Some(title) = title {
        let title_element = element(doc, head, "title");
        doc.append(title_element, NodeKind::Text(title));
    }
    element(doc, html, "body");
    state.dom_bytes = next_bytes;
    Ok(root)
}

pub(super) fn install(vm: &mut Vm, state: &Shared) -> Result<(), JsError> {
    let proto = vm.new_object();
    webidl::interface(vm, proto.clone(), "DOMImplementation", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal DOMImplementation constructor"))
    })?;
    vm.root(proto.clone());
    let doc_proto = state.borrow().document_prototype.clone();
    let host = state.clone();
    let implementation_proto = proto.clone();
    getter(
        vm,
        doc_proto.clone(),
        "implementation",
        move |vm, this, _| {
            document_node(&host, &this)?;
            if let Some(value) = vm.host_slot(&this, IMPLEMENTATION_SLOT) {
                return Ok(value);
            }
            let value = vm.new_object_with_prototype(implementation_proto.clone())?;
            vm.mark_platform_object(&value)?;
            vm.set_host_data(value.clone(), Rc::new(Implementation))?;
            vm.set_host_slot(this, IMPLEMENTATION_SLOT, value.clone())?;
            Ok(value)
        },
    )?;
    let host = state.clone();
    webidl::operation(vm, proto, "createHTMLDocument", 0, move |vm, this, args| {
        vm.host_data::<Implementation>(&this)
            .ok_or_else(|| JsError::type_error("Expected a DOMImplementation receiver"))?;
        let value = arg(args, 0);
        let title = if value.is_undefined() {
            None
        } else {
            Some(text(vm, value)?)
        };
        vm.charge_work(5 + title.as_ref().map_or(0, |value| value.len() + 2))?;
        let id = create_html(&host, title)?;
        wrap(vm, &host, Some(id))
    })?;

    let html_proto = state.borrow().html_document_prototype.clone();
    let doctype_proto = state.borrow().doctype_prototype.clone();
    for (name, prototype, parent) in [
        ("HTMLDocument", html_proto, "Document"),
        ("DocumentType", doctype_proto.clone(), "Node"),
    ] {
        let constructor = webidl::interface(vm, prototype, name, 0, move |_, _, _, _| {
            Err(JsError::type_error(format!("Illegal {name} constructor")))
        })?;
        let parent = vm.get_global(parent)?;
        vm.set_prototype(
            constructor.as_object().expect("DOM interface constructor"),
            parent.as_object(),
        )?;
    }
    let host = state.clone();
    getter(vm, doc_proto, "doctype", move |vm, this, _| {
        let root = document_node(&host, &this)?;
        let id = {
            let state = host.borrow();
            let doc = &state.document;
            doc.nodes[root.0]
                .children
                .iter()
                .copied()
                .find(|id| matches!(doc.nodes[id.0].kind, NodeKind::Doctype { .. }))
        };
        wrap(vm, &host, id)
    })?;
    for key in ["name", "publicId", "systemId"] {
        let host = state.clone();
        getter(vm, doctype_proto.clone(), key, move |_, this, _| {
            let id = node(&host, &this)?;
            let state = host.borrow();
            let NodeKind::Doctype {
                name,
                public_id,
                system_id,
            } = &state.document.nodes[id.0].kind
            else {
                return Err(JsError::type_error("Expected a DocumentType receiver"));
            };
            Ok(match key {
                "name" => name.as_str(),
                "publicId" => public_id.as_deref().unwrap_or(""),
                _ => system_id.as_deref().unwrap_or(""),
            }
            .into())
        })?;
    }
    Ok(())
}
