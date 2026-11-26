//! DOM mutation preflight and inert HTML fragment insertion. Planning never
//! detaches or imports nodes; all fallible hierarchy/resource checks precede
//! arena changes. User JavaScript is never called while a State borrow is held.
use super::*;
use crate::{
    dom::Node,
    html::{self, FragmentContext},
};
use rust_browser_javascript::JsString;
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Copy)]
struct Limits {
    nodes: usize,
    bytes: usize,
    depth: usize,
}
const LIMITS: Limits = Limits {
    nodes: NODE_LIMIT,
    bytes: DOM_BYTES_LIMIT,
    depth: DOM_DEPTH_LIMIT,
};

#[derive(Clone, Copy)]
enum Walk {
    Root,
    Children,
    Ownership,
}
fn charge(
    vm: &mut Vm,
    state: &Shared,
    parent: Option<NodeId>,
    roots: &[NodeId],
    walk: Walk,
    copy_bytes: bool,
    extra: usize,
) -> Result<(), JsError> {
    // Computing this conservative estimate is itself bounded by the arena node
    // budget. Charge before the actual operation, including on invalid requests,
    // so catching repeated hierarchy errors cannot evade native task accounting.
    let state = state.borrow();
    vm.charge_work(tree_work(
        &state.document,
        parent,
        roots,
        walk,
        copy_bytes,
        extra,
    ))
}

fn tree_work(
    document: &Document,
    parent: Option<NodeId>,
    roots: &[NodeId],
    walk: Walk,
    copy_bytes: bool,
    extra: usize,
) -> usize {
    let mut units = extra.saturating_add(parent.map_or(0, |id| {
        document.nodes[id.0]
            .children
            .len()
            .saturating_add(document.template_contents.len())
            .saturating_add(document.shadow_hosts.len())
            .saturating_add(DOM_DEPTH_LIMIT)
    }));
    let mut pending = roots.to_vec();
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        units = units.saturating_add(1);
        if copy_bytes {
            units = units.saturating_add(kind_bytes(&document.nodes[id.0].kind));
        }
        if !matches!(walk, Walk::Root) {
            pending.extend(document.nodes[id.0].children.iter().copied());
        }
        if matches!(walk, Walk::Ownership) {
            pending.extend(document.template_contents.get(&id).copied());
            pending.extend(document.shadow_roots.get(&id).map(|tree| tree.root));
        }
    }
    units
}

#[derive(Debug)]
enum Failure {
    Dom(&'static str, &'static str),
    Fatal(JsError),
}
type Checked<T> = Result<T, Failure>;
fn hierarchy(message: &'static str) -> Failure {
    Failure::Dom("HierarchyRequestError", message)
}
fn not_found() -> Failure {
    Failure::Dom(
        "NotFoundError",
        "The reference node is not a child of this parent",
    )
}
fn budget(message: &'static str) -> Failure {
    Failure::Fatal(JsError::new(ErrorKind::HeapLimit, message))
}

#[derive(Clone)]
pub(super) struct Errors {
    prototype: Value,
}
struct ExceptionData {
    name: JsString,
    message: JsString,
    code: u32,
}

const LEGACY_CODES: [(&str, Option<&str>); 25] = [
    ("INDEX_SIZE_ERR", Some("IndexSizeError")),
    ("DOMSTRING_SIZE_ERR", None),
    ("HIERARCHY_REQUEST_ERR", Some("HierarchyRequestError")),
    ("WRONG_DOCUMENT_ERR", Some("WrongDocumentError")),
    ("INVALID_CHARACTER_ERR", Some("InvalidCharacterError")),
    ("NO_DATA_ALLOWED_ERR", None),
    (
        "NO_MODIFICATION_ALLOWED_ERR",
        Some("NoModificationAllowedError"),
    ),
    ("NOT_FOUND_ERR", Some("NotFoundError")),
    ("NOT_SUPPORTED_ERR", Some("NotSupportedError")),
    ("INUSE_ATTRIBUTE_ERR", Some("InUseAttributeError")),
    ("INVALID_STATE_ERR", Some("InvalidStateError")),
    ("SYNTAX_ERR", Some("SyntaxError")),
    ("INVALID_MODIFICATION_ERR", Some("InvalidModificationError")),
    ("NAMESPACE_ERR", Some("NamespaceError")),
    ("INVALID_ACCESS_ERR", Some("InvalidAccessError")),
    ("VALIDATION_ERR", None),
    ("TYPE_MISMATCH_ERR", Some("TypeMismatchError")),
    ("SECURITY_ERR", Some("SecurityError")),
    ("NETWORK_ERR", Some("NetworkError")),
    ("ABORT_ERR", Some("AbortError")),
    ("URL_MISMATCH_ERR", Some("URLMismatchError")),
    ("QUOTA_EXCEEDED_ERR", Some("QuotaExceededError")),
    ("TIMEOUT_ERR", Some("TimeoutError")),
    ("INVALID_NODE_TYPE_ERR", Some("InvalidNodeTypeError")),
    ("DATA_CLONE_ERR", Some("DataCloneError")),
];

fn init_exception(
    vm: &mut Vm,
    object: Value,
    name: JsString,
    message: JsString,
) -> Result<Value, JsError> {
    let code = LEGACY_CODES
        .iter()
        .position(|(_, legacy)| {
            legacy.is_some_and(|legacy| legacy.encode_utf16().eq(name.as_utf16().iter().copied()))
        })
        .map_or(0, |index| index as u32 + 1);
    vm.set_host_data(
        object.clone(),
        Rc::new(ExceptionData {
            name,
            message,
            code,
        }),
    )?;
    Ok(object)
}

pub(super) fn clone_exception(
    vm: &mut Vm,
    source: &Value,
    prototype: &Value,
) -> Result<Option<Value>, JsError> {
    let Some(error) = vm.host_data::<ExceptionData>(source) else {
        return Ok(None);
    };
    let object = vm.new_object_with_prototype(prototype.clone())?;
    init_exception(vm, object, error.name.clone(), error.message.clone()).map(Some)
}

impl Errors {
    pub(super) fn exception(
        &self,
        vm: &mut Vm,
        name: &'static str,
        message: &'static str,
    ) -> JsError {
        self.convert(vm, Failure::Dom(name, message))
    }

    pub(super) fn invalid_selector(&self, vm: &mut Vm) -> JsError {
        self.exception(vm, "SyntaxError", "Invalid selector list")
    }

