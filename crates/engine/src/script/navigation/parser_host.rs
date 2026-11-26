//! The parser temporarily borrows the actual DOM arena from the realm. A
//! pending parser script never holds that borrow while running JavaScript.
use super::super::{
    Shared, dispatch_checked, event_target, fatal, mark_dirty, media, mutation_observers,
    restrict_document_policy, run_classic, text, unsupported,
};
use super::dynamic_scripts::{self, Flags};
use crate::{
    dom::{Document, Namespace, NodeId, mutation::MutationSink},
    html::{
        HtmlStream, HtmlStreamError, ParserEvent, ParserProgress, ParserScript, ParserScriptKind,
        WriteId,
    },
    loader::{DocumentPolicy, ResourceKind},
    network::{CredentialsMode, crossorigin_credentials},
};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
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
    pub charset: Option<&'static encoding_rs::Encoding>,
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
    meta_policy: DocumentPolicy,
    connect_policy: crate::loader::ConnectPolicy,
    client: Arc<Url>,
    base: Arc<Url>,
    base_node: Option<NodeId>,
    revision: Option<u64>,
    scripts: usize,
    sheets: usize,
    images: usize,
    constructing: bool,
    constructed: Option<ConstructedElement>,
}

struct ConstructedElement {
    original: NodeId,
    element: NodeId,
    parent: NodeId,
    before: Option<NodeId>,
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
            meta_policy: DocumentPolicy::default(),
            connect_policy: crate::loader::ConnectPolicy::default(),
            base: client.clone(),
            client,
            base_node: None,
            revision: None,
            scripts: 0,
            sheets: 0,
            images: 0,
            constructing: false,
            constructed: None,
        }
    }

    pub(super) fn meta_policy(&self) -> DocumentPolicy {
        self.meta_policy
    }

    pub(in crate::script) fn connect_policy(&self) -> &crate::loader::ConnectPolicy {
        &self.connect_policy
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
        let registry = state
            .as_ref()
            .map(|state| state.custom_elements.clone())
            .filter(|registry| registry.borrow().has_definitions());
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
            .set_mutation_sink((observers.is_some() || registry.is_some()).then(|| {
                let observers = observers.clone();
                let registry = registry.clone();
                let sink: MutationSink = Box::new(move |document, change| {
                    if let Some(registry) = &registry {
                        mutation_observers::note_connections(registry, document, &change);
                    }
                    if let Some(observers) = &observers {
                        observers.borrow_mut().note(document, change);
                    }
                });
                sink
            }));
        self.stream
            .set_custom_element_test(registry.as_ref().map(|registry| {
                let registry = registry.clone();
                Box::new(move |name: &str| registry.borrow().has_definition(name)) as _
            }));
        let before = self.stream.report();
        let result = restored.and_then(|_| {
            if let Some(done) = self.constructed.take() {
                self.stream.finish_custom_element(
                    done.original,
                    done.element,
                    done.parent,
                    done.before,
                );
                return Ok(Step::Yield);
            }
            self.advance_owned(write, observers.as_deref(), registry.as_deref())
        });
        self.stream.set_mutation_sink(None);
        self.stream.set_custom_element_test(None);
        if let Some(state) = state.as_deref_mut() {
            state.dom_bytes = self.stream.document_bytes();
            state.document = self.stream.take_document()?;
            state
                .dynamic_scripts
                .flags
                .extend(std::mem::take(&mut self.script_flags));
            state.parser_connect_policy = self.connect_policy.clone();
            if self.stream.report() != before || matches!(result, Ok(Step::Truncated)) {
                state.absolute_link_hrefs.clear();
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
        registry: Option<&RefCell<super::super::custom_elements::Registry>>,
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
            // Return the arena to the realm before invoking author code,
            // and before the next token adds children to a newly connected host.
            if self.stream.has_custom_element()
                || registry.is_some_and(|registry| registry.borrow().pending())
            {
                return Ok(Step::Yield);
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
            charset: crate::loader::script_charset(doc, node),
        })
    }

    /// The tree builder paused on the start tag. Invoke author code only
    /// after returning the native arena to the realm and releasing both borrows.
    pub(super) fn construct_element(
        handle: &Rc<RefCell<Self>>,
        vm: &mut Vm,
        state: &Shared,
    ) -> Result<(), JsError> {
        let Some(pending) = handle.borrow_mut().stream.take_custom_element() else {
            return Ok(());
        };
        handle.borrow_mut().constructing = true;
        let result = (|| {
            let wrapper = super::super::custom_elements::created(vm, state, pending.node)?;
            let element = super::super::node(state, &wrapper)?;
            let reactions = {
                let mut state = state.borrow_mut();
                let bytes = pending
                    .attributes
                    .iter()
                    .map(|(name, value)| name.len() + value.len())
                    .sum();
                vm.charge_work(bytes)?;
                let bytes = super::super::changed_bytes(&state, 0, bytes)?;
                let mut reactions = Vec::new();
                for (name, value) in pending.attributes {
                    if let Some(reaction) = super::super::custom_elements::attribute(
                        &state,
                        element,
                        &name,
                        None,
                        Some(&value),
                        None,
                    ) {
                        reactions.push(reaction);
                    }
                    if state.mutations.borrow().active() {
                        mutation_observers::note(
                            &state,
                            crate::dom::mutation::Mutation::Attribute {
                                target: element,
                                name: &name,
                                namespace: None,
                                old: None,
                            },
                        );
                    }
                    let crate::dom::NodeKind::Element { attrs, .. } =
                        &mut state.document.nodes[element.0].kind
                    else {
                        unreachable!("constructed HTML element")
                    };
                    attrs.insert(name, value);
                }
                state.dom_bytes = bytes;
                mark_dirty(&mut state);
                reactions
            };
            // Every token attribute is present, but the element is still
            // detached during these parser-created attribute callbacks.
            for reaction in reactions {
                super::super::custom_elements::deliver(vm, state, reaction)?;
            }
            Ok::<_, JsError>(element)
        })();
        let mut parser = handle.borrow_mut();
        parser.constructing = false;
        parser.constructed = Some(ConstructedElement {
            original: pending.node,
            element: result?,
            parent: pending.parent,
            before: pending.before,
        });
        Ok(())
    }

    fn discover(&mut self, progress: ParserProgress) {
        let doc = self.stream.document().expect("parser owns the document");
        for index in progress.new_nodes {
            let id = NodeId(index);
            if doc.namespace(id) != Namespace::Html {
                continue;
            }
            let tag = doc.tag(id);
            if tag == "meta"
                && let Some(content) = crate::loader::ConnectPolicy::meta_content(doc, id)
            {
                self.connect_policy.add_meta(content);
            }
            if doc.nodes[index].parent.is_some()
                && (matches!(tag, "input" | "button" | "select" | "textarea")
                    || media::has_controls(doc, id)
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
                "meta" => {
                    if let Some(policy) = DocumentPolicy::from_meta_element(doc, id) {
                        self.meta_policy.restrict(policy);
                    }
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
                    if doc.has_link_relation(id, "stylesheet")
                        && !doc.has_link_relation(id, "alternate")
                        && doc.attr(id, "disabled").is_none()
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
        Self::pump(handle, vm, state, Some(id)).map(|_| ())
    }

    fn pump(
        handle: &Rc<RefCell<Self>>,
        vm: &mut Vm,
        state: &Shared,
        write: Option<WriteId>,
    ) -> Result<bool, JsError> {
        if handle.borrow().stream.has_pending_script() {
            // The rest of these bytes stay queued ahead of original source.
            return Ok(false);
        }
        loop {
            let (step, work, policy) = {
                let mut parser = handle.borrow_mut();
                let before = parser.stream.report().work;
                let step = parser.advance(Some(state), write);
                let work = parser.stream.report().work - before;
                (step, work, parser.meta_policy())
            };
            // A write can run nested scripts or return to its caller before
            // the navigation scheduler sees these tokens.
            restrict_document_policy(vm, state, policy);
            let step = step.map_err(parser_error)?;
            mutation_observers::schedule(vm, state);
            Self::construct_element(handle, vm, state)?;
            super::super::custom_elements::flush(vm, state)?;
            if let Step::Blocking(tag) = step {
                let node = tag.node;
                let external = tag.external();
                handle.borrow_mut().nested_blocked = Some(tag);
                vm.charge_work(work)?;
                if external {
                    return Ok(false);
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
                    return Ok(false);
                }
            } else {
                vm.charge_work(work)?;
                match step {
                    Step::Yield => {}
                    Step::Truncated => return Err(parser_error(HtmlStreamError::ResourceLimit)),
                    Step::Complete => return Ok(true),
                    _ => return Ok(false),
                }
            }
        }
    }
}

fn open(vm: &mut Vm, host: &Shared) -> Result<(), JsError> {
    let mut state = host.borrow_mut();
    if state
        .parser
        .upgrade()
        .is_some_and(|parser| parser.borrow().stream.script_nesting() != 0)
    {
        return Ok(());
    }
    if !state.navigation_attached {
        return Err(unsupported(
            "Opening an HTML stream requires a browsing session",
        ));
    }

    let doc = &state.document;
    vm.charge_work(doc.nodes.len().saturating_add(state.handlers.len()))?;
    let connected: HashSet<_> = doc.connected_nodes().collect();
    event_target::clear_listeners(vm, &state.window)?;
    for node in &connected {
        if let Some(object) = state.wrappers.get(node) {
            event_target::clear_listeners(vm, object)?;
        }
    }
    state.handlers.retain(|(target, _), (_, root)| {
        let keep = target.is_some_and(|target| !connected.contains(&target));
        if !keep {
            vm.unroot(*root);
        }
        keep
    });

    let mut parser = ParserHost::new(Arc::new(state.url.clone()));
    parser.stream = HtmlStream::script_created(state.document.scripting_enabled);
    parser.connect_policy = state.parser_connect_policy.clone();
    // Establish the loan before using the live DOM. The original arena
    // remains in the realm, so old nodes still have their original identities.
    parser.stream.take_document().map_err(parser_error)?;
    let parser = Rc::new(RefCell::new(parser));
    let root = state.document.root;
    let removed = std::mem::take(&mut state.document.nodes[root.0].children);
    for node in &removed {
        state.document.nodes[node.0].parent = None;
    }
    if !removed.is_empty() {
        mutation_observers::note(
            &state,
            crate::dom::mutation::Mutation::Children {
                target: root,
                added: &[],
                removed: &removed,
                previous: None,
                next: None,
            },
        );
    }
    state.document.quirks_mode = crate::dom::QuirksMode::NoQuirks;
    state.document.truncated = false;
    state.ready_state = "loading";
    state.dynamic_scripts.pending.clear();
    state.image_updates.retain(|node| !connected.contains(node));
    state.parser = Rc::downgrade(&parser);
    state.replacement_parser = Some(parser);
    state.parser_wakeup = true;
    mark_dirty(&mut state);
    drop(state);
    super::super::frames::sweep(vm, host);
    mutation_observers::schedule(vm, host);
    super::super::custom_elements::flush(vm, host)
}

fn require_document(host: &Shared, receiver: &Value) -> Result<(), JsError> {
    if super::super::target(host, receiver)? != Some(host.borrow().document.root) {
        return Err(JsError::type_error("Document receiver required"));
    }
    Ok(())
}

fn check_dynamic_markup(
    vm: &mut Vm,
    state: &Shared,
    errors: &super::super::dom_api::Errors,
) -> Result<(), JsError> {
    if state
        .borrow()
        .parser
        .upgrade()
        .is_some_and(|parser| parser.borrow().constructing)
    {
        return Err(errors.exception(
            vm,
            "InvalidStateError",
            "Dynamic markup insertion during parser custom element construction",
        ));
    }
    Ok(())
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    document: Value,
    errors: &super::super::dom_api::Errors,
) -> Result<(), JsError> {
    let current = state.clone();
    super::super::getter(vm, document.clone(), "currentScript", move |vm, _, _| {
        let node = current.borrow().current_script;
        super::super::wrap(vm, &current, node)
    })?;
    let host = state.clone();
    let open_errors = errors.clone();
    super::super::method(vm, document.clone(), "open", move |vm, receiver, args| {
        require_document(&host, &receiver)?;
        if args.len() >= 3 {
            return Err(unsupported(
                "The window-opening overload of document.open is not implemented",
            ));
        }
        for value in args {
            vm.to_string(value)?;
        }
        check_dynamic_markup(vm, &host, &open_errors)?;
        open(vm, &host)?;
        Ok(receiver)
    })?;
    let host = state.clone();
    let close_errors = errors.clone();
    super::super::method(vm, document.clone(), "close", move |vm, receiver, _| {
        require_document(&host, &receiver)?;
        check_dynamic_markup(vm, &host, &close_errors)?;
        let Some(parser) = host.borrow().parser.upgrade().filter(|parser| {
            let parser = parser.borrow();
            parser.stream.is_script_created() && parser.stream.has_insertion_point()
        }) else {
            return Ok(Value::Undefined);
        };
        parser.borrow_mut().stream.finish().map_err(parser_error)?;
        host.borrow_mut().parser_wakeup = true;
        let idle = parser.borrow().stream.script_nesting() == 0;
        if idle && ParserHost::pump(&parser, vm, &host, None)? {
            let root = {
                let mut state = host.borrow_mut();
                state.ready_state = "interactive";
                state.document.root
            };
            dispatch_checked(vm, &host, Some(root), "readystatechange", false, false, &[])?;
        }
        Ok(Value::Undefined)
    })?;
    for (method, line_feed) in [("write", false), ("writeln", true)] {
        let host = state.clone();
        let errors = errors.clone();
        super::super::method(vm, document.clone(), method, move |vm, receiver, args| {
            require_document(&host, &receiver)?;
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
            check_dynamic_markup(vm, &host, &errors)?;
            let (parser, ignore) = {
                let host = host.borrow();
                (host.parser.upgrade(), host.ignore_destructive_writes != 0)
            };
            let parser = if let Some(parser) =
                parser.filter(|parser| parser.borrow().stream.has_insertion_point())
            {
                parser
            } else if ignore {
                return Ok(Value::Undefined);
            } else {
                open(vm, &host)?;
                let Some(parser) = host.borrow().parser.upgrade() else {
                    return Ok(Value::Undefined);
                };
                parser
            };
            if parser.borrow().stream.is_script_created() {
                host.borrow_mut().parser_wakeup = true;
            }
            ParserHost::write(&parser, vm, &host, &string)?;
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
        "about" => {
            matches!(url.path(), "sample-image" | "document-image")
                || (client.scheme() == "about" && crate::loader::is_home_image(url.as_str()))
        }
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
        script_encoding: None,
        cors,
        integrity,
        deadline,
    }
}
