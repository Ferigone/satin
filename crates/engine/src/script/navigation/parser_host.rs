//! The parser temporarily borrows the actual DOM arena from the realm. A
//! pending parser script never holds that borrow while running JavaScript.
use super::super::{Shared, fatal, mark_dirty, mutation_observers, run_classic, text, unsupported};
use super::dynamic_scripts::{self, Flags};
use crate::{
    dom::{Document, Namespace, NodeId, mutation::MutationSink},
    html::{
        HtmlStream, HtmlStreamError, ParserEvent, ParserProgress, ParserScript, ParserScriptKind,
        WriteId,
    },
    loader::ResourceKind,
    network::{CredentialsMode, crossorigin_credentials},
};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use url::Url;

const MAX_PARSE_STEPS: usize = 512;
const MAX_PAGE_SCRIPTS: usize = 192;
const MAX_TRACKED_SHEETS: usize = 32;
const MAX_TRACKED_IMAGES: usize = 48;
const MAX_URL_SOURCE: usize = 6 * 1024 * 1024;
const MAX_HTTP_URL: usize = 64 * 1024;

pub(super) enum Source {
    Inline(String),
    External(Result<Url, String>),
}
pub(super) struct ScriptTag {
    pub node: NodeId,
    pub kind: ParserScriptKind,
    pub asynchronous: bool,
    pub base: Arc<Url>,
    pub source: Source,
    pub cors: Option<CredentialsMode>,
    pub integrity: Option<Arc<str>>,
}
impl ScriptTag {
    pub fn external(&self) -> bool {
        matches!(self.source, Source::External(_))
    }
}

pub(super) enum Discovery {
    Sheet {
        node: NodeId,
        url: Url,
        cors: Option<CredentialsMode>,
        integrity: Option<Arc<str>>,
        media: Arc<str>,
    },
    InlineSheet {
        node: NodeId,
        base: Arc<Url>,
    },
    Image {
        node: NodeId,
        src: String,
        url: Url,
        cors: Option<CredentialsMode>,
    },
    Script(ScriptTag),
    Error {
        node: NodeId,
        message: String,
    },
}

pub(super) enum Step {
    Yield,
    ScriptCheckpoint,
    Blocking(ScriptTag),
    WriteComplete,
    NeedInput,
    Complete,
    Truncated,
}

pub(in crate::script) struct ParserHost {
    pub stream: HtmlStream,
    pub(super) discoveries: Vec<Discovery>,
    pub(super) nested_blocked: Option<ScriptTag>,
    pub(super) script_flags: HashMap<NodeId, Flags>,
    pub discarded: usize,
    pub wants_realm: bool,
    client: Arc<Url>,
    base: Arc<Url>,
    base_node: Option<NodeId>,
    revision: Option<u64>,
    scripts: usize,
    sheets: usize,
    images: usize,
}

impl ParserHost {
    pub(super) fn new(client: Arc<Url>) -> Self {
        Self {
            stream: HtmlStream::new(true),
            discoveries: vec![],
            nested_blocked: None,
            script_flags: HashMap::new(),
            discarded: 0,
            wants_realm: false,
            base: client.clone(),
            client,
            base_node: None,
            revision: None,
            scripts: 0,
            sheets: 0,
            images: 0,
        }
    }

    /// Advance several tokens, or stop at a point where the embedding must
    /// act. State is temporarily borrowed only for non-reentrant Rust work.
    pub(super) fn advance(
        &mut self,
        state: Option<&Shared>,
        write: Option<WriteId>,
    ) -> Result<Step, HtmlStreamError> {
        let mut state = state.map(|state| state.borrow_mut());
        let observers = state
            .as_ref()
            .map(|state| state.mutations.clone())
            .filter(|observers| observers.borrow().active());
        let mut restored = Ok(());
        if let Some(state) = state.as_deref_mut() {
            if self.revision != Some(state.revision) || self.client.as_ref() != &state.url {
                if self.client.as_ref() != &state.url {
                    self.client = Arc::new(state.url.clone());
                }
                self.base_node = first_base(&state.document);
                self.base = Arc::new(super::super::document_base(state));
            }
            restored = self
                .stream
                .restore_document_from_host(std::mem::take(&mut state.document), state.dom_bytes);
        }
        self.stream
            .set_mutation_sink(observers.as_ref().map(|observers| {
                let observers = observers.clone();
                let sink: MutationSink = Box::new(move |document, change| {
                    observers.borrow_mut().note(document, change);
                });
                sink
            }));
        let before = self.stream.report();
        let result = restored.and_then(|_| self.advance_owned(write, observers.as_deref()));
        self.stream.set_mutation_sink(None);
        if let Some(state) = state.as_deref_mut() {
            state.dom_bytes = self.stream.document_bytes();
            state.document = self.stream.take_document()?;
            state
                .dynamic_scripts
                .flags
                .extend(std::mem::take(&mut self.script_flags));
            if self.stream.report() != before || matches!(result, Ok(Step::Truncated)) {
                mark_dirty(state);
            }
            self.revision = Some(state.revision);
        }
        result
    }