    fn value(&self, vm: &mut Vm, name: &str, message: &str) -> Result<Value, JsError> {
        let value = vm.new_object_with_prototype(self.prototype.clone())?;
        init_exception(vm, value, name.into(), message.into())
    }
    fn convert(&self, vm: &mut Vm, failure: Failure) -> JsError {
        match failure {
            Failure::Fatal(error) => error,
            Failure::Dom(name, message) => match self.value(vm, name, message) {
                Ok(value) => JsError {
                    kind: ErrorKind::Error,
                    message: format!("{name}: {message}"),
                    line: None,
                    column: None,
                    thrown: Some(value),
                    inspected_calls: None,
                },
                Err(error) => error,
            },
        }
    }
}
pub(super) fn install_errors(vm: &mut Vm) -> Result<Errors, JsError> {
    // Capture the intrinsic Error inheritance before page code can replace
    // globals. Thrown objects keep this captured DOMException prototype.
    let prototype = vm.error_value(&JsError::new(ErrorKind::Error, ""))?;
    vm.root(prototype.clone());
    let errors = Errors {
        prototype: prototype.clone(),
    };
    let intrinsic = prototype.clone();
    let constructor = webidl::interface(
        vm,
        prototype.clone(),
        "DOMException",
        0,
        move |vm, _, args, target| {
            let target = target.ok_or_else(|| JsError::type_error("DOMException requires new"))?;
            let message = match args.first().filter(|arg| !arg.is_undefined()) {
                Some(arg) => vm.to_string(arg)?,
                None => "".into(),
            };
            let name = match args.get(1).filter(|arg| !arg.is_undefined()) {
                Some(arg) => vm.to_string(arg)?,
                None => "Error".into(),
            };
            let value = webidl::instance(vm, target, &intrinsic)?;
            init_exception(vm, value, name, message)
        },
    )?;
    for key in ["name", "message", "code"] {
        getter(vm, prototype.clone(), key, move |vm, object, _| {
            let error = vm
                .host_data::<ExceptionData>(&object)
                .ok_or_else(|| JsError::type_error("Illegal DOMException receiver"))?;
            Ok(match key {
                "name" => Value::String(error.name.clone()),
                "message" => Value::String(error.message.clone()),
                _ => error.code.into(),
            })
        })?;
    }
    for (index, (name, _)) in LEGACY_CODES.iter().enumerate() {
        let code = index as u32 + 1;
        vm.define_data_property(constructor.clone(), name, code.into(), false, true, false)?;
        vm.define_data_property(prototype.clone(), name, code.into(), false, true, false)?;
    }
    Ok(errors)
}

/// New nodes use indices local to this staging arena until commit. Only roots
/// and their template ownership subgraphs are copied; parser scaffolding and
/// detached recovery nodes are never imported.
#[derive(Default)]
struct Staged {
    nodes: Vec<Node>,
    templates: BTreeMap<NodeId, NodeId>,
    namespaces: BTreeMap<NodeId, crate::dom::Namespace>,
    attribute_namespaces: BTreeMap<(NodeId, String), crate::dom::AttributeNamespace>,
    source_ids: Vec<NodeId>,
    started_script_clones: Vec<NodeId>,
    inert_scripts: Vec<NodeId>,
    roots: Vec<NodeId>,
    bytes: usize,
}
impl Staged {
    fn singleton(kind: NodeKind) -> Self {
        let bytes = kind_bytes(&kind);
        Self {
            nodes: vec![Node {
                kind,
                parent: None,
                children: vec![],
            }],
            roots: vec![NodeId(0)],
            bytes,
            ..Self::default()
        }
    }
    fn from_forest(
        source: &Document,
        roots: &[NodeId],
        deep: bool,
        existing: usize,
        arena_bytes: usize,
        limits: Limits,
    ) -> Checked<Self> {
        let mut order = Vec::new();
        let mut mapping = HashMap::new();
        let mut pending: Vec<_> = roots.iter().rev().copied().map(|id| (id, deep)).collect();
        let mut bytes = 0usize;
        while let Some((id, descendants)) = pending.pop() {
            if mapping.contains_key(&id) {
                return Err(hierarchy("Template ownership must be an acyclic tree"));
            }
            bytes = bytes.saturating_add(kind_bytes(&source.nodes[id.0].kind));
            bytes = bytes.saturating_add(
                source
                    .attribute_namespaces
                    .range((id, String::new())..(NodeId(id.0 + 1), String::new()))
                    .map(|((_, name), _)| name.len())
                    .sum::<usize>(),
            );
            if existing.saturating_add(order.len() + 1) > limits.nodes {
                return Err(budget("DOM node budget exceeded"));
            }
            if arena_bytes.saturating_add(bytes) > limits.bytes {
                return Err(budget("DOM string data exceeds the arena byte budget"));
            }
            mapping.insert(id, NodeId(order.len()));
            order.push((id, descendants));
            if descendants {
                pending.extend(
                    source.nodes[id.0]
                        .children
                        .iter()
                        .rev()
                        .copied()
                        .map(|id| (id, true)),
                );
            }
            if let Some(&content) = source.template_contents.get(&id) {
                pending.push((content, descendants));
            }
        }
        let mut staged = Self {
            roots: roots.iter().map(|id| mapping[id]).collect(),
            source_ids: order.iter().map(|&(id, _)| id).collect(),
            bytes,
            ..Self::default()
        };
        for &(id, descendants) in &order {
            let old = &source.nodes[id.0];
            staged.nodes.push(Node {
                kind: old.kind.clone(),
                parent: old.parent.and_then(|parent| mapping.get(&parent).copied()),
                children: if descendants {
                    old.children.iter().map(|id| mapping[id]).collect()
                } else {
                    vec![]
                },
            });
            if let Some(&namespace) = source.namespaces.get(&id) {
                staged.namespaces.insert(mapping[&id], namespace);
            }
            if let NodeKind::Element { attrs, .. } = &old.kind {
                for name in attrs.keys() {
                    if let Some(&namespace) = source.attribute_namespaces.get(&(id, name.clone())) {
                        staged
                            .attribute_namespaces
                            .insert((mapping[&id], name.clone()), namespace);
                    }
                }
            }
            if let Some(&content) = source.template_contents.get(&id) {
                staged.templates.insert(mapping[&id], mapping[&content]);
            }
        }
        for &root in &staged.roots {
            staged.nodes[root.0].parent = None;
        }
        // Templates created via older host code might not yet have materialized
        // contents. Every newly cloned template gets its own contents container.
        for index in 0..staged.nodes.len() {
            if matches!(&staged.nodes[index].kind, NodeKind::Element { tag, .. } if tag == "template")
                && staged
                    .namespaces
                    .get(&NodeId(index))
                    .is_none_or(|ns| *ns == crate::dom::Namespace::Html)
                && !staged.templates.contains_key(&NodeId(index))
            {
                let content = NodeId(staged.nodes.len());
                staged.nodes.push(Node {
                    kind: NodeKind::Document,
                    parent: None,
                    children: vec![],
                });
                staged.templates.insert(NodeId(index), content);
            }
        }
        staged.check(existing, arena_bytes, limits)?;
        Ok(staged)
    }
    fn copy_script_flags(&mut self, started: impl Fn(NodeId) -> bool) {
        self.started_script_clones = self
            .source_ids
            .iter()
            .enumerate()
            .filter_map(|(index, &source)| started(source).then_some(NodeId(index)))
            .collect();
    }

    fn check(&self, existing: usize, bytes: usize, limits: Limits) -> Checked<()> {
        if existing.saturating_add(self.nodes.len()) > limits.nodes {
            return Err(budget("DOM node budget exceeded"));
        }
        if bytes.saturating_add(self.bytes) > limits.bytes {
            return Err(budget("DOM string data exceeds the arena byte budget"));
        }
        Ok(())
    }
    fn global_roots(&self, base: usize) -> Vec<NodeId> {
        self.roots.iter().map(|id| NodeId(base + id.0)).collect()
    }
    fn commit(self, state: &mut State) -> Vec<NodeId> {
        let base = state.document.nodes.len();
        let roots = self.global_roots(base);
        for mut node in self.nodes {
            node.parent = node.parent.map(|id| NodeId(base + id.0));
            for id in &mut node.children {
                id.0 += base;
            }
            state.document.nodes.push(node);
        }
        for (template, content) in self.templates {
            state
                .document
                .template_contents
                .insert(NodeId(base + template.0), NodeId(base + content.0));
        }
        for (id, namespace) in self.namespaces {
            state
                .document
                .namespaces
                .insert(NodeId(base + id.0), namespace);
        }
        for ((id, name), namespace) in self.attribute_namespaces {
            state
                .document
                .attribute_namespaces
                .insert((NodeId(base + id.0), name), namespace);
        }
        for id in self.inert_scripts {
            state
                .dynamic_scripts
                .mark_fragment_inert(NodeId(base + id.0));
        }
        for id in self.started_script_clones {
            state
                .dynamic_scripts
                .mark_clone_started(NodeId(base + id.0));
        }
        state.dom_bytes = state.dom_bytes.saturating_add(self.bytes);
        roots
    }
}

