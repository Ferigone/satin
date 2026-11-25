//! Accessible initial about:blank iframe documents. All values share the
//! parent tab's VM heap, but classic scripts execute with the child's globals.
use super::{
    ConsolePhase, Shared, State, arg, devtools_console, document_part, element, fatal, getter,
    method, scheduler, set_attribute, text, unsupported,
};
use crate::{
    dom::{Document, Namespace, NodeId},
    html::{HtmlStream, HtmlStreamError, ParserEvent, ParserScriptKind, WriteId},
};
use rust_browser_javascript::{
    ErrorKind, HostIndexedProperties, JsError, RealmId, RootId, Value, Vm,
};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::{Rc, Weak},
};

const MAX_ACTIVE_FRAMES: usize = 16;
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_DOCUMENT_NODES: usize = 8192;
const NODE_CACHE: u32 = 0;
const DEFAULT_VIEW: u32 = 1;
const ERROR_HANDLER: u32 = 0;

#[derive(Default)]
pub(super) struct Frames {
    active: HashMap<NodeId, Attached>,
}

struct Attached {
    global: RootId,
    window: Rc<FrameWindow>,
    document: Value,
}

pub(super) struct FrameWindow {
    pub(super) realm: RealmId,
    parent: Weak<RefCell<State>>,
    parser: RefCell<HtmlStream>,
    started_scripts: RefCell<HashSet<NodeId>>,
    attached: Cell<bool>,
    reporting_error: Cell<bool>,
}

/// Frame node IDs index the child parser's arena, including after document.open.
pub(super) struct FrameNode {
    frame: Rc<FrameWindow>,
    id: NodeId,
}

impl FrameNode {
    pub(super) fn with_document<T>(
        &self,
        f: impl FnOnce(&Document, NodeId, &HashSet<NodeId>) -> T,
    ) -> T {
        self.frame
            .with_document(|doc| f(doc, self.id, &self.frame.started_scripts.borrow()))
    }
}

fn eligible(doc: &Document, node: NodeId) -> bool {
    doc.tag(node) == "iframe"
        && doc.namespace(node) == Namespace::Html
        && doc.attr(node, "sandbox").is_none()
        && doc.attr(node, "srcdoc").is_none()
        && doc.attr(node, "src").is_none_or(|src| {
            let src = src.trim_ascii();
            src.is_empty() || src.eq_ignore_ascii_case("about:blank")
        })
}

fn connected(doc: &Document, node: NodeId) -> bool {
    std::iter::successors(Some(node), |id| doc.nodes[id.0].parent).any(|id| id == doc.root)
}

fn retire(vm: &mut Vm, scheduler: &mut scheduler::Scheduler, frame: Attached) {
    frame.window.attached.set(false);
    scheduler.clear_realm_timers(vm, frame.window.realm);
    vm.unroot(frame.global);
}

pub(super) fn attribute_changed(vm: &mut Vm, host: &Shared, id: NodeId, name: &str) {
    if matches!(name, "src" | "srcdoc" | "sandbox") {
        let mut state = host.borrow_mut();
        if let Some(frame) = state.frames.active.remove(&id) {
            retire(vm, &mut state.scheduler, frame);
        }
    }
}

pub(super) fn sweep(vm: &mut Vm, host: &Shared) {
    let mut state = host.borrow_mut();
    let State {
        document,
        frames,
        scheduler,
        ..
    } = &mut *state;
    let detached: Vec<_> = frames
        .active
        .keys()
        .copied()
        .filter(|&node| !connected(document, node) || !eligible(document, node))
        .collect();
    for node in detached {
        retire(
            vm,
            scheduler,
            frames.active.remove(&node).expect("registered frame"),
        );
    }
}

#[derive(Default)]
struct Index {
    revision: Option<u64>,
    nodes: Vec<NodeId>,
}

struct FrameIndex {
    host: Weak<RefCell<State>>,
    index: RefCell<Index>,
}

