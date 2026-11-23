//! An independently implemented, deliberately incomplete browser host for our VM.
//! Each tab owns one realm. Native callbacks never borrow the DOM across VM calls.
mod abort;
mod blobs;
mod canvas;
mod collections;
mod dom_api;
mod element_interface;
mod error_event;
mod event_target;
mod fetch;
mod file_list;
mod form_data;
mod forms;
mod history_api;
mod image_element;
mod intersection_observers;
mod layout_api;
mod media;
mod message_ports;
mod mutation_observers;
mod namespaces;
mod navigation;
mod navigator;
mod scheduler;
mod storage;
mod streams;
mod structured_clone;
mod text_encoding;
mod url_api;
mod webidl;
mod workers;
pub use forms::{
    ControlSnapshot, FilePickerRequest, FormKey, FormSubmission, MAX_PICKER_BYTES,
    MAX_PICKER_FILE_BYTES, MAX_PICKER_FILES, OptionSnapshot, SelectedFile,
};
pub use navigation::{DocumentSession, ResourceLoad};
pub use media::{MediaCommand, MediaKey, MediaStatus, MediaUpdate};

pub(crate) fn initial_control_snapshots(document: &Document) -> HashMap<NodeId, ControlSnapshot> {
    forms::initial_snapshots(document)
}
use crate::{
    css,
    dom::{Document, NodeId, NodeKind, mutation::Mutation},
    history::{HistoryScroll, HistoryTransition, ScriptNavigation, SessionHistory},
    loader::{DocumentPolicy, LoadedPage},
    web_storage::TabStorage,
};
use navigation::{dynamic_scripts, module_tasks};
use rust_browser_javascript::{ErrorKind, JsError, ObjectId, RootId, Value, Vm};
use scheduler::schedule_timer;
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc, time::Instant};

const NODE_LIMIT: usize = 100_000;
const DOM_BYTES_LIMIT: usize = 16 * 1024 * 1024;
const DOM_DEPTH_LIMIT: usize = 256;
type Shared = Rc<RefCell<State>>;
type FormAttributeHook = Rc<dyn Fn(&mut Vm, &Document, NodeId, &str)>;
type FormCloneHook = Rc<dyn Fn(&mut Vm, &[NodeId], usize) -> Result<(), JsError>>;

struct State {
    document: Document,
    wrappers: HashMap<NodeId, Value>,
    styles: HashMap<NodeId, Value>,
    nodes: HashMap<ObjectId, NodeId>,
    prototype: Value,
    element_prototype: Value,
    script_prototype: Value,
    canvas_prototype: Value,
    media_prototypes: media::Prototypes,
    input_prototype: Value,
    image_prototype: Value,
    image_updates: std::collections::BTreeSet<NodeId>,
    invalid_images: std::collections::HashSet<NodeId>,
    character_data_prototype: Value,
    window: Value,
    event_prototype: Value,
    error_event_prototype: Value,
    reporting_error: bool,
    handlers: HashMap<(Option<NodeId>, String), (Value, RootId)>,
    dirty: bool,
    revision: u64,
    snapshot_revision: u64,
    snapshot_document: Arc<Document>,
    ready_state: &'static str,
    current_script: Option<NodeId>,
    ignore_destructive_writes: usize,
    parser: std::rc::Weak<RefCell<navigation::ParserHost>>,
    replacement_parser: Option<Rc<RefCell<navigation::ParserHost>>>,
    parser_wakeup: bool,
    parser_connect_policy: crate::loader::ConnectPolicy,
    document_policy: DocumentPolicy,
    navigation_attached: bool,
    dynamic_scripts: dynamic_scripts::DynamicScripts,
    import_requests: module_tasks::ImportRequests,
    url: url::Url,
    base_cache: RefCell<BaseCache>,
    routing: history_api::Routing,
    executed: usize,
    diagnostics: Vec<String>,
    scheduler: scheduler::Scheduler,
    dom_bytes: usize,
    form_attribute_hook: Option<FormAttributeHook>,
    form_clone_hook: Option<FormCloneHook>,
    mutations: Rc<RefCell<mutation_observers::Observers>>,
    canvases: canvas::CanvasStore,
    media: media::Store,
}

pub struct ScriptRuntime {
    vm: Vm,
    state: Shared,
    fetch: fetch::FetchHost,
    next_stream_collection: usize,
    forms: forms::FormHost,
    layout: layout_api::LayoutHost,
    intersections: intersection_observers::Observers,
    storage: storage::StorageHost,
    messages: message_ports::MessagePorts,
    image_bitmap: canvas::ImageBitmapFactory,
    workers: workers::Workers,
}

fn mark_dirty(state: &mut State) {
    state.dirty = true;
    state.revision = state.revision.wrapping_add(1);
}

fn restrict_document_policy(vm: &mut Vm, state: &Shared, policy: DocumentPolicy) {
    let mut state = state.borrow_mut();
    state.document_policy.restrict(policy);
    vm.set_wasm_compilation_allowed(state.document_policy.allows_wasm_compilation());
}

fn document_snapshot(state: &mut State) -> Arc<Document> {
    if state.snapshot_revision != state.revision {
        state.snapshot_document = Arc::new(state.document.clone());
        state.snapshot_revision = state.revision;
    }
    state.snapshot_document.clone()
}

