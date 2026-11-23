use std::collections::BTreeMap;

#[path = "dom/mutation.rs"]
pub(crate) mod mutation;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub usize);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Namespace {
    #[default]
    Html,
    Svg,
    MathMl,
}

impl Namespace {
    pub fn uri(self) -> &'static str {
        match self {
            Self::Html => "http://www.w3.org/1999/xhtml",
            Self::Svg => "http://www.w3.org/2000/svg",
            Self::MathMl => "http://www.w3.org/1998/Math/MathML",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeNamespace {
    XLink,
    Xml,
    Xmlns,
}

impl AttributeNamespace {
    pub fn uri(self) -> &'static str {
        match self {
            Self::XLink => "http://www.w3.org/1999/xlink",
            Self::Xml => "http://www.w3.org/XML/1998/namespace",
            Self::Xmlns => "http://www.w3.org/2000/xmlns/",
        }
    }
}

#[derive(Clone, Debug)]
pub enum NodeKind {
    Document,
    Element {
        tag: String,
        attrs: BTreeMap<String, String>,
    },
    Text(String),
    Comment(String),
    Doctype {
        name: String,
        public_id: Option<String>,
        system_id: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuirksMode {
    #[default]
    NoQuirks,
    LimitedQuirks,
    Quirks,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub nodes: Vec<Node>,
    pub root: NodeId,
    pub truncated: bool,
    pub quirks_mode: QuirksMode,
    pub scripting_enabled: bool,
    /// Template element -> detached Document-kind fragment container. Its
    /// children are inert, and are not ordinary children of the template node.
    pub template_contents: BTreeMap<NodeId, NodeId>,
    /// Element namespace overrides. Missing entries mean HTML. Keys and values
    /// must be copied/remapped together with the element during arena imports.
    pub namespaces: BTreeMap<NodeId, Namespace>,
    /// Namespaced attributes, keyed by element and the qualified name stored in
    /// its attrs map. Unlisted attributes have no namespace, even with a colon.
    pub attribute_namespaces: BTreeMap<(NodeId, String), AttributeNamespace>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            nodes: vec![Node {
                kind: NodeKind::Document,
                parent: None,
                children: vec![],
            }],
            root: NodeId(0),
            truncated: false,
            quirks_mode: QuirksMode::NoQuirks,
            scripting_enabled: false,
            template_contents: BTreeMap::new(),
            namespaces: BTreeMap::new(),
            attribute_namespaces: BTreeMap::new(),
        }
    }
}

impl Document {
    /// Connected tree order; arena allocation order can retain detached nodes
    /// after HTML error recovery and later DOM mutations.
    pub fn connected_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        let mut pending = vec![self.root];
        std::iter::from_fn(move || {
            let id = pending.pop()?;
            pending.extend(self.nodes[id.0].children.iter().rev().copied());
            Some(id)
        })
    }
    pub fn append(&mut self, parent: NodeId, kind: NodeKind) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(Node {
            kind,
            parent: Some(parent),
            children: vec![],
        });
        self.nodes[parent.0].children.push(id);
        id
    }

    pub fn tag(&self, id: NodeId) -> &str {
        match &self.nodes[id.0].kind {
            NodeKind::Element { tag, .. } => tag,
            _ => "",
        }
    }

    /// Element local name; tag() is retained as an equivalent arena API.
    pub fn local_name(&self, id: NodeId) -> &str {
        self.tag(id)
    }

    pub fn namespace(&self, id: NodeId) -> Namespace {
        self.namespaces.get(&id).copied().unwrap_or_default()
    }

    pub fn namespace_uri(&self, id: NodeId) -> Option<&'static str> {
        matches!(self.nodes[id.0].kind, NodeKind::Element { .. }).then(|| self.namespace(id).uri())
    }

    pub fn attribute_namespace(&self, id: NodeId, name: &str) -> Option<AttributeNamespace> {
        self.attribute_namespaces
            .get(&(id, name.to_owned()))
            .copied()
    }

    pub fn attribute_local_name<'a>(&self, id: NodeId, name: &'a str) -> &'a str {
        if self.attribute_namespace(id, name).is_some() {
            name.split_once(':').map_or(name, |(_, local)| local)
        } else {
            name
        }
    }

    pub fn attribute_prefix<'a>(&self, id: NodeId, name: &'a str) -> Option<&'a str> {
        self.attribute_namespace(id, name)
            .and_then(|_| name.split_once(':').map(|(prefix, _)| prefix))
    }

    pub fn attr_ns(&self, id: NodeId, namespace: AttributeNamespace, local: &str) -> Option<&str> {
        if let NodeKind::Element { attrs, .. } = &self.nodes[id.0].kind {
            attrs.iter().find_map(|(name, value)| {
                (self.attribute_namespace(id, name) == Some(namespace)
                    && self.attribute_local_name(id, name) == local)
                    .then_some(value.as_str())
            })
        } else {
            None
        }
    }
    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        match &self.nodes[id.0].kind {
            NodeKind::Element { attrs, .. } => attrs.get(name).map(String::as_str),
            _ => None,
        }
    }

    pub fn text(&self, id: NodeId) -> String {
        let mut result = String::new();
        let mut pending = vec![id];
        while let Some(node) = pending.pop() {
            if let NodeKind::Text(text) = &self.nodes[node.0].kind {
                result.push_str(text);
            }
            pending.extend(self.nodes[node.0].children.iter().rev().copied());
        }
        result
    }

    pub fn elements(&self, tag: &str) -> impl Iterator<Item = NodeId> + '_ {
        let tag = tag.to_owned();
        let mut pending = vec![self.root];
        std::iter::from_fn(move || {
            while let Some(id) = pending.pop() {
                pending.extend(self.nodes[id.0].children.iter().rev().copied());
                if matches!(&self.nodes[id.0].kind, NodeKind::Element { tag: name, .. } if *name == tag)
                {
                    return Some(id);
                }
            }
            None
        })
    }

    /// HTML-only local-name search for resources and HTML DOM algorithms.
    /// elements() deliberately continues to search all element namespaces.
    pub fn html_elements(&self, tag: &str) -> impl Iterator<Item = NodeId> + '_ {
        self.elements(tag)
            .filter(|&id| self.namespace(id) == Namespace::Html)
    }

    pub fn title(&self) -> String {
        self.html_elements("title")
            .next()
            .map(|n| {
                self.text(n)
                    .split(['\t', '\n', '\x0c', '\r', ' '])
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
    }

    pub fn serialize(&self) -> String {
        self.serialize_node(self.root)
    }

    /// Serialize one node, including template contents. Document-kind nodes
    /// serialize all their children, which also supports detached fragments.
    pub fn serialize_node(&self, id: NodeId) -> String {
        self.serialize_pending(vec![(id, false)])
    }

    /// HTML serialization of an element's children. For templates this is the
    /// contents fragment; ordinary tree/text traversals intentionally omit it.
    pub fn inner_html(&self, id: NodeId) -> String {
        let target = self.template_contents.get(&id).copied().unwrap_or(id);
        if self.namespace(target) == Namespace::Html && is_void(self.tag(target)) {
            return String::new();
        }
        self.serialize_pending(
            self.nodes[target.0]
                .children
                .iter()
                .rev()
                .map(|&c| (c, false))
                .collect(),
        )
    }

    fn serialize_pending(&self, mut pending: Vec<(NodeId, bool)>) -> String {
        // Explicit work stack also keeps user-created, deeply nested DOMs safe.
        let mut out = String::new();
        while let Some((id, closing)) = pending.pop() {
            if closing {
                out.push_str("</");
                out.push_str(self.tag(id));
                out.push('>');
                continue;
            }
            match &self.nodes[id.0].kind {
                NodeKind::Document => {
                    pending.extend(self.nodes[id.0].children.iter().rev().map(|&c| (c, false)));
                }
                NodeKind::Text(s) => {
                    let raw = self.nodes[id.0].parent.is_some_and(|p| {
                        self.namespace(p) == Namespace::Html
                            && (matches!(
                                self.tag(p),
                                "script"
                                    | "style"
                                    | "xmp"
                                    | "iframe"
                                    | "noembed"
                                    | "noframes"
                                    | "plaintext"
                            ) || self.scripting_enabled && self.tag(p) == "noscript")
                    });
                    if raw {
                        out.push_str(s);
                    } else {
                        out.push_str(
                            &s.replace('&', "&amp;")
                                .replace('\u{a0}', "&nbsp;")
                                .replace('<', "&lt;")
                                .replace('>', "&gt;"),
                        );
                    }
                }
                NodeKind::Comment(data) => {
                    out.push_str("<!--");
                    out.push_str(data);
                    out.push_str("-->");
                }
                NodeKind::Doctype { name, .. } => {
                    out.push_str("<!DOCTYPE ");
                    out.push_str(name);
                    out.push('>');
                }
                NodeKind::Element { tag, attrs } => {
                    out.push('<');
                    out.push_str(tag);
                    for (k, v) in attrs {
                        out.push(' ');
                        let local = self.attribute_local_name(id, k);
                        match self.attribute_namespace(id, k) {
                            Some(AttributeNamespace::Xml) => {
                                out.push_str("xml:");
                                out.push_str(local);
                            }
                            Some(AttributeNamespace::XLink) => {
                                out.push_str("xlink:");
                                out.push_str(local);
                            }
                            Some(AttributeNamespace::Xmlns) => {
                                if local != "xmlns" {
                                    out.push_str("xmlns:");
                                }
                                out.push_str(local);
                            }
                            None => out.push_str(k),
                        }
                        out.push_str("=\"");
                        out.push_str(
                            &v.replace('&', "&amp;")
                                .replace('\u{a0}', "&nbsp;")
                                .replace('<', "&lt;")
                                .replace('>', "&gt;")
                                .replace('"', "&quot;"),
                        );
                        out.push('"');
                    }
                    out.push('>');
                    if self.namespace(id) != Namespace::Html || !is_void(tag) {
                        pending.push((id, true));
                        let content = self.template_contents.get(&id).copied().unwrap_or(id);
                        pending.extend(
                            self.nodes[content.0]
                                .children
                                .iter()
                                .rev()
                                .map(|&c| (c, false)),
                        );
                    }
                }
            }
        }
        out
    }
}

pub fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "basefont"
            | "bgsound"
            | "br"
            | "col"
            | "embed"
            | "frame"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