impl FrameIndex {
    fn read<T>(&self, operation: impl FnOnce(&[NodeId]) -> T) -> T {
        let Some(host) = self.host.upgrade() else {
            return operation(&[]);
        };
        let state = host.borrow();
        let mut index = self.index.borrow_mut();
        if index.revision != Some(state.revision) {
            index.nodes.clear();
            index.nodes.extend(
                state
                    .document
                    .html_elements("iframe")
                    .filter(|&id| eligible(&state.document, id)),
            );
            index.revision = Some(state.revision);
        }
        operation(&index.nodes)
    }
}

impl HostIndexedProperties for FrameIndex {
    fn indexed_len(&self) -> Result<usize, JsError> {
        Ok(self.read(<[NodeId]>::len))
    }

    fn indexed_get(&self, vm: &mut Vm, index: u32) -> Result<Option<Value>, JsError> {
        let Some(host) = self.host.upgrade() else {
            return Ok(None);
        };
        let Some(id) = self.read(|nodes| nodes.get(index as usize).copied()) else {
            return Ok(None);
        };
        let node = super::wrap(vm, &host, Some(id))?;
        get(vm, &host, id, node, false).map(Some)
    }
}

pub(super) fn install(vm: &mut Vm, host: &Shared, prototype: Value) -> Result<(), JsError> {
    let window = vm.global();
    let index = Rc::new(FrameIndex {
        host: Rc::downgrade(host),
        index: RefCell::default(),
    });
    vm.set_host_indexed_properties(window.clone(), index.clone())?;
    replaceable(vm, "length", move |_, _, _| Ok(index.indexed_len()?.into()))?;
    for name in ["frames", "parent"] {
        replaceable(vm, name, |vm, _, _| Ok(vm.global()))?;
    }
    vm.define_data_property(window.clone(), "top", window, false, true, false)?;
    for (name, as_document) in [("contentWindow", false), ("contentDocument", true)] {
        let host = host.clone();
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            let id = element(&host, &this)?;
            get(vm, &host, id, this, as_document)
        })?;
    }
    for name in ["src", "srcdoc"] {
        let host_get = host.clone();
        let read = vm.new_native_function(name, move |_, this, _| {
            let id = element(&host_get, &this)?;
            Ok(host_get
                .borrow()
                .document
                .attr(id, name)
                .unwrap_or("")
                .into())
        });
        let host_set = host.clone();
        let write = vm.new_native_function(name, move |vm, this, args| {
            let id = element(&host_set, &this)?;
            let value = text(vm, arg(args, 0))?;
            set_attribute(vm, &host_set, id, name, Some(value))?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype.clone(), name, Some(read), Some(write))?;
    }
    Ok(())
}

fn replaceable(
    vm: &mut Vm,
    name: &'static str,
    read: impl Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
) -> Result<(), JsError> {
    let get = vm.new_native_function(name, read);
    let set = vm.new_native_function(name, move |vm, this, args| {
        if this != vm.global() {
            return Err(JsError::type_error("Window receiver required"));
        }
        vm.define_data_property(this, name, arg(args, 0), true, true, true)?;
        Ok(Value::Undefined)
    });
    vm.define_accessor(vm.global(), name, Some(get), Some(set))
}

fn get(
    vm: &mut Vm,
    host: &Shared,
    id: NodeId,
    node: Value,
    document: bool,
) -> Result<Value, JsError> {
    let (parent, scripting) = {
        let state = host.borrow();
        if !eligible(&state.document, id) || !connected(&state.document, id) {
            return Ok(Value::Null);
        }
        if let Some(frame) = state.frames.active.get(&id) {
            return Ok(if document {
                frame.document.clone()
            } else {
                vm.root_value(frame.global).expect("attached frame root")
            });
        }
        if state.frames.active.len() >= MAX_ACTIVE_FRAMES {
            return Err(unsupported("Initial iframe realm budget exhausted"));
        }
        (state.window.clone(), state.document.scripting_enabled)
    };
    let (realm, window) = vm.new_realm()?;
    let mut parser = HtmlStream::new(scripting);
    parser.finish().map_err(parser_error)?;
    let frame = Rc::new(FrameWindow {
        realm,
        parent: Rc::downgrade(host),
        parser: RefCell::new(parser),
        started_scripts: RefCell::default(),
        attached: Cell::new(true),
        reporting_error: Cell::new(false),
    });
    let doc = vm.with_realm(realm, |vm| {
        frame.pump(vm, None)?;
        install_document(vm, &frame, parent, node)
    })?;
    let global = vm.root(window.clone());
    host.borrow_mut().frames.active.insert(
        id,
        Attached {
            global,
            window: frame,
            document: doc.clone(),
        },
    );
    Ok(if document { doc } else { window })
}