fn unsupported(message: impl Into<String>) -> JsError {
    JsError::new(ErrorKind::UnsupportedFeature, message)
}
fn fatal(error: &JsError) -> bool {
    matches!(
        error.kind,
        ErrorKind::ExecutionLimit | ErrorKind::HeapLimit | ErrorKind::InternalError
    )
}
fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}
fn text(vm: &mut Vm, value: Value) -> Result<String, JsError> {
    let value = vm.to_string(&value)?;
    vm.charge_work(value.len())?;
    Ok(value.to_string_lossy())
}
fn target(state: &Shared, this: &Value) -> Result<Option<NodeId>, JsError> {
    let state = state.borrow();
    if this == &state.window {
        return Ok(None);
    }
    state
        .nodes
        .get(
            &this
                .as_object()
                .ok_or_else(|| JsError::type_error("Illegal DOM receiver"))?,
        )
        .copied()
        .map(Some)
        .ok_or_else(|| JsError::type_error("Illegal DOM receiver"))
}
fn node(state: &Shared, this: &Value) -> Result<NodeId, JsError> {
    target(state, this)?.ok_or_else(|| JsError::type_error("Expected a Node receiver"))
}
fn element(state: &Shared, this: &Value) -> Result<NodeId, JsError> {
    let id = node(state, this)?;
    if matches!(
        state.borrow().document.nodes[id.0].kind,
        NodeKind::Element { .. }
    ) {
        Ok(id)
    } else {
        Err(JsError::type_error("Expected an Element receiver"))
    }
}
fn diagnostic(state: &Shared, message: String) {
    let mut state = state.borrow_mut();
    if state.diagnostics.len() < 64 {
        state.diagnostics.push(message.chars().take(2048).collect());
    }
}
fn console_preview(value: &Value, units: usize) -> String {
    match value {
        Value::String(value) => {
            String::from_utf16_lossy(&value.as_utf16()[..value.len().min(units)])
        }
        Value::Number(value) => value.to_string(),
        Value::BigInt(value) => format!("{value:?}"),
        Value::Bool(value) => value.to_string(),
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Object(_) => "[object]".into(),
        Value::Symbol(_) => "Symbol".into(),
    }
}
fn handler_target(state: &State, target: Option<NodeId>, event: &str) -> Option<NodeId> {
    if matches!(event, "load" | "popstate" | "hashchange")
        && target.is_some_and(|id| {
            state.document.namespace(id) == crate::dom::Namespace::Html
                && matches!(state.document.tag(id), "body" | "frameset")
        })
    {
        None
    } else {
        target
    }
}
fn wrap(vm: &mut Vm, state: &Shared, id: Option<NodeId>) -> Result<Value, JsError> {
    let Some(id) = id else {
        return Ok(Value::Null);
    };
    if let Some(value) = state.borrow().wrappers.get(&id) {
        return Ok(value.clone());
    }
    let prototype = {
        let state = state.borrow();
        if dynamic_scripts::is_script(&state.document, id) {
            state.script_prototype.clone()
        } else if crate::canvas::is_canvas(&state.document, id) {
            state.canvas_prototype.clone()
        } else if let Some(prototype) = state.media_prototypes.element(&state.document, id) {
            prototype
        } else if image_element::is_image(&state.document, id) {
            state.image_prototype.clone()
        } else if state.document.namespace(id) == crate::dom::Namespace::Html
            && state.document.tag(id) == "input"
        {
            state.input_prototype.clone()
        } else if matches!(
            state.document.nodes[id.0].kind,
            NodeKind::Text(_) | NodeKind::Comment(_)
        ) {
            state.character_data_prototype.clone()
        } else if matches!(state.document.nodes[id.0].kind, NodeKind::Element { .. }) {
            state.element_prototype.clone()
        } else {
            state.prototype.clone()
        }
    };
    let value = vm.new_object_with_prototype(prototype)?;
    vm.mark_platform_object(&value)?;
    // Wrappers are cached for node identity, including detached nodes. Reclaimed
    // with the entire tab realm; VM roots protect them between script tasks.
    vm.root(value.clone());
    let mut state = state.borrow_mut();
    state.nodes.insert(value.as_object().unwrap(), id);
    state.wrappers.insert(id, value.clone());
    Ok(value)
}
fn wrap_array(vm: &mut Vm, state: &Shared, ids: Vec<NodeId>) -> Result<Value, JsError> {
    let values = ids
        .into_iter()
        .map(|id| wrap(vm, state, Some(id)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(vm.new_array(&values))
}
fn descendants(doc: &Document, root: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    let mut pending = doc.nodes[root.0]
        .children
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>();
    std::iter::from_fn(move || {
        while let Some(id) = pending.pop() {
            pending.extend(doc.nodes[id.0].children.iter().rev().copied());
            if matches!(doc.nodes[id.0].kind, NodeKind::Element { .. }) {
                return Some(id);
            }
        }
        None
    })
}
fn kind_bytes(kind: &NodeKind) -> usize {
    match kind {
        NodeKind::Document => 0,
        NodeKind::Text(value) | NodeKind::Comment(value) => value.len(),
        NodeKind::Element { tag, attrs } => {
            tag.len()
                + attrs
                    .iter()
                    .map(|(key, value)| key.len() + value.len())
                    .sum::<usize>()
        }
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
fn changed_bytes(state: &State, removed: usize, added: usize) -> Result<usize, JsError> {
    let next = state
        .dom_bytes
        .saturating_sub(removed)
        .saturating_add(added);
    if next > DOM_BYTES_LIMIT {
        Err(JsError::new(
            ErrorKind::HeapLimit,
            "DOM string data exceeds 16 MiB",
        ))
    } else {
        Ok(next)
    }
}
fn set_attribute(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    key: &str,
    value: Option<String>,
) -> Result<(), JsError> {
    if key.is_empty()
        || key.chars().any(|c| {
            c.is_ascii_whitespace() || matches!(c, '\0' | '/' | '>' | '<' | '=' | '\'' | '"')
        })
    {
        return Err(JsError::type_error("Invalid attribute name"));
    }
    let host = state;
    let mut state = host.borrow_mut();
    let present = value.is_some();
    let key = if state.document.namespace(id) == crate::dom::Namespace::Html {
        key.to_ascii_lowercase()
    } else {
        key.to_owned()
    };
    let old = state.document.attr(id, &key);
    let changes_canvas_attribute = matches!(key.as_str(), "width" | "height")
        && (present || old.is_some())
        && state.document.attribute_namespace(id, &key).is_none();
    let next_bytes = changed_bytes(
        &state,
        old.map_or(0, |value| key.len() + value.len()),
        value.as_ref().map_or(0, |value| key.len() + value.len()),
    )?;
    if (present || old.is_some())
        && matches!(state.document.nodes[id.0].kind, NodeKind::Element { .. })
        && state.mutations.borrow().active()
    {
        let namespace = state.document.attribute_namespace(id, &key);
        let name = if namespace.is_some() {
            key.rsplit_once(':')
                .map_or(key.as_str(), |(_, local)| local)
        } else {
            &key
        };
        mutation_observers::note(
            &state,
            Mutation::Attribute {
                target: id,
                name,
                namespace,
                old,
            },
        );
    }
    if let NodeKind::Element { attrs, .. } = &mut state.document.nodes[id.0].kind {
        if let Some(value) = value {
            attrs.insert(key.clone(), value);
        } else {
            attrs.remove(&key);
        }
        let removed_namespace = state.document.attr(id, &key).is_none()
            && state
                .document
                .attribute_namespaces
                .remove(&(id, key.clone()))
                .is_some();
        if let Some(hook) = &state.form_attribute_hook {
            hook(vm, &state.document, id, &key);
        }
        if let Some(name) = key.strip_prefix("on")
            && let Some((_, root)) = {
                let target = handler_target(&state, Some(id), name);
                state.handlers.remove(&(target, name.into()))
            }
        {
            vm.unroot(root);
        }
        state.dom_bytes = next_bytes.saturating_sub(if removed_namespace { key.len() } else { 0 });
        mark_dirty(&mut state);
        if changes_canvas_attribute {
            canvas::attribute_changed(vm, &mut state, id, &key)?;
        }
        image_element::attribute_changed(&mut state, id, &key);
        drop(state);
        if key == "src" && media::is_media(&host.borrow().document, id) {
            media::source_changed(vm, host, id)?;
        }
        mutation_observers::schedule(vm, host);
        dynamic_scripts::attribute_changed(vm, host, id, &key, present)
    } else {
        Err(JsError::type_error("Expected an Element"))
    }
}
fn detach(doc: &mut Document, id: NodeId) {
    if let Some(parent) = doc.nodes[id.0].parent.take() {
        doc.nodes[parent.0].children.retain(|child| *child != id);
    }
}
fn set_text(vm: &mut Vm, host: &Shared, id: NodeId, value: String) -> Result<(), JsError> {
    let mut state = host.borrow_mut();
    if matches!(
        state.document.nodes[id.0].kind,
        NodeKind::Document | NodeKind::Doctype { .. }
    ) {
        return Ok(());
    }
    let old = match &state.document.nodes[id.0].kind {
        NodeKind::Text(value) | NodeKind::Comment(value) => value.len(),
        _ => 0,
    };
    let next_bytes = changed_bytes(&state, old, value.len())?;
    match &state.document.nodes[id.0].kind {
        NodeKind::Text(text) | NodeKind::Comment(text) => {
            mutation_observers::note(
                &state,
                Mutation::CharacterData {
                    target: id,
                    old: text,
                },
            );
            if let NodeKind::Text(text) | NodeKind::Comment(text) =
                &mut state.document.nodes[id.0].kind
            {
                *text = value;
            }
        }
        NodeKind::Element { .. } => {
            if !value.is_empty() && state.document.nodes.len() >= NODE_LIMIT {
                return Err(unsupported("DOM node budget exceeded"));
            }
            if state.mutations.borrow().active() {
                for &child in &state.document.nodes[id.0].children {
                    mutation_observers::removing(&state, id, child, true);
                }
            }
            let removed = std::mem::take(&mut state.document.nodes[id.0].children);
            for child in &removed {
                state.document.nodes[child.0].parent = None;
            }
            let added =
                (!value.is_empty()).then(|| state.document.append(id, NodeKind::Text(value)));
            mutation_observers::note(
                &state,
                Mutation::Children {
                    target: id,
                    added: added.as_slice(),
                    removed: &removed,
                    previous: None,
                    next: None,
                },
            );
        }
        _ => return Ok(()),
    }
    state.dom_bytes = next_bytes;
    mark_dirty(&mut state);
    drop(state);
    mutation_observers::schedule(vm, host);
    Ok(())
}

fn set_text_and_react(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    value: String,
) -> Result<(), JsError> {
    let changed_script = {
        let state = state.borrow();
        let doc = &state.document;
        let node = &doc.nodes[id.0];
        vm.charge_work(node.children.len())?;
        match &node.kind {
            NodeKind::Text(_) => node.parent,
            NodeKind::Element { .. } => Some(id),
            _ => None,
        }
        .filter(|&id| dynamic_scripts::is_script(doc, id))
    };
    set_text(vm, state, id, value)?;
    dynamic_scripts::post_connection(vm, state, changed_script)
}
fn method<F>(vm: &mut Vm, object: Value, name: &str, callback: F) -> Result<(), JsError>
where
    F: Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
{
    let function = vm.new_native_function(name, callback);
    vm.set_property(object, name, function)
}
fn getter<F>(vm: &mut Vm, object: Value, name: &str, callback: F) -> Result<(), JsError>
where
    F: Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
{
    let get = vm.new_native_function(name, callback);
    vm.define_accessor(object, name, Some(get), None)
}

impl ScriptRuntime {
    pub fn new(page: &LoadedPage) -> Result<Self, JsError> {
        Self::with_storage(page, &TabStorage::default())
    }

    pub fn with_storage(page: &LoadedPage, storage: &TabStorage) -> Result<Self, JsError> {
        let url = url::Url::parse(&page.url).unwrap_or_else(|_| page.base.clone());
        Self::new_for_history(page, SessionHistory::new(url), storage, Instant::now())
    }

    fn new_for_history(
        page: &LoadedPage,
        history: SessionHistory,
        storage: &TabStorage,
        started: Instant,
    ) -> Result<Self, JsError> {
        let dom_bytes = page
            .document
            .nodes
            .iter()
            .map(|node| kind_bytes(&node.kind))
            .sum::<usize>()
            .saturating_add(
                page.document
                    .attribute_namespaces
                    .keys()
                    .map(|(_, name)| name.len())
                    .sum(),
            );
        Self::new_with_document(
            page,
            (*page.document).clone(),
            dom_bytes,
            false,
            history,
            storage,
            started,
        )
    }

    fn new_with_document(
        page: &LoadedPage,
        document: Document,
        dom_bytes: usize,
        fresh_parser_snapshot: bool,
        history: SessionHistory,
        tab_storage: &TabStorage,
        started: Instant,
    ) -> Result<Self, JsError> {
        if dom_bytes > DOM_BYTES_LIMIT {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "DOM string data exceeds 16 MiB",
            ));
        }
        let mut vm = Vm::new();
        vm.set_wasm_compilation_allowed(page.document_policy.allows_wasm_compilation());
        let prototype = vm.new_object();
        vm.root(prototype.clone());
        let element_prototype = vm.new_object_with_prototype(prototype.clone())?;
        vm.root(element_prototype.clone());
        let script_prototype = vm.new_object_with_prototype(element_prototype.clone())?;
        vm.root(script_prototype.clone());
        let canvas_prototype = vm.new_object_with_prototype(element_prototype.clone())?;
        vm.root(canvas_prototype.clone());
        let media_prototypes = media::Prototypes::new(&mut vm, &element_prototype)?;
        let input_prototype = vm.new_object_with_prototype(element_prototype.clone())?;
        vm.root(input_prototype.clone());
        let image_prototype = vm.new_object_with_prototype(element_prototype.clone())?;
        vm.root(image_prototype.clone());
        let character_data_prototype = vm.new_object_with_prototype(prototype.clone())?;
        vm.root(character_data_prototype.clone());
        let window = vm.global();
        vm.forbid_private_elements(&window)?;
        let document_url = history.url();
        let root = document.root;
        let mut dynamic_scripts = dynamic_scripts::DynamicScripts::default();
        if !fresh_parser_snapshot {
            dynamic_scripts.seed_prepared(&document);
        }
        let mut canvases = canvas::CanvasStore::default();
        canvases.update_sources(page);
        let state = Rc::new(RefCell::new(State {
            document,
            wrappers: HashMap::new(),
            styles: HashMap::new(),
            nodes: HashMap::new(),
            prototype: prototype.clone(),
            element_prototype: element_prototype.clone(),
            script_prototype: script_prototype.clone(),
            canvas_prototype: canvas_prototype.clone(),
            media_prototypes,
            input_prototype: input_prototype.clone(),
            image_prototype: image_prototype.clone(),
            image_updates: Default::default(),
            invalid_images: Default::default(),
            character_data_prototype: character_data_prototype.clone(),
            window: window.clone(),
            event_prototype: Value::Undefined,
            error_event_prototype: Value::Undefined,
            reporting_error: false,
            handlers: HashMap::new(),
            dirty: fresh_parser_snapshot,
            revision: u64::from(fresh_parser_snapshot),
            snapshot_revision: 0,
            snapshot_document: page.document.clone(),
            ready_state: "loading",
            current_script: None,
            ignore_destructive_writes: 0,
            parser: std::rc::Weak::new(),
            replacement_parser: None,
            parser_wakeup: false,
            parser_connect_policy: page.meta_connect_policy.clone(),
            document_policy: page.document_policy,
            navigation_attached: false,
            dynamic_scripts,
            import_requests: module_tasks::ImportRequests::default(),
            url: document_url.clone(),
            base_cache: RefCell::default(),
            routing: history_api::Routing::new(history),
            executed: 0,
            diagnostics: vec![],
            scheduler: scheduler::Scheduler::new(started),
            dom_bytes,
            form_attribute_hook: None,
            form_clone_hook: None,
            mutations: Rc::default(),
            canvases,
            media: media::Store::default(),
        }));
        install_node(&mut vm, &state, prototype.clone(), &element_prototype)?;
        image_element::install(&mut vm, &state, image_prototype)?;
        install_character_data(&mut vm, &state, prototype.clone(), character_data_prototype)?;
        dynamic_scripts::install(&mut vm, &state, script_prototype)?;
        module_tasks::install(&mut vm, &state);
        install_event_handlers(&mut vm, &state, prototype.clone())?;
        install_event_handlers(&mut vm, &state, window.clone())?;
        let document = wrap(&mut vm, &state, Some(root))?;
        vm.set_global("document", document.clone())?;
        for name in ["window", "self", "globalThis"] {
            vm.set_global(name, window.clone())?;
        }
        navigator::install(&mut vm, &window)?;
        let console = vm.new_object();
        for name in ["log", "info", "warn", "error", "debug"] {
            let state = state.clone();
            method(&mut vm, console.clone(), name, move |_, _, args| {
                if state.borrow().diagnostics.len() >= 64 {
                    return Ok(Value::Undefined);
                }
                let mut message = String::new();
                for value in args.iter().take(64) {
                    let remaining = 2048usize.saturating_sub(message.len());
                    if remaining == 0 {
                        break;
                    }
                    if !message.is_empty() {
                        message.push(' ');
                    }
                    message.push_str(&console_preview(value, remaining));
                }
                diagnostic(&state, message);
                Ok(Value::Undefined)
            })?;
        }
        vm.set_global("console", console)?;
        scheduler::install_timers(&mut vm, &state)?;
        for name in ["body", "head", "documentElement"] {
            let state = state.clone();
            getter(&mut vm, document.clone(), name, move |vm, _, _| {
                let id = {
                    let state = state.borrow();
                    document_part(&state.document, name)
                };
                wrap(vm, &state, id)
            })?;
        }
        let state_get = state.clone();
        let get = vm.new_native_function("title", move |_, _, _| {
            Ok(state_get.borrow().document.title().into())
        });
        let state_set = state.clone();
        let set = vm.new_native_function("title", move |vm, _, args| {
            let value = text(vm, arg(args, 0))?;
            let title = state_set.borrow().document.html_elements("title").next();
            let id = if let Some(title) = title {
                title
            } else {
                let mut state = state_set.borrow_mut();
                if state.document.nodes.len() >= NODE_LIMIT {
                    return Err(unsupported("DOM node budget exceeded"));
                }
                let Some(parent) = document_part(&state.document, "head") else {
                    return Ok(Value::Undefined);
                };
                state.dom_bytes = changed_bytes(&state, 0, "title".len())?;
                let previous = state.document.nodes[parent.0].children.last().copied();
                let id = state.document.append(
                    parent,
                    NodeKind::Element {
                        tag: "title".into(),
                        attrs: Default::default(),
                    },
                );
                mutation_observers::note(
                    &state,
                    Mutation::Children {
                        target: parent,
                        added: &[id],
                        removed: &[],
                        previous,
                        next: None,
                    },
                );
                id
            };
            set_text(vm, &state_set, id, value)?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(document.clone(), "title", Some(get), Some(set))?;
        let state_ready = state.clone();
        getter(&mut vm, document.clone(), "readyState", move |_, _, _| {
            Ok(state_ready.borrow().ready_state.into())
        })?;
        let state_create = state.clone();
        method(
            &mut vm,
            document.clone(),
            "createElement",
            move |vm, _, args| {
                let tag = text(vm, arg(args, 0))?.to_ascii_lowercase();
                if tag.is_empty()
                    || !tag
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
                {
                    return Err(JsError::type_error("Invalid element name"));
                }
                let id = new_detached(
                    &state_create,
                    NodeKind::Element {
                        tag,
                        attrs: Default::default(),
                    },
                )?;
                wrap(vm, &state_create, Some(id))
            },
        )?;
        for name in ["createTextNode", "createComment"] {
            let state_create = state.clone();
            method(&mut vm, document.clone(), name, move |vm, _, args| {
                let value = text(vm, arg(args, 0))?;
                let kind = if name == "createTextNode" {
                    NodeKind::Text(value)
                } else {
                    NodeKind::Comment(value)
                };
                let id = new_detached(&state_create, kind)?;
                wrap(vm, &state_create, Some(id))
            })?;
        }
        let state_id = state.clone();
        method(
            &mut vm,
            document.clone(),
            "getElementById",
            move |vm, _, args| {
                let name = text(vm, arg(args, 0))?;
                let id = if name.is_empty() {
                    None
                } else {
                    let state = state_id.borrow();
                    descendants(&state.document, state.document.root)
                        .find(|&id| state.document.attr(id, "id") == Some(&name))
                };
                wrap(vm, &state_id, id)
            },
        )?;
        let dom_errors = dom_api::install(&mut vm, &state, prototype.clone(), document.clone())?;
        namespaces::install(&mut vm, &state, prototype.clone(), document.clone())?;
        let node_lists =
            collections::install(&mut vm, &state, prototype.clone(), document.clone())?;
        install_selectors(
            &mut vm,
            &state,
            &prototype,
            &element_prototype,
            &node_lists,
            &dom_errors,
        )?;
        mutation_observers::install(&mut vm, &state, node_lists)?;
        navigation::install_parser_methods(&mut vm, &state, document.clone())?;
        let file_lists = file_list::FileListHost::install(&mut vm)?;
        let forms = forms::FormHost::install(
            &mut vm,
            &state,
            prototype.clone(),
            document.clone(),
            file_lists.clone(),
        )?;
        forms.install_file_inputs(&mut vm, input_prototype)?;
        let cookie_store = page.cookies.clone();
        let cookie_state = state.clone();
        let get = vm.new_native_function("cookie", move |vm, _, _| {
            let value = cookie_store.document_cookie(&cookie_state.borrow().url);
            vm.charge_work(value.len())?;
            Ok(value.into())
        });
        let cookie_store = page.cookies.clone();
        let cookie_state = state.clone();
        let set = vm.new_native_function("cookie", move |vm, _, args| {
            let value = text(vm, arg(args, 0))?;
            cookie_store.set_document_cookie(&cookie_state.borrow().url, &value);
            Ok(Value::Undefined)
        });
        vm.define_accessor(document.clone(), "cookie", Some(get), Some(set))?;
        url_api::install(&mut vm)?;
        text_encoding::install(&mut vm)?;
        let abort = abort::AbortHost::install(&mut vm, Some(state.clone()))?;
        element_interface::install(&mut vm, prototype, element_prototype)?;
        let events = abort.events().clone();
        let streams = streams::StreamHost::install(&mut vm, abort.clone())?;
        let blobs = blobs::install(&mut vm, streams.clone())?;
        blobs.set_scheduler(&state);
        let fetch = fetch::FetchHost::install(&mut vm, page, abort, streams, blobs.clone())?;
        let event_constructor = vm.get_global("Event")?;
        let event_proto = vm.get_property(event_constructor.clone(), "prototype")?;
        let error_proto = error_event::install(&mut vm, event_constructor, event_proto.clone())?;
        vm.root(error_proto.clone());
        {
            let mut state = state.borrow_mut();
            state.event_prototype = event_proto;
            state.error_event_prototype = error_proto;
        }
        media::install(&mut vm, &state)?;
        media::install_streaming(&mut vm, &state, events.clone())?;
        let url = state.borrow().url.clone();
        let storage = storage::StorageHost::install(&mut vm, &state, tab_storage, &url)?;
        let form_data = form_data::install(&mut vm, &forms, &blobs)?;
        fetch.set_form_data(form_data);
        let layout = layout_api::LayoutHost::install(&mut vm, &state, forms.clone(), page)?;
        let intersections = intersection_observers::Observers::install(&mut vm, &state)?;
        let image_data = canvas::install(&mut vm, &state, canvas_prototype, &layout, &blobs)?;
        let image_bitmap = canvas::install_image_bitmap(&mut vm, &state, window.clone())?;
        let cloning =
            structured_clone::CloneHost::new(&mut vm, blobs, image_data, image_bitmap, file_lists)?;
        let messages = message_ports::install(&mut vm, events.clone(), cloning.clone())?;
        let workers = workers::install(&mut vm, &state, events, messages.clone(), page)?;
        cloning.install(&mut vm, window, messages.clone())?;
        scheduler::install_performance(&mut vm, &state)?;
        history_api::install(&mut vm, &state, document, &layout)?;
        let base_state = state.clone();
        fetch.set_base_resolver(Rc::new(move || document_base(&base_state.borrow())));
        fetch.set_document(&state);
        let next_stream_collection = vm.estimated_heap_bytes() + 1024 * 1024;
        Ok(Self {
            vm,
            state,
            fetch,
            next_stream_collection,
            forms,
            layout,
            intersections,
            storage,
            messages,
            image_bitmap,
            workers,
        })
    }
    pub fn eval(&mut self, source: &str) -> Result<Value, JsError> {
        self.vm.eval(source)
    }
    /// Static pages without interactive content allocate no VM.
    pub fn for_page(page: &mut LoadedPage) -> Result<Option<Self>, JsError> {
        Self::for_page_with_viewport(page, crate::layout::Viewport::default())
    }
    pub fn for_page_with_viewport(
        page: &mut LoadedPage,
        viewport: crate::layout::Viewport,
    ) -> Result<Option<Self>, JsError> {
        let handlers =
            page.document
                .connected_nodes()
                .any(|id| {
                    media::has_controls(&page.document, id)
                        || matches!(&page.document.nodes[id.0].kind, NodeKind::Element { attrs, .. }
                            if attrs.keys().any(|name| name.starts_with("on")))
                });
        if page.scripts.is_empty() && !handlers && page.form_controls.is_empty() {
            return Ok(None);
        }
        let mut runtime = Self::new(page)?;
        runtime.layout.set_viewport(viewport);
        runtime.run_initial(page);
        runtime.snapshot(page);
        Ok(Some(runtime))
    }
    pub fn run_initial(&mut self, page: &LoadedPage) {
        // The complete-tree helper is kept for standalone fixtures and API
        // callers. Browsing uses DocumentSession with a live insertion point.
        for deferred in [false, true] {
            for script in page
                .scripts
                .iter()
                .filter(|script| script.deferred == deferred)
            {
                if let Some(reason) = &script.unsupported {
                    diagnostic(&self.state, format!("{}: {reason}", script.name));
                    continue;
                }
                if let Some(source) = &script.source {
                    let external = page.document.attr(script.node, "src").is_some();
                    self.execute_classic(
                        script.node,
                        source,
                        &script.name,
                        if external { &script.name } else { page.base.as_str() },
                        external,
                    );
                }
            }
        }
        self.change_ready_state("interactive");
        let root = self.state.borrow().document.root;
        self.lifecycle_event(Some(root), "DOMContentLoaded", true);
        self.change_ready_state("complete");
        self.lifecycle_event(None, "load", false);
        report_unhandled(&mut self.vm, &self.state);
        self.vm.collect_garbage();
    }

    fn change_ready_state(&mut self, ready: &'static str) {
        let root = {
            let mut state = self.state.borrow_mut();
            state.ready_state = ready;
            state.document.root
        };
        self.lifecycle_event(Some(root), "readystatechange", false);
    }

    fn execute_classic(
        &mut self,
        node: NodeId,
        source: &str,
        name: &str,
        identifier: &str,
        external: bool,
    ) {
        let state = self.state.clone();
        let result = self
            .vm
            .with_task(|vm| run_classic(vm, &state, node, source, identifier, external, true));
        if let Err(error) = result {
            if fatal(&error) {
                mutation_observers::discard_jobs(&mut self.vm, &self.state);
            }
            diagnostic(&self.state, format!("{name}: {error}"));
        }
        report_unhandled(&mut self.vm, &self.state);
        self.vm.collect_garbage();
    }
    /// Returns whether the link's default navigation is allowed.
    pub fn click(&mut self, node: NodeId) -> bool {
        let state = self.state.clone();
        state.borrow_mut().media.activate();
        let forms = self.forms.clone();
        forms.activate_picker_gesture();
        let allowed = self
            .vm
            .with_task(|vm| {
                let allowed = forms.activate(vm, node)?;
                if allowed { media::default_click(vm, &state, node)?; }
                drain_microtask_queue(vm, &state)?;
                Ok(allowed)
            })
            .unwrap_or_else(|error| {
                diagnostic(&state, error.to_string());
                false
            });
        report_unhandled(&mut self.vm, &self.state);
        self.vm.collect_garbage();
        allowed
    }
    fn lifecycle_event(&mut self, target: Option<NodeId>, kind: &str, bubbles: bool) {
        let state = self.state.clone();
        let mode = if target.is_none() && kind == "load" {
            DispatchMode::WindowLoad
        } else {
            DispatchMode::Lifecycle
        };
        let result = self.vm.with_task(|vm| {
            dispatch_checked_with_mode(vm, &state, target, kind, bubbles, false, &[], mode)?;
            drain_microtask_queue(vm, &state)
        });
        if let Err(error) = result {
            if fatal(&error) {
                mutation_observers::discard_jobs(&mut self.vm, &self.state);
            }
            diagnostic(&self.state, format!("{kind}: {error}"));
        }
    }
    /// HTML chooses the activation target before dispatch. Its href may be
    /// changed by a listener, so the default action reads it after dispatch.
    pub fn click_link(&mut self, node: NodeId) -> Option<String> {
        self.click_with_default(node).1
    }
    pub fn click_with_default(&mut self, node: NodeId) -> (bool, Option<String>) {
        let anchor = {
            let state = self.state.borrow();
            let mut current = Some(node);
            let mut anchor = None;
            while let Some(id) = current {
                let doc = &state.document;
                let link = match doc.namespace(id) {
                    crate::dom::Namespace::Html => matches!(doc.tag(id), "a" | "area"),
                    crate::dom::Namespace::Svg => doc.tag(id) == "a",
                    _ => false,
                };
                if link && link_reference(doc, id).is_some() {
                    anchor = Some(id);
                    break;
                }
                current = state.document.nodes[id.0].parent;
            }
            anchor
        };
        if !self.click(node) {
            return (false, None);
        }
        (
            true,
            anchor.and_then(|id| {
                link_reference(&self.state.borrow().document, id).map(str::to_owned)
            }),
        )
    }
    pub fn diagnostics(&self) -> Vec<String> {
        self.state.borrow().diagnostics.clone()
    }
    pub fn last_diagnostic(&self) -> Option<String> {
        self.state.borrow().diagnostics.last().cloned()
    }
    pub fn executed_scripts(&self) -> usize {
        self.state.borrow().executed
    }
    pub fn take_network_requests(&self) -> Vec<(u64, crate::network::WebRequest)> {
        self.fetch.take_requests()
    }
    pub fn take_network_jobs(&self) -> Vec<crate::network::FetchJob> {
        self.fetch.take_jobs()
    }
    pub fn pending_network(&self) -> usize {
        self.fetch.pending_count()
    }
    pub fn has_queued_script_tasks(&self) -> bool {
        let state = self.state.borrow();
        state.navigation_attached
            && (state.parser_wakeup
                || state.dynamic_scripts.has_pending()
                || state.import_requests.has_pending()
                || !state.image_updates.is_empty())
    }
    pub fn take_form_submissions(&self) -> Vec<FormSubmission> {
        self.forms.take_submissions()
    }
    pub fn take_file_picker_request(&self) -> Option<FilePickerRequest> {
        self.forms.take_file_picker_request()
    }
    pub fn complete_file_picker(
        &mut self,
        request: &FilePickerRequest,
        files: Option<Vec<SelectedFile>>,
    ) -> Result<bool, JsError> {
        self.form_task(|forms, vm| forms.complete_file_picker(vm, request, files))
    }
    pub fn focused_control(&self) -> Option<NodeId> {
        self.forms.focused()
    }
    pub fn form_selected_text(&self) -> Option<String> {
        self.forms.selected_text()
    }
    fn form_task(
        &mut self,
        action: impl FnOnce(&forms::FormHost, &mut Vm) -> Result<bool, JsError>,
    ) -> Result<bool, JsError> {
        let forms = self.forms.clone();
        let state = self.state.clone();
        let result = self.vm.with_task(|vm| {
            let consumed = action(&forms, vm)?;
            drain_microtask_queue(vm, &state)?;
            Ok(consumed)
        });
        report_unhandled(&mut self.vm, &self.state);
        self.vm.collect_garbage();
        result
    }
    pub fn focus_control(&mut self, node: Option<NodeId>) -> Result<bool, JsError> {
        self.form_task(|forms, vm| forms.focus(vm, node))
    }
    pub fn insert_form_text(&mut self, text: &str) -> Result<bool, JsError> {
        self.form_task(|forms, vm| forms.insert_text(vm, text))
    }
    pub fn form_keydown(
        &mut self,
        key: FormKey,
        shift: bool,
        control: bool,
    ) -> Result<bool, JsError> {
        if !control && key != FormKey::Tab {
            self.forms.activate_picker_gesture();
        }
        self.form_task(|forms, vm| forms.keydown(vm, key, shift, control))
    }
    pub fn focus_next_control(&mut self, reverse: bool) -> Result<bool, JsError> {
        self.form_task(|forms, vm| forms.focus_next(vm, reverse))
    }
    pub fn choose_form_option(
        &mut self,
        select: NodeId,
        option: NodeId,
        toggle: bool,
        extend: bool,
    ) -> Result<bool, JsError> {
        self.form_task(|forms, vm| forms.choose_option(vm, select, option, toggle, extend))
    }
    pub fn cut_form_selection(&mut self) -> Result<bool, JsError> {
        self.form_task(|forms, vm| forms.cut_selection(vm))
    }
    pub fn resolve_reference(&self, reference: &str) -> Option<String> {
        document_base(&self.state.borrow())
            .join(reference)
            .ok()
            .map(Into::into)
    }
    pub fn take_navigation_request(&self) -> Option<ScriptNavigation> {
        self.state.borrow_mut().routing.next_command()
    }

    pub fn has_navigation_tasks(&self) -> bool {
        let state = self.state.borrow();
        state.routing.has_commands() || state.routing.has_events() || self.forms.has_submissions()
    }

    pub fn take_history_scroll(&self) -> Option<HistoryScroll> {
        self.state.borrow_mut().routing.take_scroll()
    }

    pub fn apply_history_transition(
        &mut self,
        transition: HistoryTransition,
    ) -> Result<(), JsError> {
        history_api::apply_transition(&mut self.vm, &self.state, transition)
    }
    pub fn complete_network(
        &mut self,
        id: u64,
        result: Result<crate::network::WebResponse, crate::network::FetchError>,
    ) -> Result<(), JsError> {
        self.complete_fetch(id, result.map(Into::into))
    }
    pub fn complete_fetch(
        &mut self,
        id: u64,
        result: Result<crate::network::StreamingResponse, crate::network::FetchError>,
    ) -> Result<(), JsError> {
        self.network_task(|fetch, vm| fetch.complete(vm, id, result), true)
    }
    pub fn progress_network(&mut self, id: u64) -> Result<(), JsError> {
        self.network_task(|fetch, vm| fetch.progress(vm, id), false)
    }
    pub fn progress_network_upload(&mut self, id: u64) -> Result<(), JsError> {
        self.network_task(|fetch, vm| fetch.progress_upload(vm, id), false)
    }
    pub fn cancel_pending_network(&mut self) -> Result<(), JsError> {
        self.network_task(fetch::FetchHost::cancel_all, true)
    }
    fn network_task(
        &mut self,
        action: impl FnOnce(&fetch::FetchHost, &mut Vm) -> Result<(), JsError>,
        collect: bool,
    ) -> Result<(), JsError> {
        let state = self.state.clone();
        let fetch = &self.fetch;
        let result = self.vm.with_task(|vm| {
            action(fetch, vm)?;
            drain_microtask_queue(vm, &state)
        });
        report_unhandled(&mut self.vm, &self.state);
        let bytes = self.vm.estimated_heap_bytes();
        if collect
            || self.vm.heap_cell_count() >= self.vm.limits().max_heap_objects / 2
            || bytes >= self.next_stream_collection
        {
            self.vm.collect_garbage();
            self.next_stream_collection = self.vm.estimated_heap_bytes() + 1024 * 1024;
        }
        result
    }
    fn drain_microtasks(&mut self) {
        let state = self.state.clone();
        if let Err(error) = self.vm.with_task(|vm| drain_microtask_queue(vm, &state)) {
            diagnostic(&state, error.to_string());
        }
    }
    pub fn has_pending_image_decodes(&self) -> bool {
        self.state.borrow().canvases.bitmap_deadline().is_some()
    }
    pub fn has_pending_intersection_observations(&self) -> bool {
        self.intersections.has_task() || self.intersections.needs_update(&self.layout)
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        let state = self.state.borrow();
        if self.vm.has_pending_jobs()
            || self.fetch.has_queued_requests()
            || self.fetch.has_queued_xhr_tasks()
            || self.layout.has_events()
            || self.intersections.has_task()
            || self.storage.has_events()
            || self.messages.has_tasks()
            || self.workers.has_tasks()
            || state.media.has_loads()
            || state.media.has_commands()
            || state.media.has_stream_tasks()
            || state.routing.has_events()
            || state.routing.has_commands()
            || self.forms.has_submissions()
            || state.navigation_attached
                && (state.dynamic_scripts.has_pending()
                    || state.import_requests.has_pending()
                    || !state.image_updates.is_empty())
        {
            return Some(Instant::now());
        }
        state
            .scheduler
            .next_deadline(self.intersections.needs_update(&self.layout))
            .into_iter()
            .chain(state.canvases.bitmap_deadline())
            .chain(self.fetch.xhr_deadline())
            .min()
    }
    pub fn set_foreground(&mut self, foreground: bool) {
        self.state.borrow_mut().scheduler.set_foreground(foreground);
    }

    pub fn set_worker_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.workers.set_wake(wake);
    }

    pub fn has_workers(&self) -> bool {
        self.workers.has_workers()
    }

    pub fn pump(&mut self) {
        if let Err(error) = self.pump_with_network_errors() {
            diagnostic(&self.state, error.to_string());
        }
    }

    pub fn pump_with_network_errors(&mut self) -> Result<(), JsError> {
        self.drain_microtasks();
        // These continuations are fetch tasks. Cap each pump's batch and
        // use the usual per-task checkpoint, not a reset inside a handler.
        for _ in 0..16 {
            if !self.fetch.has_ready_xhr_task() {
                break;
            }
            self.network_task(fetch::FetchHost::run_next_xhr_task, false)?;
        }
        media::run_stream_tasks(&mut self.vm, &self.state);
        if self.workers.has_tasks() {
            let state = self.state.clone();
            for _ in 0..32 {
                let result = self.vm.with_task(|vm| {
                    if !self.workers.dispatch_next(vm)? {
                        return Ok(false);
                    }
                    drain_microtask_queue(vm, &state)?;
                    Ok(true)
                });
                match result {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        diagnostic(&state, error.to_string());
                        break;
                    }
                }
            }
        }
        if self.messages.has_tasks() {
            let state = self.state.clone();
            for _ in 0..32 {
                let result = self.vm.with_task(|vm| {
                    if !self.messages.dispatch_next(vm)? {
                        return Ok(false);
                    }
                    drain_microtask_queue(vm, &state)?;
                    Ok(true)
                });
                match result {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        diagnostic(&state, error.to_string());
                        break;
                    }
                }
            }
        }
        if self.storage.has_events() {
            let state = self.state.clone();
            for _ in 0..32 {
                let result = self.vm.with_task(|vm| {
                    if !self.storage.dispatch_next(vm, &state)? {
                        return Ok(false);
                    }
                    drain_microtask_queue(vm, &state)?;
                    Ok(true)
                });
                match result {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        diagnostic(&state, error.to_string());
                        break;
                    }
                }
            }
        }
        if self.state.borrow().routing.has_events() {
            let state = self.state.clone();
            if let Err(error) = self.vm.with_task(|vm| history_api::run_events(vm, &state)) {
                diagnostic(&state, error.to_string());
            }
        }
        if self.layout.take_resize() {
            self.lifecycle_event(None, "resize", false);
        }
        for target in self.layout.take_events() {
            let state = self.state.clone();
            if let Err(error) = self.vm.with_task(|vm| {
                let bubbles = target.is_none();
                let target = target.or_else(|| Some(state.borrow().document.root));
                dispatch_checked(vm, &state, target, "scroll", bubbles, false, &[])?;
                drain_microtask_queue(vm, &state)
            }) {
                diagnostic(&state, error.to_string());
                break;
            }
        }
        canvas::run_image_bitmap_tasks(&mut self.vm, &self.state, self.image_bitmap);
        scheduler::run_timers(&mut self.vm, &self.state);
        self.intersections.notify(&mut self.vm, &self.state);
        let needs_rendering = self.intersections.needs_update(&self.layout);
        let timestamp = scheduler::run_frames(&mut self.vm, &self.state, needs_rendering);
        if let Some(timestamp) = timestamp {
            self.intersections
                .update(&self.layout, &self.state, timestamp);
        }
        report_unhandled(&mut self.vm, &self.state);
        self.vm.collect_garbage();
        Ok(())
    }
    pub fn snapshot(&self, page: &mut LoadedPage) -> bool {
        self.publish_canvas_frames();
        {
            let state = self.state.borrow();
            page.canvas_frames = state.canvases.frames.clone();
            page.document_policy.restrict(state.document_policy);
            if !state.dirty {
                return false;
            }
        }
        self.layout.snapshot_into(page);
        self.state.borrow_mut().dirty = false;
        true
    }

    /// Publish pending Canvas paints independently of DOM/layout mutations.
    pub fn publish_canvas_frames(&self) -> bool {
        self.state.borrow_mut().canvases.publish()
    }
}