    fn advance_owned(
        &mut self,
        write: Option<WriteId>,
        observers: Option<&RefCell<mutation_observers::Observers>>,
    ) -> Result<Step, HtmlStreamError> {
        let slice_end = Instant::now() + Duration::from_millis(3);
        for index in 0..MAX_PARSE_STEPS {
            match self.stream.next_event() {
                Ok(ParserEvent::Progress(progress)) => self.discover(progress),
                Ok(ParserEvent::Script(event)) => {
                    if write.is_none()
                        && observers.is_some_and(|watchers| watchers.borrow().pending_checkpoint())
                    {
                        return Ok(Step::ScriptCheckpoint);
                    }
                    let node = event.node;
                    if let Some(tag) = self.prepare(event) {
                        if tag.kind == ParserScriptKind::ClassicBlocking {
                            return Ok(Step::Blocking(tag));
                        }
                        self.discoveries.push(Discovery::Script(tag));
                        self.stream.skip_script(node)?;
                        // Apply maps and start fetches at these precise
                        // parser positions before scanning later script tags.
                        if write.is_none() {
                            return Ok(Step::Yield);
                        }
                    } else {
                        self.stream.skip_script(node)?;
                    }
                }
                Ok(ParserEvent::WriteComplete(id)) if Some(id) == write => {
                    return Ok(Step::WriteComplete);
                }
                Ok(ParserEvent::WriteComplete(_)) => {}
                Ok(ParserEvent::Complete) => return Ok(Step::Complete),
                Ok(ParserEvent::Truncated) | Err(HtmlStreamError::ResourceLimit) => {
                    return Ok(Step::Truncated);
                }
                Ok(ParserEvent::NeedInput) => return Ok(Step::NeedInput),
                Ok(ParserEvent::Paused { .. }) if write.is_some() => {
                    return Ok(Step::WriteComplete);
                }
                Ok(ParserEvent::Paused { .. }) => return Err(HtmlStreamError::ScriptMismatch),
                Err(error) => return Err(error),
            }
            if index % 32 == 31 && Instant::now() >= slice_end {
                break;
            }
        }
        Ok(Step::Yield)
    }

    fn prepare(&mut self, event: ParserScript) -> Option<ScriptTag> {
        let node = event.node;
        let doc = self.stream.document()?;
        if !connected(doc, node)
            || !dynamic_scripts::has_source(doc, node)
            || dynamic_scripts::kind(doc, node).is_none()
        {
            self.script_flags.insert(node, Flags::default());
            return None;
        }
        self.script_flags.insert(node, Flags::PARSER_STARTED);
        if event.kind == ParserScriptKind::Data
            || matches!(
                event.kind,
                ParserScriptKind::ClassicBlocking
                    | ParserScriptKind::ClassicDefer
                    | ParserScriptKind::ClassicAsync
            ) && doc.attr(node, "nomodule").is_some()
        {
            return None;
        }
        if self.scripts >= MAX_PAGE_SCRIPTS {
            self.discarded += 1;
            return None;
        }
        let source = if let Some(src) = doc.attr(node, "src") {
            Source::External(resolve_resource(&self.client, &self.base, src))
        } else {
            Source::Inline(dynamic_scripts::child_text(doc, node).collect())
        };
        self.scripts += 1;
        let cors = cors_attr(doc, node);
        Some(ScriptTag {
            node,
            kind: event.kind,
            asynchronous: event.kind == ParserScriptKind::ClassicAsync
                || doc.attr(node, "async").is_some(),
            base: self.base.clone(),
            source,
            cors,
            integrity: integrity_attr(doc, node),
        })
    }