fn frame_document(vm: &Vm, receiver: &Value) -> Result<Rc<FrameWindow>, JsError> {
    vm.host_data(receiver)
        .ok_or_else(|| JsError::type_error("Document receiver required"))
}

fn install_document(
    vm: &mut Vm,
    frame: &Rc<FrameWindow>,
    parent: Value,
    element: Value,
) -> Result<Value, JsError> {
    let window = vm.global();
    let doc = vm.new_object();
    vm.forbid_private_elements(&window)?;
    vm.mark_platform_object(&doc)?;
    vm.set_host_data(doc.clone(), frame.clone())?;
    vm.set_host_slot(doc.clone(), DEFAULT_VIEW, window.clone())?;
    let cache = vm.new_object_with_prototype(Value::Null)?;
    vm.set_host_slot(doc.clone(), NODE_CACHE, cache)?;
    for (name, value, writable, configurable) in [
        ("window", window.clone(), false, false),
        ("self", window.clone(), true, true),
        ("frames", window.clone(), true, true),
        ("parent", parent.clone(), true, true),
        ("top", parent, false, false),
        ("frameElement", element, false, true),
        ("length", 0.into(), true, true),
        ("document", doc.clone(), false, false),
    ] {
        vm.define_data_property(window.clone(), name, value, writable, true, configurable)?;
    }
    vm.set_host_slot(window.clone(), ERROR_HANDLER, Value::Null)?;
    let get = vm.new_native_function("onerror", |vm, this, _| {
        vm.host_slot(&this, ERROR_HANDLER)
            .ok_or_else(|| JsError::type_error("Window receiver required"))
    });
    let set = vm.new_native_function("onerror", |vm, this, args| {
        if this != vm.global() {
            return Err(JsError::type_error("Window receiver required"));
        }
        let value = arg(args, 0);
        let handler = if vm.is_callable(&value) {
            value
        } else {
            Value::Null
        };
        vm.set_host_slot(this, ERROR_HANDLER, handler)?;
        Ok(Value::Undefined)
    });
    vm.define_accessor(window.clone(), "onerror", Some(get), Some(set))?;
    getter(vm, doc.clone(), "defaultView", |vm, this, _| {
        frame_document(vm, &this)?;
        Ok(vm
            .host_slot(&this, DEFAULT_VIEW)
            .expect("frame Document window"))
    })?;
    for name in ["body", "head", "documentElement"] {
        getter(vm, doc.clone(), name, move |vm, this, _| {
            let host = frame_document(vm, &this)?;
            let node = host.with_document(|doc| document_part(doc, name));
            wrap(vm, &this, host, node)
        })?;
    }
    getter(vm, doc.clone(), "title", |vm, this, _| {
        let host = frame_document(vm, &this)?;
        vm.charge_work(host.parser.borrow().document_bytes())?;
        Ok(host.with_document(Document::title).into())
    })?;
    method(vm, doc.clone(), "getElementById", |vm, this, args| {
        let host = frame_document(vm, &this)?;
        let key = text(vm, arg(args, 0))?;
        let node = host.with_document(|doc| {
            doc.connected_nodes()
                .find(|&id| doc.attr(id, "id") == Some(&key))
        });
        vm.charge_work(host.with_document(|doc| doc.nodes.len()))?;
        wrap(vm, &this, host, node)
    })?;
    method(vm, doc.clone(), "open", |vm, this, args| {
        let host = frame_document(vm, &this)?;
        if args.len() >= 3 {
            return Err(unsupported(
                "The window-opening overload of document.open is not implemented",
            ));
        }
        for value in args {
            vm.to_string(value)?;
        }
        host.open(vm)?;
        Ok(this)
    })?;
    method(vm, doc.clone(), "close", |vm, this, _| {
        let host = frame_document(vm, &this)?;
        let idle = {
            let mut parser = host.parser.borrow_mut();
            if !parser.has_insertion_point() {
                return Ok(Value::Undefined);
            }
            parser.finish().map_err(parser_error)?;
            parser.script_nesting() == 0
        };
        if idle {
            host.pump(vm, None)?;
        }
        Ok(Value::Undefined)
    })?;
    for (name, newline) in [("write", false), ("writeln", true)] {
        method(vm, doc.clone(), name, move |vm, this, args| {
            let host = frame_document(vm, &this)?;
            let mut input = String::new();
            for value in args {
                let part = text(vm, value.clone())?;
                if input.len().saturating_add(part.len()) >= MAX_DOCUMENT_BYTES {
                    return Err(document_limit());
                }
                input.push_str(&part);
            }
            if newline {
                input.push('\n');
            }
            if !host.parser.borrow().has_insertion_point() {
                host.open(vm)?;
            }
            let id = {
                let mut parser = host.parser.borrow_mut();
                let used = parser
                    .report()
                    .input_bytes
                    .saturating_add(parser.document_bytes());
                if used.saturating_add(input.len()) > MAX_DOCUMENT_BYTES {
                    return Err(document_limit());
                }
                parser.write(&input).map_err(parser_error)?
            };
            host.pump(vm, Some(id))?;
            Ok(Value::Undefined)
        })?;
    }
    if let Some(host) = frame.parent.upgrade() {
        scheduler::install_frame_timers(vm, &host, frame)?;
    }
    scheduler::install_queue_microtask(vm, window)?;
    Ok(doc)
}