fn link_reference(document: &Document, id: NodeId) -> Option<&str> {
    document.attr(id, "href").or_else(|| {
        (document.namespace(id) == crate::dom::Namespace::Svg)
            .then(|| document.attr_ns(id, crate::dom::AttributeNamespace::XLink, "href"))
            .flatten()
    })
}

fn report_unhandled(vm: &mut Vm, state: &Shared) {
    for value in vm.take_unhandled_rejections() {
        let message = match value {
            Value::String(value) => {
                String::from_utf16_lossy(&value.as_utf16()[..value.len().min(2048)])
            }
            Value::Number(value) => value.to_string(),
            Value::BigInt(value) => format!("{value:?}"),
            Value::Bool(value) => value.to_string(),
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::Object(_) => "object".into(),
            Value::Symbol(_) => "Symbol".into(),
        };
        diagnostic(state, format!("Unhandled promise rejection: {message}"));
    }
}

fn document_part(document: &Document, name: &str) -> Option<NodeId> {
    let root = document.nodes[document.root.0]
        .children
        .iter()
        .copied()
        .find(|id| matches!(document.nodes[id.0].kind, NodeKind::Element { .. }))?;
    if name == "documentElement" {
        return Some(root);
    }
    if document.tag(root) != "html" || document.namespace(root) != crate::dom::Namespace::Html {
        return None;
    }
    document.nodes[root.0].children.iter().copied().find(|&id| {
        if document.namespace(id) != crate::dom::Namespace::Html {
            return false;
        }
        if name == "body" {
            matches!(document.tag(id), "body" | "frameset")
        } else {
            document.tag(id) == "head"
        }
    })
}
#[derive(Default)]
struct BaseCache {
    revision: Option<u64>,
    selected: Option<FrozenBase>,
}

