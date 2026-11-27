//! html5ever's tree sink writes directly into Satin's DOM arena.
use super::{FragmentContext, tree_builder::{PendingElement, Tree}};
use crate::dom::mutation::Mutation;
use crate::dom::{AttributeNamespace, Attributes, Document, Namespace, NodeId, NodeKind, QuirksMode};
use html5ever::{
    Attribute, LocalName, ParseOpts, QualName,
    tendril::{StrTendril, TendrilSink},
    tree_builder::{ElemName, ElementFlags, NodeOrText, QuirksMode as ServoQuirks, TreeSink},
};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
};

#[derive(Clone, Debug)]
pub(super) struct Handle {
    original: Option<NodeId>,
    replacement: Option<Rc<Cell<NodeId>>>,
    name: Option<QualName>,
    annotation_xml_integration: bool,
}

impl Handle {
    fn node(id: Option<NodeId>) -> Self {
        Self { original: id, replacement: None, name: None, annotation_xml_integration: false }
    }

    pub(super) fn id(&self) -> Option<NodeId> {
        self.replacement.as_ref().map(|replacement| replacement.get()).or(self.original)
    }
}

#[derive(Debug)]
pub(super) struct Name<'a>(&'a QualName);

impl ElemName for Name<'_> {
    fn ns(&self) -> &html5ever::Namespace {
        &self.0.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.0.local
    }
}

pub(super) struct Sink {
    pub(super) tree: RefCell<Tree>,
    pub(super) closed: Cell<Option<NodeId>>,
    pending_custom: RefCell<Option<Handle>>,
    template_fragments: RefCell<HashSet<NodeId>>,
}

impl Sink {
    pub(super) fn new(scripting: bool) -> Self {
        Self {
            tree: RefCell::new(Tree::new(scripting)),
            closed: Cell::new(None),
            pending_custom: RefCell::new(None),
            template_fragments: RefCell::new(HashSet::new()),
        }
    }

    fn create(&self, kind: NodeKind) -> Handle {
        let mut tree = self.tree.borrow_mut();
        let id = tree.create(kind);
        Handle::node((!tree.halted).then_some(id))
    }

    fn insert(&self, tree: &mut Tree, parent: NodeId, before: Option<NodeId>, child: NodeOrText<Handle>) {
        match child {
            NodeOrText::AppendNode(handle) => {
                let Some(id) = handle.id() else { return };
                if handle.replacement.is_some() && self.defer_custom(tree, &handle, id, parent, before) {
                    return;
                }
                tree.attach(id, parent, before);
            }
            NodeOrText::AppendText(text) => tree.characters_at(parent, before, &text),
        }
    }

    fn defer_custom(&self, tree: &mut Tree, handle: &Handle, node: NodeId, parent: NodeId, before: Option<NodeId>) -> bool {
        if tree.halted || self.pending_custom.borrow().is_some() || tree.doc.nodes[node.0].parent.is_some() {
            return false;
        }
        let mut ancestor = Some(parent);
        while let Some(id) = ancestor {
            if self.template_fragments.borrow().contains(&id) {
                return false;
            }
            ancestor = tree.doc.nodes[id.0].parent;
        }
        let NodeKind::Element { attrs, .. } = &mut tree.doc.nodes[node.0].kind else {
            return false;
        };
        let attributes: Vec<_> = std::mem::take(attrs).into_iter().collect();
        tree.string_bytes -= attributes.iter().map(|(name, value)| name.len() + value.len()).sum::<usize>();
        tree.pending_element = Some(PendingElement { node, parent, before, attributes });
        *self.pending_custom.borrow_mut() = Some(handle.clone());
        true
    }

    pub(super) fn finish_custom(&self, original: NodeId, element: NodeId) {
        if let Some(handle) = self.pending_custom.borrow_mut().take()
            && handle.original == Some(original)
            && let Some(replacement) = handle.replacement
        {
            replacement.set(element);
        }
    }
}

impl TreeSink for Sink {
    type Handle = Handle;
    type Output = Document;
    type ElemName<'a> = Name<'a>;

    fn finish(self) -> Document {
        self.tree.into_inner().doc
    }

