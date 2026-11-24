//! HTML insertion modes, stack of open elements and active formatting elements.
use super::foreign;
use super::tokenizer::{Content, Doctype, Tag, Token, Tokenizer, space};
use super::{
    FragmentContext, MAX_DOM_BYTES, MAX_FORMATTING_BYTES, MAX_FORMATTING_ENTRIES, MAX_NODES,
    MAX_OPEN_ELEMENTS, MAX_STREAM_WORK,
};
use crate::dom::mutation::{Mutation, MutationSink, siblings};
use crate::dom::{Document, Namespace, Node, NodeId, NodeKind, QuirksMode, is_void};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    InHeadNoscript,
    AfterHead,
    InBody,
    Text,
    InTable,
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InTemplate,
    AfterBody,
    InFrameset,
    AfterFrameset,
    AfterAfterBody,
    AfterAfterFrameset,
}

#[derive(Clone, Copy)]
enum Scope {
    Normal,
    Button,
    List,
    Table,
}

pub(super) fn parse(source: &str, scripting: bool) -> Document {
    let mut tokenizer = Tokenizer::new(source);
    let mut tree = Tree::new(scripting);
    tree.run(&mut tokenizer);
    tree.doc
}

pub(super) fn parse_fragment(source: &str, context: FragmentContext<'_>) -> Document {
    let mut tokenizer = Tokenizer::new(source);
    let mut tree = Tree::new(context.scripting_enabled);
    let tag = if context.namespace == Namespace::Html {
        context.tag.to_ascii_lowercase()
    } else {
        context.tag.to_owned()
    };
    // No start tag has been emitted in a fragment tokenizer. Consequently an
    // apparent closing tag for the context cannot be an appropriate end tag.
    tokenizer.content = match (context.namespace, tag.as_str()) {
        (Namespace::Html, name) => match name {
            "title" | "textarea" => Content::Rcdata(String::new()),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" => Content::Rawtext(String::new()),
            "script" => Content::Script(String::new()),
            "noscript" if context.scripting_enabled => Content::Rawtext(String::new()),
            "plaintext" => Content::Plaintext,
            _ => Content::Data,
        },
        _ => Content::Data,
    };
    let context_id = tree.create(NodeKind::Element {
        tag: tag.clone(),
        attrs: context.attrs.cloned().unwrap_or_default(),
    });
    if context.namespace != Namespace::Html {
        tree.doc.namespaces.insert(context_id, context.namespace);
    }
    tree.fragment_context = Some(context_id);
    tree.doc.quirks_mode = context.quirks_mode;
    let html = tree.implicit("html");
    if context.namespace == Namespace::Html && tag == "template" {
        tree.template_modes.push(Mode::InTemplate);
    }
    if context.form_ancestor || context.namespace == Namespace::Html && tag == "form" {
        tree.form = Some(tree.create(NodeKind::Element {
            tag: "form".into(),
            attrs: Default::default(),
        }));
    }
    tree.reset_mode();
    tree.run(&mut tokenizer);
    // Keep arena identities stable, including detached recovery/history nodes.
    // Finalization must run even after a resource halt returned a partial tree.
    let children = std::mem::take(&mut tree.doc.nodes[html.0].children);
    tree.detach_unchecked(html);
    for child in children {
        tree.doc.nodes[child.0].parent = Some(tree.doc.root);
        tree.doc.nodes[tree.doc.root.0].children.push(child);
    }
    tree.doc
}

impl Tree {
    fn run(&mut self, tokenizer: &mut Tokenizer<'_>) {
        loop {
            tokenizer.allow_cdata = self.allow_cdata();
            let token = tokenizer.next();
            let eof = matches!(token, Token::Eof);
            self.step(token, &mut tokenizer.content);
            if self.halted || eof {
                break;
            }
        }
    }

    pub(super) fn allow_cdata(&self) -> bool {
        !self.stack.is_empty() && self.doc.namespace(self.adjusted_current()) != Namespace::Html
    }

    pub(super) fn step(&mut self, mut token: Token, content: &mut Content) -> TreeStep {
        let new_start = self.doc.nodes.len();
        let closed = match &token {
            Token::End(name) if self.doc.tag(self.current()).eq_ignore_ascii_case(name) => {
                Some(self.current())
            }
            Token::Eof if self.mode == Mode::Text => Some(self.current()),
            _ => None,
        };
        let script = closed.filter(|&id| {
            matches!(&token,Token::End(name) if name == "script")
                && self.mode == Mode::Text
                && self.html_tag(id) == "script"
                && self.doc.scripting_enabled
                && self.fragment_context.is_none()
                && !self.parsing_template()
                && self.connected(id)
        });
        if self.skip_lf {
            self.skip_lf = false;
            if let Token::Characters(s) = &mut token
                && s.starts_with('\n')
            {
                s.remove(0);
            }
        }
        // Each reprocessing step changes an insertion mode or an open-element
        // entry. Keep the original bounded algorithm for both parser drivers.
        let mut iterations = 0;
        while !self.halted
            && self.charge_work(1 + self.stack.len() + self.formatting.len())
            && self.dispatch(&mut token, content)
        {
            iterations += 1;
            if iterations > MAX_OPEN_ELEMENTS + 32 {
                self.doc.truncated = true;
                self.halted = true;
            }
        }
        TreeStep {
            new_nodes: new_start..self.doc.nodes.len(),
            closed,
            script: script.filter(|_| !self.halted),
        }
    }
}

pub(super) struct TreeStep {
    pub new_nodes: std::ops::Range<usize>,
    pub closed: Option<NodeId>,
    pub script: Option<NodeId>,
}

pub(crate) type CustomElementTest = Box<dyn Fn(&str) -> bool>;

/// A streaming host must run the element constructor with a detached,
/// attribute-free node before the tree builder performs the actual insertion.
pub(crate) struct PendingElement {
    pub node: NodeId,
    pub parent: NodeId,
    pub before: Option<NodeId>,
    pub attributes: Vec<(String, String)>,
}

pub(super) struct Tree {
    pub doc: Document,
    pub(super) mutation_sink: Option<MutationSink>,
    pub(super) custom_element_test: Option<CustomElementTest>,
    pub(super) pending_element: Option<PendingElement>,
    stack: Vec<NodeId>,
    formatting: Vec<Option<Formatting>>,
    formatting_bytes: usize,
    pending_table_text: String,
    mode: Mode,
    original_mode: Mode,
    head: Option<NodeId>,
    form: Option<NodeId>,
    foster: bool,
    frameset_ok: bool,
    skip_lf: bool,
    pub halted: bool,
    pub string_bytes: usize,
    pub work: usize,
    fragment_context: Option<NodeId>,
    template_modes: Vec<Mode>,
}

struct Formatting {
    node: NodeId,
    token: Arc<Tag>,
    bytes: usize,
}

impl Tree {
    pub(super) fn new(scripting: bool) -> Self {
        Self {
            doc: Document {
                scripting_enabled: scripting,
                ..Document::default()
            },
            mutation_sink: None,
            custom_element_test: None,
            pending_element: None,
            stack: vec![],
            formatting: vec![],
            formatting_bytes: 0,
            pending_table_text: String::new(),
            mode: Mode::Initial,
            original_mode: Mode::Initial,
            head: None,
            form: None,
            foster: false,
            frameset_ok: true,
            skip_lf: false,
            halted: false,
            string_bytes: 0,
            work: 0,
            fragment_context: None,
            template_modes: vec![],
        }
    }

    pub(super) fn references(&self) -> Vec<NodeId> {
        let mut refs = self.stack.clone();
        refs.extend(self.formatting.iter().flatten().map(|entry| entry.node));
        refs.extend(self.head);
        refs.extend(self.form);
        refs.extend(self.fragment_context);
        refs.sort_unstable_by_key(|id| id.0);
        refs.dedup();
        refs
    }

    fn connected(&self, mut node: NodeId) -> bool {
        for _ in 0..=MAX_OPEN_ELEMENTS {
            if node == self.doc.root {
                return true;
            }
            let Some(parent) = self.doc.nodes[node.0].parent else {
                return false;
            };
            node = parent;
        }
        false
    }

    fn charge_strings(&mut self, amount: usize) -> bool {
        if self.string_bytes.saturating_add(amount) > MAX_DOM_BYTES {
            self.doc.truncated = true;
            self.halted = true;
            false
        } else {
            self.string_bytes += amount;
            true
        }
    }

    pub(super) fn charge_work(&mut self, amount: usize) -> bool {
        if self.work.saturating_add(amount) > MAX_STREAM_WORK {
            self.doc.truncated = true;
            self.halted = true;
            false
        } else {
            self.work += amount;
            true
        }
    }

    fn current(&self) -> NodeId {
        self.stack.last().copied().unwrap_or(self.doc.root)
    }
    fn current_tag(&self) -> &str {
        self.html_tag(self.current())
    }

    fn html_tag(&self, id: NodeId) -> &str {
        if self.doc.namespace(id) == Namespace::Html {
            self.doc.tag(id)
        } else {
            ""
        }
    }

    fn adjusted_current(&self) -> NodeId {
        if self.stack.len() == 1 {
            self.fragment_context.unwrap_or_else(|| self.current())
        } else {
            self.current()
        }
    }

