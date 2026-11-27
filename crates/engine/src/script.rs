//! An independently implemented, deliberately incomplete browser host for our VM.
//! A tab shares one VM between its document and accessible child realms.
//! Native callbacks never borrow the DOM across VM calls.
mod abort;
mod base64_api;
mod blobs;
mod canvas;
mod class_list;
mod collections;
mod crypto;
mod custom_elements;
mod dataset;
mod range;
mod selection;
mod devtools_console;
mod inspection_sources;
pub use inspection_sources::InspectedSource;
mod document_interface;
mod dom_api;
mod dom_implementation;
mod element_interface;
mod error_event;
mod event_target;
mod ui_events;
mod websocket;
mod fetch;
mod file_list;
mod form_data;
mod forms;
pub(crate) use forms::default_placeholder_shown;
mod frames;
mod history_api;
mod image_element;
mod link_element;
mod intersection_observers;
mod layout_api;
mod media;
mod media_queries;
mod message_ports;
mod mutation_observers;
mod namespaces;
mod navigation;
mod navigator;
mod scheduler;
mod shadow_dom;
mod storage;
mod streams;
mod structured_clone;
mod style_sheets;
mod text_encoding;
mod tree_walker;
#[cfg(feature = "twitch-bootstrap-profile")]
mod twitch_profile;
mod url_api;
mod webidl;
mod workers;
pub use devtools_console::{
    CONSOLE_RECORD_LIMIT, CONSOLE_TEXT_BYTES, ConsoleLevel, ConsoleOrigin, ConsolePhase,
    ConsoleRecord, ConsoleSnapshot,
};
pub use forms::{
    ControlSnapshot, FilePickerRequest, FormKey, FormSubmission, MAX_PICKER_BYTES,
    MAX_PICKER_FILE_BYTES, MAX_PICKER_FILES, OptionSnapshot, SelectedFile,
};
pub use media::{MediaCommand, MediaKey, MediaStatus, MediaUpdate};
pub use navigation::{DocumentSession, ResourceLoad};

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
use devtools_console::report_error;
use navigation::{dynamic_scripts, module_tasks};
use rust_browser_javascript::{ErrorKind, JsError, ObjectId, RootId, Value, Vm, VmLimits};
use scheduler::schedule_timer;
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc, time::Instant};

// Script-created identities remain valid across detach/reinsert and user-held
// DOM references. Bound the tab's lifetime arena separately from the parser's
// per-document input limit and the independent byte/depth budgets.
const NODE_LIMIT: usize = 512 * 1024;
const DOM_BYTES_LIMIT: usize = 16 * 1024 * 1024;
const DOM_DEPTH_LIMIT: usize = 256;
const DIAGNOSTIC_LIMIT: usize = 64;
const DIAGNOSTIC_CHAR_LIMIT: usize = 2048;
type Shared = Rc<RefCell<State>>;
type FormAttributeHook = Rc<dyn Fn(&mut Vm, &Document, NodeId, &str)>;
type FormCloneHook = Rc<dyn Fn(&mut Vm, &[NodeId], usize) -> Result<(), JsError>>;

struct State {
    document: Document,
    custom_elements: Rc<RefCell<custom_elements::Registry>>,
    wrappers: HashMap<NodeId, Value>,
    styles: HashMap<NodeId, Value>,
    nodes: HashMap<ObjectId, NodeId>,
    prototype: Value,
    element_prototype: Value,
    html_element_prototype: Value,
    div_prototype: Value,
    anchor_prototype: Value,
    template_prototype: Value,
    document_prototype: Value,
    html_document_prototype: Value,
    doctype_prototype: Value,
    fragment_prototype: Value,
    shadow_prototype: Value,
    iframe_prototype: Value,
    frames: frames::Frames,
    script_prototype: Value,
    canvas_prototype: Value,
    media_prototypes: media::Prototypes,
    input_prototype: Value,
    form_control_prototypes: HashMap<&'static str, Value>,
    image_prototype: Value,
    link_prototype: Value,
    style_element_prototype: Value,
    absolute_link_hrefs: HashMap<NodeId, rust_browser_javascript::JsString>,
    image_updates: std::collections::BTreeSet<NodeId>,
    link_updates: std::collections::BTreeSet<NodeId>,
    invalid_images: std::collections::HashSet<NodeId>,
    character_data_prototype: Value,
    text_prototype: Value,
    comment_prototype: Value,
    cdata_prototype: Value,
    processing_instruction_prototype: Value,
    svg_element_prototype: Value,
    window: Value,
    event_prototype: Value,
    mouse_event_prototype: Value,
    keyboard_event_prototype: Value,
    focus_event_prototype: Value,
    error_event_prototype: Value,
    reporting_error: bool,
    handlers: HashMap<(Option<NodeId>, String), (Value, RootId)>,
    dirty: bool,
    revision: u64,
    snapshot_revision: u64,
    tag_tree_revision: u64,
    appended_leaf: Option<(NodeId, NodeId)>,
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
    devtools_console: devtools_console::ConsoleBuffer,
    sources: inspection_sources::SourceArchive,
    scheduler: scheduler::Scheduler,
    microtask_collection: (usize, usize),
    dom_bytes: usize,
    form_attribute_hook: Option<FormAttributeHook>,
    form_clone_hook: Option<FormCloneHook>,
    mutations: Rc<RefCell<mutation_observers::Observers>>,
    canvases: canvas::CanvasStore,
    media: media::Store,
}

#[derive(Clone, Copy)]
pub struct MouseInput {
    pub client_x: f64,
    pub client_y: f64,
    pub screen_x: f64,
    pub screen_y: f64,
    pub alt: bool,
    pub control: bool,
    pub meta: bool,
    pub shift: bool,
}

#[derive(Clone, Copy)]
pub enum MousePhase {
    Down,
    Move { primary_down: bool },
    Up,
}

pub struct ScriptRuntime {
    #[cfg(feature = "twitch-bootstrap-profile")]
    twitch_profile: Option<twitch_profile::Profile>,
    vm: Vm,
    state: Shared,
    fetch: fetch::FetchHost,
    next_collection_bytes: usize,
    next_collection_cells: usize,
    forms: forms::FormHost,
    layout: layout_api::LayoutHost,
    media_queries: media_queries::MediaQueries,
    intersections: intersection_observers::Observers,
    storage: storage::StorageHost,
    messages: message_ports::MessagePorts,
    image_bitmap: canvas::ImageBitmapFactory,
    workers: workers::Workers,
    sockets: websocket::Sockets,
    socket_events: websocket::SocketEvents,
}

fn mark_dirty(state: &mut State) {
    state.tag_tree_revision = state.tag_tree_revision.wrapping_add(1);
    state.appended_leaf = None;
    mark_style_dirty(state);
}

fn mark_style_dirty(state: &mut State) {
    state.dirty = true;
    state.revision = state.revision.wrapping_add(1);
}

/// Bound each web realm independently. A response, its UTF-16 string and its
/// parsed data can coexist. The scheduler supplies the separate timer task bounds.
fn web_vm_limits() -> VmLimits {
    VmLimits {
        // Document and worker hosts do not yet provide cross-origin isolation.
        shared_array_buffer: false,
        // Web callbacks can re-enter JavaScript underneath several lexical blocks.
        // The VM checks native stack headroom before each deeper bytecode entry.
        max_bytecode_depth: 4096,
        max_source_bytes: crate::loader::CLASSIC_SCRIPT_LIMIT,
        max_string_units: crate::network::FETCH_RESPONSE_BODY_LIMIT,
        // Owned JavaScript frames use heap storage. Native re-entry still
        // checks the available thread stack before making another call.
        max_call_depth: usize::MAX,
        max_heap_objects: 2 * 1024 * 1024,
        max_heap_bytes: 2 * 1024 * 1024 * 1024,
        buffer_units_per_instruction: 1024,
        ..VmLimits::default()
    }
}