struct FrozenBase {
    node: NodeId,
    href: String,
    url: Option<url::Url>,
}

fn base_element(document: &Document) -> Option<(NodeId, &str)> {
    document
        .html_elements("base")
        .find_map(|id| document.attr(id, "href").map(|href| (id, href)))
}

fn resolve_base(url: &url::Url, href: &str) -> Option<url::Url> {
    url.join(href)
        .ok()
        .filter(|url| !matches!(url.scheme(), "data" | "javascript"))
}

fn live_base(document: &Document, url: &url::Url) -> url::Url {
    base_element(document)
        .and_then(|(_, href)| resolve_base(url, href))
        .unwrap_or_else(|| url.clone())
}

/// The first active base element keeps its resolved address across same-
/// document changes. Without a base element the live document URL is used.
fn document_base(state: &State) -> url::Url {
    let mut cache = state.base_cache.borrow_mut();
    if cache.revision != Some(state.revision) {
        let previous = cache.selected.take();
        cache.selected = base_element(&state.document).map(|(node, href)| {
            if let Some(base) = previous
                && base.node == node
                && base.href == href
            {
                return base;
            }
            FrozenBase {
                node,
                href: href.into(),
                url: resolve_base(&state.url, href),
            }
        });
        cache.revision = Some(state.revision);
    }
    cache
        .selected
        .as_ref()
        .and_then(|base| base.url.as_ref())
        .unwrap_or(&state.url)
        .clone()
}