struct View<'a> {
    doc: &'a Document,
    staged: Option<&'a Staged>,
}
impl View<'_> {
    fn node(&self, id: NodeId) -> &Node {
        if id.0 < self.doc.nodes.len() {
            &self.doc.nodes[id.0]
        } else {
            &self.staged.unwrap().nodes[id.0 - self.doc.nodes.len()]
        }
    }
    fn content(&self, id: NodeId) -> Option<NodeId> {
        if id.0 < self.doc.nodes.len() {
            self.doc.template_contents.get(&id).copied()
        } else {
            self.staged
                .unwrap()
                .templates
                .get(&NodeId(id.0 - self.doc.nodes.len()))
                .map(|id| NodeId(id.0 + self.doc.nodes.len()))
        }
    }
    fn children(&self, id: NodeId) -> Vec<NodeId> {
        let base = if id.0 < self.doc.nodes.len() {
            0
        } else {
            self.doc.nodes.len()
        };
        self.node(id)
            .children
            .iter()
            .map(|id| NodeId(id.0 + base))
            .collect()
    }
}
fn owners(document: &Document) -> HashMap<NodeId, NodeId> {
    document
        .template_contents
        .iter()
        .map(|(&template, &content)| (content, template))
        .collect()
}
fn parent_depth(
    document: &Document,
    parent: NodeId,
    incoming: &HashSet<NodeId>,
    limits: Limits,
) -> Checked<usize> {
    let owners = owners(document);
    let mut current = Some(parent);
    let mut depth = 0usize;
    let mut seen = HashSet::new();
    while let Some(id) = current {
        if incoming.contains(&id) || !seen.insert(id) {
            return Err(hierarchy("The insertion would create a cycle"));
        }
        depth += usize::from(matches!(
            document.nodes[id.0].kind,
            NodeKind::Element { .. }
        ));
        if depth > limits.depth {
            return Err(budget("DOM element depth exceeds the configured budget"));
        }
        current = document.nodes[id.0]
            .parent
            .or_else(|| owners.get(&id).copied())
            .or_else(|| document.shadow_hosts.get(&id).copied());
    }
    Ok(depth)
}
fn check_depth(view: &View<'_>, roots: &[NodeId], base: usize, limits: Limits) -> Checked<()> {
    let mut pending: Vec<_> = roots.iter().copied().map(|id| (id, base)).collect();
    let root_set: HashSet<_> = roots.iter().copied().collect();
    let mut seen = HashSet::new();
    while let Some((id, depth)) = pending.pop() {
        if !seen.insert(id) {
            return Err(hierarchy("Template ownership must be an acyclic tree"));
        }
        let depth = depth + usize::from(matches!(view.node(id).kind, NodeKind::Element { .. }));
        if depth > limits.depth {
            return Err(budget("DOM element depth exceeds the configured budget"));
        }
        // Variadic ParentNode methods move a descendant out of an earlier
        // argument when both are supplied. Inspect its final root depth once.
        pending.extend(
            view.children(id)
                .into_iter()
                .filter(|id| !root_set.contains(id))
                .map(|id| (id, depth)),
        );
        if let Some(content) = view.content(id) {
            pending.push((content, depth));
        }
        if let Some(tree) = view.doc.shadow_roots.get(&id) {
            pending.push((tree.root, depth));
        }
    }
    Ok(())
}

struct Plan {
    parent: NodeId,
    input: Vec<NodeId>,
    roots: Vec<NodeId>,
    removed: Vec<NodeId>,
    children: Vec<NodeId>,
    previous: Option<NodeId>,
    next: Option<NodeId>,
    combined_removal: Option<NodeId>,
}
impl Plan {
    fn commit(self, state: &mut State) -> Vec<NodeId> {
        // A new leaf at the end of a child list preserves the order of all
        // previously connected nodes. Moves, fragments and replacements do not.
        let append = match (self.input.as_slice(), self.roots.as_slice()) {
            ([id], [root])
                if id == root
                    && self.removed.is_empty()
                    && self.next.is_none()
                    && state.document.nodes[id.0].parent.is_none()
                    && state.document.nodes[id.0].children.is_empty() =>
            {
                Some((self.parent, *id))
            }
            _ => None,
        };
        let mut changed_scripts = Vec::new();
        let mut seen = HashSet::new();
        for id in self.input {
            if self.combined_removal == Some(id) {
                continue;
            }
            if matches!(state.document.nodes[id.0].kind, NodeKind::Document) {
                empty_fragment(state, id);
            } else if let Some(parent) = state.document.nodes[id.0].parent {
                if parent != self.parent
                    && dynamic_scripts::is_script(&state.document, parent)
                    && seen.insert(parent)
                {
                    changed_scripts.push(parent);
                }
                mutation_observers::removing(state, parent, id, false);
                detach(&mut state.document, id);
            }
        }
        for &id in &self.removed {
            mutation_observers::removing(state, self.parent, id, true);
            state.document.nodes[id.0].parent = None;
        }
        let destination = state.document.node_document(self.parent);
        for &id in &self.roots {
            state.document.adopt(id, destination);
        }
        for &id in &self.children {
            state.document.nodes[id.0].parent = Some(self.parent);
        }
        state.document.nodes[self.parent.0].children = self.children;
        mutation_observers::note(
            state,
            Mutation::Children {
                target: self.parent,
                added: &self.roots,
                removed: &self.removed,
                previous: self.previous,
                next: self.next,
            },
        );
        mark_dirty(state);
        state.appended_leaf = append;
        dynamic_scripts::inserted(state, self.parent, self.roots, changed_scripts)
    }
}