    fn parse_error(&self, _error: Cow<'static, str>) {}

    fn get_document(&self) -> Handle {
        Handle::node(Some(self.tree.borrow().doc.root))
    }

    fn elem_name<'a>(&'a self, target: &'a Handle) -> Name<'a> {
        Name(target.name.as_ref().expect("html5ever element handle"))
    }

    fn create_element(&self, name: QualName, attributes: Vec<Attribute>, flags: ElementFlags) -> Handle {
        let mut attrs = Attributes::new();
        let mut namespaces = Vec::new();
        for attribute in attributes {
            let qualified = attribute_name(&attribute);
            if let Some(namespace) = attribute_namespace(&attribute) {
                namespaces.push((qualified.clone(), namespace));
            }
            attrs.insert(qualified, attribute.value.to_string());
        }
        let tag = name.local.to_string();
        let namespace = match &*name.ns {
            "http://www.w3.org/1999/xhtml" => Namespace::Html,
            "http://www.w3.org/2000/svg" => Namespace::Svg,
            "http://www.w3.org/1998/Math/MathML" => Namespace::MathMl,
            _ => Namespace::Null,
        };
        let mut handle = self.create(NodeKind::Element { tag, attrs });
        handle.name = Some(name);
        handle.annotation_xml_integration = flags.mathml_annotation_xml_integration_point;
        if let Some(id) = handle.id() {
            let mut tree = self.tree.borrow_mut();
            if namespace == Namespace::Html
                && tree.custom_element_test.as_ref().is_some_and(|test| test(tree.doc.tag(id)))
            {
                handle.replacement = Some(Rc::new(Cell::new(id)));
            }
            if namespace != Namespace::Html {
                tree.doc.namespaces.insert(id, namespace);
            }
            let metadata_bytes = namespaces.iter().map(|(name, _)| name.len()).sum();
            if tree.charge_strings(metadata_bytes) {
                for (name, namespace) in namespaces {
                    tree.doc.attribute_namespaces.insert((id, name), namespace);
                }
            }
            if flags.template {
                let fragment = tree.create(NodeKind::Document);
                if !tree.halted {
                    tree.doc.template_contents.insert(id, fragment);
                    self.template_fragments.borrow_mut().insert(fragment);
                }
            }
        }
        handle
    }

    fn create_comment(&self, text: StrTendril) -> Handle {
        self.create(NodeKind::Comment(text.to_string()))
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> Handle {
        self.create(NodeKind::ProcessingInstruction {
            target: target.to_string(),
            data: data.to_string(),
        })
    }

    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        if let Some(parent) = parent.id() {
            self.insert(&mut self.tree.borrow_mut(), parent, None, child);
        }
    }

    fn append_based_on_parent_node(&self, element: &Handle, previous: &Handle, child: NodeOrText<Handle>) {
        let mut tree = self.tree.borrow_mut();
        let before = element.id().filter(|id| tree.doc.nodes[id.0].parent.is_some());
        if let Some(parent) = before.and_then(|id| tree.doc.nodes[id.0].parent).or(previous.id()) {
            self.insert(&mut tree, parent, before, child);
        }
    }

    fn append_doctype_to_document(&self, name: StrTendril, public_id: StrTendril, system_id: StrTendril) {
        let node = self.create(NodeKind::Doctype {
            name: name.to_string(),
            public_id: (!public_id.is_empty()).then(|| public_id.to_string()),
            system_id: (!system_id.is_empty()).then(|| system_id.to_string()),
        });
        self.append(&self.get_document(), NodeOrText::AppendNode(node));
    }

    fn get_template_contents(&self, target: &Handle) -> Handle {
        let tree = self.tree.borrow();
        Handle::node(target.id().and_then(|id| tree.doc.template_contents.get(&id).copied()))
    }

    fn pop(&self, handle: &Handle) {
        let Some(id) = handle.id() else { return };
        let tree = self.tree.borrow();
        if matches!(tree.doc.tag(id), "script" | "style" | "title") || self.closed.get().is_none() {
            self.closed.set(Some(id));
        }
    }

    fn same_node(&self, left: &Handle, right: &Handle) -> bool {
        left.id() == right.id()
    }

    fn set_quirks_mode(&self, mode: ServoQuirks) {
        self.tree.borrow_mut().doc.quirks_mode = match mode {
            ServoQuirks::NoQuirks => QuirksMode::NoQuirks,
            ServoQuirks::LimitedQuirks => QuirksMode::LimitedQuirks,
            ServoQuirks::Quirks => QuirksMode::Quirks,
        };
    }

    fn append_before_sibling(&self, sibling: &Handle, child: NodeOrText<Handle>) {
        let Some(id) = sibling.id() else { return };
        let mut tree = self.tree.borrow_mut();
        if let Some(parent) = tree.doc.nodes[id.0].parent {
            self.insert(&mut tree, parent, Some(id), child);
        }
    }

    fn add_attrs_if_missing(&self, target: &Handle, attributes: Vec<Attribute>) {
        let Some(id) = target.id() else { return };
        let mut tree = self.tree.borrow_mut();
        for attribute in attributes {
            let name = attribute_name(&attribute);
            let namespace = attribute_namespace(&attribute);
            if tree.doc.attr(id, &name).is_some() {
                continue;
            }
            let bytes = name.len() + attribute.value.len() + namespace.map_or(0, |_| name.len());
            if !tree.charge_strings(bytes) {
                return;
            }
            let Tree { doc, mutation_sink, .. } = &mut *tree;
            if let Some(sink) = mutation_sink {
                sink(doc, Mutation::Attribute { target: id, name: &name, namespace, old: None });
            }
            if let Some(namespace) = namespace {
                doc.attribute_namespaces.insert((id, name.clone()), namespace);
            }
            if let NodeKind::Element { attrs, .. } = &mut doc.nodes[id.0].kind {
                attrs.insert(name, attribute.value.to_string());
            }
        }
    }

    fn remove_from_parent(&self, target: &Handle) {
        if let Some(id) = target.id() {
            self.tree.borrow_mut().detach(id);
        }
    }

    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        let (Some(node), Some(new_parent)) = (node.id(), new_parent.id()) else { return };
        let mut tree = self.tree.borrow_mut();
        let children = tree.doc.nodes[node.0].children.clone();
        for child in children {
            tree.attach(child, new_parent, None);
            if tree.halted {
                break;
            }
        }
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &Handle) -> bool {
        handle.annotation_xml_integration
    }
}