fn run_classic(
    vm: &mut Vm,
    state: &Shared,
    node: NodeId,
    source: &str,
    identifier: &str,
    external: bool,
    checkpoint: bool,
) -> Result<(), JsError> {
    let old = {
        let mut state = state.borrow_mut();
        state.ignore_destructive_writes += usize::from(external);
        state.current_script.replace(node)
    };
    let evaluated = match vm.eval_with_identifier(source, identifier) {
        Ok(_) => {
            state.borrow_mut().executed += 1;
            Ok(())
        }
        Err(error) => match error_event::report(vm, state, &error, external.then_some(identifier)) {
            Ok(false) => Ok(()),
            Ok(true) => Err(error),
            Err(reporting_failure) => Err(reporting_failure),
        },
    };
    let drained = if checkpoint && evaluated.as_ref().err().is_none_or(|error| !fatal(error)) {
        drain_microtask_queue(vm, state)
    } else {
        Ok(())
    };
    {
        let mut state = state.borrow_mut();
        state.current_script = old;
        state.ignore_destructive_writes -= usize::from(external);
    }
    // Preserve fatal checkpoint failures even when evaluation also threw.
    drained?;
    evaluated
}

fn drain_microtask_queue(vm: &mut Vm, state: &Shared) -> Result<(), JsError> {
    if !mutation_observers::begin_checkpoint(state) {
        return Ok(());
    }
    let result = drain_microtask_jobs(vm, state);
    mutation_observers::finish_checkpoint(state);
    result
}

fn drain_microtask_jobs(vm: &mut Vm, state: &Shared) -> Result<(), JsError> {
    mutation_observers::schedule(vm, state);
    for _ in 0..4096 {
        if !vm.has_pending_jobs() {
            return Ok(());
        }
        match vm.drain_jobs(1) {
            Ok(_) => {}
            Err(error) if fatal(&error) => {
                mutation_observers::discard_jobs(vm, state);
                return Err(error);
            }
            Err(error) => diagnostic(state, error.to_string()),
        }
    }
    if mutation_observers::discard_jobs(vm, state) > 0 {
        diagnostic(state, "Microtask checkpoint budget exceeded".into());
    }
    Ok(())
}

fn new_detached(state: &Shared, kind: NodeKind) -> Result<NodeId, JsError> {
    let mut state = state.borrow_mut();
    let next_bytes = changed_bytes(&state, 0, kind_bytes(&kind))?;
    let doc = &mut state.document;
    if doc.nodes.len() >= NODE_LIMIT {
        return Err(unsupported("DOM node budget exceeded"));
    }
    let id = doc.append(doc.root, kind);
    detach(doc, id);
    state.dom_bytes = next_bytes;
    Ok(id)
}