    fn math_text_integration(&self, id: NodeId) -> bool {
        self.doc.namespace(id) == Namespace::MathMl
            && matches!(self.doc.tag(id), "mi" | "mo" | "mn" | "ms" | "mtext")
    }

    fn html_integration(&self, id: NodeId) -> bool {
        match self.doc.namespace(id) {
            Namespace::Svg => matches!(self.doc.tag(id), "foreignObject" | "desc" | "title"),
            Namespace::MathMl => {
                self.doc.tag(id) == "annotation-xml"
                    && self.doc.attr(id, "encoding").is_some_and(|value| {
                        value.eq_ignore_ascii_case("text/html")
                            || value.eq_ignore_ascii_case("application/xhtml+xml")
                    })
            }
            Namespace::Html | Namespace::Null => false,
        }
    }

    fn dispatch(&mut self, token: &mut Token, tokenizer: &mut Content) -> bool {
        let adjusted = self.adjusted_current();
        let namespace = self.doc.namespace(adjusted);
        let html = self.stack.is_empty()
            || namespace == Namespace::Html
            || self.math_text_integration(adjusted)
                && match token {
                    Token::Characters(_) => true,
                    Token::Start(t) => !matches!(t.name.as_str(), "mglyph" | "malignmark"),
                    _ => false,
                }
            || namespace == Namespace::MathMl
                && self.doc.tag(adjusted) == "annotation-xml"
                && matches!(token, Token::Start(t) if t.name == "svg")
            || self.html_integration(adjusted)
                && matches!(token, Token::Characters(_) | Token::Start(_))
            || matches!(token, Token::Eof);
        if html {
            self.process(token, tokenizer)
        } else {
            self.foreign(token, tokenizer, namespace)
        }
    }

    fn foreign(
        &mut self,
        token: &mut Token,
        tokenizer: &mut Content,
        namespace: Namespace,
    ) -> bool {
        let breakout = match token {
            Token::Start(t) => foreign::breakout(t),
            Token::End(name) => matches!(name.as_str(), "br" | "p"),
            _ => false,
        };
        if breakout {
            while !self.stack.is_empty() {
                let current = self.current();
                if self.doc.namespace(current) == Namespace::Html
                    || self.math_text_integration(current)
                    || self.html_integration(current)
                {
                    break;
                }
                self.stack.pop();
            }
            return self.process(token, tokenizer);
        }
        match token {
            Token::Characters(text) => {
                // NULL alone does not clear frameset-ok in the foreign rules.
                if text.bytes().any(|b| b != 0 && !space(b)) {
                    self.frameset_ok = false;
                }
                self.characters(&text.replace('\0', "\u{fffd}"));
            }
            Token::Comment(text) => self.comment(text, None),
            Token::Doctype(_) => {}
            Token::Start(tag) => {
                self.insert_foreign(tag, namespace);
                if tag.self_closing && !self.halted {
                    self.stack.pop();
                }
            }
            Token::End(name) => {
                // The virtual html root cannot be removed by a foreign end tag.
                // At an HTML ancestor the token uses the current HTML mode.
                for index in (1..self.stack.len()).rev() {
                    let node = self.stack[index];
                    if self.doc.tag(node).eq_ignore_ascii_case(name) {
                        self.stack.truncate(index);
                        return false;
                    }
                    if self.doc.namespace(self.stack[index - 1]) == Namespace::Html {
                        return self.process(token, tokenizer);
                    }
                }
            }
            Token::Eof => unreachable!(),
        }
        false
    }

    fn create(&mut self, kind: NodeKind) -> NodeId {
        if self.halted {
            return self.current();
        }
        if self.doc.nodes.len() >= MAX_NODES {
            self.doc.truncated = true;
            self.halted = true;
            return self.current();
        }
        if !self.charge_strings(kind_bytes(&kind)) {
            return self.current();
        }
        let id = NodeId(self.doc.nodes.len());
        self.doc.nodes.push(Node {
            kind,
            parent: None,
            children: vec![],
        });
        id
    }

    fn detach(&mut self, id: NodeId) {
        if let Some(parent) = self.doc.nodes[id.0].parent {
            let scans = 1 + usize::from(self.mutation_sink.is_some());
            if !self.charge_work(
                self.doc.nodes[parent.0]
                    .children
                    .len()
                    .saturating_mul(scans),
            ) {
                return;
            }
            self.detach_unchecked(id);
        }
    }

    fn detach_unchecked(&mut self, id: NodeId) {
        if let Some(parent) = self.doc.nodes[id.0].parent {
            if let Some(sink) = &mut self.mutation_sink {
                let (previous, next) = siblings(&self.doc, parent, id);
                sink(&self.doc, Mutation::Removing { parent, child: id });
                sink(
                    &self.doc,
                    Mutation::Children {
                        target: parent,
                        added: &[],
                        removed: &[id],
                        previous,
                        next,
                    },
                );
            }
            self.doc.nodes[id.0].parent = None;
            self.doc.nodes[parent.0]
                .children
                .retain(|&child| child != id);
        }
    }

    fn attach(&mut self, id: NodeId, parent: NodeId, before: Option<NodeId>) {
        if self.halted {
            return;
        }
        // Adoption can move ancestors as well as leaves. Refuse a cycle if a
        // future recovery change violates the insertion algorithm's invariant.
        let mut ancestor = Some(parent);
        let mut depth = 0;
        while let Some(node) = ancestor {
            depth += 1;
            if node == id || depth > MAX_OPEN_ELEMENTS + 1 {
                self.doc.truncated = true;
                self.halted = true;
                return;
            }
            ancestor = self.doc.nodes[node.0]
                .parent
                .or_else(|| self.doc.shadow_hosts.get(&node).copied());
        }
        let scan = if before.is_some() {
            self.doc.nodes[parent.0].children.len().saturating_mul(2)
        } else {
            0
        };
        if !self.charge_work(depth.saturating_add(scan)) {
            return;
        }
        self.detach(id);
        if self.halted {
            return;
        }
        let index = before.and_then(|child| {
            self.doc.nodes[parent.0]
                .children
                .iter()
                .position(|&n| n == child)
        });
        if let Some(sink) = &mut self.mutation_sink {
            let children = &self.doc.nodes[parent.0].children;
            let index = index.unwrap_or(children.len());
            let previous = index.checked_sub(1).map(|previous| children[previous]);
            let next = children.get(index).copied();
            sink(
                &self.doc,
                Mutation::Children {
                    target: parent,
                    added: &[id],
                    removed: &[],
                    previous,
                    next,
                },
            );
        }
        self.doc.nodes[id.0].parent = Some(parent);
        if let Some(index) = index {
            self.doc.nodes[parent.0].children.insert(index, id);
        } else {
            self.doc.nodes[parent.0].children.push(id);
        }
    }

    fn location(&self, target: Option<NodeId>) -> (NodeId, Option<NodeId>) {
        let mut target = target.unwrap_or_else(|| self.current());
        if self.foster
            && matches!(
                self.html_tag(target),
                "table" | "tbody" | "tfoot" | "thead" | "tr"
            )
        {
            if let Some(index) = self
                .stack
                .iter()
                .rposition(|&id| matches!(self.html_tag(id), "table" | "template"))
            {
                let table = self.stack[index];
                if self.html_tag(table) == "template" {
                    return (
                        self.doc
                            .template_contents
                            .get(&table)
                            .copied()
                            .unwrap_or(table),
                        None,
                    );
                }
                if let Some(parent) = self.doc.nodes[table.0].parent {
                    return (parent, Some(table));
                }
                return (self.stack[index.saturating_sub(1)], None);
            }
            return (self.stack.first().copied().unwrap_or(self.doc.root), None);
        }
        if let Some(&content) = self.doc.template_contents.get(&target) {
            target = content;
        }
        (target, None)
    }

    fn push(&mut self, id: NodeId) {
        if self.halted {
            return;
        }
        if self.stack.len() >= MAX_OPEN_ELEMENTS {
            self.doc.truncated = true;
            self.halted = true;
        } else {
            self.stack.push(id);
        }
    }

    fn insert(&mut self, tag: &Tag) -> NodeId {
        if self
            .custom_element_test
            .as_ref()
            .is_some_and(|test| test(&tag.name))
            && !self.parsing_template()
        {
            let id = self.create(NodeKind::Element {
                tag: tag.name.clone(),
                attrs: Default::default(),
            });
            if !self.halted {
                let (parent, before) = self.location(None);
                self.pending_element = Some(PendingElement {
                    node: id,
                    parent,
                    before,
                    attributes: tag
                        .merge_attribute_order
                        .iter()
                        .map(|name| (name.clone(), tag.attrs[name].clone()))
                        .collect(),
                });
                self.push(id);
            }
            return id;
        }
        let id = self.create(NodeKind::Element {
            tag: tag.name.clone(),
            attrs: tag.attrs.clone(),
        });
        let (parent, before) = self.location(None);
        self.attach(id, parent, before);
        self.push(id);
        id
    }

    pub(super) fn finish_custom_element(
        &mut self,
        original: NodeId,
        element: NodeId,
        parent: NodeId,
        before: Option<NodeId>,
    ) {
        if original != element
            && let Some(entry) = self
                .stack
                .iter_mut()
                .rev()
                .find(|entry| **entry == original)
        {
            *entry = element;
        }
        self.attach(element, parent, before);
    }