fn empty_fragment(state: &mut State, id: NodeId) {
    if state.mutations.borrow().active() {
        for &child in &state.document.nodes[id.0].children {
            mutation_observers::removing(state, id, child, true);
        }
    }
    let children = std::mem::take(&mut state.document.nodes[id.0].children);
    for child in &children {
        state.document.nodes[child.0].parent = None;
    }
    mutation_observers::note(
        state,
        Mutation::Children {
            target: id,
            added: &[],
            removed: &children,
            previous: None,
            next: None,
        },
    );
}
#[allow(
    clippy::too_many_arguments,
    reason = "One shared atomic planner keeps the DOM pre-insert/replace parameters and resource context explicit"
)]
fn plan(
    document: &Document,
    parent: NodeId,
    input: &[NodeId],
    before: Option<NodeId>,
    replace: Option<NodeId>,
    replace_all: bool,
    staged: Option<&Staged>,
    limits: Limits,
) -> Checked<Plan> {
    if !matches!(
        document.nodes[parent.0].kind,
        NodeKind::Document | NodeKind::Element { .. }
    ) {
        return Err(hierarchy("This node cannot contain children"));
    }
    for reference in before.iter().chain(replace.iter()) {
        if document.nodes[reference.0].parent != Some(parent) {
            return Err(not_found());
        }
    }
    let view = View {
        doc: document,
        staged,
    };
    let mut roots = Vec::new();
    let mut converted = HashSet::new();
    for &id in input {
        if document.is_document(id) {
            return Err(hierarchy("A Document cannot be inserted"));
        }
        if matches!(view.node(id).kind, NodeKind::Document) {
            if converted.insert(id) {
                roots.extend(
                    view.children(id)
                        .into_iter()
                        .filter(|&child| converted.insert(child)),
                );
            }
        } else {
            roots.push(id);
            converted.insert(id);
        }
    }
    // Converting variadic Node/string arguments to a temporary fragment keeps
    // only the last occurrence of a Node, at its last argument position.
    let mut moved = converted;
    moved.clear();
    roots.reverse();
    roots.retain(|id| moved.insert(*id));
    roots.reverse();
    let incoming: HashSet<_> = input.iter().chain(&roots).copied().collect();
    let depth = parent_depth(document, parent, &incoming, limits)?;
    check_depth(&view, &roots, depth, limits)?;
    for &id in &roots {
        match view.node(id).kind {
            NodeKind::Element { .. }
            | NodeKind::Comment(_)
            | NodeKind::Text(_)
            | NodeKind::Cdata(_)
            | NodeKind::ProcessingInstruction { .. } => {}
            NodeKind::Doctype { .. } if document.is_document(parent) => {}
            _ => {
                return Err(hierarchy(
                    "This node kind cannot be inserted into this parent",
                ));
            }
        }
    }
    let old = &document.nodes[parent.0].children;
    // A single Node argument to replaceChildren is passed directly to
    // replace-all. Its removal from this same parent is part of that record.
    let combined_removal =
        if replace_all && input.len() == 1 && view.node(input[0]).parent == Some(parent) {
            Some(input[0])
        } else {
            None
        };
    let boundary = if replace_all {
        0
    } else {
        before
            .or(replace)
            .and_then(|id| old.iter().position(|&child| child == id))
            .unwrap_or(old.len())
    };
    let mut children = Vec::with_capacity(old.len().saturating_add(roots.len()));
    let mut removed = Vec::new();
    let mut insertion = None;
    for (index, &id) in old.iter().enumerate() {
        if index == boundary {
            insertion = Some(children.len());
            children.extend(roots.iter().copied());
        }
        if moved.contains(&id) && combined_removal != Some(id) {
            continue;
        }
        if replace_all || replace == Some(id) {
            removed.push(id);
        } else {
            children.push(id);
        }
    }
    if boundary == old.len() {
        insertion = Some(children.len());
        children.extend(roots.iter().copied());
    }
    if document.is_document(parent) {
        let mut element = false;
        let mut doctype = false;
        for &id in &children {
            match view.node(id).kind {
                NodeKind::Element { .. } if !element => element = true,
                NodeKind::Doctype { .. } if !doctype && !element => doctype = true,
                NodeKind::Comment(_) | NodeKind::ProcessingInstruction { .. } => {}
                _ => {
                    return Err(hierarchy(
                        "A Document permits one element and one preceding doctype, and no text children",
                    ));
                }
            }
        }
    }
    let insertion = insertion.expect("planned insertion boundary is within the parent");
    let previous = if replace_all {
        None
    } else if replace.is_some() {
        boundary.checked_sub(1).map(|index| old[index])
    } else {
        insertion.checked_sub(1).map(|index| children[index])
    };
    let next = if replace_all {
        None
    } else {
        children.get(insertion + roots.len()).copied()
    };
    Ok(Plan {
        parent,
        input: input.to_vec(),
        roots,
        removed,
        children,
        previous,
        next,
        combined_removal,
    })
}

fn context(state: &State, id: NodeId, body_fallback: bool) -> FragmentContext<'_> {
    let document = &state.document;
    let tag = document.tag(id);
    let tag = if tag.is_empty()
        || body_fallback && tag == "html" && document.namespace(id) == crate::dom::Namespace::Html
    {
        "body"
    } else {
        tag
    };
    let mut current = Some(id);
    let mut form_ancestor = false;
    // Template contents are detached and use their own inert tree, rather than
    // crossing back to the template host for nearest-form lookup.
    while let Some(node) = current {
        if document.tag(node) == "form" && document.namespace(node) == crate::dom::Namespace::Html {
            form_ancestor = true;
            break;
        }
        current = document.nodes[node.0].parent;
    }
    let active = document.node_document(id) == document.root;
    FragmentContext {
        tag,
        namespace: document.namespace(id),
        attrs: match &document.nodes[id.0].kind {
            NodeKind::Element { attrs, .. } => Some(attrs),
            _ => None,
        },
        scripting_enabled: active && document.scripting_enabled,
        quirks_mode: if active {
            document.quirks_mode
        } else {
            crate::dom::QuirksMode::NoQuirks
        },
        form_ancestor,
    }
}
fn html_stage(state: &State, source: &str, id: NodeId, body_fallback: bool) -> Checked<Staged> {
    let fragment = html::parse_fragment_with_context(source, context(state, id, body_fallback));
    if fragment.truncated {
        return Err(budget("HTML fragment exceeded the parser resource budget"));
    }
    let mut staged = Staged::from_forest(
        &fragment,
        &fragment.nodes[fragment.root.0].children,
        true,
        state.document.nodes.len(),
        state.dom_bytes,
        LIMITS,
    )?;
    staged.inert_scripts = staged
        .source_ids
        .iter()
        .enumerate()
        .filter_map(|(index, &source)| {
            dynamic_scripts::is_script(&fragment, source).then_some(NodeId(index))
        })
        .collect();
    Ok(staged)
}
#[allow(
    clippy::too_many_arguments,
    reason = "HTML insertion passes its parsing context and DOM replacement position together to the shared preflight"
)]
fn insert_html(
    state: &Shared,
    parent: NodeId,
    context_id: NodeId,
    fallback: bool,
    before: Option<NodeId>,
    replace: Option<NodeId>,
    all: bool,
    source: &str,
) -> Checked<Vec<NodeId>> {
    let mut state = state.borrow_mut();
    let staged = html_stage(&state, source, context_id, fallback)?;
    let roots = staged.global_roots(state.document.nodes.len());
    let plan = plan(
        &state.document,
        parent,
        &roots,
        before,
        replace,
        all,
        Some(&staged),
        LIMITS,
    )?;
    staged.commit(&mut state);
    Ok(plan.commit(&mut state))
}

fn ensure_template(state: &Shared, template: NodeId) -> Checked<NodeId> {
    let mut state = state.borrow_mut();
    if let Some(&content) = state.document.template_contents.get(&template) {
        return Ok(content);
    }
    if state.document.nodes.len() >= NODE_LIMIT {
        return Err(budget("DOM node budget exceeded"));
    }
    let content = NodeId(state.document.nodes.len());
    state.document.nodes.push(Node {
        kind: NodeKind::Document,
        parent: None,
        children: vec![],
    });
    state.document.template_contents.insert(template, content);
    Ok(content)
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    prototype: Value,
    document: Value,
) -> Result<Errors, JsError> {
    let errors = install_errors(vm)?;
    install_mutations(vm, state, prototype.clone(), &errors)?;
    install_html(vm, state, prototype.clone(), &errors)?;
    install_cloning(vm, state, prototype, document, &errors)?;
    Ok(errors)
}