fn wrap(
    vm: &mut Vm,
    doc: &Value,
    frame: Rc<FrameWindow>,
    id: Option<NodeId>,
) -> Result<Value, JsError> {
    let Some(id) = id else { return Ok(Value::Null) };
    let cache = vm
        .host_slot(doc, NODE_CACHE)
        .expect("frame document wrapper cache");
    let key = id.0.to_string();
    let cached = vm.get_property(cache.clone(), &key)?;
    if !cached.is_undefined() {
        return Ok(cached);
    }
    let wrapper = vm.new_object();
    vm.mark_platform_object(&wrapper)?;
    vm.set_host_data(
        wrapper.clone(),
        Rc::new(FrameNode {
            frame: frame.clone(),
            id,
        }),
    )?;
    getter(vm, wrapper.clone(), "textContent", move |vm, _, _| {
        let value = frame.with_document(|doc| doc.text(id));
        vm.charge_work(value.len())?;
        Ok(value.into())
    })?;
    vm.define_data_property(cache, &key, wrapper.clone(), false, false, false)?;
    Ok(wrapper)
}

impl FrameWindow {
    fn with_document<T>(&self, read: impl FnOnce(&Document) -> T) -> T {
        read(
            self.parser
                .borrow()
                .document()
                .expect("frame owns its document"),
        )
    }

    pub(super) fn is_attached(&self) -> bool {
        self.attached.get()
    }

    fn open(&self, vm: &mut Vm) -> Result<(), JsError> {
        let mut parser = self.parser.borrow_mut();
        if parser.script_nesting() != 0 {
            return Ok(());
        }
        vm.with_realm(self.realm, |vm| {
            vm.set_host_slot(vm.global(), ERROR_HANDLER, Value::Null)
        })?;
        let retained = parser.document_bytes();
        let mut replacement = HtmlStream::script_created(
            parser
                .document()
                .expect("frame owns its document")
                .scripting_enabled,
        );
        replacement.take_document().map_err(parser_error)?;
        let mut document = parser.take_document().map_err(parser_error)?;
        let children = std::mem::take(&mut document.nodes[document.root.0].children);
        for id in children {
            document.nodes[id.0].parent = None;
        }
        document.quirks_mode = crate::dom::QuirksMode::NoQuirks;
        document.truncated = false;
        replacement
            .restore_document_from_host(document, retained)
            .map_err(parser_error)?;
        *parser = replacement;
        Ok(())
    }

    fn pump(&self, vm: &mut Vm, write: Option<WriteId>) -> Result<(), JsError> {
        vm.with_realm(self.realm, |vm| self.advance(vm, write))
    }