    fn discover(&mut self, progress: ParserProgress) {
        let doc = self.stream.document().expect("parser owns the document");
        for index in progress.new_nodes {
            let id = NodeId(index);
            if doc.namespace(id) != Namespace::Html {
                continue;
            }
            let tag = doc.tag(id);
            if doc.nodes[index].parent.is_some()
                && (matches!(tag, "input" | "button" | "select" | "textarea")
                    || matches!(&doc.nodes[index].kind, crate::dom::NodeKind::Element { attrs, .. }
                        if attrs.keys().any(|name| name.starts_with("on"))))
                && connected(doc, id)
            {
                self.wants_realm = true;
            }
            match tag {
                "script" => {
                    self.script_flags.insert(id, Flags::PARSING);
                }
                "base"
                    if self.base_node.is_none()
                        && doc.attr(id, "href").is_some()
                        && connected(doc, id) =>
                {
                    self.base_node = Some(id);
                    self.base = base_for(doc, &self.client, self.base_node);
                }
                "link"
                    if doc.attr(id, "rel").is_some_and(|rel| {
                        rel.split_ascii_whitespace()
                            .any(|part| part.eq_ignore_ascii_case("stylesheet"))
                            && !rel
                                .split_ascii_whitespace()
                                .any(|part| part.eq_ignore_ascii_case("alternate"))
                    }) && doc.attr(id, "disabled").is_none()
                        && connected(doc, id) =>
                {
                    if self.sheets >= MAX_TRACKED_SHEETS {
                        self.discarded += 1;
                        continue;
                    }
                    if doc.attr(id, "type").is_some_and(|ty| {
                        !ty.trim().is_empty() && !ty.trim().eq_ignore_ascii_case("text/css")
                    }) {
                        continue;
                    }
                    if let Some(href) = doc.attr(id, "href") {
                        self.sheets += 1;
                        match resolve_resource(&self.client, &self.base, href) {
                            Ok(url) => self.discoveries.push(Discovery::Sheet {
                                node: id,
                                url,
                                cors: cors_attr(doc, id),
                                integrity: integrity_attr(doc, id),
                                media: Arc::from(doc.attr(id, "media").unwrap_or("")),
                            }),
                            Err(message) => self
                                .discoveries
                                .push(Discovery::Error { node: id, message }),
                        }
                    }
                }
                "img" if connected(doc, id) => {
                    if self.images >= MAX_TRACKED_IMAGES {
                        self.discarded += 1;
                        continue;
                    }
                    if let Some(src) = doc.attr(id, "src") {
                        self.images += 1;
                        match resolve_resource(&self.client, &self.base, src) {
                            Ok(url) => self.discoveries.push(Discovery::Image {
                                node: id,
                                src: src.into(),
                                url,
                                cors: cors_attr(doc, id),
                            }),
                            Err(message) => self
                                .discoveries
                                .push(Discovery::Error { node: id, message }),
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(id) = progress.closed
            && dynamic_scripts::is_script(doc, id)
        {
            // Scripts in template contents can run when moved into the active
            // document. An unfinished script consumed at EOF has started.
            let flags = if self.stream.report().completed {
                Flags::PARSER_STARTED
            } else {
                Flags::default()
            };
            self.script_flags.insert(id, flags);
        }
        if let Some(id) = progress.closed
            && doc.tag(id) == "style"
            && connected(doc, id)
            && self.sheets < MAX_TRACKED_SHEETS
        {
            self.sheets += 1;
            self.discoveries.push(Discovery::InlineSheet {
                node: id,
                base: self.base.clone(),
            });
        }
    }

    pub(super) fn write(
        handle: &Rc<RefCell<Self>>,
        vm: &mut Vm,
        state: &Shared,
        value: &str,
    ) -> Result<(), JsError> {
        let id = handle
            .borrow_mut()
            .stream
            .write(value)
            .map_err(parser_error)?;
        if handle.borrow().nested_blocked.is_some() {
            // The rest of these bytes stay queued ahead of original source.
            return Ok(());
        }
        loop {
            let (step, work) = {
                let mut parser = handle.borrow_mut();
                let before = parser.stream.report().work;
                let step = parser
                    .advance(Some(state), Some(id))
                    .map_err(parser_error)?;
                let work = parser.stream.report().work - before;
                (step, work)
            };
            mutation_observers::schedule(vm, state);
            if let Step::Blocking(tag) = step {
                let node = tag.node;
                let external = tag.external();
                handle.borrow_mut().nested_blocked = Some(tag);
                vm.charge_work(work)?;
                if external {
                    return Ok(());
                }
                let Some(ScriptTag {
                    source: Source::Inline(source),
                    base,
                    ..
                }) = handle.borrow_mut().nested_blocked.take()
                else {
                    unreachable!("inline node has a text source");
                };
                handle
                    .borrow_mut()
                    .stream
                    .begin_script(node)
                    .map_err(parser_error)?;
                let execution = run_classic(vm, state, node, &source, base.as_str(), false, false);
                handle
                    .borrow_mut()
                    .stream
                    .resume_script(node)
                    .map_err(parser_error)?;
                if let Err(error) = execution {
                    if fatal(&error) {
                        return Err(error);
                    }
                    super::super::diagnostic(state, format!("Written inline script: {error}"));
                }
                if handle.borrow().nested_blocked.is_some() {
                    return Ok(());
                }
            } else {
                vm.charge_work(work)?;
                match step {
                    Step::Yield => {}
                    Step::Truncated => return Err(parser_error(HtmlStreamError::ResourceLimit)),
                    _ => return Ok(()),
                }
            }
        }
    }
}

pub(super) fn install(vm: &mut Vm, state: &Shared, document: Value) -> Result<(), JsError> {
    let current = state.clone();
    super::super::getter(vm, document.clone(), "currentScript", move |vm, _, _| {
        let node = current.borrow().current_script;
        super::super::wrap(vm, &current, node)
    })?;
    for (method, line_feed) in [("write", false), ("writeln", true)] {
        let host = state.clone();
        super::super::method(vm, document.clone(), method, move |vm, receiver, args| {
            if super::super::target(&host, &receiver)? != Some(host.borrow().document.root) {
                return Err(JsError::type_error("Document receiver required"));
            }
            let mut string = String::new();
            for arg in args {
                let piece = text(vm, arg.clone())?;
                if string.len().saturating_add(piece.len()) > crate::html::MAX_INPUT_BYTES {
                    return Err(JsError::new(
                        ErrorKind::HeapLimit,
                        "document.write source exceeds 8 MiB",
                    ));
                }
                vm.charge_work(piece.len())?;
                string.push_str(&piece);
            }
            if line_feed {
                string.push('\n');
            }
            let (parser, ignore) = {
                let host = host.borrow();
                (host.parser.upgrade(), host.ignore_destructive_writes != 0)
            };
            if let Some(parser) =
                parser.filter(|parser| parser.borrow().stream.has_insertion_point())
            {
                ParserHost::write(&parser, vm, &host, &string)?;
            } else if !ignore {
                return Err(unsupported(
                    "Destructive document.write/document.open is not implemented",
                ));
            }
            Ok(Value::Undefined)
        })?;
    }
    Ok(())
}

fn parser_error(error: HtmlStreamError) -> JsError {
    JsError::new(
        if error == HtmlStreamError::ResourceLimit {
            ErrorKind::HeapLimit
        } else {
            ErrorKind::InternalError
        },
        error.to_string(),
    )
}

pub(super) fn connected(doc: &Document, mut node: NodeId) -> bool {
    loop {
        if node == doc.root {
            return true;
        }
        if let Some(parent) = doc.nodes[node.0].parent {
            node = parent;
        } else {
            return false;
        }
    }
}
fn first_base(doc: &Document) -> Option<NodeId> {
    doc.html_elements("base")
        .find(|id| doc.attr(*id, "href").is_some())
}
fn base_for(doc: &Document, client: &Arc<Url>, node: Option<NodeId>) -> Arc<Url> {
    node.and_then(|id| doc.attr(id, "href"))
        .and_then(|href| client.join(href).ok())
        .filter(|url| !matches!(url.scheme(), "data" | "javascript"))
        .map(Arc::new)
        .unwrap_or_else(|| client.clone())
}
pub(super) fn cors_attr(doc: &Document, node: NodeId) -> Option<CredentialsMode> {
    crossorigin_credentials(doc.attr(node, "crossorigin"))
}
pub(super) fn integrity_attr(doc: &Document, node: NodeId) -> Option<Arc<str>> {
    doc.attr(node, "integrity")
        .filter(|value| !value.is_empty())
        .map(Arc::from)
}

pub(super) fn resolve_resource(client: &Url, base: &Url, source: &str) -> Result<Url, String> {
    let source = source.trim_matches(|c: char| c.is_ascii_whitespace());
    if source.is_empty() || source.len() > MAX_URL_SOURCE {
        return Err("Resource URL is empty or exceeds its size budget".into());
    }
    let url = base
        .join(source)
        .map_err(|e| format!("Invalid resource URL: {e}"))?;
    let allowed = match url.scheme() {
        "http" | "https" => url.as_str().len() <= MAX_HTTP_URL,
        "data" => url.as_str().len() <= MAX_URL_SOURCE,
        "file" => client.scheme() == "file" && url.as_str().len() <= MAX_HTTP_URL,
        "about" => matches!(url.path(), "sample-image" | "document-image"),
        _ => false,
    };
    if !allowed {
        return Err("Resource URL or scheme is not allowed".into());
    }
    Ok(url)
}

pub(super) fn resource_request(
    url: Url,
    client: &Arc<Url>,
    kind: ResourceKind,
    cors: Option<CredentialsMode>,
    integrity: Option<Arc<str>>,
    deadline: Instant,
) -> crate::loader::ResourceRequest {
    crate::loader::ResourceRequest {
        url,
        client: client.clone(),
        kind,
        cors,
        integrity,
        deadline,
    }
}
