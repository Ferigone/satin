//! Resumable document parsing and parser insertion-point ownership. The browser
//! drives script execution/resources; this module never calls a JavaScript VM.
use super::{
    MAX_DOM_BYTES, MAX_INPUT_BYTES, MAX_NODES, MAX_OPEN_ELEMENTS,
    servo_stream::Parser,
    tree_builder::{CustomElementTest, PendingElement, Tree, kind_bytes},
};
use crate::dom::mutation::MutationSink;
use crate::dom::{Document, Namespace, NodeId, NodeKind};
use std::collections::{BTreeSet, HashSet, VecDeque};

pub const MAX_STREAM_WORK: usize = 64 * 1024 * 1024;
pub const MAX_WRITE_CALLS: usize = 4096;
pub const MAX_SCRIPT_NESTING: usize = 32;
pub const STREAM_ADVANCE_BYTES: usize = 16 * 1024;
pub const MAX_STREAM_INPUT_PIECES: usize = MAX_WRITE_CALLS * 2 + MAX_SCRIPT_NESTING + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WriteId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParserScriptKind {
    ClassicBlocking,
    ClassicDefer,
    ClassicAsync,
    Module,
    ImportMap,
    Data,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParserScript {
    pub node: NodeId,
    pub kind: ParserScriptKind,
    pub external: bool,
    /// Prospective execution nesting, recalculated when a pending script is
    /// delivered again after an outer script has returned.
    pub depth: usize,
    pub parser_inserted: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParserProgress {
    pub new_nodes: std::ops::Range<usize>,
    /// Closed current element (including completed raw style/title/script).
    pub closed: Option<NodeId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParserEvent {
    NeedInput,
    Progress(ParserProgress),
    Script(ParserScript),
    Paused { script: NodeId },
    WriteComplete(WriteId),
    Complete,
    Truncated,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HtmlStreamError {
    DocumentLeased,
    DocumentNotLeased,
    InputClosed,
    NoInsertionPoint,
    ScriptMismatch,
    NestedExternalPending,
    InvalidDocument,
    ResourceLimit,
}
impl std::fmt::Display for HtmlStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTML parser {self:?}")
    }
}
impl std::error::Error for HtmlStreamError {}
type Result<T> = std::result::Result<T, HtmlStreamError>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HtmlStreamReport {
    pub input_bytes: usize,
    pub work: usize,
    pub write_calls: usize,
    pub scripts_prepared: usize,
    pub truncated: bool,
    pub completed: bool,
}
enum Input {
    Source { text: String, offset: usize },
    WriteEnd(WriteId),
    Point(u64),
}
struct ActiveScript {
    node: NodeId,
    point: u64,
}
struct Lease {
    root: NodeId,
    nodes: usize,
    scripting_enabled: bool,
    references: Vec<(NodeId, Namespace, String)>,
}

pub struct HtmlStream {
    tree: Tree,
    parser: Parser,
    input: VecDeque<Input>,
    script_created: bool,
    closed: bool,
    next_id: u64,
    active: Vec<ActiveScript>,
    pending: Option<ParserScript>,
    parser_scripts: HashSet<NodeId>,
    lease: Option<Lease>,
    report: HtmlStreamReport,
}
impl HtmlStream {
    pub fn new(scripting_enabled: bool) -> Self {
        Self {
            tree: Tree::new(scripting_enabled),
            parser: Parser::new(scripting_enabled),
            input: VecDeque::new(),
            script_created: false,
            closed: false,
            next_id: 1,
            active: vec![],
            pending: None,
            parser_scripts: HashSet::new(),
            lease: None,
            report: HtmlStreamReport::default(),
        }
    }
    pub(crate) fn script_created(scripting_enabled: bool) -> Self {
        let mut stream = Self::new(scripting_enabled);
        stream.script_created = true;
        stream
    }
    pub(crate) fn is_script_created(&self) -> bool {
        self.script_created
    }
    pub fn report(&self) -> HtmlStreamReport {
        HtmlStreamReport {
            work: self.tree.work,
            ..self.report
        }
    }
    pub fn document(&self) -> Option<&Document> {
        self.lease.is_none().then_some(&self.tree.doc)
    }
    pub(crate) fn set_mutation_sink(&mut self, sink: Option<MutationSink>) {
        self.tree.mutation_sink = sink;
    }
    pub(crate) fn set_custom_element_test(&mut self, test: Option<CustomElementTest>) {
        self.tree.custom_element_test = test;
    }
    pub(crate) fn take_custom_element(&mut self) -> Option<PendingElement> {
        self.tree.pending_element.take()
    }
    pub(crate) fn has_custom_element(&self) -> bool {
        self.tree.pending_element.is_some()
    }
    pub(crate) fn finish_custom_element(
        &mut self,
        original: NodeId,
        element: NodeId,
        parent: NodeId,
        before: Option<NodeId>,
    ) {
        self.parser.finish_custom(original, element);
        self.tree.attach(element, parent, before);
    }
    /// Retained arena strings, including names duplicated in namespace metadata.
    /// While leased, this is the count at the time of take_document().
    pub fn document_bytes(&self) -> usize {
        self.tree.string_bytes
    }
    pub fn is_parser_inserted_script(&self, node: NodeId) -> bool {
        self.parser_scripts.contains(&node)
    }
    pub fn script_nesting(&self) -> usize {
        self.active.len()
    }
    pub(crate) fn has_pending_script(&self) -> bool {
        self.pending.is_some()
    }
    pub fn has_insertion_point(&self) -> bool {
        (!self.active.is_empty() || self.script_created)
            && !self.report.completed
            && !self.report.truncated
    }

    fn halt(&mut self) {
        self.report.truncated = true;
        self.tree.halted = true;
        if self.lease.is_none() {
            self.tree.doc.truncated = true;
        }
    }
    fn work(&mut self, amount: usize) -> Result<()> {
        if !self.tree.charge_work(amount) {
            self.halt();
            return Err(HtmlStreamError::ResourceLimit);
        }
        Ok(())
    }
    fn bytes(&mut self, amount: usize) -> Result<()> {
        if self.report.truncated || self.report.input_bytes.saturating_add(amount) > MAX_INPUT_BYTES
        {
            self.halt();
            return Err(HtmlStreamError::ResourceLimit);
        }
        self.work(amount)?;
        self.report.input_bytes += amount;
        Ok(())
    }
    /// Append decoded network text. EOF is separate; a chunk boundary never
    /// terminates a tag, reference, comment, raw-text state or script escape.
    pub fn feed(&mut self, text: &str) -> Result<()> {
        if self.closed || self.report.completed {
            return Err(HtmlStreamError::InputClosed);
        }
        self.bytes(text.len())?;
        if !text.is_empty() {
            if let Some(Input::Source { text: previous, .. }) = self.input.back_mut() {
                previous.push_str(text);
            } else {
                self.input.push_back(Input::Source {
                    text: text.into(),
                    offset: 0,
                });
            }
        }
        Ok(())
    }
    /// End network input. Active script insertion points and written pieces are
    /// drained before the tokenizer can observe the real EOF.
    pub fn finish(&mut self) -> Result<()> {
        if self.report.truncated {
            return Err(HtmlStreamError::ResourceLimit);
        }
        self.closed = true;
        Ok(())
    }
    pub fn take_document(&mut self) -> Result<Document> {
        if self.lease.is_some() {
            return Err(HtmlStreamError::DocumentLeased);
        }
        let references = if self.tree.halted || self.report.completed {
            vec![]
        } else {
            self.parser
                .references()
                .into_iter()
                .map(|id| {
                    (
                        id,
                        self.tree.doc.namespace(id),
                        self.tree.doc.tag(id).to_owned(),
                    )
                })
                .collect()
        };
        self.lease = Some(Lease {
            root: self.tree.doc.root,
            nodes: self.tree.doc.nodes.len(),
            scripting_enabled: self.tree.doc.scripting_enabled,
            references,
        });
        Ok(std::mem::take(&mut self.tree.doc))
    }
    /// Return the very same arena after legal DOM mutations. Arena indices must
    /// not be compacted/reused. Open-element/formatting/form pointers deliberately
    /// remain attached to their original nodes even if those nodes were removed
    /// or reparented by script. The returned document is retained on an error so
    /// the embedding can recover it through take_document() after aborting.
    pub fn restore_document(&mut self, document: Document) -> Result<()> {
        let lease = self.receive_document(document)?;
        let result = self
            .validate_lease(&lease)
            .and_then(|()| self.validate_document());
        self.finish_restore(result)
    }

    /// The browser's DOM host already validates its mutations and maintains
    /// exact string accounting. Avoid rescanning that entire arena on each UI
    /// batch, while still checking the parser's live node identities.
    #[allow(dead_code, reason = "Used by the browser parser driver integration")]
    pub(crate) fn restore_document_from_host(
        &mut self,
        document: Document,
        retained_bytes: usize,
    ) -> Result<()> {
        let lease = self.receive_document(document)?;
        let bytes = (retained_bytes <= MAX_DOM_BYTES)
            .then_some(retained_bytes)
            .ok_or(HtmlStreamError::ResourceLimit);
        let result = self.validate_lease(&lease).and(bytes);
        self.finish_restore(result)
    }

    fn receive_document(&mut self, document: Document) -> Result<Lease> {
        let lease = self
            .lease
            .take()
            .ok_or(HtmlStreamError::DocumentNotLeased)?;
        self.tree.doc = document;
        Ok(lease)
    }

    fn finish_restore(&mut self, result: Result<usize>) -> Result<()> {
        match result {
            Ok(bytes) => {
                self.tree.string_bytes = bytes;
                if self.report.truncated {
                    self.tree.doc.truncated = true;
                }
                Ok(())
            }
            Err(error) => {
                self.halt();
                Err(error)
            }
        }
    }
    fn validate_lease(&self, lease: &Lease) -> Result<()> {
        let doc = &self.tree.doc;
        if doc.root != lease.root
            || doc.scripting_enabled != lease.scripting_enabled
            || doc.nodes.len() < lease.nodes
            || doc.root.0 >= doc.nodes.len()
            || !matches!(doc.nodes[doc.root.0].kind, NodeKind::Document)
            || doc.nodes[doc.root.0].parent.is_some()
        {
            return Err(HtmlStreamError::InvalidDocument);
        }
        if doc.nodes.len() > MAX_NODES {
            return Err(HtmlStreamError::ResourceLimit);
        }
        for (id, namespace, tag) in &lease.references {
            if id.0 >= doc.nodes.len() || doc.namespace(*id) != *namespace || doc.tag(*id) != tag {
                return Err(HtmlStreamError::InvalidDocument);
            }
        }
        Ok(())
    }
    fn validate_document(&mut self) -> Result<usize> {
        let doc = &self.tree.doc;
        let count = doc.nodes.len();
        let mut incoming = vec![false; count];
        let mut parents = doc.nodes.iter().map(|n| n.parent).collect::<Vec<_>>();
        let mut bytes = 0usize;
        let mut edges = 0usize;
        for (index, node) in doc.nodes.iter().enumerate() {
            bytes = bytes.saturating_add(kind_bytes(&node.kind));
            edges = edges.saturating_add(node.children.len());
            if bytes > MAX_DOM_BYTES || edges > count {
                return Err(HtmlStreamError::ResourceLimit);
            }
            if node.parent.is_some_and(|id| id.0 >= count) {
                return Err(HtmlStreamError::InvalidDocument);
            }
            for &child in &node.children {
                if child.0 >= count
                    || incoming[child.0]
                    || doc.nodes[child.0].parent != Some(NodeId(index))
                {
                    return Err(HtmlStreamError::InvalidDocument);
                }
                incoming[child.0] = true;
            }
        }
        for (index, node) in doc.nodes.iter().enumerate() {
            if incoming[index] != node.parent.is_some() {
                return Err(HtmlStreamError::InvalidDocument);
            }
        }
        for &id in doc.namespaces.keys() {
            if id.0 >= count || !matches!(doc.nodes[id.0].kind, NodeKind::Element { .. }) {
                return Err(HtmlStreamError::InvalidDocument);
            }
        }
        for (id, name) in doc.attribute_namespaces.keys() {
            if id.0 >= count || doc.attr(*id, name).is_none() {
                return Err(HtmlStreamError::InvalidDocument);
            }
            bytes = bytes.saturating_add(name.len());
        }
        if bytes > MAX_DOM_BYTES {
            return Err(HtmlStreamError::ResourceLimit);
        }
        let mut contents = BTreeSet::new();
        for (&template, &content) in &doc.template_contents {
            if template.0 >= count
                || content.0 >= count
                || content == doc.root
                || doc.namespace(template) != Namespace::Html
                || doc.tag(template) != "template"
                || !matches!(doc.nodes[content.0].kind, NodeKind::Document)
                || doc.nodes[content.0].parent.is_some()
                || !contents.insert(content)
            {
                return Err(HtmlStreamError::InvalidDocument);
            }
            parents[content.0] = Some(template);
        }
        for (&host, tree) in &doc.shadow_roots {
            let root = tree.root;
            if host.0 >= count
                || root.0 >= count
                || root == doc.root
                || doc.namespace(host) != Namespace::Html
                || !matches!(doc.nodes[host.0].kind, NodeKind::Element { .. })
                || !matches!(doc.nodes[root.0].kind, NodeKind::Document)
                || doc.nodes[root.0].parent.is_some()
                || !contents.insert(root)
                || doc.shadow_hosts.get(&root) != Some(&host)
            {
                return Err(HtmlStreamError::InvalidDocument);
            }
            parents[root.0] = Some(host);
        }
        if doc.shadow_roots.len() != doc.shadow_hosts.len() {
            return Err(HtmlStreamError::InvalidDocument);
        }
        let mut depth = vec![0usize; count];
        let mut visiting = vec![false; count];
        for index in 0..count {
            if depth[index] != 0 {
                continue;
            }
            let mut chain = vec![];
            let mut current = Some(NodeId(index));
            let mut base = 0;
            while let Some(id) = current {
                if depth[id.0] != 0 {
                    base = depth[id.0];
                    break;
                }
                if visiting[id.0] {
                    return Err(HtmlStreamError::InvalidDocument);
                }
                if chain.len() > MAX_OPEN_ELEMENTS * 2 + 2 {
                    return Err(HtmlStreamError::ResourceLimit);
                }
                visiting[id.0] = true;
                chain.push(id);
                current = parents[id.0];
            }
            while let Some(id) = chain.pop() {
                base += 1;
                if base > MAX_OPEN_ELEMENTS * 2 + 2 {
                    return Err(HtmlStreamError::ResourceLimit);
                }
                depth[id.0] = base;
                visiting[id.0] = false;
            }
        }
        self.work(
            bytes
                .saturating_add(count.saturating_mul(4))
                .saturating_add(edges),
        )?;
        Ok(bytes)
    }
    pub fn begin_script(&mut self, node: NodeId) -> Result<()> {
        if self.report.truncated {
            return Err(HtmlStreamError::ResourceLimit);
        }
        let Some(script) = &self.pending else {
            return Err(HtmlStreamError::ScriptMismatch);
        };
        if script.node != node || script.kind != ParserScriptKind::ClassicBlocking {
            return Err(HtmlStreamError::ScriptMismatch);
        }
        if script.external && !self.active.is_empty() {
            return Err(HtmlStreamError::NestedExternalPending);
        }
        if self.active.len() >= MAX_SCRIPT_NESTING || self.input.len() >= MAX_STREAM_INPUT_PIECES {
            self.halt();
            return Err(HtmlStreamError::ResourceLimit);
        }
        self.work(1)?;
        self.pending = None;
        let point = self.next_id;
        self.next_id += 1;
        self.input.push_front(Input::Point(point));
        self.active.push(ActiveScript { node, point });
        Ok(())
    }
    pub fn skip_script(&mut self, node: NodeId) -> Result<()> {
        if self
            .pending
            .as_ref()
            .is_none_or(|script| script.node != node)
        {
            return Err(HtmlStreamError::ScriptMismatch);
        }
        self.pending = None;
        Ok(())
    }
    pub fn resume_script(&mut self, node: NodeId) -> Result<()> {
        let Some(script) = self.active.last() else {
            return Err(HtmlStreamError::ScriptMismatch);
        };
        if script.node != node {
            return Err(HtmlStreamError::ScriptMismatch);
        }
        let point = script.point;
        let Some(index) = self
            .input
            .iter()
            .position(|piece| matches!(piece,Input::Point(id) if *id==point))
        else {
            return Err(HtmlStreamError::ScriptMismatch);
        };
        self.input.remove(index);
        self.active.pop();
        if !self.report.truncated {
            self.work(index + 1)?;
        }
        Ok(())
    }
    /// Insert before the executing script's marker, or at the end of a
    /// script-created stream. Pump until WriteComplete or a blocking script.
    pub fn write(&mut self, text: &str) -> Result<WriteId> {
        if !self.has_insertion_point() {
            return Err(HtmlStreamError::NoInsertionPoint);
        }
        let pieces = if text.is_empty() { 1 } else { 2 };
        if self.report.write_calls >= MAX_WRITE_CALLS
            || self.input.len() + pieces > MAX_STREAM_INPUT_PIECES
        {
            self.halt();
            return Err(HtmlStreamError::ResourceLimit);
        }
        let index = if let Some(script) = self.active.last() {
            self.input
                .iter()
                .position(|piece| matches!(piece, Input::Point(id) if *id == script.point))
                .ok_or(HtmlStreamError::NoInsertionPoint)?
        } else {
            self.input.len()
        };
        self.work(index.saturating_mul(pieces + 1) + 1)?;
        self.bytes(text.len())?;
        self.report.write_calls += 1;
        let id = WriteId(self.next_id);
        self.next_id += 1;
        self.input.insert(index, Input::WriteEnd(id));
        if !text.is_empty() {
            self.input.insert(
                index,
                Input::Source {
                    text: text.into(),
                    offset: 0,
                },
            );
        }
        Ok(id)
    }
    pub fn next_event(&mut self) -> Result<ParserEvent> {
        if self.lease.is_some() {
            return Err(HtmlStreamError::DocumentLeased);
        }
        loop {
            if self.report.truncated || self.tree.halted {
                self.halt();
                return Ok(ParserEvent::Truncated);
            }
            if let Some(progress) = self.parser.pop_progress() {
                for index in progress.new_nodes.clone() {
                    let id = NodeId(index);
                    if self.tree.doc.namespace(id) == Namespace::Html && self.tree.doc.tag(id) == "script" {
                        self.parser_scripts.insert(id);
                    }
                }
                return Ok(ParserEvent::Progress(progress));
            }
            if self.report.completed {
                return Ok(ParserEvent::Complete);
            }
            if let Some(script) = &mut self.pending {
                // A host checkpoint can mutate the attributes before preparation.
                *script = classify(&self.tree.doc, script.node, self.active.len() + 1);
                return Ok(ParserEvent::Script(script.clone()));
            }
            match self.input.front_mut() {
                Some(Input::Point(_)) => {
                    return Ok(ParserEvent::Paused {
                        script: self
                            .active
                            .last()
                            .ok_or(HtmlStreamError::ScriptMismatch)?
                            .node,
                    });
                }
                Some(Input::WriteEnd(id)) => {
                    let id = *id;
                    self.input.pop_front();
                    return Ok(ParserEvent::WriteComplete(id));
                }
                Some(Input::Source { text, offset }) => {
                    if *offset == text.len() {
                        self.input.pop_front();
                        continue;
                    }
                    let start = *offset;
                    let mut end = (start + STREAM_ADVANCE_BYTES).min(text.len());
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    let chunk = &text[start..end];
                    let (script, remaining) = self.parser.feed(&mut self.tree, chunk);
                    let amount = end - start - remaining.len().min(end - start);
                    if chunk.ends_with(&remaining) {
                        *offset = end - remaining.len();
                    } else {
                        *offset = end;
                        self.input.push_front(Input::Source { text: remaining, offset: 0 });
                    }
                    self.work(amount.saturating_mul(2))?;
                    if let Some(node) = script {
                        self.report.scripts_prepared += 1;
                        self.pending = Some(classify(&self.tree.doc, node, self.active.len() + 1));
                    } else if amount >= STREAM_ADVANCE_BYTES && self.parser.progress_empty() {
                        let count = self.tree.doc.nodes.len();
                        return Ok(ParserEvent::Progress(ParserProgress {
                            new_nodes: count..count,
                            closed: None,
                        }));
                    }
                }
                None if self.closed => {
                    self.parser.finish(&mut self.tree);
                    self.report.completed = true;
                }
                None => return Ok(ParserEvent::NeedInput),
            }
        }
    }
}

fn classify(doc: &Document, node: NodeId, depth: usize) -> ParserScript {
    let external = doc.attr(node, "src").is_some();
    let kind = match doc.attr(node, "type") {
        Some(value) if value.eq_ignore_ascii_case("module") => ParserScriptKind::Module,
        Some(value) if value.eq_ignore_ascii_case("importmap") => ParserScriptKind::ImportMap,
        value => {
            let classic = if let Some(value) = value {
                value.is_empty() || javascript_type(ascii_trim(value), false)
            } else {
                doc.attr(node, "language")
                    .is_none_or(|value| value.is_empty() || javascript_type(value, true))
            };
            if !classic || legacy_event_disables_script(doc, node) {
                ParserScriptKind::Data
            } else if external && doc.attr(node, "async").is_some() {
                ParserScriptKind::ClassicAsync
            } else if external && doc.attr(node, "defer").is_some() {
                ParserScriptKind::ClassicDefer
            } else {
                ParserScriptKind::ClassicBlocking
            }
        }
    };
    ParserScript {
        node,
        kind,
        external,
        depth,
        parser_inserted: true,
    }
}
fn ascii_trim(value: &str) -> &str {
    value.trim_matches(['\t', '\n', '\x0c', '\r', ' '])
}

fn legacy_event_disables_script(doc: &Document, node: NodeId) -> bool {
    let (Some(target), Some(event)) = (doc.attr(node, "for"), doc.attr(node, "event")) else {
        return false;
    };
    let event = ascii_trim(event);
    !ascii_trim(target).eq_ignore_ascii_case("window")
        || !(event.eq_ignore_ascii_case("onload") || event.eq_ignore_ascii_case("onload()"))
}

fn javascript_type(value: &str, legacy_language: bool) -> bool {
    const TEXT_TYPES: &[&str] = &[
        "ecmascript",
        "javascript",
        "javascript1.0",
        "javascript1.1",
        "javascript1.2",
        "javascript1.3",
        "javascript1.4",
        "javascript1.5",
        "jscript",
        "livescript",
        "x-ecmascript",
        "x-javascript",
    ];
    if legacy_language {
        return TEXT_TYPES
            .iter()
            .any(|kind| value.eq_ignore_ascii_case(kind));
    }
    let Some((section, name)) = value.split_once('/') else {
        return false;
    };
    if section.eq_ignore_ascii_case("text") {
        TEXT_TYPES
            .iter()
            .any(|kind| name.eq_ignore_ascii_case(kind))
    } else {
        section.eq_ignore_ascii_case("application")
            && ["ecmascript", "javascript", "x-ecmascript", "x-javascript"]
                .iter()
                .any(|kind| name.eq_ignore_ascii_case(kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_lease_byte_accounting_carries_namespaces_and_detached_nodes() {
        let mut parser = HtmlStream::new(true);
        parser.feed("<svg><a xlink:href='#marker'>x</a></svg><template><i>t</i></template><script>x</script>").unwrap();
        loop {
            match parser.next_event().unwrap() {
                ParserEvent::Script(_) => break,
                ParserEvent::Progress(_) => {}
                event => panic!("expected a script event, got {event:?}"),
            }
        }
        let before = parser.document_bytes();
        let mut document = parser.take_document().unwrap();
        let measured = document
            .nodes
            .iter()
            .map(|node| kind_bytes(&node.kind))
            .sum::<usize>()
            + document
                .attribute_namespaces
                .keys()
                .map(|(_, name)| name.len())
                .sum::<usize>();
        assert_eq!(before, measured);
        let root = document.root;
        document.append(root, NodeKind::Comment("host mutation".into()));
        parser
            .restore_document_from_host(document, before + "host mutation".len())
            .unwrap();
        assert_eq!(parser.document_bytes(), before + "host mutation".len());
    }
}