    fn insert_foreign(&mut self, tag: &Tag, namespace: Namespace) -> NodeId {
        let tag = foreign::adjust(tag, namespace);
        let namespaced: Vec<_> = tag
            .attrs
            .keys()
            .filter_map(|name| {
                foreign::attribute_namespace(name).map(|namespace| (name.clone(), namespace))
            })
            .collect();
        let metadata_bytes = namespaced.iter().map(|(name, _)| name.len()).sum();
        let id = self.create(NodeKind::Element {
            tag: tag.name,
            attrs: tag.attrs,
        });
        if self.halted {
            return id;
        }
        if !self.charge_strings(metadata_bytes) {
            self.string_bytes -= kind_bytes(&self.doc.nodes[id.0].kind);
            self.doc.nodes.pop();
            return self.current();
        }
        // Commit metadata before a depth limit can halt pushing the new node.
        // A returned partial tree must not silently turn its last element HTML.
        self.doc.namespaces.insert(id, namespace);
        for (name, namespace) in namespaced {
            self.doc.attribute_namespaces.insert((id, name), namespace);
        }
        let (parent, before) = self.location(None);
        self.attach(id, parent, before);
        self.push(id);
        id
    }

    fn implicit(&mut self, name: &str) -> NodeId {
        self.insert(&Tag::new(name))
    }

    fn parsing_template(&self) -> bool {
        self.fragment_context
            .is_some_and(|id| self.html_tag(id) == "template")
            || self.stack.iter().any(|&id| self.html_tag(id) == "template")
    }

    fn start_template(&mut self, tag: &Tag) {
        if self.doc.nodes.len() > MAX_NODES.saturating_sub(2)
            || self.stack.len() >= MAX_OPEN_ELEMENTS
        {
            self.doc.truncated = true;
            self.halted = true;
            return;
        }
        let template = self.insert(tag);
        if self.halted {
            return;
        }
        let content = self.create(NodeKind::Document);
        if self.halted {
            return;
        }
        self.doc.template_contents.insert(template, content);
        self.formatting_marker();
        self.frameset_ok = false;
        self.template_modes.push(Mode::InTemplate);
        self.mode = Mode::InTemplate;
    }

    fn end_template(&mut self) {
        if !self.stack.iter().any(|&id| self.html_tag(id) == "template") {
            return;
        }
        self.pop_through("template");
        self.clear_formatting();
        self.template_modes.pop();
        self.reset_mode();
    }

    fn template_eof(&mut self) -> bool {
        if !self.stack.iter().any(|&id| self.html_tag(id) == "template") {
            return false;
        }
        self.end_template();
        true
    }

    fn comment(&mut self, data: &str, target: Option<NodeId>) {
        let id = self.create(NodeKind::Comment(data.into()));
        let (parent, before) = self.location(target);
        self.attach(id, parent, before);
    }

    fn characters(&mut self, text: &str) {
        if text.is_empty() || self.halted {
            return;
        }
        let (parent, before) = self.location(None);
        if before.is_some() && !self.charge_work(self.doc.nodes[parent.0].children.len()) {
            return;
        }
        let children = &self.doc.nodes[parent.0].children;
        let previous = if let Some(before) = before {
            children
                .iter()
                .position(|&n| n == before)
                .and_then(|i| i.checked_sub(1))
                .map(|i| children[i])
        } else {
            children.last().copied()
        };
        if let Some(previous) = previous
            && matches!(self.doc.nodes[previous.0].kind, NodeKind::Text(_))
        {
            if self.charge_strings(text.len()) {
                if let Some(sink) = &mut self.mutation_sink
                    && let NodeKind::Text(existing) = &self.doc.nodes[previous.0].kind
                {
                    sink(
                        &self.doc,
                        Mutation::CharacterData {
                            target: previous,
                            old: existing,
                        },
                    );
                }
                if let NodeKind::Text(existing) = &mut self.doc.nodes[previous.0].kind {
                    existing.push_str(text);
                }
            }
            return;
        }
        let id = self.create(NodeKind::Text(text.into()));
        self.attach(id, parent, before);
    }

    // Character tokens are batched by the tokenizer; these modes must consume
    // their leading whitespace separately before reprocessing the remainder.
    fn leading_space(&mut self, token: &mut Token, insert: bool) -> bool {
        if let Token::Characters(text) = token {
            let count = text.bytes().take_while(|&b| space(b)).count();
            if insert {
                self.characters(&text[..count]);
            }
            if count != 0 {
                text.drain(..count);
            }
            return text.is_empty();
        }
        false
    }

    fn scope(&self, name: &str, scope: Scope) -> bool {
        for &id in self.stack.iter().rev() {
            let tag = self.html_tag(id);
            if tag == name {
                return true;
            }
            if boundary(self.doc.namespace(id), self.doc.tag(id), scope) {
                return false;
            }
        }
        false
    }

    fn node_in_scope(&self, node: NodeId) -> bool {
        for &id in self.stack.iter().rev() {
            if id == node {
                return true;
            }
            if boundary(self.doc.namespace(id), self.doc.tag(id), Scope::Normal) {
                return false;
            }
        }
        false
    }

    fn pop_through(&mut self, name: &str) {
        if let Some(index) = self.stack.iter().rposition(|&id| self.html_tag(id) == name) {
            self.stack.truncate(index);
        }
    }

    fn implied_end(&mut self, except: &str) {
        while self.current_tag() != except
            && matches!(
                self.current_tag(),
                "dd" | "dt" | "li" | "optgroup" | "option" | "p" | "rb" | "rp" | "rt" | "rtc"
            )
        {
            self.stack.pop();
        }
    }

    fn close_p(&mut self) {
        self.implied_end("p");
        self.pop_through("p");
    }

    fn clear_to(&mut self, tags: &[&str]) {
        while !self.stack.is_empty() && !tags.contains(&self.current_tag()) {
            self.stack.pop();
        }
    }

    fn clear_formatting(&mut self) {
        while let Some(item) = self.formatting.pop() {
            let Some(entry) = item else {
                break;
            };
            self.formatting_bytes -= entry.bytes;
        }
    }

    fn formatting_marker(&mut self) {
        if self.formatting.len() >= MAX_FORMATTING_ENTRIES {
            self.doc.truncated = true;
            self.halted = true;
        } else {
            self.formatting.push(None);
        }
    }

    fn remove_formatting(&mut self, index: usize) -> Formatting {
        let entry = self
            .formatting
            .remove(index)
            .expect("formatting element entry");
        self.formatting_bytes -= entry.bytes;
        entry
    }