fn install_node(
    vm: &mut Vm,
    state: &Shared,
    prototype: Value,
    element_prototype: &Value,
) -> Result<(), JsError> {
    // FormHost installs the live value accessor; a reflected attribute here
    // would shadow it for every element.
    for (property, attr_name, owner) in [
        ("id", "id", element_prototype),
        ("className", "class", element_prototype),
        ("href", "href", &prototype),
    ] {
        let get_state = state.clone();
        let get = vm.new_native_function(property, move |vm, this, _| {
            let id = element(&get_state, &this)?;
            let state = get_state.borrow();
            let value = state.document.attr(id, attr_name).unwrap_or("");
            vm.charge_work(value.len())?;
            Ok(value.into())
        });
        let set_state = state.clone();
        let set = vm.new_native_function(property, move |vm, this, args| {
            let id = element(&set_state, &this)?;
            let value = text(vm, arg(args, 0))?;
            set_attribute(vm, &set_state, id, attr_name, Some(value))?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(owner.clone(), property, Some(get), Some(set))?;
    }
    for property in [
        "parentNode",
        "firstChild",
        "lastChild",
        "nextSibling",
        "previousSibling",
        "ownerDocument",
    ] {
        let state = state.clone();
        getter(vm, prototype.clone(), property, move |vm, this, _| {
            let id = node(&state, &this)?;
            let related = {
                let state = state.borrow();
                let doc = &state.document;
                let current = &doc.nodes[id.0];
                match property {
                    "parentNode" => current.parent,
                    "firstChild" => current.children.first().copied(),
                    "lastChild" => current.children.last().copied(),
                    "ownerDocument" => (id != doc.root).then_some(doc.root),
                    _ => current.parent.and_then(|parent| {
                        let children = &doc.nodes[parent.0].children;
                        let index = children.iter().position(|child| *child == id)?;
                        if property == "nextSibling" {
                            children.get(index + 1).copied()
                        } else {
                            index
                                .checked_sub(1)
                                .and_then(|index| children.get(index).copied())
                        }
                    }),
                }
            };
            wrap(vm, &state, related)
        })?;
    }
    for property in ["nodeType", "nodeName", "tagName"] {
        let state = state.clone();
        let element_only = property == "tagName";
        let owner = if element_only {
            element_prototype
        } else {
            &prototype
        };
        getter(vm, owner.clone(), property, move |_, this, _| {
            let id = if element_only {
                element(&state, &this)?
            } else {
                node(&state, &this)?
            };
            let state = state.borrow();
            let (number, name) = match &state.document.nodes[id.0].kind {
                NodeKind::Document if id != state.document.root => {
                    (11, "#document-fragment".into())
                }
                NodeKind::Document => (9, "#document".into()),
                NodeKind::Text(_) => (3, "#text".into()),
                NodeKind::Comment(_) => (8, "#comment".into()),
                NodeKind::Doctype { name, .. } => (10, name.clone()),
                NodeKind::Element { tag, .. } => (
                    1,
                    if state.document.namespace(id) == crate::dom::Namespace::Html {
                        tag.to_ascii_uppercase()
                    } else {
                        tag.clone()
                    },
                ),
            };
            Ok(if property == "nodeType" {
                number.into()
            } else {
                name.into()
            })
        })?;
    }
    for name in [
        "getAttribute",
        "setAttribute",
        "removeAttribute",
        "hasAttribute",
    ] {
        let state = state.clone();
        let owner = element_prototype.clone();
        method(vm, owner, name, move |vm, this, args| {
            let id = element(&state, &this)?;
            let key = text(vm, arg(args, 0))?;
            let key = if state.borrow().document.namespace(id) == crate::dom::Namespace::Html {
                key.to_ascii_lowercase()
            } else {
                key
            };
            match name {
                "getAttribute" => {
                    let state = state.borrow();
                    let value = state.document.attr(id, &key);
                    vm.charge_work(value.map_or(0, str::len))?;
                    Ok(value.map(Value::from).unwrap_or(Value::Null))
                }
                "hasAttribute" => Ok(state.borrow().document.attr(id, &key).is_some().into()),
                "setAttribute" => {
                    let value = text(vm, arg(args, 1))?;
                    set_attribute(vm, &state, id, &key, Some(value))?;
                    Ok(Value::Undefined)
                }
                _ => {
                    set_attribute(vm, &state, id, &key, None)?;
                    Ok(Value::Undefined)
                }
            }
        })?;
    }
    install_style(vm, state, prototype)
}

fn install_selectors(
    vm: &mut Vm,
    state: &Shared,
    node_prototype: &Value,
    element_prototype: &Value,
    node_lists: &collections::StaticNodeLists,
    errors: &dom_api::Errors,
) -> Result<(), JsError> {
    for name in ["querySelector", "querySelectorAll", "matches", "closest"] {
        let state = state.clone();
        let node_lists = node_lists.clone();
        let errors = errors.clone();
        let element_only = matches!(name, "matches" | "closest");
        let owner = if element_only {
            element_prototype
        } else {
            node_prototype
        };
        method(vm, owner.clone(), name, move |vm, this, args| {
            let id = if element_only {
                element(&state, &this)?
            } else {
                node(&state, &this)?
            };
            let source = args
                .first()
                .ok_or_else(|| JsError::type_error("Missing required selector argument"))?;
            let source = text(vm, source.clone())?;
            if source.len() > 16 * 1024 {
                return Err(JsError::new(
                    ErrorKind::HeapLimit,
                    "DOM selector list exceeds 16 KiB",
                ));
            }
            let selectors =
                css::DomSelectorList::parse(&source).ok_or_else(|| errors.invalid_selector(vm))?;
            let ids = {
                let state = state.borrow();
                let doc = &state.document;
                match name {
                    "matches" => return Ok(selectors.matches(doc, id).into()),
                    "closest" => std::iter::successors(Some(id), |id| doc.nodes[id.0].parent)
                        .find(|&id| selectors.matches(doc, id))
                        .into_iter()
                        .collect(),
                    _ => {
                        let matches = descendants(doc, id).filter(|&id| selectors.matches(doc, id));
                        if name == "querySelector" {
                            matches.take(1).collect::<Vec<_>>()
                        } else {
                            matches.collect::<Vec<_>>()
                        }
                    }
                }
            };
            if name == "querySelectorAll" {
                node_lists.create(vm, ids.into())
            } else {
                wrap(vm, &state, ids.first().copied())
            }
        })?;
    }
    Ok(())
}

fn install_character_data(
    vm: &mut Vm,
    state: &Shared,
    node_prototype: Value,
    character_data_prototype: Value,
) -> Result<(), JsError> {
    for (property, prototype) in [
        ("nodeValue", node_prototype),
        ("data", character_data_prototype),
    ] {
        let get_state = state.clone();
        let get = vm.new_native_function(property, move |vm, this, _| {
            let id = node(&get_state, &this)?;
            let state = get_state.borrow();
            match &state.document.nodes[id.0].kind {
                NodeKind::Text(value) | NodeKind::Comment(value) => {
                    vm.charge_work(value.len())?;
                    Ok(value.clone().into())
                }
                _ if property == "data" => Err(JsError::type_error("Expected CharacterData")),
                _ => Ok(Value::Null),
            }
        });
        let set_state = state.clone();
        let set = vm.new_native_function(property, move |vm, this, args| {
            let id = node(&set_state, &this)?;
            let value = arg(args, 0);
            let value = if property == "nodeValue" && value.is_nullish() {
                String::new()
            } else {
                text(vm, value)?
            };
            if matches!(
                set_state.borrow().document.nodes[id.0].kind,
                NodeKind::Text(_) | NodeKind::Comment(_)
            ) {
                set_text_and_react(vm, &set_state, id, value)?;
            } else if property == "data" {
                return Err(JsError::type_error("Expected CharacterData"));
            }
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype, property, Some(get), Some(set))?;
    }
    Ok(())
}

struct InlineStyleNode(NodeId);

fn inline_declarations(state: &Shared, id: NodeId) -> Vec<css::Declaration> {
    let state = state.borrow();
    css::declarations(state.document.attr(id, "style").unwrap_or(""))
}

fn inline_style_value(state: &Shared, id: NodeId, name: &str) -> String {
    css::inline_style::specified(&inline_declarations(state, id), name).0
}

fn cssom_property_name(vm: &mut Vm, name: Value) -> Result<String, JsError> {
    let mut name = text(vm, name)?;
    if !name.starts_with("--") {
        name.make_ascii_lowercase();
    }
    Ok(name)
}

fn cssom_property_value(vm: &mut Vm, value: Value) -> Result<String, JsError> {
    if matches!(value, Value::Null) {
        Ok(String::new())
    } else {
        text(vm, value)
    }
}

fn inline_style_node(vm: &Vm, style: &Value) -> Result<NodeId, JsError> {
    vm.host_data::<InlineStyleNode>(style)
        .map(|node| node.0)
        .ok_or_else(|| JsError::type_error("Expected an inline CSS style"))
}

fn install_style(vm: &mut Vm, state: &Shared, prototype: Value) -> Result<(), JsError> {
    // Each document shares these CSS accessors across its inline styles.
    let style_prototype = vm.new_object();
    for &(property, css_name) in css::LOGICAL_BOX_LONGHANDS
        .iter()
        .chain(css::LOGICAL_BOX_SHORTHANDS)
        .chain(&[
            ("cssFloat", "float"),
            ("opacity", "opacity"),
            ("paddingLeft", "padding-left"),
        ])
    {
        let get_state = state.clone();
        let get = vm.new_native_function(property, move |vm, this, _| {
            let id = inline_style_node(vm, &this)?;
            Ok(inline_style_value(&get_state, id, css_name).into())
        });
        let set_state = state.clone();
        let set = vm.new_native_function(property, move |vm, this, args| {
            let id = inline_style_node(vm, &this)?;
            let value = cssom_property_value(vm, arg(args, 0))?;
            update_style(vm, &set_state, id, css_name, &value, false)?;
            Ok(Value::Undefined)
        });
        if property != css_name {
            vm.define_accessor(
                style_prototype.clone(),
                css_name,
                Some(get.clone()),
                Some(set.clone()),
            )?;
        }
        vm.define_accessor(style_prototype.clone(), property, Some(get), Some(set))?;
    }
    let get_state = state.clone();
    method(
        vm,
        style_prototype.clone(),
        "getPropertyValue",
        move |vm, this, args| {
            let id = inline_style_node(vm, &this)?;
            let name = cssom_property_name(vm, arg(args, 0))?;
            Ok(inline_style_value(&get_state, id, &name).into())
        },
    )?;
    let get_state = state.clone();
    method(
        vm,
        style_prototype.clone(),
        "getPropertyPriority",
        move |vm, this, args| {
            let id = inline_style_node(vm, &this)?;
            let name = cssom_property_name(vm, arg(args, 0))?;
            let declarations = inline_declarations(&get_state, id);
            let important = css::inline_style::specified(&declarations, &name).1;
            Ok(if important { "important" } else { "" }.into())
        },
    )?;
    let remove_state = state.clone();
    method(
        vm,
        style_prototype.clone(),
        "removeProperty",
        move |vm, this, args| {
            let id = inline_style_node(vm, &this)?;
            let name = cssom_property_name(vm, arg(args, 0))?;
            let declarations = inline_declarations(&remove_state, id);
            let previous = css::inline_style::specified(&declarations, &name).0;
            write_inline_style(vm, &remove_state, id, &name, declarations, None)?;
            Ok(previous.into())
        },
    )?;
    vm.root(style_prototype.clone());
    let state = state.clone();
    getter(vm, prototype, "style", move |vm, this, _| {
        let id = element(&state, &this)?;
        if let Some(style) = state.borrow().styles.get(&id) {
            return Ok(style.clone());
        }
        let style = vm.new_object_with_prototype(style_prototype.clone())?;
        vm.mark_platform_object(&style)?;
        vm.set_host_data(style.clone(), Rc::new(InlineStyleNode(id)))?;
        for (property, css_name) in [
            ("backgroundColor", "background-color"),
            ("background", "background"),
            ("color", "color"),
            ("width", "width"),
            ("height", "height"),
            ("display", "display"),
            ("fontSize", "font-size"),
            ("padding", "padding"),
            ("margin", "margin"),
            ("position", "position"),
            ("top", "top"),
            ("right", "right"),
            ("bottom", "bottom"),
            ("left", "left"),
            ("zIndex", "z-index"),
            ("overflow", "overflow"),
            ("overflowX", "overflow-x"),
            ("overflowY", "overflow-y"),
            ("transform", "transform"),
            ("transformOrigin", "transform-origin"),
            ("boxSizing", "box-sizing"),
            ("minWidth", "min-width"),
            ("maxWidth", "max-width"),
        ] {
            let get_state = state.clone();
            let get = vm.new_native_function(property, move |_, _, _| {
                Ok(inline_style_value(&get_state, id, css_name).into())
            });
            let set_state = state.clone();
            let set = vm.new_native_function(property, move |vm, _, args| {
                let value = cssom_property_value(vm, arg(args, 0))?;
                update_style(vm, &set_state, id, css_name, &value, false)?;
                Ok(Value::Undefined)
            });
            vm.define_accessor(style.clone(), property, Some(get), Some(set))?;
        }
        let get_state = state.clone();
        let get = vm.new_native_function("cssText", move |_, _, _| {
            Ok(get_state
                .borrow()
                .document
                .attr(id, "style")
                .unwrap_or("")
                .into())
        });
        let set_state = state.clone();
        let set = vm.new_native_function("cssText", move |vm, _, args| {
            let value = text(vm, arg(args, 0))?;
            set_attribute(vm, &set_state, id, "style", Some(value))?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(style.clone(), "cssText", Some(get), Some(set))?;
        let set_state = state.clone();
        method(vm, style.clone(), "setProperty", move |vm, _, args| {
            let key = cssom_property_name(vm, arg(args, 0))?;
            let value = cssom_property_value(vm, arg(args, 1))?;
            let priority = if arg(args, 2).is_nullish() {
                String::new()
            } else {
                text(vm, arg(args, 2))?
            };
            let important = priority.eq_ignore_ascii_case("important");
            if value.is_empty() || priority.is_empty() || important {
                update_style(vm, &set_state, id, &key, &value, important)?;
            }
            Ok(Value::Undefined)
        })?;
        vm.root(style.clone());
        state.borrow_mut().styles.insert(id, style.clone());
        Ok(style)
    })
}
fn update_style(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    key: &str,
    value: &str,
    important: bool,
) -> Result<(), JsError> {
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        || key.is_empty()
    {
        return Ok(());
    }
    let replacement = if value.is_empty() {
        None
    } else {
        let mut declarations = css::declarations(&format!("{key}:{value};"));
        if declarations.len() != 1 || declarations[0].name != key || declarations[0].important {
            return Ok(());
        }
        let mut declaration = declarations.pop().unwrap();
        if let Some(property) = css::inline_style::ReflectedLonghand::named(key) {
            let Some(value) = property.serialize(value) else {
                return Ok(());
            };
            declaration.value = value;
        } else if !css::supports_declaration(key, value) {
            return Ok(());
        }
        declaration.important = important;
        Some(declaration)
    };
    write_inline_style(
        vm,
        state,
        id,
        key,
        inline_declarations(state, id),
        replacement,
    )
}

fn write_inline_style(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    name: &str,
    mut declarations: Vec<css::Declaration>,
    replacement: Option<css::Declaration>,
) -> Result<(), JsError> {
    if !css::inline_style::edit(&mut declarations, name, replacement) {
        return Ok(());
    }
    let new = declarations
        .into_iter()
        .map(|d| {
            format!(
                "{}:{}{};",
                d.name,
                d.value,
                if d.important { "!important" } else { "" }
            )
        })
        .collect::<String>();
    set_attribute(vm, state, id, "style", Some(new))
}

fn install_event_handlers(vm: &mut Vm, state: &Shared, object: Value) -> Result<(), JsError> {
    install_event_handler_list(vm, state, object, &[
        "onclick",
        "onload",
        "onerror",
        "onDOMContentLoaded",
        "onbeforeinput",
        "oninput",
        "onchange",
        "onsubmit",
        "onformdata",
        "onreset",
        "oninvalid",
        "onfocus",
        "onblur",
        "onfocusin",
        "onfocusout",
        "onkeydown",
        "onscroll",
        "onresize",
        "onpopstate",
        "onhashchange",
        "onstorage",
    ])
}

fn install_event_handler_list(vm: &mut Vm, state: &Shared, object: Value, properties: &[&str]) -> Result<(), JsError> {
    for &property in properties {
        let name = property.strip_prefix("on").unwrap().to_owned();
        let get_state = state.clone();
        let get_name = name.clone();
        let get = vm.new_native_function(property, move |vm, this, _| {
            let receiver = target(&get_state, &this)?;
            let target = handler_target(&get_state.borrow(), receiver, &get_name);
            if let Some((value, _)) = get_state.borrow().handlers.get(&(target, get_name.clone())) {
                return Ok(value.clone());
            }
            let source = handler_source(&get_state.borrow(), target, &get_name);
            if let Some(source) = source {
                match vm.eval(&format!("(function(event){{\n{source}\n}})")) {
                    Ok(value) => {
                        let root = vm.root(value.clone());
                        get_state
                            .borrow_mut()
                            .handlers
                            .insert((target, get_name.clone()), (value.clone(), root));
                        Ok(value)
                    }
                    Err(error) => {
                        if fatal(&error) {
                            return Err(error);
                        }
                        diagnostic(&get_state, error.to_string());
                        Ok(Value::Null)
                    }
                }
            } else {
                Ok(Value::Null)
            }
        });
        let set_state = state.clone();
        let set = vm.new_native_function(property, move |vm, this, args| {
            let target = target(&set_state, &this)?;
            let target = handler_target(&set_state.borrow(), target, &name);
            let callback = arg(args, 0);
            let key = (target, name.clone());
            let mut state = set_state.borrow_mut();
            if let Some((_, root)) = state.handlers.remove(&key) {
                vm.unroot(root);
            }
            let callback = if vm.is_callable(&callback) {
                callback
            } else {
                Value::Null
            };
            let root = vm.root(callback.clone());
            state.handlers.insert(key, (callback, root));
            Ok(Value::Undefined)
        });
        vm.define_accessor(object.clone(), property, Some(get), Some(set))?;
    }
    Ok(())
}

fn handler_source(state: &State, target: Option<NodeId>, event: &str) -> Option<String> {
    let id = target.or_else(|| {
        if matches!(event, "load" | "popstate" | "hashchange") {
            state
                .document
                .elements("body")
                .next()
                .or_else(|| state.document.elements("frameset").next())
        } else {
            None
        }
    })?;
    state
        .document
        .attr(id, &format!("on{event}"))
        .map(str::to_owned)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DispatchMode {
    Ordinary,
    Lifecycle,
    WindowLoad,
}

impl DispatchMode {
    fn checkpoint(self, vm: &mut Vm, state: &Shared) -> Result<(), JsError> {
        if self != Self::Ordinary {
            // Lifecycle events start in the host. Cleanup after a callback
            // can run jobs before the next listener because no JS caller is
            // still on the stack (unlike dispatchEvent from a script).
            drain_microtask_queue(vm, state)?;
        }
        Ok(())
    }
}

// All DOM events share this propagation algorithm and preserve fatal host errors.
#[allow(clippy::too_many_arguments)]
fn dispatch_checked(
    vm: &mut Vm,
    state: &Shared,
    target: Option<NodeId>,
    kind: &str,
    bubbles: bool,
    cancelable: bool,
    properties: &[(&str, Value)],
) -> Result<bool, JsError> {
    dispatch_checked_with_mode(
        vm,
        state,
        target,
        kind,
        bubbles,
        cancelable,
        properties,
        DispatchMode::Ordinary,
    )
}

#[allow(clippy::too_many_arguments)]
fn dispatch_checked_with_mode(
    vm: &mut Vm,
    state: &Shared,
    target: Option<NodeId>,
    kind: &str,
    bubbles: bool,
    cancelable: bool,
    properties: &[(&str, Value)],
    mode: DispatchMode,
) -> Result<bool, JsError> {
    let prototype = state.borrow().event_prototype.clone();
    let object = vm.new_object_with_prototype(prototype)?;
    let event =
        event_target::init_event(vm, object, kind.into(), [bubbles, cancelable, false], true)?;
    dispatch_prepared_with_mode(vm, state, target, event, properties, mode)
}

fn dispatch_prepared(
    vm: &mut Vm,
    state: &Shared,
    target: Option<NodeId>,
    event: Value,
    properties: &[(&str, Value)],
) -> Result<bool, JsError> {
    dispatch_prepared_with_mode(vm, state, target, event, properties, DispatchMode::Ordinary)
}

fn dispatch_prepared_with_mode(
    vm: &mut Vm,
    state: &Shared,
    target: Option<NodeId>,
    event: Value,
    properties: &[(&str, Value)],
    mode: DispatchMode,
) -> Result<bool, JsError> {
    let info = event_target::event(vm, &event)?;
    let kind = info.kind.to_string_lossy();
    let target_value = if mode == DispatchMode::WindowLoad {
        // HTML's legacy target override affects Event.target, not the
        // invocation path: only Window receives this load event.
        let root = state.borrow().document.root;
        wrap(vm, state, Some(root))?
    } else if let Some(target) = target {
        wrap(vm, state, Some(target))?
    } else {
        state.borrow().window.clone()
    };
    let mut path = vec![target];
    if let Some(mut current) = target {
        let state = state.borrow();
        while let Some(parent) = state.document.nodes[current.0].parent {
            path.push(Some(parent));
            current = parent;
        }
        if current == state.document.root && kind != "load" {
            path.push(None);
        }
    }
    let path: Rc<[Option<NodeId>]> = path.into();
    info.begin(
        vm,
        &event,
        target_value,
        event_target::DispatchPath::Dom(Rc::downgrade(state), path.clone()),
    )?;
    let root = vm.root(event.clone());
    let result = vm.with_task(|vm| -> Result<(), JsError> {
        for (name, value) in properties {
            vm.define_data_property(event.clone(), name, value.clone(), false, true, true)?;
        }
        let ancestors = &path[1..];
        let visits = ancestors
            .iter()
            .rev()
            .map(|target| (*target, true, 1))
            .chain([(target, true, 2), (target, false, 2)])
            .chain(
                ancestors
                    .iter()
                    .take(if info.bubbles { ancestors.len() } else { 0 })
                    .map(|target| (*target, false, 3)),
            );
        for (current, capture, phase) in visits {
            if info.stopped.get() {
                break;
            }
            let this = if let Some(id) = current {
                wrap(vm, state, Some(id))?
            } else {
                state.borrow().window.clone()
            };
            info.current(vm, &event, this.clone(), phase)?;
            info.invoke_with_checkpoint(
                vm,
                &this,
                &event,
                capture,
                |_, error| {
                    diagnostic(state, format!("{kind}: {error}"));
                    Ok(())
                },
                |vm| mode.checkpoint(vm, state),
            )?;
            if !capture && !info.immediate.get() {
                let key = (current, kind.clone());
                let handler = state
                    .borrow()
                    .handlers
                    .get(&key)
                    .map(|(value, _)| value.clone());
                let handler = if let Some(value) = handler {
                    vm.is_callable(&value).then_some(value)
                } else {
                    let source = handler_source(&state.borrow(), current, &kind);
                    if let Some(source) = source {
                        match vm.eval(&format!("(function(event){{\n{source}\n}})")) {
                            Ok(value) => Some(value),
                            Err(error) => {
                                if fatal(&error) {
                                    return Err(error);
                                }
                                diagnostic(state, format!("on{kind}: {error}"));
                                None
                            }
                        }
                    } else {
                        None
                    }
                };
                if let Some(handler) = handler {
                    let error_args = if current.is_none() && kind == "error" {
                        error_event::handler_args(vm, &event)
                    } else {
                        None
                    };
                    let args = error_args
                        .as_ref()
                        .map_or(std::slice::from_ref(&event), |args| args.as_slice());
                    match vm.call(handler, this, args) {
                        Ok(Value::Bool(value))
                            if info.cancelable && value == error_args.is_some() =>
                        {
                            info.prevented.set(true);
                        }
                        Err(error) => {
                            if fatal(&error) {
                                return Err(error);
                            }
                            diagnostic(state, format!("on{kind}: {error}"));
                        }
                        _ => {}
                    }
                    mode.checkpoint(vm, state)?;
                }
            }
        }
        Ok(())
    });
    let cleanup = info.end(vm, event);
    vm.unroot(root);
    cleanup?;
    if let Err(error) = result {
        if fatal(&error) {
            mutation_observers::discard_jobs(vm, state);
        }
        return Err(error);
    }
    Ok(!info.prevented.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_views_and_title_setter_preserve_replaced_root_hierarchy() {
        let mut page =
            LoadedPage::from_html("about:test", "<!doctype html><title>Old</title><p>Body</p>");
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        assert_eq!(runtime.eval("document.replaceChildren(document.createElement('first'));document.title='ignored';document.head===null&&document.body===null&&document.documentElement.tagName==='FIRST'&&document.title==='';").unwrap(),Value::Bool(true));
        runtime.snapshot(&mut page);
        assert_eq!(page.document.nodes[page.document.root.0].children.len(), 1);
    }
    #[test]
    fn console_previews_are_bounded_and_never_call_object_to_string() {
        let page = LoadedPage::from_html("about:test", "<!doctype html><body></body>");
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        assert_eq!(
            runtime
                .eval(
                    "var calls=0;console.log({toString:function(){calls++;return 'bad';}});calls;"
                )
                .unwrap(),
            Value::from(0)
        );
        runtime
            .eval("var long='x'.repeat(20000);for(var i=0;i<1000;i++)console.log(long);")
            .unwrap();
        let logs = runtime.diagnostics();
        assert_eq!(logs.len(), 64);
        assert!(logs.iter().all(|log| log.len() <= 2048));
    }
    #[test]
    fn repeated_dom_attribute_copies_consume_the_task_work_budget() {
        let value = "x".repeat(8192);
        let page = LoadedPage::from_html(
            "about:test",
            &format!("<!doctype html><p id='p' title='{value}' class='{value}'>x</p>"),
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let error=runtime.eval("var p=document.getElementById('p');for(var i=0;i<1000;i++)p.getAttribute('title');").unwrap_err();
        assert_eq!(error.kind, ErrorKind::ExecutionLimit);
        let error = runtime
            .eval("for(var i=0;i<1000;i++)p.className;")
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ExecutionLimit);
        assert_eq!(runtime.eval("1+2").unwrap(), Value::from(3));
    }
    #[test]
    fn node_identity_mutations_events_and_roots_survive_gc() {
        let mut page = LoadedPage::from_html(
            "https://example.test/",
            "<!doctype html><title>Before</title><main><button id='button'>Click</button><p id='out'></p></main><script>var count = 0; var button = document.getElementById('button'); button.addEventListener('click', function(e) { count++; document.getElementById('out').textContent = 'Count ' + count; e.preventDefault(); });</script>",
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        runtime.run_initial(&page);
        assert!(
            runtime.diagnostics().is_empty(),
            "{:?}",
            runtime.diagnostics()
        );
        assert_eq!(
            runtime
                .eval("button === document.querySelector('#button')")
                .unwrap(),
            Value::Bool(true)
        );
        let button = page.document.elements("button").next().unwrap();
        assert!(!runtime.click(button));
        assert!(!runtime.click(button));
        assert!(runtime.snapshot(&mut page));
        let out = page.document.elements("p").next().unwrap();
        assert_eq!(page.document.text(out), "Count 2");
        runtime.eval("var p = document.createElement('p'); p.id = 'added'; p.textContent = 'New'; document.body.appendChild(p); document.title = 'After'; p.style.color = 'red';").unwrap();
        runtime.snapshot(&mut page);
        assert_eq!(page.title(), "After");
        assert!(page.document.serialize().contains("color:red;"));
        assert!(runtime.eval("p.appendChild(document.body)").is_err());
    }
    #[test]
    fn capture_bubble_once_removal_and_inline_handler_defaults() {
        let page = LoadedPage::from_html(
            "about:test",
            "<!doctype html><main><a id='link' href='#next' onclick='return false'>Link</a></main>",
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        runtime.eval("var order = ''; var a = document.getElementById('link'); document.addEventListener('click', function(){order += 'c';}, true); a.addEventListener('click', function(){order += 't';}, {once:true}); document.body.addEventListener('click', function(){order += 'b';});").unwrap();
        let link = page.document.elements("a").next().unwrap();
        assert!(!runtime.click(link));
        assert!(!runtime.click(link));
        assert_eq!(runtime.eval("order").unwrap(), Value::from("ctbcb"));
    }
    #[test]
    fn task_order_cancellation_arguments_and_idle_deadline() {
        let page = LoadedPage::from_html(
            "about:test",
            "<!doctype html><script>var order='s'; setTimeout(function(x){order+=x;queueMicrotask(function(){order+='m';});},0,'t'); var id=setTimeout(function(){order+='bad';},0); clearTimeout(id); queueMicrotask(function(){order+='q';}); document.addEventListener('DOMContentLoaded',function(){order+='d';});</script>",
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        runtime.run_initial(&page);
        assert_eq!(runtime.eval("order").unwrap(), Value::from("sqd"));
        assert!(runtime.next_deadline().is_some());
        runtime.pump();
        assert_eq!(runtime.eval("order").unwrap(), Value::from("sqdtm"));
        assert!(runtime.next_deadline().is_none());
    }
    #[test]
    fn fatal_listener_budget_stops_dispatch_and_realm_can_run_next_task() {
        let page = LoadedPage::from_html(
            "about:test",
            "<!doctype html><button id='button'>Go</button>",
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        runtime.eval("var calls=0;var b=document.getElementById('button');b.addEventListener('click',function(){calls++;while(true){}});b.addEventListener('click',function(){calls++;});").unwrap();
        let button = page.document.elements("button").next().unwrap();
        runtime.click(button);
        assert_eq!(runtime.eval("calls").unwrap(), Value::from(1));
        assert!(
            runtime
                .diagnostics()
                .iter()
                .any(|message| message.contains("ExecutionLimit") || message.contains("HeapLimit")),
            "{:?}",
            runtime.diagnostics()
        );
        assert_eq!(runtime.eval("1+2").unwrap(), Value::from(3));
    }
    #[test]
    fn finite_listeners_and_microtasks_share_the_outer_instruction_budget() {
        let page = LoadedPage::from_html(
            "about:test",
            "<!doctype html><button id='button'>Go</button>",
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        runtime.eval("var calls=0;var s='a'.repeat(200);var needle='a'.repeat(100)+'b';var b=document.getElementById('button');for(var i=0;i<128;i++)b.addEventListener('click',function(){calls++;s.indexOf(needle);});").unwrap();
        runtime.click(page.document.elements("button").next().unwrap());
        let calls = runtime.eval("calls").unwrap().as_number().unwrap();
        assert!(
            calls > 1. && calls < 128.,
            "separate callbacks reset the budget: {calls}"
        );
        runtime.eval("calls=0;var callback=function(){calls++;s.indexOf(needle);};for(var i=0;i<128;i++)queueMicrotask(callback);").unwrap();
        runtime.pump();
        let calls = runtime.eval("calls").unwrap().as_number().unwrap();
        assert!(
            calls > 1. && calls < 128.,
            "microtasks reset the budget: {calls}"
        );
        assert!(runtime.next_deadline().is_none());
    }
}