fn attribute_name(attribute: &Attribute) -> String {
    attribute.name.prefix.as_ref().map_or_else(
        || attribute.name.local.to_string(),
        |prefix| format!("{prefix}:{}", attribute.name.local),
    )
}

fn attribute_namespace(attribute: &Attribute) -> Option<AttributeNamespace> {
    match &*attribute.name.ns {
        "http://www.w3.org/1999/xlink" => Some(AttributeNamespace::XLink),
        "http://www.w3.org/XML/1998/namespace" => Some(AttributeNamespace::Xml),
        "http://www.w3.org/2000/xmlns/" => Some(AttributeNamespace::Xmlns),
        _ => None,
    }
}

fn qual_name(tag: &str, namespace: Namespace) -> QualName {
    QualName::new(None, html5ever::Namespace::from(namespace.uri()), LocalName::from(tag))
}

pub(super) fn options(scripting: bool, quirks: QuirksMode) -> ParseOpts {
    let mut options = ParseOpts::default();
    options.tree_builder.scripting_enabled = scripting;
    options.tree_builder.quirks_mode = match quirks {
        QuirksMode::NoQuirks => ServoQuirks::NoQuirks,
        QuirksMode::LimitedQuirks => ServoQuirks::LimitedQuirks,
        QuirksMode::Quirks => ServoQuirks::Quirks,
    };
    options
}

pub(super) fn parse(normalized: &str, scripting: bool) -> Document {
    html5ever::parse_document(Sink::new(scripting), options(scripting, QuirksMode::NoQuirks)).one(normalized)
}

pub(super) fn parse_fragment(normalized: &str, context: FragmentContext<'_>) -> Document {
    let sink = Sink::new(context.scripting_enabled);
    sink.tree.borrow_mut().doc.quirks_mode = context.quirks_mode;
    let tag = if context.namespace == Namespace::Html {
        context.tag.to_ascii_lowercase()
    } else {
        context.tag.to_owned()
    };
    let attrs = context.attrs.into_iter().flatten().map(|(name, value)| Attribute {
        name: qual_name(name, Namespace::Null),
        value: StrTendril::from(value.as_str()),
    }).collect();
    let element = html5ever::tree_builder::create_element(&sink, qual_name(&tag, context.namespace), attrs);
    let form = (context.form_ancestor || context.namespace == Namespace::Html && tag == "form").then(|| {
        html5ever::tree_builder::create_element(&sink, qual_name("form", Namespace::Html), vec![])
    });
    let mut document = html5ever::driver::parse_fragment_for_element(
        sink,
        options(context.scripting_enabled, context.quirks_mode),
        element,
        context.scripting_enabled,
        form,
    ).one(normalized);
    if let [html] = document.nodes[document.root.0].children.as_slice() {
        let html = *html;
        let children = std::mem::take(&mut document.nodes[html.0].children);
        document.nodes[html.0].parent = None;
        for child in &children {
            document.nodes[child.0].parent = Some(document.root);
        }
        document.nodes[document.root.0].children = children;
    }
    document
}