    fn advance(&self, vm: &mut Vm, write: Option<WriteId>) -> Result<(), JsError> {
        loop {
            let event = {
                let mut parser = self.parser.borrow_mut();
                let work = parser.report().work;
                let event = parser.next_event().map_err(parser_error)?;
                vm.charge_work(parser.report().work.saturating_sub(work))?;
                if parser
                    .document()
                    .is_some_and(|doc| doc.nodes.len() > MAX_DOCUMENT_NODES)
                {
                    return Err(document_limit());
                }
                event
            };
            match event {
                ParserEvent::Script(script) => {
                    if self.with_document(|doc| {
                        super::dynamic_scripts::has_source(doc, script.node)
                            && super::dynamic_scripts::kind(doc, script.node).is_some()
                    }) {
                        self.started_scripts.borrow_mut().insert(script.node);
                    }
                    let source = self.with_document(|doc| {
                        (!script.external
                            && script.kind == ParserScriptKind::ClassicBlocking
                            && doc.attr(script.node, "nomodule").is_none())
                        .then(|| doc.text(script.node))
                    });
                    let Some(source) = source else {
                        self.parser
                            .borrow_mut()
                            .skip_script(script.node)
                            .map_err(parser_error)?;
                        continue;
                    };
                    self.parser
                        .borrow_mut()
                        .begin_script(script.node)
                        .map_err(parser_error)?;
                    let (result, recoverable_child_limit) =
                        vm.eval_embedded_script(&source, "about:blank");
                    let result = self.report_error(vm, result, recoverable_child_limit);
                    self.parser
                        .borrow_mut()
                        .resume_script(script.node)
                        .map_err(parser_error)?;
                    result?;
                }
                ParserEvent::Progress(progress) => {
                    if let Some(node) = progress.closed
                        && self.parser.borrow().report().completed
                        && self.with_document(|doc| super::dynamic_scripts::is_script(doc, node))
                    {
                        self.started_scripts.borrow_mut().insert(node);
                    }
                    for node in progress.new_nodes {
                        let policy = self.with_document(|doc| {
                            crate::loader::DocumentPolicy::from_meta_element(doc, NodeId(node))
                        });
                        if policy.is_some_and(|p| !p.allows_wasm_compilation()) {
                            vm.set_wasm_compilation_allowed(false);
                        }
                    }
                }
                ParserEvent::WriteComplete(id) if Some(id) == write => return Ok(()),
                ParserEvent::WriteComplete(_) => {}
                ParserEvent::Complete | ParserEvent::NeedInput | ParserEvent::Paused { .. } => {
                    return Ok(());
                }
                ParserEvent::Truncated => return Err(document_limit()),
            }
        }
    }

    pub(super) fn report_script_error(
        &self,
        vm: &mut Vm,
        result: Result<Value, JsError>,
    ) -> Result<(), JsError> {
        self.report_error(vm, result, false)
    }

    fn report_error(
        &self,
        vm: &mut Vm,
        result: Result<Value, JsError>,
        recoverable_child_limit: bool,
    ) -> Result<(), JsError> {
        let Err(error) = result else { return Ok(()) };
        if fatal(&error) && !recoverable_child_limit {
            return Err(error);
        }
        if !self.reporting_error.replace(true) {
            let window = vm.global();
            let handled = (|| {
                let handler = vm
                    .host_slot(&window, ERROR_HANDLER)
                    .expect("frame Window handler slot");
                if !vm.is_callable(&handler) {
                    return Ok(false);
                }
                let reason = error
                    .thrown
                    .clone()
                    .map_or_else(|| vm.error_value(&error), Ok)?;
                let message = if error.kind == ErrorKind::Thrown {
                    format!("Uncaught {}", error.message)
                } else {
                    format!("Uncaught {}: {}", error.kind.name(), error.message)
                };
                let result = vm.call(
                    handler,
                    window,
                    &[
                        message.into(),
                        "about:blank".into(),
                        error.line.unwrap_or(0).into(),
                        error.column.unwrap_or(0).into(),
                        reason,
                    ],
                )?;
                Ok::<_, JsError>(result == Value::Bool(true))
            })();
            self.reporting_error.set(false);
            match handled {
                Ok(true) => return Ok(()),
                Err(error) if fatal(&error) => return Err(error),
                _ => {}
            }
        }
        if let Some(host) = self.parent.upgrade() {
            devtools_console::record_error(
                &host,
                ConsolePhase::ClassicScript,
                Some("about:blank"),
                &error,
            );
        }
        Ok(())
    }
}