fn restrict_document_policy(vm: &mut Vm, state: &Shared, policy: &DocumentPolicy) {
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
// Also used for loader/policy/worker notices, so text here alone is not
// evidence of a browser-reported error in the document realm.
fn diagnostic(state: &Shared, message: String) {
    let mut state = state.borrow_mut();
    if state.diagnostics.len() < DIAGNOSTIC_LIMIT {
        state
            .diagnostics
            .push(message.chars().take(DIAGNOSTIC_CHAR_LIMIT).collect());
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
        } else if state.document.namespace(id) == crate::dom::Namespace::Html
            && state.document.tag(id) == "iframe"
        {
            state.iframe_prototype.clone()
        } else if crate::canvas::is_canvas(&state.document, id) {
            state.canvas_prototype.clone()
        } else if let Some(prototype) = state.media_prototypes.element(&state.document, id) {
            prototype
        } else if link_element::is_link(&state.document, id) {
            state.link_prototype.clone()
        } else if style_sheets::is_style_element(&state.document, id) {
            state.style_element_prototype.clone()
        } else if image_element::is_image(&state.document, id) {
            state.image_prototype.clone()
        } else if state.document.namespace(id) == crate::dom::Namespace::Html
            && state.document.tag(id) == "input"
        {
            state.input_prototype.clone()
        } else if matches!(state.document.nodes[id.0].kind, NodeKind::Text(_)) {
            state.text_prototype.clone()
        } else if matches!(state.document.nodes[id.0].kind, NodeKind::Cdata(_)) {
            state.cdata_prototype.clone()
        } else if matches!(
            state.document.nodes[id.0].kind,
            NodeKind::ProcessingInstruction { .. }
        ) {
            state.processing_instruction_prototype.clone()
        } else if matches!(state.document.nodes[id.0].kind, NodeKind::Comment(_)) {
            state.comment_prototype.clone()
        } else if matches!(state.document.nodes[id.0].kind, NodeKind::Element { .. }) {
            match state.document.namespace(id) {
                crate::dom::Namespace::Html if state.document.tag(id) == "a" => state.anchor_prototype.clone(),
                crate::dom::Namespace::Html if state.document.tag(id) == "div" => state.div_prototype.clone(),
                crate::dom::Namespace::Html if state.document.tag(id) == "template" => state.template_prototype.clone(),
                crate::dom::Namespace::Html => state
                    .form_control_prototypes
                    .get(state.document.tag(id))
                    .unwrap_or(&state.html_element_prototype)
                    .clone(),
                crate::dom::Namespace::Svg => state.svg_element_prototype.clone(),
                _ => state.element_prototype.clone(),
            }
        } else if state.document.shadow_hosts.contains_key(&id) {
            state.shadow_prototype.clone()
        } else if state.document.is_html_document(id) {
            state.html_document_prototype.clone()
        } else if matches!(state.document.nodes[id.0].kind, NodeKind::Doctype { .. }) {
            state.doctype_prototype.clone()
        } else if state.document.is_document(id) {
            state.document_prototype.clone()
        } else if matches!(state.document.nodes[id.0].kind, NodeKind::Document) {
            state.fragment_prototype.clone()
        } else {
            state.prototype.clone()
        }
    };
    wrap_new_node(vm, state, id, prototype)
}