fn install_mutations(
    vm: &mut Vm,
    state: &Shared,
    prototype: Value,
    errors: &Errors,
) -> Result<(), JsError> {
    for name in ["appendChild", "insertBefore", "replaceChild"] {
        let state = state.clone();
        let errors = errors.clone();
        method(vm, prototype.clone(), name, move |vm, this, args| {
            let parent = node(&state, &this)?;
            if name != "appendChild" && args.len() < 2 {
                return Err(JsError::type_error(
                    "This Node mutation requires two arguments",
                ));
            }
            let value = arg(args, 0);
            let child = node(&state, &value)?;
            let reference =
                if name == "appendChild" || name == "insertBefore" && arg(args, 1).is_nullish() {
                    None
                } else {
                    Some(node(&state, &arg(args, 1))?)
                };
            charge(
                vm,
                &state,
                Some(parent),
                &[child],
                Walk::Ownership,
                false,
                0,
            )?;
            let result = {
                let mut state = state.borrow_mut();
                let mutation = plan(
                    &state.document,
                    parent,
                    &[child],
                    if name == "insertBefore" {
                        reference
                    } else {
                        None
                    },
                    if name == "replaceChild" {
                        reference
                    } else {
                        None
                    },
                    false,
                    None,
                    LIMITS,
                );
                mutation.map(|mutation| mutation.commit(&mut state))
            };
            let candidates = result.map_err(|failure| errors.convert(vm, failure))?;
            mutation_observers::schedule(vm, &state);
            dynamic_scripts::post_connection(vm, &state, candidates)?;
            Ok(if name == "replaceChild" {
                arg(args, 1)
            } else {
                value
            })
        })?;
    }
    for name in ["append", "prepend", "replaceChildren"] {
        let state = state.clone();
        let errors = errors.clone();
        method(vm, prototype.clone(), name, move |vm, this, args| {
            let parent = node(&state, &this)?;
            enum Input {
                Node(NodeId),
                Text(String),
            }
            let mut converted = Vec::with_capacity(args.len());
            let mut existing_roots = Vec::new();
            let mut bytes = 0usize;
            for value in args {
                let id = {
                    let state = state.borrow();
                    value
                        .as_object()
                        .and_then(|id| state.nodes.get(&id).copied())
                };
                if let Some(id) = id {
                    converted.push(Input::Node(id));
                    existing_roots.push(id);
                } else {
                    let value = text(vm, value.clone())?;
                    bytes = bytes.saturating_add(value.len());
                    converted.push(Input::Text(value));
                }
            }
            charge(
                vm,
                &state,
                Some(parent),
                &existing_roots,
                Walk::Ownership,
                false,
                bytes,
            )?;
            let result = {
                let mut state = state.borrow_mut();
                let base = state.document.nodes.len();
                let mut staged = Staged::default();
                let mut input = Vec::with_capacity(converted.len());
                for value in converted {
                    match value {
                        Input::Node(id) => input.push(id),
                        Input::Text(value) => {
                            let local = NodeId(staged.nodes.len());
                            staged.bytes = staged.bytes.saturating_add(value.len());
                            staged.nodes.push(Node {
                                kind: NodeKind::Text(value),
                                parent: None,
                                children: vec![],
                            });
                            staged.roots.push(local);
                            input.push(NodeId(base + local.0));
                        }
                    }
                }
                staged.check(base, state.dom_bytes, LIMITS).and_then(|_| {
                    let before = if name == "prepend" {
                        state.document.nodes[parent.0].children.first().copied()
                    } else {
                        None
                    };
                    let mutation = plan(
                        &state.document,
                        parent,
                        &input,
                        before,
                        None,
                        name == "replaceChildren",
                        Some(&staged),
                        LIMITS,
                    )?;
                    staged.commit(&mut state);
                    Ok(mutation.commit(&mut state))
                })
            };
            let candidates = result.map_err(|failure| errors.convert(vm, failure))?;
            mutation_observers::schedule(vm, &state);
            dynamic_scripts::post_connection(vm, &state, candidates)?;
            Ok(Value::Undefined)
        })?;
    }
    let remove_state = state.clone();
    let remove_errors = errors.clone();
    method(
        vm,
        prototype.clone(),
        "removeChild",
        move |vm, this, args| {
            let parent = node(&remove_state, &this)?;
            let value = arg(args, 0);
            let child = node(&remove_state, &value)?;
            charge(vm, &remove_state, Some(parent), &[], Walk::Root, false, 1)?;
            let result = {
                let mut state = remove_state.borrow_mut();
                if state.document.nodes[child.0].parent != Some(parent) {
                    Err(not_found())
                } else {
                    mutation_observers::removing(&state, parent, child, false);
                    detach(&mut state.document, child);
                    mark_dirty(&mut state);
                    Ok(())
                }
            };
            result.map_err(|failure| remove_errors.convert(vm, failure))?;
            mutation_observers::schedule(vm, &remove_state);
            dynamic_scripts::post_connection(vm, &remove_state, [parent])?;
            Ok(value)
        },
    )?;
    let remove_state = state.clone();
    method(vm, prototype.clone(), "remove", move |vm, this, _| {
        let id = node(&remove_state, &this)?;
        let parent = remove_state.borrow().document.nodes[id.0].parent;
        charge(vm, &remove_state, parent, &[], Walk::Root, false, 1)?;
        {
            let mut state = remove_state.borrow_mut();
            if matches!(state.document.nodes[id.0].kind, NodeKind::Document) {
                return Err(JsError::type_error(
                    "Document and DocumentFragment do not implement ChildNode.remove",
                ));
            }
            if let Some(parent) = parent {
                mutation_observers::removing(&state, parent, id, false);
                detach(&mut state.document, id);
                mark_dirty(&mut state);
            }
        }
        mutation_observers::schedule(vm, &remove_state);
        dynamic_scripts::post_connection(vm, &remove_state, parent)?;
        Ok(Value::Undefined)
    })?;

    // Parent install_node predates DocumentFragment. Override this accessor so a
    // fragment has real textContent and participates in atomic replace-all.
    let get_state = state.clone();
    let get = vm.new_native_function("textContent", move |vm, this, _| {
        let id = node(&get_state, &this)?;
        let null = {
            let state = get_state.borrow();
            state.document.is_document(id)
                || matches!(state.document.nodes[id.0].kind, NodeKind::Doctype { .. })
        };
        if !null {
            charge(vm, &get_state, None, &[id], Walk::Children, true, 0)?;
        }
        let state = get_state.borrow();
        if state.document.is_document(id)
            || matches!(state.document.nodes[id.0].kind, NodeKind::Doctype { .. })
        {
            return Ok(Value::Null);
        }
        Ok(match state.document.nodes[id.0].kind.character_data() {
            Some(value) => value.clone().into(),
            None => state.document.text(id).into(),
        })
    });
    let set_state = state.clone();
    let set_errors = errors.clone();
    let set = vm.new_native_function("textContent", move |vm, this, args| {
        let id = node(&set_state, &this)?;
        let value = arg(args, 0);
        let value = if value.is_nullish() {
            String::new()
        } else {
            text(vm, value)?
        };
        charge(
            vm,
            &set_state,
            Some(id),
            &[],
            Walk::Root,
            false,
            value.len(),
        )?;
        if set_state.borrow().document.nodes[id.0]
            .kind
            .character_data()
            .is_some()
        {
            set_text_and_react(vm, &set_state, id, value)?;
            return Ok(Value::Undefined);
        }
        let result = {
            let mut state = set_state.borrow_mut();
            if state.document.is_document(id)
                || matches!(state.document.nodes[id.0].kind, NodeKind::Doctype { .. })
            {
                return Ok(Value::Undefined);
            }
            let staged = if value.is_empty() {
                Staged::default()
            } else {
                Staged::singleton(NodeKind::Text(value))
            };
            staged
                .check(state.document.nodes.len(), state.dom_bytes, LIMITS)
                .and_then(|_| {
                    let roots = staged.global_roots(state.document.nodes.len());
                    let mutation = plan(
                        &state.document,
                        id,
                        &roots,
                        None,
                        None,
                        true,
                        Some(&staged),
                        LIMITS,
                    )?;
                    staged.commit(&mut state);
                    Ok(mutation.commit(&mut state))
                })
        };
        let candidates = result.map_err(|failure| set_errors.convert(vm, failure))?;
        mutation_observers::schedule(vm, &set_state);
        dynamic_scripts::post_connection(vm, &set_state, candidates)?;
        Ok(Value::Undefined)
    });
    vm.define_accessor(prototype, "textContent", Some(get), Some(set))
}