    fn formatting_index(&self, node: NodeId) -> Option<usize> {
        self.formatting
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.node == node))
    }

    fn reconstruct(&mut self) {
        let Some(Some(last)) = self.formatting.last() else {
            return;
        };
        if self.stack.contains(&last.node) {
            return;
        }
        let mut index = self.formatting.len() - 1;
        while index > 0 {
            match &self.formatting[index - 1] {
                None => break,
                Some(entry) if self.stack.contains(&entry.node) => break,
                _ => index -= 1,
            }
        }
        while index < self.formatting.len() && !self.halted {
            let token = self.formatting[index].as_ref().unwrap().token.clone();
            let id = self.insert(&token);
            if !self.halted {
                self.formatting[index].as_mut().unwrap().node = id;
            }
            index += 1;
        }
    }

    fn add_formatting(&mut self, id: NodeId, token: &Tag) {
        if self.halted {
            return;
        }
        // Noah's Ark clause: at most three identical entries after the marker.
        let start = self
            .formatting
            .iter()
            .rposition(Option::is_none)
            .map_or(0, |i| i + 1);
        let bytes = tag_bytes(token);
        let mut first_identical = None;
        let mut identical = 0;
        for index in start..self.formatting.len() {
            let other = self.formatting[index].as_ref().unwrap();
            let cost = if other.token.name == token.name {
                other.bytes.min(bytes).saturating_add(1)
            } else {
                1
            };
            if !self.charge_work(cost) {
                return;
            }
            let other = self.formatting[index].as_ref().unwrap();
            if other.token.name == token.name && other.token.attrs == token.attrs {
                first_identical.get_or_insert(index);
                identical += 1;
                if identical == 3 {
                    break;
                }
            }
        }
        if identical == 3 {
            self.remove_formatting(first_identical.unwrap());
        }
        if self.formatting.len() >= MAX_FORMATTING_ENTRIES
            || self.formatting_bytes.saturating_add(bytes) > MAX_FORMATTING_BYTES
        {
            self.doc.truncated = true;
            self.halted = true;
            return;
        }
        self.formatting_bytes += bytes;
        self.formatting.push(Some(Formatting {
            node: id,
            token: Arc::new(token.clone()),
            bytes,
        }));
    }

    fn generic_end(&mut self, name: &str) {
        for index in (0..self.stack.len()).rev() {
            let tag = self.html_tag(self.stack[index]);
            if tag == name {
                self.implied_end(name);
                self.stack.truncate(index);
                return;
            }
            if special(
                self.doc.namespace(self.stack[index]),
                self.doc.tag(self.stack[index]),
            ) {
                return;
            }
        }
    }

    fn adoption(&mut self, subject: &str) {
        let current = self.current();
        if self.html_tag(current) == subject && self.formatting_index(current).is_none() {
            self.stack.pop();
            return;
        }
        for _ in 0..8 {
            let start = self
                .formatting
                .iter()
                .rposition(Option::is_none)
                .map_or(0, |i| i + 1);
            let Some(format_index) = (start..self.formatting.len())
                .rev()
                .find(|&i| self.formatting[i].as_ref().unwrap().token.name == subject)
            else {
                self.generic_end(subject);
                return;
            };
            let entry = self.formatting[format_index].as_ref().unwrap();
            let formatting = entry.node;
            let formatting_token = entry.token.clone();
            let Some(stack_index) = self.stack.iter().position(|&id| id == formatting) else {
                self.remove_formatting(format_index);
                return;
            };
            if !self.node_in_scope(formatting) {
                return;
            }
            let Some(block_index) = (stack_index + 1..self.stack.len()).find(|&i| {
                special(
                    self.doc.namespace(self.stack[i]),
                    self.doc.tag(self.stack[i]),
                )
            }) else {
                self.stack.truncate(stack_index);
                self.remove_formatting(format_index);
                return;
            };
            let block = self.stack[block_index];
            let common = self.stack[stack_index.saturating_sub(1)];
            let mut bookmark = format_index;
            let mut last = block;
            let mut node_index = block_index;
            let mut inner = 0;
            loop {
                inner += 1;
                node_index -= 1;
                let old = self.stack[node_index];
                if old == formatting {
                    break;
                }
                if !self.charge_work(self.formatting.len()) {
                    return;
                }
                let mut active = self.formatting_index(old);
                if inner > 3
                    && let Some(index) = active
                {
                    self.remove_formatting(index);
                    if index < bookmark {
                        bookmark -= 1;
                    }
                    active = None;
                }
                let Some(active) = active else {
                    self.stack.remove(node_index);
                    continue;
                };
                let token = self.formatting[active].as_ref().unwrap().token.clone();
                let new = self.create(NodeKind::Element {
                    tag: token.name.clone(),
                    attrs: token.attrs.clone(),
                });
                if self.halted {
                    return;
                }
                self.stack[node_index] = new;
                self.formatting[active].as_mut().unwrap().node = new;
                if last == block {
                    bookmark = active + 1;
                }
                self.attach(last, new, None);
                last = new;
            }
            let (parent, before) = self.location(Some(common));
            self.attach(last, parent, before);
            let new = self.create(NodeKind::Element {
                tag: formatting_token.name.clone(),
                attrs: formatting_token.attrs.clone(),
            });
            if self.halted {
                return;
            }
            // The fresh node cannot be an ancestor of these children. Move the
            // child vector as a whole to avoid repeatedly scanning its prefix.
            if !self.charge_work(self.doc.nodes[block.0].children.len()) {
                return;
            }
            if let Some(sink) = &mut self.mutation_sink {
                let children = &self.doc.nodes[block.0].children;
                // Adoption moves the children in order. The destination is
                // freshly created, so only removing from the old tree can
                // reach a registered observer at this point.
                for (index, &child) in children.iter().enumerate() {
                    sink(
                        &self.doc,
                        Mutation::Removing {
                            parent: block,
                            child,
                        },
                    );
                    sink(
                        &self.doc,
                        Mutation::Children {
                            target: block,
                            added: &[],
                            removed: &[child],
                            previous: None,
                            next: children.get(index + 1).copied(),
                        },
                    );
                }
            }
            let children = std::mem::take(&mut self.doc.nodes[block.0].children);
            for &child in &children {
                self.doc.nodes[child.0].parent = Some(new);
            }
            self.doc.nodes[new.0].children = children;
            self.attach(new, block, None);
            if let Some(index) = self.formatting_index(formatting) {
                let mut entry = self.remove_formatting(index);
                if index < bookmark {
                    bookmark = bookmark.saturating_sub(1);
                }
                entry.node = new;
                self.formatting_bytes += entry.bytes;
                self.formatting
                    .insert(bookmark.min(self.formatting.len()), Some(entry));
            }
            self.stack.retain(|&id| id != formatting);
            if let Some(index) = self.stack.iter().position(|&id| id == block) {
                self.stack.insert(index + 1, new);
            }
        }
    }

    fn reset_mode(&mut self) {
        for (index, &id) in self.stack.iter().enumerate().rev() {
            let last = index == 0;
            let tag = if last {
                self.html_tag(self.fragment_context.unwrap_or(id))
            } else {
                self.html_tag(id)
            };
            self.mode = match tag {
                "td" | "th" if !last => Mode::InCell,
                "tr" => Mode::InRow,
                "tbody" | "thead" | "tfoot" => Mode::InTableBody,
                "caption" => Mode::InCaption,
                "colgroup" => Mode::InColumnGroup,
                "table" => Mode::InTable,
                "template" => self
                    .template_modes
                    .last()
                    .copied()
                    .unwrap_or(Mode::InTemplate),
                "head" if !last => Mode::InHead,
                "body" => Mode::InBody,
                "frameset" => Mode::InFrameset,
                "html" => {
                    if self.head.is_some() {
                        Mode::AfterHead
                    } else {
                        Mode::BeforeHead
                    }
                }
                _ if last => Mode::InBody,
                _ => continue,
            };
            return;
        }
        self.mode = Mode::InBody;
    }

    fn raw(&mut self, tag: &Tag, tokenizer: &mut Content, content: Content) {
        self.insert(tag);
        *tokenizer = content;
        self.original_mode = self.mode;
        self.mode = Mode::Text;
    }

    fn merge_attrs(&mut self, id: NodeId, tag: &Tag) {
        let bytes = tag
            .attrs
            .iter()
            .filter(|(name, _)| self.doc.attr(id, name).is_none())
            .map(|(name, value)| name.len() + value.len())
            .sum();
        if !self.charge_strings(bytes) {
            return;
        }
        if let Some(sink) = &mut self.mutation_sink {
            for name in tag
                .merge_attribute_order
                .iter()
                .filter(|name| self.doc.attr(id, name).is_none())
            {
                sink(
                    &self.doc,
                    Mutation::Attribute {
                        target: id,
                        name,
                        namespace: None,
                        old: None,
                    },
                );
            }
        }
        if let NodeKind::Element { attrs, .. } = &mut self.doc.nodes[id.0].kind {
            for (name, value) in &tag.attrs {
                attrs.entry(name.clone()).or_insert_with(|| value.clone());
            }
        }
    }

    fn process(&mut self, token: &mut Token, tokenizer: &mut Content) -> bool {
        if matches!(token, Token::Eof)
            && !self.template_modes.is_empty()
            && !matches!(self.mode, Mode::Text | Mode::InTableText)
        {
            return self.template_eof();
        }
        match self.mode {
            Mode::Initial => {
                if self.leading_space(token, false) {
                    return false;
                }
                match token {
                    Token::Comment(s) => {
                        self.comment(s, Some(self.doc.root));
                        return false;
                    }
                    Token::Doctype(d) => {
                        let id = self.create(NodeKind::Doctype {
                            name: d.name.clone(),
                            public_id: d.public_id.clone(),
                            system_id: d.system_id.clone(),
                        });
                        self.attach(id, self.doc.root, None);
                        self.doc.quirks_mode = quirks(d);
                        self.mode = Mode::BeforeHtml;
                        return false;
                    }
                    _ => {
                        self.doc.quirks_mode = QuirksMode::Quirks;
                        self.mode = Mode::BeforeHtml;
                        return true;
                    }
                }
            }
            Mode::BeforeHtml => {
                if self.leading_space(token, false) {
                    return false;
                }
                match token {
                    Token::Comment(s) => {
                        self.comment(s, Some(self.doc.root));
                        return false;
                    }
                    Token::Doctype(_) => return false,
                    Token::Start(t) if t.name == "html" => {
                        self.insert(t);
                        self.mode = Mode::BeforeHead;
                        return false;
                    }
                    Token::End(t) if !matches!(t.as_str(), "head" | "body" | "html" | "br") => {
                        return false;
                    }
                    _ => {
                        self.implicit("html");
                        self.mode = Mode::BeforeHead;
                        return true;
                    }
                }
            }
            Mode::BeforeHead => {
                if self.leading_space(token, false) {
                    return false;
                }
                match token {
                    Token::Comment(s) => {
                        self.comment(s, None);
                        return false;
                    }
                    Token::Doctype(_) => return false,
                    Token::Start(t) if t.name == "html" => {
                        self.in_body(token, tokenizer);
                        return false;
                    }
                    Token::Start(t) if t.name == "head" => {
                        self.head = Some(self.insert(t));
                        self.mode = Mode::InHead;
                        return false;
                    }
                    Token::End(t) if !matches!(t.as_str(), "head" | "body" | "html" | "br") => {
                        return false;
                    }
                    _ => {
                        self.head = Some(self.implicit("head"));
                        self.mode = Mode::InHead;
                        return true;
                    }
                }
            }
            Mode::InHead => {
                if self.leading_space(token, true) {
                    return false;
                }
                match token {
                    Token::Comment(s) => self.comment(s, None),
                    Token::Doctype(_) => {}
                    Token::Start(t) if t.name == "html" => self.in_body(token, tokenizer),
                    Token::Start(t)
                        if matches!(
                            t.name.as_str(),
                            "base" | "basefont" | "bgsound" | "link" | "meta"
                        ) =>
                    {
                        self.insert(t);
                        self.stack.pop();
                    }
                    Token::Start(t) if t.name == "title" => {
                        self.raw(t, tokenizer, Content::Rcdata(t.name.clone()))
                    }
                    Token::Start(t) if matches!(t.name.as_str(), "style" | "noframes") => {
                        self.raw(t, tokenizer, Content::Rawtext(t.name.clone()))
                    }
                    Token::Start(t) if t.name == "script" => {
                        self.raw(t, tokenizer, Content::Script(t.name.clone()))
                    }
                    Token::Start(t) if t.name == "noscript" => {
                        if self.doc.scripting_enabled {
                            self.raw(t, tokenizer, Content::Rawtext(t.name.clone()));
                        } else {
                            self.insert(t);
                            self.mode = Mode::InHeadNoscript;
                        }
                    }
                    Token::Start(t) if t.name == "head" => {}
                    Token::Start(t) if t.name == "template" => self.start_template(t),
                    Token::End(t) if t == "template" => self.end_template(),
                    Token::End(t) if t == "head" => {
                        self.stack.pop();
                        self.mode = Mode::AfterHead;
                    }
                    Token::End(t) if !matches!(t.as_str(), "body" | "html" | "br") => {}
                    _ => {
                        self.stack.pop();
                        self.mode = Mode::AfterHead;
                        return true;
                    }
                }
            }
            Mode::InHeadNoscript => {
                if matches!(token, Token::End(s) if s == "noscript") {
                    self.stack.pop();
                    self.mode = Mode::InHead;
                } else if matches!(token, Token::Start(t) if matches!(t.name.as_str(), "html" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "style"))
                    || matches!(token, Token::Comment(_) | Token::Doctype(_))
                    || matches!(token, Token::Characters(s) if s.bytes().all(space))
                {
                    let saved = self.mode;
                    self.mode = Mode::InHead;
                    self.process(token, tokenizer);
                    if self.mode == Mode::InHead {
                        self.mode = saved;
                    }
                    if self.mode == Mode::Text {
                        self.original_mode = saved;
                    }
                } else if matches!(token, Token::Start(t) if matches!(t.name.as_str(), "head" | "noscript"))
                    || matches!(token, Token::End(s) if s != "br")
                {
                } else {
                    self.stack.pop();
                    self.mode = Mode::InHead;
                    return true;
                }
            }
            Mode::AfterHead => {
                if self.leading_space(token, true) {
                    return false;
                }
                match token {
                    Token::Comment(s) => self.comment(s, None),
                    Token::Doctype(_) => {}
                    Token::Start(t) if t.name == "html" => self.in_body(token, tokenizer),
                    Token::Start(t) if t.name == "body" => {
                        self.insert(t);
                        self.frameset_ok = false;
                        self.mode = Mode::InBody;
                    }
                    Token::Start(t) if t.name == "frameset" => {
                        self.insert(t);
                        self.mode = Mode::InFrameset;
                    }
                    Token::Start(t) if head_tag(&t.name) => {
                        if let Some(head) = self.head {
                            self.push(head);
                            let saved = self.mode;
                            self.mode = Mode::InHead;
                            self.process(token, tokenizer);
                            if self.mode == Mode::InHead {
                                self.mode = saved;
                            }
                            // Text mode returns to AfterHead after its closing token.
                            if self.mode == Mode::Text {
                                self.original_mode = saved;
                            }
                            self.stack.retain(|&id| id != head);
                        }
                    }
                    Token::Start(t) if t.name == "head" => {}
                    Token::End(t) if !matches!(t.as_str(), "body" | "html" | "br") => {}
                    _ => {
                        self.implicit("body");
                        self.mode = Mode::InBody;
                        return true;
                    }
                }
            }
            Mode::Text => match token {
                Token::Characters(s) => self.characters(s),
                Token::End(_) => {
                    self.stack.pop();
                    self.mode = self.original_mode;
                }
                Token::Eof => {
                    self.stack.pop();
                    self.mode = self.original_mode;
                    return true;
                }
                _ => {}
            },
            Mode::InBody => self.in_body(token, tokenizer),
            Mode::InTable => return self.in_table(token, tokenizer),
            Mode::InTableText => {
                if let Token::Characters(text) = token {
                    self.pending_table_text.push_str(&text.replace('\0', ""));
                } else {
                    let text = std::mem::take(&mut self.pending_table_text);
                    if text.bytes().all(space) {
                        self.characters(&text);
                    } else {
                        self.foster = true;
                        self.in_body(&Token::Characters(text), tokenizer);
                        self.foster = false;
                    }
                    self.mode = self.original_mode;
                    return true;
                }
            }
            Mode::InCaption => {
                if matches!(token, Token::End(s) if s == "caption") {
                    if self.scope("caption", Scope::Table) {
                        self.implied_end("");
                        self.pop_through("caption");
                        self.clear_formatting();
                        self.mode = Mode::InTable;
                    }
                } else if matches!(token, Token::Start(t) if matches!(t.name.as_str(), "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"))
                    || matches!(token, Token::End(s) if s == "table")
                {
                    if self.scope("caption", Scope::Table) {
                        self.implied_end("");
                        self.pop_through("caption");
                        self.clear_formatting();
                        self.mode = Mode::InTable;
                        return true;
                    }
                } else if !matches!(token, Token::End(s) if matches!(s.as_str(), "body" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"))
                {
                    self.in_body(token, tokenizer);
                }
            }
            Mode::InColumnGroup => {
                if self.leading_space(token, true) {
                    return false;
                }
                match token {
                    Token::Comment(s) => self.comment(s, None),
                    Token::Doctype(_) => {}
                    Token::Start(t) if t.name == "html" => self.in_body(token, tokenizer),
                    Token::Start(t) if t.name == "col" => {
                        self.insert(t);
                        self.stack.pop();
                    }
                    Token::Start(t) if t.name == "template" => self.start_template(t),
                    Token::End(t) if t == "template" => self.end_template(),
                    Token::End(s) if s == "colgroup" => {
                        if self.current_tag() == "colgroup" {
                            self.stack.pop();
                            self.mode = Mode::InTable;
                        }
                    }
                    Token::End(s) if s == "col" => {}
                    Token::Eof => self.in_body(token, tokenizer),
                    _ => {
                        if self.current_tag() == "colgroup" {
                            self.stack.pop();
                            self.mode = Mode::InTable;
                            return true;
                        }
                    }
                }
            }
            Mode::InTableBody => return self.in_table_body(token, tokenizer),
            Mode::InRow => return self.in_row(token, tokenizer),
            Mode::InCell => return self.in_cell(token, tokenizer),
            Mode::InTemplate => match token {
                Token::Characters(_) | Token::Comment(_) | Token::Doctype(_) => {
                    self.in_body(token, tokenizer)
                }
                Token::Start(t) if head_tag(&t.name) => self.in_body(token, tokenizer),
                Token::End(t) if t == "template" => self.end_template(),
                Token::Start(t) => {
                    let mode = match t.name.as_str() {
                        "caption" | "colgroup" | "tbody" | "tfoot" | "thead" => Mode::InTable,
                        "col" => Mode::InColumnGroup,
                        "tr" => Mode::InTableBody,
                        "td" | "th" => Mode::InRow,
                        _ => Mode::InBody,
                    };
                    if let Some(current) = self.template_modes.last_mut() {
                        *current = mode;
                    }
                    self.mode = mode;
                    return true;
                }
                Token::Eof => return self.template_eof(),
                _ => {}
            },
            Mode::AfterBody => {
                if matches!(token, Token::Characters(s) if s.bytes().all(space)) {
                    self.in_body(token, tokenizer);
                } else {
                    match token {
                        Token::Comment(s) => self.comment(s, self.stack.first().copied()),
                        Token::Doctype(_) | Token::Eof => {}
                        Token::Start(t) if t.name == "html" => self.in_body(token, tokenizer),
                        Token::End(s) if s == "html" => {
                            if self.fragment_context.is_none() {
                                self.mode = Mode::AfterAfterBody;
                            }
                        }
                        _ => {
                            self.mode = Mode::InBody;
                            return true;
                        }
                    }
                }
            }
            Mode::AfterAfterBody | Mode::AfterAfterFrameset => {
                if self.mode == Mode::AfterAfterFrameset
                    && let Token::Characters(text) = token
                {
                    let spaces: String = text
                        .chars()
                        .filter(|&c| c.is_ascii() && space(c as u8))
                        .collect();
                    self.in_body(&Token::Characters(spaces), tokenizer);
                    return false;
                }
                match token {
                    Token::Comment(s) => self.comment(s, Some(self.doc.root)),
                    Token::Doctype(_) | Token::Eof => {}
                    Token::Characters(s) if s.bytes().all(space) => self.in_body(token, tokenizer),
                    Token::Start(t) if t.name == "html" => self.in_body(token, tokenizer),
                    Token::Start(t)
                        if t.name == "noframes" && self.mode == Mode::AfterAfterFrameset =>
                    {
                        self.raw(t, tokenizer, Content::Rawtext(t.name.clone()))
                    }
                    _ if self.mode == Mode::AfterAfterBody => {
                        self.mode = Mode::InBody;
                        return true;
                    }
                    _ => {}
                }
            }
            Mode::InFrameset | Mode::AfterFrameset => {
                if let Token::Characters(text) = token {
                    let spaces: String = text
                        .chars()
                        .filter(|&c| c.is_ascii() && space(c as u8))
                        .collect();
                    self.characters(&spaces);
                    return false;
                }
                match token {
                    Token::Comment(s) => self.comment(s, None),
                    Token::Start(t) if t.name == "html" => self.in_body(token, tokenizer),
                    Token::Start(t) if t.name == "noframes" => {
                        self.raw(t, tokenizer, Content::Rawtext(t.name.clone()))
                    }
                    Token::Start(t) if t.name == "frameset" && self.mode == Mode::InFrameset => {
                        self.insert(t);
                    }
                    Token::Start(t) if t.name == "frame" && self.mode == Mode::InFrameset => {
                        self.insert(t);
                        self.stack.pop();
                    }
                    Token::End(s) if s == "frameset" && self.mode == Mode::InFrameset => {
                        if self.current_tag() != "html" {
                            self.stack.pop();
                            if self.fragment_context.is_none() && self.current_tag() != "frameset" {
                                self.mode = Mode::AfterFrameset;
                            }
                        }
                    }
                    Token::End(s) if s == "html" && self.mode == Mode::AfterFrameset => {
                        self.mode = Mode::AfterAfterFrameset
                    }
                    _ => {}
                }
            }
        }
        false
    }

    fn in_body(&mut self, token: &Token, tokenizer: &mut Content) {
        match token {
            Token::Characters(text) => {
                let text = text.replace('\0', "");
                if text.is_empty() {
                    return;
                }
                self.reconstruct();
                self.characters(&text);
                if !text.bytes().all(space) {
                    self.frameset_ok = false;
                }
            }
            Token::Comment(s) => self.comment(s, None),
            Token::Doctype(_) | Token::Eof => {}
            Token::Start(t) => {
                let name = t.name.as_str();
                match name {
                    "html" => {
                        if self.parsing_template() {
                            return;
                        }
                        if let Some(&html) = self.stack.first() {
                            self.merge_attrs(html, t);
                        }
                    }
                    "body" => {
                        if self.parsing_template() {
                            return;
                        }
                        if self
                            .stack
                            .get(1)
                            .is_some_and(|&id| self.html_tag(id) == "body")
                        {
                            self.frameset_ok = false;
                            self.merge_attrs(self.stack[1], t);
                        }
                    }
                    "frameset" => {
                        if self.frameset_ok
                            && self
                                .stack
                                .get(1)
                                .is_some_and(|&id| self.html_tag(id) == "body")
                        {
                            self.detach(self.stack[1]);
                            self.stack.truncate(1);
                            self.insert(t);
                            self.mode = Mode::InFrameset;
                        }
                    }
                    "base" | "basefont" | "bgsound" | "link" | "meta" => {
                        self.insert(t);
                        self.stack.pop();
                    }
                    "title" => self.raw(t, tokenizer, Content::Rcdata(name.into())),
                    "style" | "noframes" => self.raw(t, tokenizer, Content::Rawtext(name.into())),
                    "script" => self.raw(t, tokenizer, Content::Script(name.into())),
                    "template" => self.start_template(t),
                    "address" | "article" | "aside" | "blockquote" | "center" | "details"
                    | "dialog" | "dir" | "div" | "dl" | "fieldset" | "figcaption" | "figure"
                    | "footer" | "header" | "hgroup" | "main" | "menu" | "nav" | "ol" | "p"
                    | "search" | "section" | "summary" | "ul" => {
                        if self.scope("p", Scope::Button) {
                            self.close_p();
                        }
                        self.insert(t);
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        if self.scope("p", Scope::Button) {
                            self.close_p();
                        }
                        if heading(self.current_tag()) {
                            self.stack.pop();
                        }
                        self.insert(t);
                    }
                    "pre" | "listing" => {
                        if self.scope("p", Scope::Button) {
                            self.close_p();
                        }
                        self.insert(t);
                        self.skip_lf = true;
                        self.frameset_ok = false;
                    }
                    "form" => {
                        if self.form.is_none() || self.parsing_template() {
                            if self.scope("p", Scope::Button) {
                                self.close_p();
                            }
                            let form = self.insert(t);
                            if !self.parsing_template() {
                                self.form = Some(form);
                            }
                        }
                    }
                    "li" | "dd" | "dt" => {
                        self.frameset_ok = false;
                        for &id in self.stack.clone().iter().rev() {
                            let tag = self.html_tag(id);
                            if (name == "li" && tag == "li")
                                || (name != "li" && matches!(tag, "dd" | "dt"))
                            {
                                let tag = tag.to_owned();
                                self.implied_end(&tag);
                                self.pop_through(&tag);
                                break;
                            }
                            if special(self.doc.namespace(id), self.doc.tag(id))
                                && !matches!(tag, "address" | "div" | "p")
                            {
                                break;
                            }
                        }
                        if self.scope("p", Scope::Button) {
                            self.close_p();
                        }
                        self.insert(t);
                    }
                    "plaintext" => {
                        if self.scope("p", Scope::Button) {
                            self.close_p();
                        }
                        self.insert(t);
                        *tokenizer = Content::Plaintext;
                    }
                    "button" => {
                        if self.scope("button", Scope::Normal) {
                            self.implied_end("");
                            self.pop_through("button");
                        }
                        self.reconstruct();
                        self.insert(t);
                        self.frameset_ok = false;
                    }
                    "a" | "b" | "big" | "code" | "em" | "font" | "i" | "nobr" | "s" | "small"
                    | "strike" | "strong" | "tt" | "u" => {
                        if name == "a" {
                            let start = self
                                .formatting
                                .iter()
                                .rposition(Option::is_none)
                                .map_or(0, |i| i + 1);
                            if let Some(old) = self.formatting[start..]
                                .iter()
                                .rev()
                                .flatten()
                                .find(|entry| entry.token.name == "a")
                                .map(|entry| entry.node)
                            {
                                self.adoption("a");
                                self.stack.retain(|&id| id != old);
                                if let Some(index) = self.formatting_index(old) {
                                    self.remove_formatting(index);
                                }
                            }
                        }
                        self.reconstruct();
                        if name == "nobr" && self.scope("nobr", Scope::Normal) {
                            self.adoption("nobr");
                            self.reconstruct();
                        }
                        let id = self.insert(t);
                        self.add_formatting(id, t);
                    }
                    "applet" | "marquee" | "object" => {
                        self.reconstruct();
                        self.insert(t);
                        self.formatting_marker();
                        self.frameset_ok = false;
                    }
                    "table" => {
                        if self.doc.quirks_mode != QuirksMode::Quirks
                            && self.scope("p", Scope::Button)
                        {
                            self.close_p();
                        }
                        self.insert(t);
                        self.frameset_ok = false;
                        self.mode = Mode::InTable;
                    }
                    "area" | "br" | "embed" | "img" | "keygen" | "wbr" => {
                        self.reconstruct();
                        self.insert(t);
                        self.stack.pop();
                        self.frameset_ok = false;
                    }
                    "input" => {
                        if self
                            .fragment_context
                            .is_some_and(|id| self.html_tag(id) == "select")
                        {
                            return;
                        }
                        if self.scope("select", Scope::Normal) {
                            self.pop_through("select");
                        }
                        self.reconstruct();
                        self.insert(t);
                        self.stack.pop();
                        if !t
                            .attrs
                            .get("type")
                            .is_some_and(|s| s.eq_ignore_ascii_case("hidden"))
                        {
                            self.frameset_ok = false;
                        }
                    }
                    "param" | "source" | "track" => {
                        self.insert(t);
                        self.stack.pop();
                    }
                    "hr" => {
                        if self.scope("p", Scope::Button) {
                            self.close_p();
                        }
                        if self.scope("select", Scope::Normal) {
                            self.implied_end("");
                        }
                        self.insert(t);
                        self.stack.pop();
                        self.frameset_ok = false;
                    }
                    "image" => {
                        let mut tag = t.clone();
                        tag.name = "img".into();
                        self.in_body(&Token::Start(tag), tokenizer);
                    }
                    "textarea" => {
                        self.raw(t, tokenizer, Content::Rcdata(name.into()));
                        self.skip_lf = true;
                        self.frameset_ok = false;
                    }
                    "xmp" => {
                        if self.scope("p", Scope::Button) {
                            self.close_p();
                        }
                        self.reconstruct();
                        self.frameset_ok = false;
                        self.raw(t, tokenizer, Content::Rawtext(name.into()));
                    }
                    "iframe" => {
                        self.frameset_ok = false;
                        self.raw(t, tokenizer, Content::Rawtext(name.into()));
                    }
                    "noembed" => self.raw(t, tokenizer, Content::Rawtext(name.into())),
                    "noscript" if self.doc.scripting_enabled => {
                        self.raw(t, tokenizer, Content::Rawtext(name.into()))
                    }
                    "select" => {
                        if self
                            .fragment_context
                            .is_some_and(|id| self.html_tag(id) == "select")
                        {
                            return;
                        }
                        if self.scope("select", Scope::Normal) {
                            self.pop_through("select");
                        } else {
                            self.reconstruct();
                            self.insert(t);
                            self.frameset_ok = false;
                        }
                    }
                    "option" => {
                        if self.scope("select", Scope::Normal) {
                            self.implied_end("optgroup");
                        } else if self.current_tag() == "option" {
                            self.stack.pop();
                        }
                        self.reconstruct();
                        self.insert(t);
                    }
                    "optgroup" => {
                        if self.scope("select", Scope::Normal) {
                            self.implied_end("");
                        } else if self.current_tag() == "option" {
                            self.stack.pop();
                        }
                        self.reconstruct();
                        self.insert(t);
                    }
                    "rb" | "rtc" => {
                        if self.scope("ruby", Scope::Normal) {
                            self.implied_end("");
                        }
                        self.insert(t);
                    }
                    "rp" | "rt" => {
                        if self.scope("ruby", Scope::Normal) {
                            self.implied_end("rtc");
                        }
                        self.insert(t);
                    }
                    "caption" | "col" | "colgroup" | "frame" | "head" | "tbody" | "td"
                    | "tfoot" | "th" | "thead" | "tr" => {}
                    "math" | "svg" => {
                        self.reconstruct();
                        let namespace = if name == "svg" {
                            Namespace::Svg
                        } else {
                            Namespace::MathMl
                        };
                        self.insert_foreign(t, namespace);
                        if t.self_closing && !self.halted {
                            self.stack.pop();
                        }
                    }
                    _ => {
                        self.reconstruct();
                        self.insert(t);
                        if is_void(name) {
                            self.stack.pop();
                        }
                    }
                }
            }
            Token::End(name) => match name.as_str() {
                "body" => {
                    if self.scope("body", Scope::Normal) {
                        self.mode = Mode::AfterBody;
                    }
                }
                "html" => {
                    if self.scope("body", Scope::Normal) {
                        self.mode = if self.fragment_context.is_some() {
                            Mode::AfterBody
                        } else {
                            Mode::AfterAfterBody
                        };
                    }
                }
                "template" => self.end_template(),
                "address" | "article" | "aside" | "blockquote" | "button" | "center"
                | "details" | "dialog" | "dir" | "div" | "dl" | "fieldset" | "figcaption"
                | "figure" | "footer" | "header" | "hgroup" | "listing" | "main" | "menu"
                | "nav" | "ol" | "pre" | "search" | "section" | "select" | "summary" | "ul" => {
                    if self.scope(name, Scope::Normal) {
                        self.implied_end("");
                        self.pop_through(name);
                    }
                }
                "form" => {
                    if self.parsing_template() {
                        if self.scope("form", Scope::Normal) {
                            self.implied_end("");
                            self.pop_through("form");
                        }
                        return;
                    }
                    if let Some(form) = self.form.take()
                        && self.node_in_scope(form)
                    {
                        self.implied_end("");
                        self.stack.retain(|&id| id != form);
                    }
                }
                "p" => {
                    if !self.scope("p", Scope::Button) {
                        self.implicit("p");
                    }
                    self.close_p();
                }
                "li" | "dd" | "dt" => {
                    if self.scope(
                        name,
                        if name == "li" {
                            Scope::List
                        } else {
                            Scope::Normal
                        },
                    ) {
                        self.implied_end(name);
                        self.pop_through(name);
                    }
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    if (1..=6).any(|n| self.scope(&format!("h{n}"), Scope::Normal)) {
                        self.implied_end("");
                        while let Some(id) = self.stack.pop() {
                            if heading(self.html_tag(id)) {
                                break;
                            }
                        }
                    }
                }
                "a" | "b" | "big" | "code" | "em" | "font" | "i" | "nobr" | "s" | "small"
                | "strike" | "strong" | "tt" | "u" => self.adoption(name),
                "applet" | "marquee" | "object" => {
                    if self.scope(name, Scope::Normal) {
                        self.implied_end("");
                        self.pop_through(name);
                        self.clear_formatting();
                    }
                }
                "br" => self.in_body(&Token::Start(Tag::new("br")), tokenizer),
                _ => self.generic_end(name),
            },
        }
    }

    fn in_table(&mut self, token: &mut Token, tokenizer: &mut Content) -> bool {
        match token {
            Token::Characters(_)
                if matches!(
                    self.current_tag(),
                    "table" | "tbody" | "tfoot" | "thead" | "tr"
                ) =>
            {
                self.pending_table_text.clear();
                self.original_mode = self.mode;
                self.mode = Mode::InTableText;
                return true;
            }
            Token::Comment(s) => self.comment(s, None),
            Token::Doctype(_) => {}
            Token::Start(t) if t.name == "caption" => {
                self.clear_to(&["table", "template", "html"]);
                self.formatting_marker();
                self.insert(t);
                self.mode = Mode::InCaption;
            }
            Token::Start(t) if t.name == "colgroup" => {
                self.clear_to(&["table", "template", "html"]);
                self.insert(t);
                self.mode = Mode::InColumnGroup;
            }
            Token::Start(t) if t.name == "col" => {
                self.clear_to(&["table", "template", "html"]);
                self.implicit("colgroup");
                self.mode = Mode::InColumnGroup;
                return true;
            }
            Token::Start(t) if matches!(t.name.as_str(), "tbody" | "tfoot" | "thead") => {
                self.clear_to(&["table", "template", "html"]);
                self.insert(t);
                self.mode = Mode::InTableBody;
            }
            Token::Start(t) if matches!(t.name.as_str(), "td" | "th" | "tr") => {
                self.clear_to(&["table", "template", "html"]);
                self.implicit("tbody");
                self.mode = Mode::InTableBody;
                return true;
            }
            Token::Start(t) if t.name == "table" => {
                if self.scope("table", Scope::Table) {
                    self.pop_through("table");
                    self.reset_mode();
                    return true;
                }
            }
            Token::End(t) if t == "table" => {
                if self.scope("table", Scope::Table) {
                    self.pop_through("table");
                    self.reset_mode();
                }
            }
            Token::End(t)
                if matches!(
                    t.as_str(),
                    "body"
                        | "caption"
                        | "col"
                        | "colgroup"
                        | "html"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) => {}
            Token::Start(t) if matches!(t.name.as_str(), "style" | "script" | "template") => {
                self.in_body(token, tokenizer)
            }
            Token::End(t) if t == "template" => self.end_template(),
            Token::Start(t)
                if t.name == "input"
                    && t.attrs
                        .get("type")
                        .is_some_and(|s| s.eq_ignore_ascii_case("hidden")) =>
            {
                self.insert(t);
                self.stack.pop();
            }
            Token::Start(t) if t.name == "form" => {
                if self.form.is_none() && !self.parsing_template() {
                    self.form = Some(self.insert(t));
                    self.stack.pop();
                }
            }
            Token::Eof => {}
            _ => {
                self.foster = true;
                self.in_body(token, tokenizer);
                self.foster = false;
            }
        }
        false
    }

    fn in_table_body(&mut self, token: &mut Token, tokenizer: &mut Content) -> bool {
        match token {
            Token::Start(t) if t.name == "tr" => {
                self.clear_to(&["tbody", "tfoot", "thead", "template", "html"]);
                self.insert(t);
                self.mode = Mode::InRow;
            }
            Token::Start(t) if matches!(t.name.as_str(), "td" | "th") => {
                self.clear_to(&["tbody", "tfoot", "thead", "template", "html"]);
                self.implicit("tr");
                self.mode = Mode::InRow;
                return true;
            }
            Token::End(t) if matches!(t.as_str(), "tbody" | "tfoot" | "thead") => {
                if self.scope(t, Scope::Table) {
                    self.clear_to(&["tbody", "tfoot", "thead", "template", "html"]);
                    self.stack.pop();
                    self.mode = Mode::InTable;
                }
            }
            Token::Start(t)
                if matches!(
                    t.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead"
                ) =>
            {
                return self.close_table_body();
            }
            Token::End(t) if t == "table" => return self.close_table_body(),
            Token::End(t)
                if matches!(
                    t.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th" | "tr"
                ) => {}
            _ => return self.in_table(token, tokenizer),
        }
        false
    }

    fn close_table_body(&mut self) -> bool {
        if ["tbody", "thead", "tfoot"]
            .iter()
            .any(|name| self.scope(name, Scope::Table))
        {
            self.clear_to(&["tbody", "tfoot", "thead", "template", "html"]);
            self.stack.pop();
            self.mode = Mode::InTable;
            true
        } else {
            false
        }
    }

    fn in_row(&mut self, token: &mut Token, tokenizer: &mut Content) -> bool {
        match token {
            Token::Start(t) if matches!(t.name.as_str(), "td" | "th") => {
                self.clear_to(&["tr", "template", "html"]);
                self.insert(t);
                self.mode = Mode::InCell;
                self.formatting_marker();
            }
            Token::End(t) if t == "tr" => {
                self.close_row();
            }
            Token::Start(t)
                if matches!(
                    t.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead" | "tr"
                ) =>
            {
                return self.close_row();
            }
            Token::End(t) if t == "table" => return self.close_row(),
            Token::End(t) if matches!(t.as_str(), "tbody" | "tfoot" | "thead") => {
                if self.scope(t, Scope::Table) {
                    return self.close_row();
                }
            }
            Token::End(t)
                if matches!(
                    t.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th"
                ) => {}
            _ => return self.in_table(token, tokenizer),
        }
        false
    }

    fn close_row(&mut self) -> bool {
        if self.scope("tr", Scope::Table) {
            self.clear_to(&["tr", "template", "html"]);
            self.stack.pop();
            self.mode = Mode::InTableBody;
            true
        } else {
            false
        }
    }

    fn in_cell(&mut self, token: &mut Token, tokenizer: &mut Content) -> bool {
        match token {
            Token::End(t) if matches!(t.as_str(), "td" | "th") => {
                if self.scope(t, Scope::Table) {
                    self.implied_end("");
                    self.pop_through(t);
                    self.clear_formatting();
                    self.mode = Mode::InRow;
                }
            }
            Token::Start(t)
                if matches!(
                    t.name.as_str(),
                    "caption"
                        | "col"
                        | "colgroup"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                return self.close_cell();
            }
            Token::End(t) if matches!(t.as_str(), "table" | "tbody" | "tfoot" | "thead" | "tr") => {
                if self.scope(t, Scope::Table) {
                    return self.close_cell();
                }
            }
            Token::End(t)
                if matches!(t.as_str(), "body" | "caption" | "col" | "colgroup" | "html") => {}
            _ => self.in_body(token, tokenizer),
        }
        false
    }

    fn close_cell(&mut self) -> bool {
        if self.scope("td", Scope::Table) || self.scope("th", Scope::Table) {
            self.implied_end("");
            while let Some(id) = self.stack.pop() {
                if matches!(self.html_tag(id), "td" | "th") {
                    break;
                }
            }
            self.clear_formatting();
            self.mode = Mode::InRow;
            true
        } else {
            false
        }
    }
}

pub(super) fn kind_bytes(kind: &NodeKind) -> usize {
    match kind {
        NodeKind::Document => 0,
        NodeKind::Element { tag, attrs } => {
            tag.len()
                + attrs
                    .iter()
                    .map(|(name, value)| name.len() + value.len())
                    .sum::<usize>()
        }
        NodeKind::Text(text) | NodeKind::Comment(text) => text.len(),
        NodeKind::Doctype {
            name,
            public_id,
            system_id,
        } => {
            name.len()
                + public_id.as_ref().map_or(0, String::len)
                + system_id.as_ref().map_or(0, String::len)
        }
    }
}

fn tag_bytes(tag: &Tag) -> usize {
    tag.name.len()
        + tag
            .attrs
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>()
}

fn head_tag(name: &str) -> bool {
    matches!(
        name,
        "base"
            | "basefont"
            | "bgsound"
            | "link"
            | "meta"
            | "noframes"
            | "script"
            | "style"
            | "title"
            | "template"
    )
}

fn heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

fn boundary(namespace: Namespace, name: &str, scope: Scope) -> bool {
    if namespace != Namespace::Html {
        return !matches!(scope, Scope::Table) && foreign_special(namespace, name);
    }
    match scope {
        Scope::Table => matches!(name, "html" | "table" | "template"),
        _ => {
            matches!(
                name,
                "applet"
                    | "caption"
                    | "html"
                    | "table"
                    | "td"
                    | "th"
                    | "marquee"
                    | "object"
                    | "select"
                    | "template"
            ) || matches!(scope, Scope::Button) && name == "button"
                || matches!(scope, Scope::List) && matches!(name, "ol" | "ul")
        }
    }
}

fn foreign_special(namespace: Namespace, name: &str) -> bool {
    match namespace {
        Namespace::MathMl => matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"),
        Namespace::Svg => matches!(name, "foreignObject" | "desc" | "title"),
        Namespace::Html | Namespace::Null => false,
    }
}

fn special(namespace: Namespace, name: &str) -> bool {
    if namespace != Namespace::Html {
        return foreign_special(namespace, name);
    }
    matches!(
        name,
        "address"
            | "applet"
            | "area"
            | "article"
            | "aside"
            | "base"
            | "basefont"
            | "bgsound"
            | "blockquote"
            | "body"
            | "br"
            | "button"
            | "caption"
            | "center"
            | "col"
            | "colgroup"
            | "dd"
            | "details"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "embed"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "frame"
            | "frameset"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "header"
            | "hgroup"
            | "hr"
            | "html"
            | "iframe"
            | "img"
            | "input"
            | "keygen"
            | "li"
            | "link"
            | "listing"
            | "main"
            | "marquee"
            | "menu"
            | "meta"
            | "nav"
            | "noembed"
            | "noframes"
            | "noscript"
            | "object"
            | "ol"
            | "p"
            | "param"
            | "plaintext"
            | "pre"
            | "script"
            | "search"
            | "section"
            | "select"
            | "source"
            | "style"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "template"
            | "textarea"
            | "tfoot"
            | "th"
            | "thead"
            | "title"
            | "tr"
            | "track"
            | "ul"
            | "wbr"
            | "xmp"
    )
}

fn quirks(d: &Doctype) -> QuirksMode {
    let public = d.public_id.as_deref().unwrap_or("").to_ascii_lowercase();
    let system = d.system_id.as_deref().unwrap_or("").to_ascii_lowercase();
    if d.force_quirks
        || d.name != "html"
        || matches!(
            public.as_str(),
            "-//w3o//dtd w3 html strict 3.0//en//" | "-/w3c/dtd html 4.0 transitional/en" | "html"
        )
        || system == "http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd"
        || QUIRKS_PREFIXES
            .iter()
            .any(|prefix| public.starts_with(prefix))
        || d.system_id.is_none()
            && [
                "-//w3c//dtd html 4.01 frameset//",
                "-//w3c//dtd html 4.01 transitional//",
            ]
            .iter()
            .any(|prefix| public.starts_with(prefix))
    {
        QuirksMode::Quirks
    } else if [
        "-//w3c//dtd xhtml 1.0 frameset//",
        "-//w3c//dtd xhtml 1.0 transitional//",
    ]
    .iter()
    .any(|prefix| public.starts_with(prefix))
        || d.system_id.is_some()
            && [
                "-//w3c//dtd html 4.01 frameset//",
                "-//w3c//dtd html 4.01 transitional//",
            ]
            .iter()
            .any(|prefix| public.starts_with(prefix))
    {
        QuirksMode::LimitedQuirks
    } else {
        QuirksMode::NoQuirks
    }
}

const QUIRKS_PREFIXES: &[&str] = &[
    "+//silmaril//dtd html pro v0r11 19970101//",
    "-//as//dtd html 3.0 aswedit + extensions//",
    "-//advasoft ltd//dtd html 3.0 aswedit + extensions//",
    "-//ietf//dtd html 2.0 level 1//",
    "-//ietf//dtd html 2.0 level 2//",
    "-//ietf//dtd html 2.0 strict level 1//",
    "-//ietf//dtd html 2.0 strict level 2//",
    "-//ietf//dtd html 2.0 strict//",
    "-//ietf//dtd html 2.0//",
    "-//ietf//dtd html 2.1e//",
    "-//ietf//dtd html 3.0//",
    "-//ietf//dtd html 3.2 final//",
    "-//ietf//dtd html 3.2//",
    "-//ietf//dtd html 3//",
    "-//ietf//dtd html level 0//",
    "-//ietf//dtd html level 1//",
    "-//ietf//dtd html level 2//",
    "-//ietf//dtd html level 3//",
    "-//ietf//dtd html strict level 0//",
    "-//ietf//dtd html strict level 1//",
    "-//ietf//dtd html strict level 2//",
    "-//ietf//dtd html strict level 3//",
    "-//ietf//dtd html strict//",
    "-//ietf//dtd html//",
    "-//metrius//dtd metrius presentational//",
    "-//microsoft//dtd internet explorer 2.0 html strict//",
    "-//microsoft//dtd internet explorer 2.0 html//",
    "-//microsoft//dtd internet explorer 2.0 tables//",
    "-//microsoft//dtd internet explorer 3.0 html strict//",
    "-//microsoft//dtd internet explorer 3.0 html//",
    "-//microsoft//dtd internet explorer 3.0 tables//",
    "-//netscape comm. corp.//dtd html//",
    "-//netscape comm. corp.//dtd strict html//",
    "-//o'reilly and associates//dtd html 2.0//",
    "-//o'reilly and associates//dtd html extended 1.0//",
    "-//o'reilly and associates//dtd html extended relaxed 1.0//",
    "-//sq//dtd html 2.0 hotmetal + extensions//",
    "-//softquad software//dtd hotmetal pro 6.0::19990601::extensions to html 4.0//",
    "-//softquad//dtd hotmetal pro 4.0::19971010::extensions to html 4.0//",
    "-//spyglass//dtd html 2.0 extended//",
    "-//sun microsystems corp.//dtd hotjava html//",
    "-//sun microsystems corp.//dtd hotjava strict html//",
    "-//w3c//dtd html 3 1995-03-24//",
    "-//w3c//dtd html 3.2 draft//",
    "-//w3c//dtd html 3.2 final//",
    "-//w3c//dtd html 3.2//",
    "-//w3c//dtd html 3.2s draft//",
    "-//w3c//dtd html 4.0 frameset//",
    "-//w3c//dtd html 4.0 transitional//",
    "-//w3c//dtd html experimental 19960712//",
    "-//w3c//dtd html experimental 970421//",
    "-//w3c//dtd w3 html//",
    "-//w3o//dtd w3 html 3.0//",
    "-//webtechs//dtd mozilla html 2.0//",
    "-//webtechs//dtd mozilla html//",
];