fn wrap_new_node(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    prototype: Value,
) -> Result<Value, JsError> {
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
        NodeKind::Text(value) | NodeKind::Cdata(value) | NodeKind::Comment(value) => value.len(),
        NodeKind::ProcessingInstruction { target, data } => target.len() + data.len(),
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
fn valid_attribute_name(name: &str) -> bool {
    !name.is_empty()
        && !name.bytes().any(|byte| {
            matches!(byte, b' ' | b'\t' | b'\n' | 12 | b'\r' | 0 | b'/' | b'=' | b'>')
        })
}

fn set_attribute(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    key: &str,
    value: Option<String>,
) -> Result<(), JsError> {
    mutate_attribute(vm, state, id, key, value, None)
}

fn mutate_attribute(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    key: &str,
    value: Option<String>,
    qualified: Option<Option<crate::dom::AttributeNamespace>>,
) -> Result<(), JsError> {
    if !valid_attribute_name(key) {
        return Err(JsError::type_error("Invalid attribute name"));
    }
    let host = state;
    let mut state = host.borrow_mut();
    let present = value.is_some();
    let key = if qualified.is_none() && state.document.html_element_in_html_document(id) {
        key.to_ascii_lowercase()
    } else {
        key.to_owned()
    };
    let old = state.document.attr(id, &key);
    let previous_namespace = state.document.attribute_namespace(id, &key);
    let namespace = qualified.unwrap_or(previous_namespace);
    if old.is_some() && namespace != previous_namespace {
        return Err(unsupported("Attributes with identical qualified names in distinct namespaces"));
    }
    let local_name = if namespace.is_some() {
        key.split_once(':').map_or(key.as_str(), |(_, local)| local)
    } else {
        &key
    };
    let custom_reaction = custom_elements::attribute(
        &state,
        id,
        local_name,
        old,
        value.as_deref(),
        namespace,
    );
    let changes_canvas_attribute = matches!(key.as_str(), "width" | "height")
        && (present || old.is_some())
        && namespace.is_none();
    let next_bytes = changed_bytes(
        &state,
        old.map_or(0, |value| key.len() + value.len() + usize::from(previous_namespace.is_some()) * key.len()),
        value.as_ref().map_or(0, |value| key.len() + value.len() + usize::from(namespace.is_some()) * key.len()),
    )?;
    if (present || old.is_some())
        && matches!(state.document.nodes[id.0].kind, NodeKind::Element { .. })
        && state.mutations.borrow().active()
    {
        mutation_observers::note(
            &state,
            Mutation::Attribute {
                target: id,
                name: local_name,
                namespace,
                old,
            },
        );
    }
    if let NodeKind::Element { attrs, .. } = &mut state.document.nodes[id.0].kind {
        if let Some(value) = value {
            attrs.insert(key.clone(), value);
        } else {
            attrs.shift_remove(&key);
        }
        if let Some(namespace) = namespace {
            if present {
                state.document.attribute_namespaces.insert((id, key.clone()), namespace);
            } else {
                state.document.attribute_namespaces.remove(&(id, key.clone()));
            }
        }
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
        state.dom_bytes = next_bytes;
        mark_style_dirty(&mut state);
        if changes_canvas_attribute {
            canvas::attribute_changed(vm, &mut state, id, &key)?;
        }
        if key == "href" {
            state.absolute_link_hrefs.remove(&id);
        }
        image_element::attribute_changed(&mut state, id, &key);
        link_element::attribute_changed(&mut state, id, &key);
        drop(state);
        frames::attribute_changed(vm, host, id, &key);
        if key == "src" && media::is_media(&host.borrow().document, id) {
            media::source_changed(vm, host, id)?;
        }
        mutation_observers::schedule(vm, host);
        dynamic_scripts::attribute_changed(vm, host, id, &key, present)?;
        if let Some(reaction) = custom_reaction {
            custom_elements::deliver(vm, host, reaction)?;
        }
        Ok(())
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
    let old = state.document.nodes[id.0]
        .kind
        .character_data()
        .map_or(0, String::len);
    let next_bytes = changed_bytes(&state, old, value.len())?;
    if let Some(old) = state.document.nodes[id.0].kind.character_data() {
        mutation_observers::note(&state, Mutation::CharacterData { target: id, old });
        *state.document.nodes[id.0]
            .kind
            .character_data_mut()
            .unwrap() = value;
    } else if matches!(state.document.nodes[id.0].kind, NodeKind::Element { .. }) {
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
        let added = (!value.is_empty()).then(|| state.document.append(id, NodeKind::Text(value)));
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
    } else {
        return Ok(());
    }
    state.dom_bytes = next_bytes;
    mark_dirty(&mut state);
    drop(state);
    frames::sweep(vm, host);
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
            NodeKind::Text(_) | NodeKind::Cdata(_) => node.parent,
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
        let mut vm = Vm::with_limits(web_vm_limits());
        vm.set_wasm_compilation_allowed(page.document_policy.allows_wasm_compilation());
        let prototype = vm.new_object();
        vm.root(prototype.clone());
        let document_prototype = vm.new_object_with_prototype(prototype.clone())?;
        vm.root(document_prototype.clone());
        let html_document_prototype = vm.new_object_with_prototype(document_prototype.clone())?;
        vm.root(html_document_prototype.clone());
        let doctype_prototype = vm.new_object_with_prototype(prototype.clone())?;
        vm.root(doctype_prototype.clone());
        let fragment_prototype = vm.new_object_with_prototype(prototype.clone())?;
        vm.root(fragment_prototype.clone());
        let shadow_prototype = vm.new_object_with_prototype(fragment_prototype.clone())?;
        vm.root(shadow_prototype.clone());
        let element_prototype = vm.new_object_with_prototype(prototype.clone())?;
        vm.root(element_prototype.clone());
        let html_element_prototype = vm.new_object_with_prototype(element_prototype.clone())?;
        vm.root(html_element_prototype.clone());
        let template_prototype = vm.new_object_with_prototype(html_element_prototype.clone())?;
        vm.root(template_prototype.clone());
        let div_prototype = vm.new_object_with_prototype(html_element_prototype.clone())?;
        vm.root(div_prototype.clone());
        let anchor_prototype = vm.new_object_with_prototype(html_element_prototype.clone())?;
        vm.root(anchor_prototype.clone());
        let iframe_prototype = vm.new_object_with_prototype(html_element_prototype.clone())?;
        vm.root(iframe_prototype.clone());
        let script_prototype = vm.new_object_with_prototype(html_element_prototype.clone())?;
        vm.root(script_prototype.clone());
        let canvas_prototype = vm.new_object_with_prototype(html_element_prototype.clone())?;
        vm.root(canvas_prototype.clone());
        let media_prototypes = media::Prototypes::new(&mut vm, &html_element_prototype)?;
        let input_prototype = vm.new_object_with_prototype(html_element_prototype.clone())?;
        vm.root(input_prototype.clone());
        let mut form_control_prototypes = HashMap::new();
        for (tag, _, _) in forms::TYPED_CONTROLS {
            let control = vm.new_object_with_prototype(html_element_prototype.clone())?;
            vm.root(control.clone());
            form_control_prototypes.insert(*tag, control);
        }
        let link_prototype = vm.new_object_with_prototype(html_element_prototype.clone())?;
        vm.root(link_prototype.clone());
        let style_element_prototype = vm.new_object_with_prototype(html_element_prototype.clone())?;
        vm.root(style_element_prototype.clone());
        let image_prototype = vm.new_object_with_prototype(html_element_prototype.clone())?;
        vm.root(image_prototype.clone());
        let character_data_prototype = vm.new_object_with_prototype(prototype.clone())?;
        vm.root(character_data_prototype.clone());
        let window = vm.global();
        vm.forbid_private_elements(&window)?;
        let document_url = history.url();
        let text_prototype = vm.new_object_with_prototype(character_data_prototype.clone())?;
        let comment_prototype = vm.new_object_with_prototype(character_data_prototype.clone())?;
        let cdata_prototype = vm.new_object_with_prototype(text_prototype.clone())?;
        let processing_instruction_prototype =
            vm.new_object_with_prototype(character_data_prototype.clone())?;
        let svg_element_prototype = vm.new_object_with_prototype(element_prototype.clone())?;
        for prototype in [
            &text_prototype,
            &comment_prototype,
            &cdata_prototype,
            &processing_instruction_prototype,
            &svg_element_prototype,
        ] {
            vm.root(prototype.clone());
        }
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
            html_element_prototype: html_element_prototype.clone(),
            div_prototype,
            anchor_prototype,
            template_prototype,
            document_prototype: document_prototype.clone(),
            html_document_prototype,
            doctype_prototype,
            fragment_prototype,
            shadow_prototype,
            custom_elements: Rc::default(),
            iframe_prototype: iframe_prototype.clone(),
            frames: frames::Frames::default(),
            script_prototype: script_prototype.clone(),
            canvas_prototype: canvas_prototype.clone(),
            media_prototypes,
            input_prototype: input_prototype.clone(),
            form_control_prototypes,
            image_prototype: image_prototype.clone(),
            link_prototype: link_prototype.clone(),
            style_element_prototype: style_element_prototype.clone(),
            absolute_link_hrefs: HashMap::new(),
            image_updates: Default::default(),
            link_updates: Default::default(),
            invalid_images: Default::default(),
            character_data_prototype: character_data_prototype.clone(),
            window: window.clone(),
            event_prototype: Value::Undefined,
            mouse_event_prototype: Value::Undefined,
            keyboard_event_prototype: Value::Undefined,
            focus_event_prototype: Value::Undefined,
            error_event_prototype: Value::Undefined,
            text_prototype,
            comment_prototype,
            cdata_prototype,
            processing_instruction_prototype,
            svg_element_prototype,
            reporting_error: false,
            handlers: HashMap::new(),
            dirty: fresh_parser_snapshot,
            revision: u64::from(fresh_parser_snapshot),
            snapshot_revision: 0,
            tag_tree_revision: 0,
            appended_leaf: None,
            snapshot_document: page.document.clone(),
            ready_state: "loading",
            current_script: None,
            ignore_destructive_writes: 0,
            parser: std::rc::Weak::new(),
            replacement_parser: None,
            parser_wakeup: false,
            parser_connect_policy: page.meta_connect_policy.clone(),
            document_policy: page.document_policy.clone(),
            navigation_attached: false,
            dynamic_scripts,
            import_requests: module_tasks::ImportRequests::default(),
            url: document_url.clone(),
            base_cache: RefCell::default(),
            routing: history_api::Routing::new(history),
            executed: 0,
            diagnostics: vec![],
            devtools_console: Default::default(),
            sources: Default::default(),
            scheduler: scheduler::Scheduler::new(started),
            microtask_collection: (128 * 1024 * 1024, 128 * 1024),
            dom_bytes,
            form_attribute_hook: None,
            form_clone_hook: None,
            mutations: Rc::default(),
            canvases,
            media: media::Store::default(),
        }));
        install_node(&mut vm, &state, prototype.clone(), &element_prototype)?;
        frames::install(&mut vm, &state, iframe_prototype)?;
        link_element::install(&mut vm, &state, link_prototype)?;
        image_element::install(&mut vm, &state, image_prototype)?;
        install_character_data(&mut vm, &state, prototype.clone(), character_data_prototype)?;
        dynamic_scripts::install(&mut vm, &state, script_prototype)?;
        module_tasks::install(&mut vm, &state);
        install_event_handlers(&mut vm, &state, prototype.clone())?;
        install_event_handlers(&mut vm, &state, window.clone())?;
        let document = wrap(&mut vm, &state, Some(root))?;
        vm.set_global("document", document.clone())?;
        for name in ["self", "globalThis"] {
            vm.set_global(name, window.clone())?;
        }
        // The [LegacyUnforgeable] window accessor always returns this page's global.
        let get_window = vm.new_native_function("get window", |vm, this, _| {
            let global = vm.global();
            if !this.is_nullish() && this != global {
                return Err(JsError::type_error("Window receiver required"));
            }
            Ok(global)
        });
        let descriptor = vm.new_object_with_prototype(Value::Null)?;
        vm.set_property(descriptor.clone(), "get", get_window)?;
        vm.set_property(descriptor.clone(), "enumerable", true.into())?;
        let object = vm.get_global("Object")?;
        let define = vm.get_property(object, "defineProperty")?;
        vm.call(
            define,
            Value::Undefined,
            &[window.clone(), "window".into(), descriptor],
        )?;
        navigator::install(&mut vm, &window)?;
        install_css_namespace(&mut vm, window.clone())?;
        let console = devtools_console::install(&mut vm, &state)?;
        scheduler::install_timers(&mut vm, &state)?;
        let state_get = state.clone();
        let get = vm.new_native_function("title", move |_, this, _| {
            let root = document_node(&state_get, &this)?;
            Ok(state_get.borrow().document.title_in(root).into())
        });
        let state_set = state.clone();
        let set = vm.new_native_function("title", move |vm, this, args| {
            let root = document_node(&state_set, &this)?;
            let value = text(vm, arg(args, 0))?;
            let title = state_set.borrow().document.title_element(root);
            let id = if let Some(title) = title {
                title
            } else {
                let mut state = state_set.borrow_mut();
                if state.document.nodes.len() >= NODE_LIMIT {
                    return Err(unsupported("DOM node budget exceeded"));
                }
                let Some(parent) = document_part_in(&state.document, root, "head") else {
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
        vm.define_accessor(document_prototype.clone(), "title", Some(get), Some(set))?;
        let dom_errors = dom_api::install(
            &mut vm,
            &state,
            prototype.clone(),
            document_prototype.clone(),
        )?;
        let address = state.borrow().url.clone();
        crypto::install(&mut vm, &window, &address, &dom_errors)?;
        document_interface::install(&mut vm, &state, document_prototype.clone(), &dom_errors)?;
        tree_walker::install(&mut vm, &state, &document_prototype, &dom_errors)?;
        style_sheets::install(&mut vm, &state, style_element_prototype, &dom_errors)?;
        class_list::install(&mut vm, &state, &element_prototype, &dom_errors)?;
        namespaces::install_attributes(&mut vm, &state, &element_prototype, &dom_errors)?;
        namespaces::install(
            &mut vm,
            &state,
            prototype.clone(),
            document_prototype.clone(),
        )?;
        let node_lists = collections::install(
            &mut vm,
            &state,
            prototype.clone(),
            document_prototype.clone(),
        )?;
        navigation::install_parser_methods(&mut vm, &state, document.clone(), &dom_errors)?;
        let file_lists = file_list::FileListHost::install(&mut vm)?;
        let forms = forms::FormHost::install(
            &mut vm,
            &state,
            prototype.clone(),
            document.clone(),
            file_lists.clone(),
        )?;
        install_selectors(
            &mut vm,
            &state,
            &prototype,
            &element_prototype,
            &node_lists,
            &forms,
            &dom_errors,
        )?;
        mutation_observers::install(&mut vm, &state, node_lists)?;
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
        devtools_console::install_public_error(&mut vm, &state, console)?;
        element_interface::install(&mut vm, &state, prototype, element_prototype, &dom_errors)?;
        dom_implementation::install(&mut vm, &state)?;
        shadow_dom::install(&mut vm, &state, &dom_errors)?;
        element_interface::install_html(&mut vm, &state, html_element_prototype)?;
        dataset::install(&mut vm, &state, &dom_errors)?;
        let range_prototype = range::install(&mut vm, &state, &dom_errors)?;
        selection::install(&mut vm, &state, &dom_errors, range_prototype)?;
        forms.install_typed_controls(&mut vm)?;
        custom_elements::install(&mut vm, &state, &dom_errors)?;
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
        let media_queries =
            media_queries::MediaQueries::install(&mut vm, &window, &layout, &events)?;
        let intersections = intersection_observers::Observers::install(&mut vm, &state)?;
        let image_data = canvas::install(&mut vm, &state, canvas_prototype, &layout, &blobs)?;
        let image_bitmap = canvas::install_image_bitmap(&mut vm, &state, window.clone())?;
        let cloning =
            structured_clone::CloneHost::new(&mut vm, blobs.clone(), image_data, image_bitmap, file_lists)?;
        let messages = message_ports::install(&mut vm, events.clone(), cloning.clone())?;
        let workers = workers::install(&mut vm, &state, events.clone(), messages.clone(), page)?;
        let (sockets, socket_events) = websocket::install(
            &mut vm, &state, events, messages.clone(), blobs, page,
        )?;
        cloning.install(&mut vm, window, messages.clone())?;
        scheduler::install_performance(&mut vm, &state)?;
        base64_api::install(&mut vm)?;
        history_api::install(&mut vm, &state, document, &layout)?;
        let base_state = state.clone();
        fetch.set_base_resolver(Rc::new(move || document_base(&base_state.borrow())));
        fetch.set_document(&state);
        let next_collection_bytes = vm.estimated_heap_bytes() + 1024 * 1024;
        let next_collection_cells = vm.limits().max_heap_objects / 2;
        Ok(Self {
            #[cfg(feature = "twitch-bootstrap-profile")]
            twitch_profile: None,
            vm,
            state,
            fetch,
            next_collection_bytes,
            next_collection_cells,
            forms,
            layout,
            media_queries,
            intersections,
            storage,
            messages,
            image_bitmap,
            workers,
            sockets,
            socket_events,
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
        let handlers = page.document.connected_nodes().any(|id| {
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
                        if external {
                            &script.name
                        } else {
                            page.base.as_str()
                        },
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
        let run = |vm: &mut Vm| run_classic(vm, &state, node, source, identifier, external, true);
        #[cfg(feature = "twitch-bootstrap-profile")]
        let result = match self.twitch_profile {
            Some(profile) => profile.run(&mut self.vm, identifier, run),
            None => scheduler::run_script_task(&mut self.vm, run),
        };
        #[cfg(not(feature = "twitch-bootstrap-profile"))]
        let result = scheduler::run_script_task(&mut self.vm, run);
        if let Err(error) = result {
            if fatal(&error) {
                mutation_observers::discard_jobs(&mut self.vm, &self.state);
            }
            // The VM referrer can be a base URL rather than the script's
            // location. Use the known script node, not a guessed URL.
            devtools_console::report_classic_error(&self.state, node, &error, || {
                format!("{name}: {error}")
            });
        }
        report_unhandled(&mut self.vm, &self.state);
        #[cfg(feature = "twitch-bootstrap-profile")]
        if let Some(profile) = self.twitch_profile {
            profile.collect(self);
            return;
        }
        self.collect_after_task(true);
    }
    /// Returns whether the link's default navigation is allowed.
    pub fn click(&mut self, node: NodeId) -> bool {
        self.activate_click(node, None)
    }

    fn activate_click(&mut self, node: NodeId, input: Option<MouseInput>) -> bool {
        let state = self.state.clone();
        state.borrow_mut().media.activate();
        let forms = self.forms.clone();
        forms.activate_picker_gesture();
        let allowed = scheduler::run_input_task(&mut self.vm, |vm| {
            let allowed = forms.activate_click(vm, node, input)?;
            if allowed {
                media::default_click(vm, &state, node)?;
            }
            drain_microtask_queue(vm, &state)?;
            Ok(allowed)
        })
        .unwrap_or_else(|error| {
            report_error(
                &state,
                ConsolePhase::HostTask,
                Some("click"),
                &error,
                || error.to_string(),
            );
            false
        });
        report_unhandled(&mut self.vm, &self.state);
        self.vm.collect_garbage();
        allowed
    }

    pub fn mouse_input(&mut self, node: NodeId, phase: MousePhase, input: MouseInput) {
        let state = self.state.clone();
        let (kind, buttons, detail) = match phase {
            MousePhase::Down => ("mousedown", 1, 1),
            MousePhase::Move { primary_down } => ("mousemove", i32::from(primary_down), 0),
            MousePhase::Up => ("mouseup", 0, 1),
        };
        let result = scheduler::run_input_task(&mut self.vm, |vm| {
            dispatch_mouse(vm, &state, node, kind, input, buttons, detail)?;
            drain_microtask_queue(vm, &state)
        });
        if let Err(error) = result {
            if fatal(&error) {
                mutation_observers::discard_jobs(&mut self.vm, &state);
            }
            report_error(&state, ConsolePhase::HostTask, Some(kind), &error, || {
                format!("{kind}: {error}")
            });
        }
        report_unhandled(&mut self.vm, &state);
        self.collect_after_task(false);
    }
    fn lifecycle_event(&mut self, target: Option<NodeId>, kind: &str, bubbles: bool) {
        let state = self.state.clone();
        let mode = if target.is_none() && kind == "load" {
            DispatchMode::WindowLoad
        } else {
            DispatchMode::Host
        };
        let result = scheduler::run_script_task(&mut self.vm, |vm| {
            dispatch_checked_with_mode(vm, &state, target, kind, bubbles, false, &[], mode)?;
            drain_microtask_queue(vm, &state)
        });
        if let Err(error) = result {
            if fatal(&error) {
                mutation_observers::discard_jobs(&mut self.vm, &self.state);
            }
            report_error(
                &self.state,
                ConsolePhase::HostTask,
                Some(kind),
                &error,
                || format!("{kind}: {error}"),
            );
        }
    }
    /// HTML chooses the activation target before dispatch. Its href may be
    /// changed by a listener, so the default action reads it after dispatch.
    pub fn click_link(&mut self, node: NodeId) -> Option<String> {
        self.click_with_default(node).1
    }
    pub fn click_with_default(&mut self, node: NodeId) -> (bool, Option<String>) {
        self.click_default(node, None)
    }

    pub fn click_at_with_default(
        &mut self,
        node: NodeId,
        input: MouseInput,
    ) -> (bool, Option<String>) {
        self.click_default(node, Some(input))
    }

    fn click_default(
        &mut self,
        node: NodeId,
        input: Option<MouseInput>,
    ) -> (bool, Option<String>) {
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
        if !self.activate_click(node, input) {
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
    /// Inspect pending structured records without consuming them.
    pub fn console_snapshot(&self) -> ConsoleSnapshot {
        self.state.borrow().devtools_console.snapshot()
    }
    /// Drain pending records and reset the structured drop count. This does not
    /// touch `diagnostics()` or the VM. A fresh document runtime starts empty.
    pub fn take_console_snapshot(&self) -> ConsoleSnapshot {
        self.state.borrow_mut().devtools_console.take_snapshot()
    }
    /// Read the captured classic script previews without cloning their source bytes.
    pub fn with_inspected_scripts<T>(
        &self,
        read: impl FnOnce(&[InspectedSource], usize) -> T,
    ) -> T {
        let state = self.state.borrow();
        let (entries, omitted) = state.sources.view();
        read(entries, omitted)
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
                || !state.image_updates.is_empty()
                || !state.link_updates.is_empty())
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
        let result = scheduler::run_input_task(&mut self.vm, |vm| {
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
        let result = scheduler::run_script_task(&mut self.vm, |vm| {
            action(fetch, vm)?;
            drain_host_microtasks(vm, &state)
        });
        report_unhandled(&mut self.vm, &self.state);
        self.collect_after_task(collect);
        result
    }

    fn collect_after_task(&mut self, eager: bool) {
        const MIB: usize = 1024 * 1024;
        const LARGE_HEAP: usize = 16 * MIB;

        let bytes = self.vm.estimated_heap_bytes();
        if !(eager && bytes < LARGE_HEAP)
            && bytes < self.next_collection_bytes
            && self.vm.heap_cell_count() < self.next_collection_cells
        {
            return;
        }
        self.vm.collect_garbage();
        self.sockets.sweep(&self.vm);
        let live = self.vm.estimated_heap_bytes();
        let limits = self.vm.limits();
        if live < LARGE_HEAP {
            self.next_collection_bytes = live + MIB;
            self.next_collection_cells = limits.max_heap_objects / 2;
            return;
        }
        // A retained manifest may survive thousands of small document tasks.
        // Trace it again after measurable allocation, with less headroom when
        // close to either hard VM limit. No execution budget resets occur here.
        let threshold = |used: usize, cap: usize, min_step: usize, max_step: usize| {
            let growth = (used / 8).clamp(min_step, max_step);
            used.saturating_add(growth.min((cap.saturating_sub(used) / 2).max(1)))
        };
        self.next_collection_bytes = threshold(live, limits.max_heap_bytes, MIB, 16 * MIB);
        self.next_collection_cells = threshold(
            self.vm.heap_cell_count(),
            limits.max_heap_objects,
            512,
            4096,
        );
    }
    fn drain_microtasks(&mut self) {
        let state = self.state.clone();
        if let Err(error) = scheduler::run_script_task(&mut self.vm, |vm| drain_host_microtasks(vm, &state)) {
            report_error(&state, ConsolePhase::Microtask, None, &error, || {
                error.to_string()
            });
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
            || self.sockets.has_tasks()
            || state.media.has_loads()
            || state.media.has_commands()
            || state.media.has_stream_tasks()
            || state.routing.has_events()
            || state.routing.has_commands()
            || self.forms.has_submissions()
            || state.navigation_attached
                && (state.dynamic_scripts.has_pending()
                    || state.import_requests.has_pending()
                    || !state.image_updates.is_empty()
                    || !state.link_updates.is_empty())
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
        self.sockets.set_wake(wake.clone());
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
        frames::sweep(&mut self.vm, &self.state);
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
        if self.sockets.has_tasks() {
            let state = self.state.clone();
            for _ in 0..32 {
                let result = scheduler::run_script_task(&mut self.vm, |vm| {
                    if !self.sockets.dispatch_next(vm, &self.socket_events)? {
                        return Ok(false);
                    }
                    drain_host_microtasks(vm, &state)?;
                    Ok(true)
                });
                match result {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        report_error(&state, ConsolePhase::HostTask, Some("WebSocket event delivery"),
                            &error, || error.to_string());
                        break;
                    }
                }
            }
        }
        if self.workers.has_tasks() {
            let state = self.state.clone();
            for _ in 0..32 {
                let result = scheduler::run_script_task(&mut self.vm, |vm| {
                    if !self.workers.dispatch_next(vm)? {
                        return Ok(false);
                    }
                    drain_host_microtasks(vm, &state)?;
                    Ok(true)
                });
                match result {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        report_error(
                            &state,
                            ConsolePhase::HostTask,
                            Some("Worker event delivery"),
                            &error,
                            || error.to_string(),
                        );
                        break;
                    }
                }
            }
        }
        if self.messages.has_tasks() {
            let state = self.state.clone();
            for _ in 0..32 {
                let result = scheduler::run_script_task(&mut self.vm, |vm| {
                    if !self.messages.dispatch_next(vm)? {
                        return Ok(false);
                    }
                    drain_host_microtasks(vm, &state)?;
                    Ok(true)
                });
                match result {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        report_error(
                            &state,
                            ConsolePhase::HostTask,
                            Some("MessagePort event delivery"),
                            &error,
                            || error.to_string(),
                        );
                        break;
                    }
                }
            }
        }
        if self.storage.has_events() {
            let state = self.state.clone();
            for _ in 0..32 {
                let result = scheduler::run_script_task(&mut self.vm, |vm| {
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
                        report_error(
                            &state,
                            ConsolePhase::HostTask,
                            Some("storage event delivery"),
                            &error,
                            || error.to_string(),
                        );
                        break;
                    }
                }
            }
        }
        if self.state.borrow().routing.has_events() {
            let state = self.state.clone();
            if let Err(error) = scheduler::run_script_task(&mut self.vm, |vm| history_api::run_events(vm, &state)) {
                report_error(
                    &state,
                    ConsolePhase::HostTask,
                    Some("history event delivery"),
                    &error,
                    || error.to_string(),
                );
            }
        }
        if self.layout.take_resize() {
            self.lifecycle_event(None, "resize", false);
            if let Err(error) = scheduler::run_script_task(&mut self.vm, |vm| {
                self.media_queries.report_changes(vm)
            }) {
                report_error(
                    &self.state,
                    ConsolePhase::HostTask,
                    Some("media query change"),
                    &error,
                    || error.to_string(),
                );
            }
        }
        for target in self.layout.take_events() {
            let state = self.state.clone();
            if let Err(error) = scheduler::run_script_task(&mut self.vm, |vm| {
                let bubbles = target.is_none();
                let target = target.or_else(|| Some(state.borrow().document.root));
                dispatch_checked(vm, &state, target, "scroll", bubbles, false, &[])?;
                drain_microtask_queue(vm, &state)
            }) {
                report_error(
                    &state,
                    ConsolePhase::HostTask,
                    Some("scroll"),
                    &error,
                    || error.to_string(),
                );
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
        self.collect_after_task(true);
        Ok(())
    }
    pub fn snapshot(&self, page: &mut LoadedPage) -> bool {
        self.publish_canvas_frames();
        {
            let state = self.state.borrow();
            page.canvas_frames = state.canvases.frames.clone();
            page.document_policy.restrict(&state.document_policy);
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
        devtools_console::report_rejection(state, vm, &value);
    }
}

fn document_node(state: &Shared, value: &Value) -> Result<NodeId, JsError> {
    let id = node(state, value)?;
    if state.borrow().document.is_document(id) {
        Ok(id)
    } else {
        Err(JsError::type_error("Expected a Document receiver"))
    }
}

fn document_part(document: &Document, name: &str) -> Option<NodeId> {
    document_part_in(document, document.root, name)
}

fn document_part_in(document: &Document, root: NodeId, name: &str) -> Option<NodeId> {
    let root = document.nodes[root.0]
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

#[cfg(all(windows, feature = "media-startup-diagnostics"))]
mod startup_measure;

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
        state.sources.capture(
            Some(node),
            external.then_some(identifier),
            || format!("Script #{}", node.0),
            source,
        );
        state.ignore_destructive_writes += usize::from(external);
        state.current_script.replace(node)
    };
    #[cfg(all(windows, feature = "media-startup-diagnostics"))]
    let evaluation = startup_measure::evaluate(vm, source, identifier, node, external);
    #[cfg(not(all(windows, feature = "media-startup-diagnostics")))]
    let evaluation = vm.eval_with_identifier(source, identifier);
    let evaluated = match evaluation {
        Ok(_) => {
            state.borrow_mut().executed += 1;
            Ok(())
        }
        Err(error) => {
            match error_event::report(vm, state, &error, external.then_some(identifier)) {
                Ok(false) => Ok(()),
                Ok(true) => Err(error),
                Err(reporting_failure) => Err(reporting_failure),
            }
        }
    };
    let drained = if checkpoint && evaluated.as_ref().err().is_none_or(|error| !fatal(error)) {
        let thrown = evaluated.as_ref().err().and_then(|error| error.thrown.clone());
        let keep = thrown.map(|value| vm.root(value));
        let result = drain_host_microtasks(vm, state);
        if let Some(root) = keep {
            vm.unroot(root);
        }
        result
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
    drain_microtask_checkpoint(vm, state, false)
}

fn drain_host_microtasks(vm: &mut Vm, state: &Shared) -> Result<(), JsError> {
    drain_microtask_checkpoint(vm, state, true)
}

fn drain_microtask_checkpoint(vm: &mut Vm, state: &Shared, host_boundary: bool) -> Result<(), JsError> {
    if !mutation_observers::begin_checkpoint(state) {
        return Ok(());
    }
    let result = drain_microtask_jobs(vm, state, host_boundary);
    mutation_observers::finish_checkpoint(state);
    result
}

fn drain_microtask_jobs(vm: &mut Vm, state: &Shared, host_boundary: bool) -> Result<(), JsError> {
    if host_boundary {
        collect_microtask_garbage(vm, state);
    }
    mutation_observers::schedule(vm, state);
    for _ in 0..4096 {
        if !vm.has_pending_jobs() {
            return Ok(());
        }
        let result = if host_boundary {
            vm.drain_jobs_at_callback_boundary(1)
        } else {
            vm.drain_jobs(1)
        };
        match result {
            Ok(_) => {}
            Err(error) if fatal(&error) => {
                mutation_observers::discard_jobs(vm, state);
                return Err(error);
            }
            Err(error) => report_error(state, ConsolePhase::Microtask, None, &error, || {
                error.to_string()
            }),
        }
        if host_boundary {
            collect_microtask_garbage(vm, state);
        }
    }
    if mutation_observers::discard_jobs(vm, state) > 0 {
        diagnostic(state, "Microtask checkpoint budget exceeded".into());
    }
    Ok(())
}

fn collect_microtask_garbage(vm: &mut Vm, state: &Shared) {
    const MIB: usize = 1024 * 1024;
    let (bytes_at, cells_at) = state.borrow().microtask_collection;
    let bytes = vm.estimated_heap_bytes();
    let cells = vm.heap_cell_count();
    if bytes < bytes_at && cells < cells_at {
        return;
    }
    let Some(stats) = vm.collect_between_callbacks() else {
        return;
    };
    let live = vm.estimated_heap_bytes();
    let next = |used: usize, cap: usize, min: usize, max: usize| {
        used.saturating_add((used / 8).clamp(min, max).min(
            (cap.saturating_sub(used) / 2).max(min),
        ))
    };
    state.borrow_mut().microtask_collection = (
        next(live, vm.limits().max_heap_bytes, 16 * MIB, 96 * MIB),
        next(stats.after, vm.limits().max_heap_objects, 16 * 1024, 96 * 1024),
    );
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
    let host = state.clone();
    webidl::operation(vm, prototype.clone(), "contains", 1, move |vm, this, args| {
        let ancestor = node(&host, &this)?;
        webidl::required(args, 1, "Node.contains")?;
        if args[0].is_nullish() {
            return Ok(false.into());
        }
        let mut next = Some(node(&host, &args[0])?);
        let state = host.borrow();
        while let Some(current) = next {
            vm.charge_work(1)?;
            if current == ancestor {
                return Ok(true.into());
            }
            next = state.document.nodes[current.0].parent;
        }
        Ok(false.into())
    })?;
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
    let connected = state.clone();
    getter(vm, prototype.clone(), "isConnected", move |_, this, _| {
        let id = node(&connected, &this)?;
        let state = connected.borrow();
        let doc = &state.document;
        Ok(doc.connected_to(id, doc.node_document(id)).into())
    })?;
    for property in [
        "parentNode",
        "parentElement",
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
                    "parentElement" => current.parent.filter(|parent| {
                        matches!(doc.nodes[parent.0].kind, NodeKind::Element { .. })
                    }),
                    "firstChild" => current.children.first().copied(),
                    "lastChild" => current.children.last().copied(),
                    "ownerDocument" => (!doc.is_document(id)).then(|| doc.node_document(id)),
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
                NodeKind::Document if !state.document.is_document(id) => {
                    (11, "#document-fragment".into())
                }
                NodeKind::Document => (9, "#document".into()),
                NodeKind::Text(_) => (3, "#text".into()),
                NodeKind::Cdata(_) => (4, "#cdata-section".into()),
                NodeKind::ProcessingInstruction { target, .. } => (7, target.clone()),
                NodeKind::Comment(_) => (8, "#comment".into()),
                NodeKind::Doctype { name, .. } => (10, name.clone()),
                NodeKind::Element { tag, .. } => (
                    1,
                    if state.document.html_element_in_html_document(id) {
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
    for name in ["hasAttributes", "getAttributeNames"] {
        let state = state.clone();
        webidl::operation(vm, element_prototype.clone(), name, 0, move |vm, this, _| {
            let id = element(&state, &this)?;
            let state = state.borrow();
            let NodeKind::Element { attrs, .. } = &state.document.nodes[id.0].kind else {
                unreachable!("element receiver was checked")
            };
            if name == "hasAttributes" {
                return Ok((!attrs.is_empty()).into());
            }
            vm.charge_work(attrs.len())?;
            let mut names = Vec::with_capacity(attrs.len());
            for name in attrs.keys() {
                vm.charge_work(name.len())?;
                names.push(Value::from(name.as_str()));
            }
            Ok(vm.new_array(&names))
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
            let key = if state.borrow().document.html_element_in_html_document(id) {
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
    forms: &forms::FormHost,
    errors: &dom_api::Errors,
) -> Result<(), JsError> {
    for name in ["querySelector", "querySelectorAll", "matches", "closest"] {
        let state = state.clone();
        let node_lists = node_lists.clone();
        let errors = errors.clone();
        let forms = forms.clone();
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
                let form_state = |node, pseudo| forms.matches_form_pseudo(doc, node, pseudo);
                let mut context = css::SelectorContext::new(&form_state);
                context.scope = Some(id);
                match name {
                    "matches" => return Ok(selectors.matches(doc, id, &mut context).into()),
                    "closest" => std::iter::successors(Some(id), |id| doc.nodes[id.0].parent)
                        .find(|&id| selectors.matches(doc, id, &mut context))
                        .into_iter()
                        .collect(),
                    _ => {
                        let matches = descendants(doc, id)
                            .filter(|&id| selectors.matches(doc, id, &mut context));
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
    let length_state = state.clone();
    getter(
        vm,
        character_data_prototype.clone(),
        "length",
        move |vm, this, _| {
            let id = node(&length_state, &this)?;
            let state = length_state.borrow();
            let Some(value) = state.document.nodes[id.0].kind.character_data() else {
                return Err(JsError::type_error("Expected CharacterData"));
            };
            vm.charge_work(value.len())?;
            Ok((value.encode_utf16().count() as u32).into())
        },
    )?;
    for (property, prototype) in [
        ("nodeValue", node_prototype),
        ("data", character_data_prototype),
    ] {
        let get_state = state.clone();
        let get = vm.new_native_function(property, move |vm, this, _| {
            let id = node(&get_state, &this)?;
            let state = get_state.borrow();
            match state.document.nodes[id.0].kind.character_data() {
                Some(value) => {
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
            if set_state.borrow().document.nodes[id.0]
                .kind
                .character_data()
                .is_some()
            {
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

fn install_css_namespace(vm: &mut Vm, window: Value) -> Result<(), JsError> {
    let namespace = vm.new_object();
    webidl::tag(vm, namespace.clone(), "CSS")?;
    webidl::operation(vm, namespace.clone(), "supports", 1, |vm, _, args| {
        webidl::required(args, 1, "CSS.supports")?;
        let first = text(vm, args[0].clone())?;
        let supported = if let Some(value) = args.get(1) {
            let value = text(vm, value.clone())?;
            css::supports::property(&first, &value)
        } else {
            css::supports::query(&first)
        };
        Ok(supported.into())
    })?;
    vm.define_data_property(window, "CSS", namespace, true, false, true)
}

fn install_style(vm: &mut Vm, state: &Shared, prototype: Value) -> Result<(), JsError> {
    // Each document shares these CSS accessors across its inline styles.
    let style_prototype = vm.new_object();
    for &(property, css_name) in layout_api::shared_style_properties().chain(&[
        ("cssFloat", "float"),
        ("listStyleType", "list-style-type"),
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
    let Ok(replacement) = inline_replacement(key, value, important) else {
        return Ok(());
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

impl ScriptRuntime {
    /// Edit the live author style through the same native declaration and DOM
    /// mutation paths as CSSStyleDeclaration, without evaluating page code.
    pub fn set_inspected_style(
        &mut self,
        id: NodeId,
        name: &str,
        value: &str,
        important: bool,
    ) -> Result<(), String> {
        if name.len() > 256 || value.len() > 16_384 {
            return Err("Property or value exceeds the editing limit".into());
        }
        {
            let state = self.state.borrow();
            let doc = &state.document;
            if !doc
                .nodes
                .get(id.0)
                .is_some_and(|node| matches!(node.kind, NodeKind::Element { .. }))
            {
                return Err("Select an element in the current document".into());
            }
            if !doc.is_connected(id) {
                return Err("The selected element was removed".into());
            }
        }
        let mut key = name.trim().to_owned();
        if !key.starts_with("--") {
            key.make_ascii_lowercase();
        }
        let replacement = inline_replacement(&key, value.trim(), important)?;
        write_inline_style(
            &mut self.vm,
            &self.state,
            id,
            &key,
            inline_declarations(&self.state, id),
            replacement,
        )
        .map_err(|error| error.to_string())
    }
}

fn inline_replacement(
    key: &str,
    value: &str,
    important: bool,
) -> Result<Option<css::Declaration>, &'static str> {
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        || key.is_empty()
    {
        return Err("Invalid CSS property name");
    }
    Ok(if value.is_empty() {
        None
    } else {
        let mut declarations = css::declarations(&format!("{key}:{value};"));
        if declarations.len() != 1 || declarations[0].name != key || declarations[0].important {
            return Err("Enter one CSS value; use the Important control for priority");
        }
        let mut declaration = declarations.pop().unwrap();
        if let Some(property) = css::inline_style::ReflectedLonghand::named(key) {
            let Some(value) = property.serialize(value) else {
                return Err("Unsupported CSS property or invalid value");
            };
            declaration.value = value;
        } else if !css::supports_declaration(key, value) {
            return Err("Unsupported CSS property or invalid value");
        }
        declaration.important = important;
        Some(declaration)
    })
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
    install_event_handler_list(
        vm,
        state,
        object,
        &[
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
            "onwheel",
            "onresize",
            "onpopstate",
            "onhashchange",
            "onstorage",
            "onanimationstart",
            "onanimationiteration",
            "onanimationend",
            "onanimationcancel",
        ],
    )
}

fn install_event_handler_list(
    vm: &mut Vm,
    state: &Shared,
    object: Value,
    properties: &[&str],
) -> Result<(), JsError> {
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
                        report_error(
                            &get_state,
                            ConsolePhase::EventHandlerCompile,
                            Some(&get_name),
                            &error,
                            || error.to_string(),
                        );
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
    Host,
    WindowLoad,
}

impl DispatchMode {
    fn checkpoint(self, vm: &mut Vm, state: &Shared) -> Result<(), JsError> {
        if self != Self::Ordinary {
            // Host events start in the host. Cleanup after a callback
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
    let keyboard = matches!(kind, "keydown" | "keyup" | "keypress");
    let focus = matches!(kind, "focus" | "blur" | "focusin" | "focusout");
    let object = if kind == "click" {
        let prototype = state.borrow().mouse_event_prototype.clone();
        let object = vm.new_object_with_prototype(prototype)?;
        ui_events::initialize_click(vm, &object)?;
        object
    } else if keyboard {
        let prototype = state.borrow().keyboard_event_prototype.clone();
        let object = vm.new_object_with_prototype(prototype)?;
        ui_events::initialize_key_input(vm, &object, properties)?;
        object
    } else if focus {
        let prototype = state.borrow().focus_event_prototype.clone();
        let object = vm.new_object_with_prototype(prototype)?;
        let related = properties.iter().find(|(name, _)| *name == "relatedTarget")
            .map_or(Value::Null, |(_, value)| value.clone());
        ui_events::initialize_focus_input(vm, &object, related)?;
        object
    } else {
        vm.new_object_with_prototype(state.borrow().event_prototype.clone())?
    };
    let event =
        event_target::init_event(vm, object, kind.into(), [bubbles, cancelable, keyboard || focus], true)?;
    let properties = if keyboard || focus { &[] } else { properties };
    dispatch_prepared_with_mode(vm, state, target, event, properties, mode)
}

fn dispatch_mouse(
    vm: &mut Vm,
    state: &Shared,
    target: NodeId,
    kind: &str,
    input: MouseInput,
    buttons: i32,
    detail: i32,
) -> Result<bool, JsError> {
    let prototype = state.borrow().mouse_event_prototype.clone();
    let object = vm.new_object_with_prototype(prototype)?;
    ui_events::initialize_mouse_input(vm, &object, input, buttons, detail)?;
    let event = event_target::init_event(vm, object, kind.into(), [true, true, true], true)?;
    dispatch_prepared(vm, state, Some(target), event, &[])
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
    let call: event_target::ListenerCall = if mode == DispatchMode::Ordinary {
        Vm::call
    } else {
        Vm::call_at_callback_boundary
    };
    let result = scheduler::run_script_task(vm, |vm| -> Result<(), JsError> {
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
                call,
                |_, error| {
                    report_error(state, ConsolePhase::DomListener, Some(&kind), error, || {
                        format!("{kind}: {error}")
                    });
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
                                report_error(
                                    state,
                                    ConsolePhase::EventHandlerCompile,
                                    Some(&kind),
                                    &error,
                                    || format!("on{kind}: {error}"),
                                );
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
                    match call(vm, handler, this, args) {
                        Ok(Value::Bool(value))
                            if info.cancelable && value == error_args.is_some() =>
                        {
                            info.prevented.set(true);
                        }
                        Err(error) => {
                            if fatal(&error) {
                                return Err(error);
                            }
                            report_error(
                                state,
                                ConsolePhase::EventHandler,
                                Some(&kind),
                                &error,
                                || format!("on{kind}: {error}"),
                            );
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
    fn large_live_document_collects_on_growth_and_leaves_idle_pumps_alone() {
        let page = LoadedPage::from_html("https://example.test/", "<body></body>");
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let retained = runtime
            .vm
            .finish_buffer_string(vec![b'x' as u16; 9 * 1024 * 1024])
            .unwrap();
        runtime.vm.set_global("catalog", retained).unwrap();
        runtime.pump_with_network_errors().unwrap();
        let baseline = runtime.vm.heap_cell_count();
        let before = runtime.next_collection_bytes;
        for _ in 0..32 {
            let _ = runtime.vm.new_object();
        }
        let with_garbage = runtime.vm.heap_cell_count();
        runtime.pump_with_network_errors().unwrap();
        assert_eq!(runtime.next_collection_bytes, before);
        assert!(runtime.vm.heap_cell_count() >= with_garbage);
        // Exhaust the object allocation interval with unreachable objects.
        while runtime.vm.heap_cell_count() < runtime.next_collection_cells {
            let _ = runtime.vm.new_object();
        }
        runtime.pump_with_network_errors().unwrap();
        assert!(runtime.vm.heap_cell_count() <= baseline + 4);
        assert_eq!(
            runtime.eval("catalog.length").unwrap(),
            Value::from(9 * 1024 * 1024)
        );
    }

    #[test]
    fn classic_scripts_reuse_collection_thresholds_for_large_live_pages() {
        let page = LoadedPage::from_html("https://example.test/", "<script>0</script>");
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let retained = runtime
            .vm
            .finish_buffer_string(vec![b'x' as u16; 9 * 1024 * 1024])
            .unwrap();
        runtime.vm.set_global("catalog", retained).unwrap();
        let script = &page.scripts[0];
        let run = |runtime: &mut ScriptRuntime| {
            runtime.execute_classic(script.node, "0", &script.name, page.base.as_str(), false);
        };

        run(&mut runtime);
        let orphan = runtime.vm.new_object();
        run(&mut runtime);
        assert!(runtime.vm.get_property(orphan.clone(), "x").is_ok());
        while runtime.vm.heap_cell_count() < runtime.next_collection_cells {
            runtime.vm.new_object();
        }
        run(&mut runtime);
        assert!(runtime.vm.get_property(orphan, "x").is_err());
        assert_eq!(
            runtime.eval("catalog.length").unwrap(),
            Value::from(9 * 1024 * 1024)
        );
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
        // Supply a deterministic outer fuel budget to test inheritance. Real
        // window tasks now use the duration watchdog for the same boundary.
        let work = runtime.vm.new_native_function("chargeWork", |vm, _, _| {
            vm.charge_work(16_000)?;
            Ok(Value::Undefined)
        });
        runtime.vm.set_global("chargeWork", work).unwrap();
        runtime.eval(r#"
            var calls = 0, b = document.getElementById('button');
            function work() { calls++; chargeWork(); }
            for (var i = 0; i < 128; i++) b.addEventListener('click', function() { work(); });
        "#).unwrap();
        let state = runtime.state.clone();
        let target = page.document.elements("button").next().unwrap();
        assert!(runtime.vm.with_task_limits(1_000_000, None, |vm| {
            dispatch_checked(vm, &state, Some(target), "click", true, true, &[])
        }).is_err());
        let calls = runtime.eval("calls").unwrap().as_number().unwrap();
        assert!(
            calls > 1. && calls < 128.,
            "separate callbacks reset the budget: {calls}"
        );
        runtime.eval("calls = 0; for (var i = 0; i < 128; i++) queueMicrotask(work);").unwrap();
        assert!(runtime.vm.with_task_limits(1_000_000, None, |vm| {
            drain_microtask_queue(vm, &state)
        }).is_err());
        let calls = runtime.eval("calls").unwrap().as_number().unwrap();
        assert!(
            calls > 1. && calls < 128.,
            "microtasks reset the budget: {calls}"
        );
        assert!(runtime.next_deadline().is_none());
    }
}