fn html_string(vm: &mut Vm, value: Value) -> Result<String, JsError> {
    if value == Value::Null {
        Ok(String::new())
    } else {
        text(vm, value)
    }
}

pub(super) fn install_shadow_html(
    vm: &mut Vm,
    state: &Shared,
    prototype: Value,
    errors: &Errors,
) -> Result<(), JsError> {
    let get_state = state.clone();
    let get = vm.new_native_function("get innerHTML", move |vm, this, _| {
        let root = node(&get_state, &this)?;
        if !get_state.borrow().document.shadow_hosts.contains_key(&root) {
            return Err(JsError::type_error("Expected a ShadowRoot receiver"));
        }
        charge(vm, &get_state, None, &[root], Walk::Children, true, 0)?;
        Ok(get_state.borrow().document.inner_html(root).into())
    });
    let set_state = state.clone();
    let errors = errors.clone();
    let set = vm.new_native_function("set innerHTML", move |vm, this, args| {
        let root = node(&set_state, &this)?;
        let host = set_state
            .borrow()
            .document
            .shadow_hosts
            .get(&root)
            .copied()
            .ok_or_else(|| JsError::type_error("Expected a ShadowRoot receiver"))?;
        let source = html_string(vm, arg(args, 0))?;
        charge(
            vm,
            &set_state,
            Some(root),
            &[],
            Walk::Root,
            false,
            source.len().saturating_mul(4),
        )?;
        let candidates = insert_html(&set_state, root, host, false, None, None, true, &source)
            .map_err(|failure| errors.convert(vm, failure))?;
        mutation_observers::schedule(vm, &set_state);
        dynamic_scripts::post_connection(vm, &set_state, candidates)?;
        Ok(Value::Undefined)
    });
    vm.define_accessor(prototype, "innerHTML", Some(get), Some(set))
}
fn install_html(
    vm: &mut Vm,
    state: &Shared,
    prototype: Value,
    errors: &Errors,
) -> Result<(), JsError> {
    for property in ["innerHTML", "outerHTML"] {
        let get_state = state.clone();
        let get = vm.new_native_function(property, move |vm, this, _| {
            let id = element(&get_state, &this)?;
            charge(vm, &get_state, None, &[id], Walk::Ownership, true, 0)?;
            let state = get_state.borrow();
            Ok(if property == "innerHTML" {
                state.document.inner_html(id)
            } else {
                state.document.serialize_node(id)
            }
            .into())
        });
        let set_state = state.clone();
        let errors = errors.clone();
        let set = vm.new_native_function(property, move |vm, this, args| {
            let id = element(&set_state, &this)?;
            let source = html_string(vm, arg(args, 0))?;
            charge(
                vm,
                &set_state,
                Some(id),
                &[],
                Walk::Root,
                false,
                source.len().saturating_mul(4),
            )?;
            let candidates = if property == "innerHTML" {
                let template = set_state.borrow().document.tag(id) == "template";
                let parent = if template {
                    ensure_template(&set_state, id)
                        .map_err(|failure| errors.convert(vm, failure))?
                } else {
                    id
                };
                if parent != id {
                    charge(vm, &set_state, Some(parent), &[], Walk::Root, false, 0)?;
                }
                insert_html(&set_state, parent, id, false, None, None, true, &source)
                    .map_err(|failure| errors.convert(vm, failure))?
            } else {
                let parent = set_state.borrow().document.nodes[id.0].parent;
                let Some(parent) = parent else {
                    return Ok(Value::Undefined);
                };
                if set_state.borrow().document.is_document(parent) {
                    return Err(errors.convert(
                        vm,
                        Failure::Dom(
                            "NoModificationAllowedError",
                            "Cannot replace a Document's root element through outerHTML",
                        ),
                    ));
                }
                charge(vm, &set_state, Some(parent), &[], Walk::Root, false, 0)?;
                insert_html(
                    &set_state,
                    parent,
                    parent,
                    false,
                    None,
                    Some(id),
                    false,
                    &source,
                )
                .map_err(|failure| errors.convert(vm, failure))?
            };
            mutation_observers::schedule(vm, &set_state);
            dynamic_scripts::post_connection(vm, &set_state, candidates)?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype.clone(), property, Some(get), Some(set))?;
    }
    let adjacent_state = state.clone();
    let errors = errors.clone();
    method(
        vm,
        prototype,
        "insertAdjacentHTML",
        move |vm, this, args| {
            let id = element(&adjacent_state, &this)?;
            if args.len() < 2 {
                return Err(JsError::type_error(
                    "insertAdjacentHTML requires two arguments",
                ));
            }
            let position = text(vm, arg(args, 0))?.to_ascii_lowercase();
            let source = text(vm, arg(args, 1))?;
            let placement = {
                let state = adjacent_state.borrow();
                match position.as_str() {
                    "afterbegin" => Ok((id, state.document.nodes[id.0].children.first().copied())),
                    "beforeend" => Ok((id, None)),
                    "beforebegin" | "afterend" => match state.document.nodes[id.0].parent {
                        Some(parent) if !state.document.is_document(parent) => {
                            let before = if position == "beforebegin" {
                                Some(id)
                            } else {
                                let siblings = &state.document.nodes[parent.0].children;
                                siblings
                                    .iter()
                                    .position(|&child| child == id)
                                    .and_then(|index| siblings.get(index + 1).copied())
                            };
                            Ok((parent, before))
                        }
                        _ => Err(Failure::Dom(
                            "NoModificationAllowedError",
                            "This element has no modifiable parent for adjacent insertion",
                        )),
                    },
                    _ => Err(Failure::Dom(
                        "SyntaxError",
                        "Position must be beforebegin, afterbegin, beforeend, or afterend",
                    )),
                }
            };
            let (parent, before) = placement.map_err(|failure| errors.convert(vm, failure))?;
            charge(
                vm,
                &adjacent_state,
                Some(parent),
                &[],
                Walk::Root,
                false,
                source.len().saturating_mul(4),
            )?;
            let candidates = insert_html(
                &adjacent_state,
                parent,
                parent,
                true,
                before,
                None,
                false,
                &source,
            )
            .map_err(|failure| errors.convert(vm, failure))?;
            mutation_observers::schedule(vm, &adjacent_state);
            dynamic_scripts::post_connection(vm, &adjacent_state, candidates)?;
            Ok(Value::Undefined)
        },
    )
}

pub(super) fn new_fragment(
    vm: &mut Vm,
    state: &Shared,
    errors: &Errors,
    prototype: Value,
) -> Result<Value, JsError> {
    charge(vm, state, None, &[], Walk::Root, false, 1)?;
    let id = {
        let mut state = state.borrow_mut();
        let staged = Staged::singleton(NodeKind::Document);
        staged
            .check(state.document.nodes.len(), state.dom_bytes, LIMITS)
            .map_err(|failure| errors.convert(vm, failure))?;
        staged.commit(&mut state)[0]
    };
    wrap_new_node(vm, state, id, prototype)
}

/// Build a detached copy in the target arena. Source IDs stay relative to
/// their own document until staging has mapped the tree to fresh indices.
fn stage_clone(source: &Document, id: NodeId, deep: bool, target: &State) -> Checked<Staged> {
    let base = target.document.nodes.len();
    let staged = Staged::from_forest(source, &[id], deep, base, target.dom_bytes, LIMITS)?;
    check_depth(
        &View {
            doc: &target.document,
            staged: Some(&staged),
        },
        &staged.global_roots(base),
        0,
        LIMITS,
    )?;
    Ok(staged)
}

fn clone_local_node(
    vm: &mut Vm,
    host: &Shared,
    id: NodeId,
    destination: NodeId,
    deep: bool,
    errors: &Errors,
) -> Result<Value, JsError> {
    if host.borrow().document.shadow_hosts.contains_key(&id) {
        return Err(errors.exception(
            vm,
            "NotSupportedError",
            "Shadow roots cannot be cloned or imported directly",
        ));
    }
    charge(
        vm,
        host,
        None,
        &[id],
        if deep { Walk::Ownership } else { Walk::Root },
        true,
        1,
    )?;
    let mut state = host.borrow_mut();
    let mut staged = stage_clone(&state.document, id, deep, &state)
        .map_err(|failure| errors.convert(vm, failure))?;
    if let Some(hook) = &state.form_clone_hook {
        hook(vm, &staged.source_ids, state.document.nodes.len())?;
    }
    staged.copy_script_flags(|source| state.dynamic_scripts.has_started(source));
    let clone = staged.commit(&mut state)[0];
    state.document.adopt(clone, destination);
    drop(state);
    wrap(vm, host, Some(clone))
}

fn import_subtree(vm: &mut Vm, options: &Value) -> Result<bool, JsError> {
    if matches!(options, Value::Null) {
        return Ok(true); // An empty dictionary.
    }
    if options.as_object().is_none() {
        return Ok(options.is_truthy());
    }
    let registry = vm.get_property(options.clone(), "customElementRegistry")?;
    if !registry.is_undefined() {
        return Err(JsError::type_error("Expected a CustomElementRegistry"));
    }
    Ok(!vm.get_property(options.clone(), "selfOnly")?.is_truthy())
}

enum ImportSource {
    Document,
    Local(NodeId),
    Frame(Rc<frames::FrameNode>),
    Xml,
}

impl ImportSource {
    fn from_value(vm: &Vm, state: &Shared, value: &Value) -> Result<Self, JsError> {
        if let Some(source) = vm.host_data::<frames::FrameNode>(value) {
            return Ok(Self::Frame(source));
        }
        if vm.host_data::<frames::FrameWindow>(value).is_some() {
            return Ok(Self::Document);
        }
        if let Some(kind) = fetch::xml_node_type(vm, value) {
            return Ok(if kind == 9 { Self::Document } else { Self::Xml });
        }
        let id = node(state, value)?;
        Ok(if state.borrow().document.is_document(id) {
            Self::Document
        } else {
            Self::Local(id)
        })
    }
}

fn import_frame_node(
    vm: &mut Vm,
    host: &Shared,
    source: &frames::FrameNode,
    destination: NodeId,
    deep: bool,
    errors: &Errors,
) -> Result<Value, JsError> {
    let staged = source.with_document(|doc, id, started| {
        vm.charge_work(tree_work(
            doc,
            None,
            &[id],
            if deep { Walk::Ownership } else { Walk::Root },
            true,
            1,
        ))?;
        let mut staged = stage_clone(doc, id, deep, &host.borrow())
            .map_err(|failure| errors.convert(vm, failure))?;
        staged.copy_script_flags(|id| started.contains(&id));
        Ok::<_, JsError>(staged)
    })?;
    let clone = staged.commit(&mut host.borrow_mut())[0];
    host.borrow_mut().document.adopt(clone, destination);
    wrap(vm, host, Some(clone))
}

fn install_cloning(
    vm: &mut Vm,
    state: &Shared,
    prototype: Value,
    document: Value,
    errors: &Errors,
) -> Result<(), JsError> {
    let content_state = state.clone();
    let content_errors = errors.clone();
    let template_prototype = state.borrow().template_prototype.clone();
    getter(vm, template_prototype, "content", move |vm, this, _| {
        let id = node(&content_state, &this)?;
        charge(vm, &content_state, None, &[], Walk::Root, false, 1)?;
        let is_template = {
            let s = content_state.borrow();
            s.document.tag(id) == "template"
                && s.document.namespace(id) == crate::dom::Namespace::Html
        };
        if !is_template {
            return Err(JsError::type_error("Expected HTMLTemplateElement"));
        }
        let content = ensure_template(&content_state, id)
            .map_err(|failure| content_errors.convert(vm, failure))?;
        wrap(vm, &content_state, Some(content))
    })?;
    let clone_state = state.clone();
    let clone_errors = errors.clone();
    method(vm, prototype, "cloneNode", move |vm, this, args| {
        let id = node(&clone_state, &this)?;
        if clone_state.borrow().document.is_document(id) {
            return Err(unsupported(
                "Cloning a Document with a separate owner realm is not implemented",
            ));
        }
        let owner = clone_state.borrow().document.node_document(id);
        clone_local_node(
            vm,
            &clone_state,
            id,
            owner,
            arg(args, 0).is_truthy(),
            &clone_errors,
        )
    })?;
    let import_state = state.clone();
    let import_errors = errors.clone();
    webidl::operation(
        vm,
        document.clone(),
        "importNode",
        1,
        move |vm, this, args| {
            let receiver = super::document_node(&import_state, &this)?;
            webidl::required(args, 1, "Document.importNode")?;
            let source = ImportSource::from_value(vm, &import_state, &args[0])?;
            let deep = import_subtree(vm, &arg(args, 1))?;
            match source {
                ImportSource::Document => Err(import_errors.exception(
                    vm,
                    "NotSupportedError",
                    "A Document cannot be imported",
                )),
                ImportSource::Local(id) => {
                    clone_local_node(vm, &import_state, id, receiver, deep, &import_errors)
                }
                ImportSource::Frame(source) => {
                    import_frame_node(vm, &import_state, &source, receiver, deep, &import_errors)
                }
                ImportSource::Xml => Err(unsupported(
                    "Importing a read-only XML response node into an HTML document is not implemented",
                )),
            }
        },
    )?;
    let fragment_state = state.clone();
    let fragment_errors = errors.clone();
    method(
        vm,
        document.clone(),
        "createDocumentFragment",
        move |vm, this, _| {
            let receiver = super::document_node(&fragment_state, &this)?;
            let prototype = fragment_state.borrow().fragment_prototype.clone();
            let fragment = new_fragment(vm, &fragment_state, &fragment_errors, prototype)?;
            let id = node(&fragment_state, &fragment)?;
            fragment_state.borrow_mut().document.adopt(id, receiver);
            Ok(fragment)
        },
    )?;
    // Materialize template contents at creation, so subsequent setters never
    // need to spend a hidden node before their atomic insertion preflight.
    let create_state = state.clone();
    let create_errors = errors.clone();
    method(vm, document, "createElement", move |vm, this, args| {
        let receiver = super::document_node(&create_state, &this)?;
        let html = create_state.borrow().document.is_html_document(receiver);
        if args.is_empty() {
            return Err(JsError::type_error(
                "createElement requires an element name",
            ));
        }
        let name = vm.to_string(&arg(args, 0))?;
        let scalar_name = char::decode_utf16(name.as_utf16().iter().copied()).all(|ch| ch.is_ok());
        let mut tag = name.to_string_lossy();
        if html {
            tag.make_ascii_lowercase();
        }
        charge(
            vm,
            &create_state,
            None,
            &[],
            Walk::Root,
            false,
            tag.len().saturating_add(2),
        )?;
        if !scalar_name || !element_name(&tag) {
            return Err(create_errors.convert(
                vm,
                Failure::Dom("InvalidCharacterError", "The element name is invalid"),
            ));
        }
        let result = {
            let mut state = create_state.borrow_mut();
            let template = html && tag == "template";
            let mut staged = Staged::singleton(NodeKind::Element {
                tag,
                attrs: Default::default(),
            });
            if template {
                staged.nodes.push(Node {
                    kind: NodeKind::Document,
                    parent: None,
                    children: vec![],
                });
                staged.templates.insert(NodeId(0), NodeId(1));
            }
            staged
                .check(state.document.nodes.len(), state.dom_bytes, LIMITS)
                .map(|_| staged.commit(&mut state)[0])
        };
        let id = result.map_err(|failure| create_errors.convert(vm, failure))?;
        if receiver == create_state.borrow().document.root {
            super::custom_elements::created(vm, &create_state, id)
        } else {
            {
                let mut state = create_state.borrow_mut();
                if !html {
                    state
                        .document
                        .namespaces
                        .insert(id, crate::dom::Namespace::Null);
                }
                state.document.adopt(id, receiver);
            }
            wrap(vm, &create_state, Some(id))
        }
    })
}

fn element_name(name: &str) -> bool {
    // DOM createElement validates XML Name, including XML 1.0 Fifth Edition's
    // explicit Unicode ranges, rather than Rust's differing alphabetic classes.
    fn start(ch: char) -> bool {
        matches!(ch as u32, 0x3a | 0x41..=0x5a | 0x5f | 0x61..=0x7a | 0xc0..=0xd6 | 0xd8..=0xf6 | 0xf8..=0x2ff | 0x370..=0x37d | 0x37f..=0x1fff | 0x200c..=0x200d | 0x2070..=0x218f | 0x2c00..=0x2fef | 0x3001..=0xd7ff | 0xf900..=0xfdcf | 0xfdf0..=0xfffd | 0x10000..=0xeffff)
    }
    let mut chars = name.chars();
    chars.next().is_some_and(start) && chars.all(|ch| start(ch) || matches!(ch as u32, 0x2d | 0x2e | 0x30..=0x39 | 0xb7 | 0x300..=0x36f | 0x203f..=0x2040))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(doc: &mut Document, parent: NodeId, tag: &str) -> NodeId {
        doc.append(
            parent,
            NodeKind::Element {
                tag: tag.into(),
                attrs: Default::default(),
            },
        )
    }
    fn small() -> Limits {
        Limits {
            nodes: 12,
            bytes: 32,
            depth: 4,
        }
    }

    #[test]
    fn preflight_reparent_depth_is_atomic_with_a_small_budget() {
        let mut document = Document::default();
        let root = document.root;
        let a = element(&mut document, root, "a");
        let b = element(&mut document, a, "b");
        let c = element(&mut document, b, "c");
        let branch = element(&mut document, a, "branch");
        let leaf = element(&mut document, branch, "leaf");
        let before = document.serialize();
        let result = plan(&document, c, &[branch], None, None, false, None, small());
        assert!(matches!(
            result,
            Err(Failure::Fatal(JsError {
                kind: ErrorKind::HeapLimit,
                ..
            }))
        ));
        assert_eq!(document.serialize(), before);
        assert_eq!(document.nodes[branch.0].parent, Some(a));
        assert_eq!(document.nodes[leaf.0].parent, Some(branch));
    }

    #[test]
    fn import_node_and_byte_preflight_never_changes_the_target_arena() {
        let mut target = Document::default();
        let root = target.root;
        element(&mut target, root, "target");
        let source = html::parse_fragment("<b>123456789</b><i>X</i>", "div", false);
        let roots = &source.nodes[source.root.0].children;
        let before = target.serialize();
        let count = target.nodes.len();
        assert!(matches!(
            Staged::from_forest(
                &source,
                roots,
                true,
                count,
                0,
                Limits {
                    nodes: 4,
                    ..small()
                }
            ),
            Err(Failure::Fatal(JsError {
                kind: ErrorKind::HeapLimit,
                ..
            }))
        ));
        assert!(matches!(
            Staged::from_forest(&source, roots, true, count, 30, small()),
            Err(Failure::Fatal(JsError {
                kind: ErrorKind::HeapLimit,
                ..
            }))
        ));
        assert_eq!(target.nodes.len(), count);
        assert_eq!(target.serialize(), before);
    }

    #[test]
    fn template_ownership_depth_and_cycle_preflight_are_atomic() {
        let document = html::parse(
            "<!doctype html><body><main><section></section><template><b><i>X</i></b></template></main>",
        );
        let section = document.elements("section").next().unwrap();
        let template = document.elements("template").next().unwrap();
        let content = document.template_contents[&template];
        let before = document.serialize();
        assert!(matches!(
            plan(
                &document,
                section,
                &[template],
                None,
                None,
                false,
                None,
                small()
            ),
            Err(Failure::Fatal(JsError {
                kind: ErrorKind::HeapLimit,
                ..
            }))
        ));
        assert!(matches!(
            plan(
                &document,
                content,
                &[template],
                None,
                None,
                false,
                None,
                LIMITS
            ),
            Err(Failure::Dom("HierarchyRequestError", _))
        ));
        assert_eq!(document.serialize(), before);
        // A final child-order plan can be inspected without touching even one
        // parent pointer; its preflight is independent from the commit path.
        let main = document.elements("main").next().unwrap();
        let order = plan(
            &document,
            main,
            &[template],
            Some(section),
            None,
            false,
            None,
            LIMITS,
        )
        .unwrap();
        assert_eq!(order.children, vec![template, section]);
        assert_eq!(document.serialize(), before);
    }

    #[test]
    fn invalid_document_fragment_preflight_preserves_all_fragment_children() {
        let mut document = html::parse("<!doctype html><body></body>");
        let fragment = document.append(document.root, NodeKind::Document);
        detach(&mut document, fragment);
        let first = element(&mut document, fragment, "first");
        let second = element(&mut document, fragment, "second");
        assert!(matches!(
            plan(
                &document,
                document.root,
                &[fragment],
                None,
                None,
                false,
                None,
                LIMITS
            ),
            Err(Failure::Dom("HierarchyRequestError", _))
        ));
        assert_eq!(document.nodes[fragment.0].children, vec![first, second]);
        assert_eq!(document.nodes[first.0].parent, Some(fragment));
        assert_eq!(document.nodes[second.0].parent, Some(fragment));
    }
}