fn document_limit() -> JsError {
    JsError::new(
        ErrorKind::HeapLimit,
        "Initial iframe document budget exhausted",
    )
}

fn parser_error(error: HtmlStreamError) -> JsError {
    if error == HtmlStreamError::ResourceLimit {
        document_limit()
    } else {
        JsError::new(ErrorKind::InternalError, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use crate::{loader::LoadedPage, script::ScriptRuntime};
    use rust_browser_javascript::{ErrorKind, Value};

    #[test]
    fn initial_iframe_writes_classic_scripts_and_calls_parent_functions() {
        let page = LoadedPage::from_html(
            "https://example.test/",
            "<!doctype html><iframe hidden></iframe>",
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let result = runtime.eval(r#"
            var frame = document.querySelector('iframe');
            var child = frame.contentWindow, doc = frame.contentDocument, shared = {};
            var equal = frames === window && frames[0] === child && length === 1 &&
                doc === child.document && doc.defaultView === child && child.parent === window;
            doc.open();
            child.onResult = function (value, own) { shared.same = value === shared; shared.own = own; return Array; };
            child.shared = shared;
            doc.writeln("<script>window.errors = 0; onerror = function () { ++this.errors; return true; };</script>");
            doc.writeln("<script>var =;</script><script>throw Error('test');</script>");
            doc.writeln("<script>");
            doc.writeln("var local = {}; var ctor = onResult(shared, local);");
            var duringWrite = shared.own === undefined;
            doc.writeln("</script>");
            doc.write("<p id='text'>done</p>");
            doc.close();
            var wrote = doc.getElementById('text').textContent === 'done';
            frame.remove();
            equal && duringWrite && wrote && child.errors === 2 && shared.same && shared.own === child.local &&
                child.ctor === Array && child.Object !== Object && Object.getPrototypeOf(child.local) === child.Object.prototype &&
                frame.contentWindow === null && length === 0;
        "#).unwrap();
        assert_eq!(result, Value::Bool(true));
        assert!(
            runtime.diagnostics().is_empty(),
            "{:?}",
            runtime.diagnostics()
        );
    }

    #[test]
    fn child_script_budget_failure_reaches_its_error_handler_and_the_parent_continues() {
        let page = LoadedPage::from_html("https://example.test/", "<iframe></iframe>");
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let value = runtime
            .eval(
                r#"
            var child = document.querySelector('iframe').contentWindow;
            var d = child.document, finished = 0, problem = '';
            d.open();
            child.onerror = function (message) { problem = message; return true; };
            d.write('<script>while (true);</script>');
            child.finish = function () { finished++; };
            d.write('<script>finish()</script>');
            finished === 1 && problem.indexOf('ExecutionLimit') >= 0;
        "#,
            )
            .unwrap();
        assert_eq!(value, Value::Bool(true));
        let heap_error = runtime.eval(r#"
            problem = '';
            d.write('<script>var allocations = []; while (true) { allocations.push({}); }</script>');
            d.write('<script>finish()</script>');
            finished === 2 && (problem.indexOf('HeapLimit') >= 0 || problem.indexOf('ExecutionLimit') >= 0);
        "#).unwrap();
        assert_eq!(heap_error, Value::Bool(true));
        // Re-entering several scripts cannot replenish the enclosing task.
        let error = runtime
            .eval(
                r#"
            for (var i = 0; i < 100; i++) d.write('<script>while (true);</script>');
        "#,
            )
            .unwrap_err();
        assert!(matches!(
            error.kind,
            ErrorKind::ExecutionLimit | ErrorKind::HeapLimit
        ));
    }

    #[test]
    fn child_operation_limits_report_the_original_error_and_resume_its_caller() {
        let page = LoadedPage::from_html("https://example.test/", "<iframe></iframe>");
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let value = runtime.eval(r#"
            var child = document.querySelector('iframe').contentWindow, errors = [];
            child.document.open();
            child.onerror = function (message) { errors.push(message); return true; };
            child.document.write('<script>new RegExp("(?:a".repeat(200) + "hello" + ")".repeat(200));</script>');
            child.document.write('<script>"x".repeat(67108865);</script>');
            child.document.write('<script>this.finished = true;</script>');
            errors.join("|") + "|" + child.finished;
        "#).unwrap();
        assert_eq!(
            value,
            Value::from(
                "Uncaught ExecutionLimit: Regular expression nesting limit exceeded|Uncaught ExecutionLimit: String code-unit budget exhausted|true"
            )
        );
    }

    #[test]
    fn unsupported_child_script_reports_to_window_onerror_before_the_following_script() {
        let page = LoadedPage::from_html("https://example.test/", "<iframe></iframe>");
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let value = runtime.eval(r#"
            var child = document.querySelector('iframe').contentWindow, d = child.document;
            d.open();
            var observed = '';
            child.onerror = function (message, url, line, col, error) {
                if (message.indexOf('UnsupportedFeature') >= 0 && error.message) observed += 'error ';
                return true;
            };
            child.finish = function () { observed += 'next'; };
            d.write('<script>try { eval(String.fromCharCode(39, 55296, 39)); } catch (e) { finish(); }</script>');
            d.write('<script>finish()</script>');
            observed;
        "#).unwrap();
        assert_eq!(value, Value::from("error next"));
    }

    #[test]
    fn discarded_initial_realms_release_their_globals_and_intrinsics() {
        let page = LoadedPage::from_html("https://example.test/", "<body></body>");
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        runtime.eval("function cycle() { const f = document.createElement('iframe'); document.body.appendChild(f); const w = f.contentWindow; w.marker = {}; f.remove(); }").unwrap();
        runtime.vm.collect_garbage();
        let before = runtime.vm.heap_cell_count();
        for _ in 0..32 {
            runtime.eval("cycle()").unwrap();
            runtime.pump_with_network_errors().unwrap();
        }
        runtime.vm.collect_garbage();
        assert!(runtime.state.borrow().frames.active.is_empty());
        // The DOM arena retains stable node identities and one wrapper per
        // created element. Entire per-frame intrinsic graphs must not remain.
        assert!(runtime.vm.heap_cell_count() <= before + 4 * 32);
    }

    #[test]
    fn initial_iframe_timers_use_the_child_window_and_are_cancelled_on_removal() {
        let page = LoadedPage::from_html(
            "https://example.test/",
            "<!doctype html><iframe hidden></iframe>",
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        runtime
            .eval(
                r#"
            var frame = document.querySelector('iframe'), child = frame.contentWindow, hits = 0;
            child.hit = function (receiver) { if (receiver === child) hits++; };
            child.document.write("<script>setTimeout(function () { hit(this); }, 0);</script>");
            child.document.close();
        "#,
            )
            .unwrap();
        runtime.pump();
        assert_eq!(runtime.eval("hits").unwrap(), Value::from(1));
        runtime
            .eval("child.setTimeout(function () { hits++; }, 0); frame.remove();")
            .unwrap();
        runtime.pump();
        assert_eq!(runtime.eval("hits").unwrap(), Value::from(1));
    }

    #[test]
    fn navigated_and_sandboxed_iframes_do_not_expose_an_initial_child_document() {
        let page = LoadedPage::from_html(
            "https://example.test/",
            "<!doctype html><iframe id='f'></iframe><iframe id='s' sandbox></iframe>",
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let result = runtime
            .eval(
                r#"
            var frame = document.getElementById('f'), sandbox = document.getElementById('s');
            var initial = frame.contentWindow;
            frame.src = 'https://elsewhere.invalid/';
            var unavailable = frame.contentWindow === null && frame.contentDocument === null &&
                sandbox.contentWindow === null && sandbox.contentDocument === null;
            frame.removeAttribute('src');
            unavailable && frame.contentWindow !== null && frame.contentWindow !== initial;
        "#,
            )
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }
}
