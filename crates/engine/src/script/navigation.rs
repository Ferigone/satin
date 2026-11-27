//! Shared native/headless navigation driver. HTML, its resources, and the
//! realm advance as separate bounded tasks; the DOM has one owner at a time.
pub(in crate::script) mod dynamic_scripts;
mod font_loads;
mod import_map;
mod module_graph;
pub(in crate::script) mod module_tasks;
mod parser_host;
#[cfg(test)]
mod tests;

use super::*;
use crate::{
    html::ParserScriptKind,
    layout::Viewport,
    loader::{
        DocumentInput, DocumentResource, OpenedPage, ResourceBody, ResourceKind, ResourceRequest,
        CLASSIC_SCRIPT_LIMIT, DOCUMENT_CLASSIC_SCRIPT_LIMIT, image_asset_key,
    },
    network::CredentialsMode,
};
use font_loads::{FontKey, FontLoads};
use module_graph::{ModuleGraph, ModuleKey, ModuleSource, Readiness};
use module_tasks::ModuleTasks;
pub(super) use parser_host::ParserHost;
use parser_host::{Discovery, ScriptTag, Source, Step, resource_request};
use std::collections::{HashSet, VecDeque};
use std::time::Duration;
use url::Url;

const RESOURCE_COUNT: usize = 768;
const RESOURCE_URL_BYTES: usize = 24 * 1024 * 1024;
const CONCURRENT_RESOURCES: usize = 4;
const DECODED_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const STYLESHEET_BYTES: usize = 8 * 1024 * 1024;
const DOCUMENT_RESOURCE_TIMEOUT: Duration = Duration::from_secs(210);
const DOCUMENT_RESOURCE_QUEUE_TIMEOUT: Duration = Duration::from_secs(240);

pub struct ResourceLoad {
    pub id: u64,
    pub request: ResourceRequest,
    queue_deadline: Instant,
}

enum Consumer {
    Classic(NodeId),
    Sheet {
        node: NodeId,
        source: Arc<StylesheetSource>,
        media: Arc<str>,
        parser_inserted: bool,
    },
    Image(Arc<str>),
    Module(ModuleKey, CredentialsMode),
    Font(FontKey),
    Media(MediaKey),
}

#[derive(PartialEq, Eq)]
struct StylesheetSource {
    url: Url,
    cors: Option<CredentialsMode>,
    integrity: Option<Arc<str>>,
}

impl StylesheetSource {
    fn from_document(state: &State, client: &Url, node: NodeId) -> Result<Option<Self>, String> {
        let document = &state.document;
        let Some(href) = link_element::stylesheet_href(document, node)
            .filter(|_| parser_host::connected(document, node))
        else {
            return Ok(None);
        };
        let base = super::document_base(state);
        Ok(Some(Self {
            url: parser_host::resolve_resource(client, &base, href)?,
            cors: parser_host::cors_attr(document, node),
            integrity: parser_host::integrity_attr(document, node),
        }))
    }
}

struct ResourceQueue {
    next: u64,
    url_bytes: usize,
    waiting: VecDeque<ResourceLoad>,
    pending: HashMap<u64, (Consumer, Instant)>,
    active: HashSet<u64>,
    retry_at: Option<Instant>,
}
impl ResourceQueue {
    fn new() -> Self {
        Self {
            next: 1,
            url_bytes: 0,
            waiting: VecDeque::new(),
            pending: HashMap::new(),
            active: HashSet::new(),
            retry_at: None,
        }
    }
    fn queue_deadline() -> Instant {
        Instant::now() + DOCUMENT_RESOURCE_QUEUE_TIMEOUT
    }
    fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .values()
            .map(|(_, deadline)| *deadline)
            .chain(
                self.retry_at.filter(|_| {
                    !self.waiting.is_empty() && self.active.len() < CONCURRENT_RESOURCES
                }),
            )
            .min()
    }
    fn queue(&mut self, request: ResourceRequest, consumer: Consumer) -> Result<(), String> {
        let size = request
            .url
            .as_str()
            .len()
            .saturating_add(request.integrity.as_ref().map_or(0, |source| source.len()));
        if self.next > RESOURCE_COUNT as u64 {
            return Err("Document resource count limit exceeded".into());
        }
        if self.url_bytes.saturating_add(size) > RESOURCE_URL_BYTES {
            return Err("Document resource URL byte limit exceeded".into());
        }
        let queue_deadline = request.deadline;
        if Instant::now() >= queue_deadline {
            return Err("Document resource queue deadline exceeded".into());
        }
        let id = self.next;
        self.next += 1;
        self.url_bytes += size;
        self.pending.insert(id, (consumer, queue_deadline));
        self.waiting.push_back(ResourceLoad {
            id,
            request,
            queue_deadline,
        });
        Ok(())
    }
    fn take(&mut self, now: Instant) -> Option<ResourceLoad> {
        if self.active.len() >= CONCURRENT_RESOURCES || self.retry_at.is_some_and(|next| next > now)
        {
            return None;
        }
        self.retry_at = None;
        if now >= self.waiting.front()?.queue_deadline {
            // Leave expired work for advance(), which delivers its failure.
            return None;
        }
        let mut load = self.waiting.pop_front()?;
        let (_, deadline) = self
            .pending
            .get_mut(&load.id)
            .expect("queued resource exists");
        *deadline = now + DOCUMENT_RESOURCE_TIMEOUT;
        load.request.deadline = *deadline;
        self.active.insert(load.id);
        Some(load)
    }
    fn retry(&mut self, mut load: ResourceLoad) {
        if self.active.remove(&load.id)
            && let Some((_, deadline)) = self.pending.get_mut(&load.id)
        {
            *deadline = load.queue_deadline;
            load.request.deadline = load.queue_deadline;
            self.waiting.push_front(load);
            self.retry_at = Some(Instant::now() + Duration::from_millis(8));
        }
    }
    fn has_stylesheets(&self, viewport: (f32, f32)) -> bool {
        self.pending.values().any(|(value, _)| {
            matches!(value, Consumer::Sheet { media, parser_inserted: true, .. }
            if css::stylesheet_media_applies(media, viewport))
        })
    }
    fn blocks_load(&self) -> bool {
        self.pending
            .values()
            .any(|(value, _)| !matches!(value, Consumer::Font(_) | Consumer::Module(..)))
    }
    fn discard_markup_requests(&mut self) {
        self.pending.retain(|_, (consumer, _)| {
            !matches!(
                consumer,
                Consumer::Classic(_) | Consumer::Sheet { .. } | Consumer::Image(_)
            )
        });
        self.active.retain(|id| self.pending.contains_key(id));
        self.waiting
            .retain(|load| self.pending.contains_key(&load.id));
        if self.waiting.is_empty() {
            self.retry_at = None;
        }
    }
}

enum ScriptCode {
    Classic {
        name: String,
        source: String,
        identifier: Arc<Url>,
    },
    FetchingClassic,
    Module(ModuleKey),
    Failed(Arc<str>),
}
struct Scheduled {
    node: NodeId,
    external: bool,
    code: ScriptCode,
}
enum ImageState {
    Pending(Vec<NodeId>),
    Failed,
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Parsing,
    Deferred,
    Loading,
    Complete,
}

pub struct DocumentSession {
    streaming_json: Option<crate::raw_json::SharedSource>,
    parser: Option<Rc<RefCell<ParserHost>>>,
    script_replaced_input: bool,
    history: crate::history::SessionHistory,
    storage: crate::web_storage::TabStorage,
    phase: Phase,
    client: Arc<Url>,
    started: Instant,
    response_start: Instant,
    resources: ResourceQueue,
    fonts: FontLoads,
    modules: ModuleGraph,
    module_tasks: ModuleTasks,
    blocking: Option<Scheduled>,
    deferred: VecDeque<Scheduled>,
    asynchronous: VecDeque<Scheduled>,
    ordered_dynamic: VecDeque<Scheduled>,
    messages: Vec<(NodeId, String)>,
    resource_events: Vec<(NodeId, bool)>,
    sheet_sources: HashMap<NodeId, Arc<StylesheetSource>>,
    images: HashMap<Arc<str>, ImageState>,
    decoded_images: usize,
    classic_bytes: usize,
    css_bytes: usize,
    runnable: bool,
    dirty: bool,
    resources_dirty: bool,
    preview_at: Instant,
    discarded: usize,
    parser_script_count: usize,
    device_pixel_ratio: f64,
}

impl DocumentSession {
    pub fn start(opened: OpenedPage) -> Result<(LoadedPage, Self), String> {
        let url = Url::parse(&opened.page.url).map_err(|e| e.to_string())?;
        Self::start_with_history(opened, crate::history::SessionHistory::new(url))
    }

    pub fn start_with_history(
        opened: OpenedPage,
        history: crate::history::SessionHistory,
    ) -> Result<(LoadedPage, Self), String> {
        Self::start_in_tab(opened, history, crate::web_storage::TabStorage::default())
    }

    pub fn start_in_tab(
        opened: OpenedPage,
        history: crate::history::SessionHistory,
        storage: crate::web_storage::TabStorage,
    ) -> Result<(LoadedPage, Self), String> {
        let OpenedPage {
            page,
            input,
            started,
            response_start,
            ..
        } = opened;
        let client = Arc::new(Url::parse(&page.url).map_err(|e| e.to_string())?);
        history.commit_address((*client).clone());
        let streaming_json = if matches!(input, DocumentInput::StreamingJson) {
            page.raw_json.clone()
        } else {
            None
        };
        let parser = if matches!(input, DocumentInput::Html(_) | DocumentInput::Streaming) {
            let mut parser = ParserHost::new(client.clone());
            if let DocumentInput::Html(source) = input {
                parser.stream.feed(&source).map_err(|e| e.to_string())?;
                parser.stream.finish().map_err(|e| e.to_string())?;
            }
            Some(Rc::new(RefCell::new(parser)))
        } else {
            None
        };
        let decoded_images = page.images.values().map(|image| image.as_raw().len()).sum();
        Ok((
            page,
            Self {
                streaming_json,
                parser,
                script_replaced_input: false,
                history,
                storage,
                phase: Phase::Parsing,
                client,
                started,
                response_start,
                resources: ResourceQueue::new(),
                fonts: FontLoads::default(),
                modules: ModuleGraph::default(),
                module_tasks: ModuleTasks::default(),
                blocking: None,
                deferred: VecDeque::new(),
                asynchronous: VecDeque::new(),
                ordered_dynamic: VecDeque::new(),
                messages: vec![],
                resource_events: vec![],
                sheet_sources: HashMap::new(),
                images: HashMap::new(),
                decoded_images,
                classic_bytes: 0,
                css_bytes: 0,
                runnable: true,
                dirty: true,
                resources_dirty: false,
                preview_at: Instant::now(),
                discarded: 0,
                parser_script_count: 0,
                device_pixel_ratio: 1.0,
            },
        ))
    }

    /// Set the output scale before the first script and propagate subsequent changes.
    pub fn set_device_pixel_ratio(&mut self, ratio: f64, runtime: Option<&ScriptRuntime>) {
        if ratio.is_finite() && ratio > 0.0 {
            self.device_pixel_ratio = ratio;
            if let Some(runtime) = runtime {
                runtime.set_device_pixel_ratio(ratio);
            }
        }
    }

    pub fn loaded(&self) -> bool {
        self.phase == Phase::Complete
    }
    pub fn done(&self) -> bool {
        self.loaded()
            && self.resources.pending.is_empty()
            && self.asynchronous.is_empty()
            && self.ordered_dynamic.is_empty()
            && !self.module_tasks.waiting_on_graph()
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.runnable {
            Some(Instant::now())
        } else {
            self.resources.next_deadline()
        }
    }
    pub fn needs_advance(&self, runtime: Option<&ScriptRuntime>) -> bool {
        runtime.is_some_and(|realm| {
            realm.has_queued_script_tasks()
                || realm.state.borrow().media.has_loads()
                || self.module_tasks.ready(&self.modules, realm)
        }) || self
            .next_deadline()
            .is_some_and(|next| next <= Instant::now())
    }
    pub fn next_resource(&mut self) -> Option<ResourceLoad> {
        self.resources.take(Instant::now())
    }
    pub fn stop(&mut self, page: &mut LoadedPage, mut runtime: Option<&mut ScriptRuntime>) {
        self.streaming_json = None;
        self.sync_parser_policy(page, runtime.as_deref_mut());
        if let Some(realm) = runtime.as_deref_mut() {
            {
                let mut state = realm.state.borrow_mut();
                state.ready_state = "complete";
                state.parser = std::rc::Weak::new();
                state.replacement_parser = None;
                state.parser_wakeup = false;
                state.dynamic_scripts.pending.clear();
                state.image_updates.clear();
                state.link_updates.clear();
            }
            self.module_tasks.stop_queued(realm);
        } else if let Some(parser) = &self.parser
            && let Ok(document) = parser.borrow_mut().stream.take_document()
        {
            page.base = live_base(&document, &self.client);
            page.document = Arc::new(document);
            page.refresh_stylesheets();
        }
        self.parser = None;
        self.phase = Phase::Complete;
        self.blocking = None;
        self.deferred.clear();
        self.asynchronous.clear();
        self.ordered_dynamic.clear();
        self.messages.clear();
        self.resource_events.clear();
        self.images
            .retain(|_, state| matches!(state, ImageState::Failed));
        page.stats.skipped_resources += self.resources.pending.len();
        for (_, (consumer, _)) in self.resources.pending.drain() {
            match consumer {
                Consumer::Module(key, _) => self.modules.fail(key, "Module fetch stopped".into()),
                Consumer::Media(key) => {
                    if let Some(realm) = runtime.as_deref_mut() {
                        realm.accept_media_bytes(key, Err("Media fetch stopped".into()));
                    }
                }
                _ => {}
            }
        }
        self.resources.waiting.clear();
        self.resources.active.clear();
        self.resources.retry_at = None;
        self.runnable = false;
        self.dirty = false;
        if let Some(realm) = runtime {
            realm.snapshot(page);
        }
    }
    pub fn retry_resource(&mut self, load: ResourceLoad) {
        self.resources.retry(load);
    }
    pub fn feed(&mut self, source: &str) -> Result<(), String> {
        if let Some(json) = &self.streaming_json {
            json.lock().unwrap().push(source);
            self.runnable = true;
            return Ok(());
        }
        if let Some(parser) = self.parser.as_ref().filter(|_| !self.script_replaced_input) {
            match parser.borrow_mut().stream.feed(source) {
                Ok(()) | Err(crate::html::HtmlStreamError::ResourceLimit) => {}
                Err(error) => return Err(error.to_string()),
            }
            self.runnable = true;
        }
        Ok(())
    }
    pub fn finish_input(&mut self) -> Result<(), String> {
        if self.streaming_json.take().is_some() {
            self.runnable = true;
            return Ok(());
        }
        if let Some(parser) = self.parser.as_ref().filter(|_| !self.script_replaced_input) {
            match parser.borrow_mut().stream.finish() {
                Ok(()) | Err(crate::html::HtmlStreamError::ResourceLimit) => {}
                Err(error) => return Err(error.to_string()),
            }
            self.runnable = true;
        }
        Ok(())
    }

    pub fn inspected_modules(&self) -> (&[super::InspectedSource], usize) {
        self.modules.sources.view()
    }

    /// Lazily create this navigation's own realm, also for a Console command on a script-free page.
    pub fn ensure_realm(
        &mut self,
        page: &LoadedPage,
        runtime: &mut Option<ScriptRuntime>,
        viewport: Viewport,
    ) -> Result<(), String> {
        if runtime.is_some() {
            return Ok(());
        }
        let realm = if let Some(parser) = &self.parser {
            let (document, bytes, script_flags) = {
                let mut parser = parser.borrow_mut();
                let bytes = parser.stream.document_bytes();
                let document = parser.stream.take_document().map_err(|e| e.to_string())?;
                (document, bytes, std::mem::take(&mut parser.script_flags))
            };
            let realm = ScriptRuntime::new_with_document(
                page,
                document,
                bytes,
                true,
                self.history.clone(),
                &self.storage,
                self.started,
            )
            .map_err(|e| e.to_string())?;
            {
                let mut state = realm.state.borrow_mut();
                state.parser = Rc::downgrade(parser);
                state.dynamic_scripts.flags = script_flags;
            }
            realm
        } else {
            ScriptRuntime::new_for_history(page, self.history.clone(), &self.storage, self.started)
                .map_err(|e| e.to_string())?
        };
        {
            let mut state = realm.state.borrow_mut();
            state.navigation_attached = true;
            state.scheduler.note_response_start(self.response_start);
        }
        realm.set_viewport(viewport);
        realm.set_device_pixel_ratio(self.device_pixel_ratio);
        // This case is used when a late resource handler is the first caller.
        if self.phase != Phase::Parsing {
            realm.state.borrow_mut().ready_state = if self.loaded() {
                "complete"
            } else {
                "interactive"
            };
        }
        *runtime = Some(realm);
        Ok(())
    }

    fn adopt_replacement_parser(&mut self, runtime: Option<&ScriptRuntime>) {
        let Some(realm) = runtime else { return };
        let mut state = realm.state.borrow_mut();
        self.runnable |= std::mem::take(&mut state.parser_wakeup);
        let Some(parser) = state.replacement_parser.take() else {
            return;
        };
        self.parser = Some(parser);
        self.script_replaced_input = true;
        self.phase = Phase::Parsing;
        self.blocking = None;
        self.deferred.clear();
        self.asynchronous.clear();
        self.ordered_dynamic.clear();
        self.resource_events.clear();
        self.messages.clear();
        self.images.clear();
        self.sheet_sources.clear();
        self.resources.discard_markup_requests();
        self.discarded = 0;
        self.dirty = true;
    }

    pub fn advance(
        &mut self,
        page: &mut LoadedPage,
        runtime: &mut Option<ScriptRuntime>,
        viewport: Viewport,
    ) -> Result<bool, String> {
        if self.streaming_json.is_some() {
            self.runnable = false;
            return Ok(true);
        }
        self.adopt_replacement_parser(runtime.as_ref());
        let now = Instant::now();
        let end = now + Duration::from_millis(5);
        if self
            .resources
            .pending
            .values()
            .any(|(_, deadline)| now >= *deadline)
        {
            let mut expired = self
                .resources
                .pending
                .iter()
                .filter_map(|(&id, (_, deadline))| (now >= *deadline).then_some(id))
                .collect::<Vec<_>>();
            expired.sort_unstable();
            self.resources
                .waiting
                .retain(|load| now < load.queue_deadline);
            for id in expired {
                let reason = if self.resources.active.contains(&id) {
                    "Document resource transfer deadline exceeded"
                } else {
                    "Document resource queue deadline exceeded"
                };
                self.complete_resource(id, Err(reason.into()), page, runtime, viewport)?;
            }
        }
        self.runnable = true;
        for _ in 0..24 {
            self.adopt_replacement_parser(runtime.as_ref());
            self.flush_dynamic_scripts(page, runtime, viewport)?;
            self.flush_parser_discoveries(page, runtime, viewport)?;
            if let Some(realm) = runtime {
                loop {
                    let load = realm.state.borrow_mut().media.take_load();
                    let Some((key, request)) = load else {
                        break;
                    };
                    if let Err(error) = self.resources.queue(request, Consumer::Media(key)) {
                        realm.accept_media_bytes(key, Err(error));
                    }
                }
            }
            self.sync_fonts(page)?;
            self.update_resources(page, runtime);
            if !self.resource_events.is_empty() {
                let events = std::mem::take(&mut self.resource_events);
                self.deliver_events(page, runtime, viewport, events)?;
            }
            if let Some(realm) = runtime {
                realm.set_viewport(viewport);
                if self.module_tasks.progress(&self.modules, realm) {
                    self.dirty = true;
                    if Instant::now() >= end {
                        break;
                    }
                    continue;
                }
            }
            let ready_async = if let Some(index) = self
                .asynchronous
                .iter()
                .position(|script| self.ready(script))
            {
                self.asynchronous.remove(index)
            } else if self
                .ordered_dynamic
                .front()
                .is_some_and(|script| self.ready(script))
            {
                self.ordered_dynamic.pop_front()
            } else {
                None
            };
            if let Some(script) = ready_async {
                self.execute(
                    script,
                    false,
                    runtime.as_mut().expect("async script has a realm"),
                )?;
            } else if self.blocking.is_some() {
                if self
                    .resources
                    .has_stylesheets((viewport.width, viewport.height))
                    || self
                        .blocking
                        .as_ref()
                        .is_some_and(|script| !self.ready(script))
                {
                    self.runnable = false;
                    break;
                }
                let script = self.blocking.take().unwrap();
                self.execute(
                    script,
                    true,
                    runtime.as_mut().expect("blocking script has a realm"),
                )?;
            } else if self.phase == Phase::Parsing {
                let step = self.parser.as_ref().map_or(Ok(Step::Complete), |parser| {
                    parser
                        .borrow_mut()
                        .advance(runtime.as_ref().map(|realm| &realm.state), None)
                });
                self.sync_parser_policy(page, runtime.as_mut());
                let step = step.map_err(|error| error.to_string())?;
                if let Some(realm) = runtime {
                    mutation_observers::schedule(&mut realm.vm, &realm.state);
                    if let Some(parser) = &self.parser {
                        ParserHost::construct_element(parser, &mut realm.vm, &realm.state)
                            .map_err(|error| error.to_string())?;
                    }
                    custom_elements::flush(&mut realm.vm, &realm.state)
                        .map_err(|error| error.to_string())?;
                }
                self.dirty = true;
                match step {
                    Step::ScriptCheckpoint => {
                        if let Some(realm) = runtime {
                            realm.drain_microtasks();
                        }
                    }
                    Step::Blocking(tag) => {
                        self.flush_parser_discoveries(page, runtime, viewport)?;
                        self.blocking = Some(self.schedule(tag, page, runtime, viewport)?);
                    }
                    Step::Complete | Step::Truncated => {
                        self.flush_parser_discoveries(page, runtime, viewport)?;
                        if self.wants_realm(page) {
                            self.ensure_realm(page, runtime, viewport)?;
                        }
                        self.phase = Phase::Deferred;
                        if let Some(realm) = runtime {
                            let still_loading = realm.state.borrow().ready_state == "loading";
                            if still_loading {
                                realm.change_ready_state("interactive");
                            }
                        }
                    }
                    Step::NeedInput => {
                        self.runnable = false;
                        break;
                    }
                    Step::Yield => {}
                    Step::WriteComplete => unreachable!("only a write call sees its marker"),
                }
            } else if self.phase == Phase::Deferred {
                if let Some(script) = self.deferred.front() {
                    if self
                        .resources
                        .has_stylesheets((viewport.width, viewport.height))
                        || !self.ready(script)
                    {
                        self.runnable = false;
                        break;
                    }
                    let script = self.deferred.pop_front().unwrap();
                    self.execute(
                        script,
                        false,
                        runtime.as_mut().expect("deferred script has a realm"),
                    )?;
                } else {
                    self.phase = Phase::Loading;
                    if let Some(realm) = runtime {
                        let root = realm.state.borrow().document.root;
                        realm.lifecycle_event(Some(root), "DOMContentLoaded", true);
                    }
                }
            } else {
                if self.phase == Phase::Loading
                    && self.asynchronous.is_empty()
                    && self.ordered_dynamic.is_empty()
                    && !self.resources.blocks_load()
                {
                    self.phase = Phase::Complete;
                    self.dirty = true;
                    page.stats.elapsed_ms = self.started.elapsed().as_millis();
                    if let Some(realm) = runtime {
                        realm.change_ready_state("complete");
                        realm.lifecycle_event(None, "load", false);
                        report_unhandled(&mut realm.vm, &realm.state);
                        realm.vm.collect_garbage();
                    }
                }
                self.runnable = false;
                break;
            }
            if Instant::now() >= end {
                break;
            }
        }
        // NeedInput can arrive immediately after a discovery. Queue it even
        // if the server is waiting for that resource before it sends more.
        self.adopt_replacement_parser(runtime.as_ref());
        self.flush_dynamic_scripts(page, runtime, viewport)?;
        self.flush_parser_discoveries(page, runtime, viewport)?;
        self.sync_fonts(page)?;
        self.update_resources(page, runtime);
        if !self.resource_events.is_empty() {
            let events = std::mem::take(&mut self.resource_events);
            self.deliver_events(page, runtime, viewport, events)?;
        }
        page.stats.script_count = self.parser_script_count
            + runtime
                .as_ref()
                .map_or(0, |realm| realm.state.borrow().dynamic_scripts.prepared);
        if self.dirty && (!self.runnable || now >= self.preview_at) {
            self.dirty = false;
            self.preview_at = now + Duration::from_millis(16);
            if let Some(realm) = runtime {
                realm.snapshot(page);
            } else if let Some(parser) = &self.parser {
                let mut parser = parser.borrow_mut();
                let document = if self.loaded() {
                    parser
                        .stream
                        .take_document()
                        .map_err(|error| error.to_string())?
                } else {
                    parser
                        .stream
                        .document()
                        .expect("without a realm the parser owns the DOM")
                        .clone()
                };
                page.base = live_base(&document, &self.client);
                page.document = Arc::new(document);
                page.refresh_stylesheets();
            }
            if self.loaded() {
                // The DOM and realm remain live. Input and parser stacks are
                // no longer needed, while the graph still serves new scripts.
                self.parser = None;
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn flush_dynamic_scripts(
        &mut self,
        page: &mut LoadedPage,
        runtime: &mut Option<ScriptRuntime>,
        viewport: Viewport,
    ) -> Result<(), String> {
        let Some(realm) = runtime else {
            return Ok(());
        };
        self.flush_link_updates(page, realm);
        let updates = std::mem::take(&mut realm.state.borrow_mut().image_updates);
        if !updates.is_empty() {
            let mut state = realm.state.borrow_mut();
            let base = super::document_base(&state);
            for node in updates {
                let Some(src) = state.document.attr(node, "src").map(str::to_owned) else {
                    continue;
                };
                match parser_host::resolve_resource(&self.client, &base, &src) {
                    Ok(url) => self.discover_image(
                        page,
                        node,
                        src,
                        url,
                        parser_host::cors_attr(&state.document, node),
                    ),
                    Err(_) => {
                        state.invalid_images.insert(node);
                        self.resource_events.push((node, false));
                    }
                }
            }
        }
        if self
            .module_tasks
            .begin(realm, &mut self.modules, &self.client, &mut self.resources)
        {
            self.runnable = true;
        }
        let pending = std::mem::take(&mut realm.state.borrow_mut().dynamic_scripts.pending);
        for tag in pending {
            let asynchronous = tag.asynchronous;
            let script = self.schedule(tag, page, runtime, viewport)?;
            if asynchronous {
                self.asynchronous.push_back(script);
            } else {
                self.ordered_dynamic.push_back(script);
            }
            self.runnable = true;
        }
        Ok(())
    }

    fn flush_link_updates(&mut self, page: &mut LoadedPage, realm: &ScriptRuntime) {
        let updates = std::mem::take(&mut realm.state.borrow_mut().link_updates);
        for node in updates {
            let (source, media) = {
                let state = realm.state.borrow();
                (
                    StylesheetSource::from_document(&state, &self.client, node),
                    Arc::from(state.document.attr(node, "media").unwrap_or("")),
                )
            };
            let result = match source {
                Ok(Some(source)) => self.queue_stylesheet(page, node, source, media, false),
                other => {
                    self.sheet_sources.remove(&node);
                    self.remove_stylesheet(page, node);
                    other.map(|_| ())
                }
            };
            if let Err(error) = result {
                diagnostic(&realm.state, error);
                page.stats.skipped_resources += 1;
                self.resource_events.push((node, false));
            }
        }
    }

    fn remove_stylesheet(&mut self, page: &mut LoadedPage, node: NodeId) {
        if let Some(old) = page.linked_stylesheets.remove(&node) {
            self.css_bytes = self.css_bytes.saturating_sub(old.len());
            self.resources_dirty = true;
        }
    }

    fn queue_stylesheet(
        &mut self,
        page: &mut LoadedPage,
        node: NodeId,
        source: StylesheetSource,
        media: Arc<str>,
        parser_inserted: bool,
    ) -> Result<(), String> {
        if self.sheet_sources.get(&node).is_some_and(|old| old.as_ref() == &source) {
            return Ok(());
        }
        self.sheet_sources.remove(&node);
        self.remove_stylesheet(page, node);
        let request = resource_request(
            source.url.clone(),
            &self.client,
            ResourceKind::Stylesheet,
            source.cors,
            source.integrity.clone(),
            ResourceQueue::queue_deadline(),
        );
        let source = Arc::new(source);
        self.resources.queue(request, Consumer::Sheet {
            node,
            source: source.clone(),
            media,
            parser_inserted,
        })?;
        self.sheet_sources.insert(node, source);
        Ok(())
    }

    fn wants_realm(&self, page: &LoadedPage) -> bool {
        self.parser.as_ref().map_or_else(
            || {
                page.document.connected_nodes().any(|id| {
                    media::has_controls(&page.document, id)
                        || matches!(&page.document.nodes[id.0].kind, NodeKind::Element { attrs, .. }
                            if attrs.keys().any(|name| name.starts_with("on")))
                })
            },
            |parser| parser.borrow().wants_realm,
        )
    }

    fn flush_parser_discoveries(
        &mut self,
        page: &mut LoadedPage,
        runtime: &mut Option<ScriptRuntime>,
        viewport: Viewport,
    ) -> Result<(), String> {
        self.sync_parser_policy(page, runtime.as_mut());
        let Some(parser) = self.parser.as_ref() else {
            return Ok(());
        };
        let (discoveries, nested, discarded, script_count) = {
            let mut parser = parser.borrow_mut();
            (
                std::mem::take(&mut parser.discoveries),
                parser.nested_blocked.take(),
                parser.discarded,
                parser.stream.report().scripts_prepared,
            )
        };
        page.stats.skipped_resources += discarded.saturating_sub(self.discarded);
        self.discarded = discarded;
        self.parser_script_count = script_count;
        for event in discoveries {
            match event {
                Discovery::Script(tag) if tag.kind == ParserScriptKind::ImportMap => {
                    match tag.source {
                        Source::Inline(source) => {
                            if let Err(message) = self.modules.imports.register(&source, &tag.base)
                            {
                                self.messages.push((tag.node, message));
                            }
                        }
                        Source::External(_) => self
                            .messages
                            .push((tag.node, "Import maps must be inline".into())),
                    }
                }
                Discovery::Script(tag) => {
                    let is_async = tag.asynchronous;
                    let script = self.schedule(tag, page, runtime, viewport)?;
                    if is_async {
                        self.asynchronous.push_back(script);
                    } else {
                        self.deferred.push_back(script);
                    }
                }
                Discovery::Sheet {
                    node,
                    url,
                    cors,
                    integrity,
                    media,
                } => {
                    let source = StylesheetSource { url, cors, integrity };
                    if let Err(message) = self.queue_stylesheet(page, node, source, media, true) {
                        self.messages.push((node, message));
                    }
                }
                Discovery::InlineSheet { node, base } => {
                    let sheet = |doc: &Document| {
                        let active_type = doc.attr(node, "type").is_none_or(|ty| {
                            ty.trim().is_empty() || ty.trim().eq_ignore_ascii_case("text/css")
                        });
                        (active_type
                            && css::stylesheet_media_applies(
                                doc.attr(node, "media").unwrap_or(""),
                                (viewport.width, viewport.height),
                            ))
                        .then(|| doc.text(node))
                    };
                    let source = if let Some(realm) = runtime {
                        sheet(&realm.state.borrow().document)
                    } else {
                        sheet(
                            self.parser
                                .as_ref()
                                .unwrap()
                                .borrow()
                                .stream
                                .document()
                                .unwrap(),
                        )
                    };
                    if let Some(source) = source {
                        self.fonts.register(
                            node,
                            &source,
                            base,
                            (viewport.width, viewport.height),
                            &self.client,
                            &mut self.resources,
                        );
                    }
                }
                Discovery::Image {
                    node,
                    src,
                    url,
                    cors,
                } => self.discover_image(page, node, src, url, cors),
                Discovery::Error { node, message } => self.messages.push((node, message)),
            }
        }
        if let Some(tag) = nested {
            self.blocking = Some(self.schedule(tag, page, runtime, viewport)?);
        }
        if !self.messages.is_empty() {
            if runtime.is_some() || self.wants_realm(page) {
                self.ensure_realm(page, runtime, viewport)?;
                let realm = runtime.as_mut().unwrap();
                for (node, message) in self.messages.drain(..) {
                    diagnostic(&realm.state, message);
                    realm.lifecycle_event(Some(node), "error", false);
                    page.stats.skipped_resources += 1;
                }
            } else {
                page.stats.skipped_resources += self.messages.len();
                self.messages.clear();
            }
        }
        Ok(())
    }

    fn sync_parser_policy(&self, page: &mut LoadedPage, runtime: Option<&mut ScriptRuntime>) {
        if let Some(parser) = &self.parser {
            let parser = parser.borrow();
            page.document_policy.restrict(parser.meta_policy());
            page.meta_connect_policy = parser.connect_policy().clone();
            if let Some(realm) = runtime {
                restrict_document_policy(&mut realm.vm, &realm.state, &page.document_policy);
                realm
                    .workers
                    .set_meta_connect_policy(&page.meta_connect_policy);
                realm.fetch.set_xhr_meta_policy(&page.meta_connect_policy);
                realm.sockets.set_meta_policy(&page.meta_connect_policy);
            }
        }
    }

    fn schedule(
        &mut self,
        tag: ScriptTag,
        page: &LoadedPage,
        runtime: &mut Option<ScriptRuntime>,
        viewport: Viewport,
    ) -> Result<Scheduled, String> {
        self.ensure_realm(page, runtime, viewport)?;
        let external = tag.external();
        let node = tag.node;
        let code = if tag.kind == ParserScriptKind::Module {
            let credentials = tag.cors.unwrap_or(CredentialsMode::SameOrigin);
            let root = match tag.source {
                Source::Inline(source) => self.modules.inline(
                    source,
                    tag.base,
                    &self.client,
                    credentials,
                    &mut runtime.as_mut().unwrap().vm,
                    &mut self.resources,
                ),
                Source::External(url) => url.and_then(|url| {
                    self.modules.external(
                        url,
                        &self.client,
                        credentials,
                        tag.integrity,
                        &mut self.resources,
                    )
                }),
            };
            match root {
                Ok(key) => ScriptCode::Module(key),
                Err(error) => ScriptCode::Failed(error.into()),
            }
        } else {
            match tag.source {
                Source::Inline(source) => {
                    self.classic_code(source, format!("inline script #{}", node.0), tag.base)
                }
                Source::External(url) => {
                    let result = url.and_then(|url| {
                        let mut request = resource_request(
                            url,
                            &self.client,
                            ResourceKind::ClassicScript,
                            tag.cors,
                            tag.integrity,
                            ResourceQueue::queue_deadline(),
                        );
                        request.script_encoding = Some(tag.charset.unwrap_or_else(|| page.encoding()));
                        self.resources.queue(request, Consumer::Classic(node))
                    });
                    match result {
                        Ok(()) => ScriptCode::FetchingClassic,
                        Err(error) => ScriptCode::Failed(error.into()),
                    }
                }
            }
        };
        Ok(Scheduled {
            node,
            external,
            code,
        })
    }

    fn classic_code(&mut self, source: String, name: String, identifier: Arc<Url>) -> ScriptCode {
        if source.len() > CLASSIC_SCRIPT_LIMIT
            || source.len() > DOCUMENT_CLASSIC_SCRIPT_LIMIT - self.classic_bytes
        {
            return ScriptCode::Failed("Classic script source budget exceeded".into());
        }
        self.classic_bytes += source.len();
        ScriptCode::Classic {
            name,
            source,
            identifier,
        }
    }

    fn ready(&self, script: &Scheduled) -> bool {
        match script.code {
            ScriptCode::FetchingClassic => false,
            ScriptCode::Module(key) => !matches!(self.modules.readiness(key), Readiness::Pending),
            _ => true,
        }
    }
    fn execute(
        &mut self,
        script: Scheduled,
        blocking: bool,
        realm: &mut ScriptRuntime,
    ) -> Result<(), String> {
        let node = script.node;
        let result = match script.code {
            ScriptCode::Classic {
                name,
                source,
                identifier,
            } => {
                if blocking {
                    self.parser
                        .as_ref()
                        .unwrap()
                        .borrow_mut()
                        .stream
                        .begin_script(node)
                        .map_err(|e| e.to_string())?;
                }
                realm.execute_classic(node, &source, &name, identifier.as_str(), script.external);
                if blocking {
                    self.parser
                        .as_ref()
                        .unwrap()
                        .borrow_mut()
                        .stream
                        .resume_script(node)
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            }
            ScriptCode::Failed(error) => {
                if blocking {
                    self.parser
                        .as_ref()
                        .unwrap()
                        .borrow_mut()
                        .stream
                        .skip_script(node)
                        .map_err(|e| e.to_string())?;
                }
                Err(error)
            }
            ScriptCode::Module(key) => match self.modules.readiness(key) {
                Readiness::Ready(module) => {
                    self.module_tasks
                        .execute_script(&self.modules, realm, module);
                    Ok(())
                }
                Readiness::Failed(error) => Err(error),
                Readiness::Pending => unreachable!("only ready modules execute"),
            },
            ScriptCode::FetchingClassic => unreachable!("only ready scripts execute"),
        };
        self.dirty = true;
        if let Err(error) = result {
            diagnostic(&realm.state, error.to_string());
            realm.lifecycle_event(Some(node), "error", false);
        } else if script.external {
            realm.lifecycle_event(Some(node), "load", false);
        }
        Ok(())
    }

    fn discover_image(
        &mut self,
        page: &mut LoadedPage,
        node: NodeId,
        src: String,
        url: Url,
        cors: Option<CredentialsMode>,
    ) {
        if src != url.as_str() {
            Arc::make_mut(&mut page.image_references).insert(node, (src, url.to_string()));
            self.resources_dirty = true;
        }
        let key = image_asset_key(url.as_str(), cors);
        if page.images.contains_key(&key) || page.svg_images.contains_key(&key) {
            self.resource_events.push((node, true));
            return;
        }
        let key: Arc<str> = Arc::from(key);
        if let Some(state) = self.images.get_mut(&key) {
            if let ImageState::Pending(waiters) = state {
                waiters.push(node);
            } else {
                self.resource_events.push((node, false));
            }
            return;
        }
        let request = resource_request(
            url,
            &self.client,
            ResourceKind::Image,
            cors,
            None,
            ResourceQueue::queue_deadline(),
        );
        match self.resources.queue(request, Consumer::Image(key.clone())) {
            Ok(()) => {
                self.images.insert(key, ImageState::Pending(vec![node]));
            }
            Err(message) => {
                Arc::make_mut(&mut page.image_availability)
                    .insert(key.to_string(), crate::loader::ImageAvailability::Failed);
                self.resources_dirty = true;
                self.resource_events.push((node, false));
                self.messages.push((node, message));
            }
        }
    }

    fn update_resources(&mut self, page: &mut LoadedPage, runtime: &mut Option<ScriptRuntime>) {
        if self.resources_dirty {
            self.resources_dirty = false;
            self.dirty = true;
            if let Some(realm) = runtime {
                realm.update_resources(page);
            } else {
                page.refresh_stylesheets();
            }
        }
    }
    fn sync_fonts(&mut self, page: &mut LoadedPage) -> Result<(), String> {
        if let Some(fonts) = self.fonts.take_set().map_err(|error| error.to_string())? {
            page.fonts = Arc::new(fonts);
            self.resources_dirty = true;
        }
        Ok(())
    }

    pub fn complete_resource(
        &mut self,
        id: u64,
        result: Result<DocumentResource, String>,
        page: &mut LoadedPage,
        runtime: &mut Option<ScriptRuntime>,
        viewport: Viewport,
    ) -> Result<(), String> {
        let Some((consumer, _)) = self.resources.pending.remove(&id) else {
            return Ok(());
        };
        self.resources.active.remove(&id);
        self.runnable = true;
        let transfer = result.as_ref().ok().map(|asset| asset.transferred);
        let mut accepted = false;
        let mut events = Vec::new();
        match consumer {
            Consumer::Classic(node) => {
                let code = match result {
                    Ok(DocumentResource {
                        url,
                        body: ResourceBody::Text(source),
                        ..
                    }) => {
                        accepted = true;
                        self.classic_code(
                            source,
                            url.as_str().chars().take(240).collect(),
                            Arc::new(url),
                        )
                    }
                    Err(error) => ScriptCode::Failed(error.into()),
                    _ => return Err("Unexpected classic script resource payload".into()),
                };
                if let Some(script) = self
                    .blocking
                    .iter_mut()
                    .chain(&mut self.deferred)
                    .chain(&mut self.ordered_dynamic)
                    .find(|script| script.node == node)
                {
                    script.code = code;
                } else if let Some(index) = self
                    .asynchronous
                    .iter()
                    .position(|script| script.node == node)
                {
                    // Preserve response arrival order across batched worker
                    // completions before the next navigation task.
                    let mut script = self.asynchronous.remove(index).unwrap();
                    script.code = code;
                    self.asynchronous.push_back(script);
                }
            }
            Consumer::Module(key, credentials) => match result {
                Ok(DocumentResource { url, body, .. }) => {
                    let source = match body {
                        ResourceBody::Text(source) => ModuleSource::JavaScript(source),
                        ResourceBody::ModuleBytes(bytes) => ModuleSource::Typed(bytes),
                        _ => return Err("Unexpected module payload".into()),
                    };
                    accepted = true;
                    let realm = runtime.as_mut().expect("module fetch has a realm");
                    self.modules.parsed_response(
                        key,
                        source,
                        Arc::new(url),
                        &self.client,
                        credentials,
                        &mut realm.vm,
                        &mut self.resources,
                    );
                }
                Err(error) => self.modules.fail(key, error),
            },
            Consumer::Sheet { node, source: request, media, .. } => {
                let admitted_request = self.sheet_sources.get(&node)
                    .is_some_and(|source| Arc::ptr_eq(source, &request));
                let (current, media) = runtime.as_ref().map_or((true, media), |realm| {
                    let state = realm.state.borrow();
                    let document = &state.document;
                    let active = document.nodes.get(node.0).is_some()
                        && parser_host::connected(document, node)
                        && link_element::stylesheet_href(document, node).is_some();
                    let unchanged = !state.link_updates.contains(&node)
                        || StylesheetSource::from_document(&state, &self.client, node)
                            .is_ok_and(|source| source.as_ref() == Some(request.as_ref()));
                    (active && unchanged, Arc::from(document.attr(node, "media").unwrap_or("")))
                });
                if admitted_request && current
                    && let Ok(DocumentResource {
                    url,
                    body: ResourceBody::Text(source),
                    ..
                }) = result
                    && self.css_bytes + source.len() <= STYLESHEET_BYTES
                {
                    self.css_bytes += source.len();
                    if css::stylesheet_media_applies(&media, (viewport.width, viewport.height)) {
                        self.fonts.register(
                            node,
                            &source,
                            Arc::new(url),
                            (viewport.width, viewport.height),
                            &self.client,
                            &mut self.resources,
                        );
                    }
                    page.linked_stylesheets.insert(node, Arc::from(source));
                    accepted = true;
                    self.resources_dirty = true;
                }
                if admitted_request && current {
                    events.push(node);
                } else if admitted_request {
                    self.sheet_sources.remove(&node);
                }
            }
            Consumer::Image(key) => {
                if let Some(ImageState::Pending(waiters)) = self.images.remove(&key) {
                    events = waiters;
                }
                if let Some(realm) = runtime {
                    let state = realm.state.borrow();
                    events.retain(|node| {
                        !state.image_updates.contains(node)
                            && super::canvas::image_request_key(&state, *node).as_deref()
                                == Some(key.as_ref())
                    });
                }
                let memory = DECODED_IMAGE_BYTES.saturating_sub(self.decoded_images);
                let mut origin_clean = false;
                if let Ok(DocumentResource {
                    body,
                    origin_clean: clean,
                    ..
                }) = result
                {
                    origin_clean = clean;
                    match body {
                        ResourceBody::Image(image) if image.as_raw().len() <= memory => {
                            self.decoded_images += image.as_raw().len();
                            page.images.insert(key.to_string(), image);
                            accepted = true;
                        }
                        ResourceBody::Svg(svg) if svg.retained_bytes() <= memory => {
                            self.decoded_images += svg.retained_bytes();
                            page.svg_images.insert(key.to_string(), svg);
                            accepted = true;
                        }
                        _ => {}
                    }
                }
                let availability = if accepted {
                    crate::loader::ImageAvailability::decoded(origin_clean)
                } else {
                    crate::loader::ImageAvailability::Failed
                };
                Arc::make_mut(&mut page.image_availability).insert(key.to_string(), availability);
                if !accepted {
                    self.images.insert(key, ImageState::Failed);
                }
                self.resources_dirty = true;
            }
            Consumer::Font(key) => {
                if let Ok(DocumentResource {
                    body: ResourceBody::Font(face),
                    ..
                }) = result
                {
                    accepted = self
                        .fonts
                        .admit(&key, face, &self.client, &mut self.resources);
                } else {
                    self.fonts.fail(&key, &self.client, &mut self.resources);
                }
                self.sync_fonts(page)?;
            }
            Consumer::Media(key) => {
                let media = result.and_then(|asset| match asset.body {
                    ResourceBody::Media(bytes) => Ok((asset.url.to_string(), bytes)),
                    _ => Err("Unexpected media resource payload".into()),
                });
                let downloaded = media.is_ok();
                if let Some(realm) = runtime {
                    accepted = realm.accept_media_bytes(key, media) && downloaded;
                }
            }
        }
        if let Some(bytes) = transfer.filter(|_| accepted) {
            page.stats.bytes += bytes;
            page.stats.resources += 1;
        } else {
            page.stats.skipped_resources += 1;
        }
        self.update_resources(page, runtime);
        self.deliver_events(
            page,
            runtime,
            viewport,
            events.into_iter().map(|node| (node, accepted)).collect(),
        )
    }

    fn deliver_events(
        &mut self,
        page: &LoadedPage,
        runtime: &mut Option<ScriptRuntime>,
        viewport: Viewport,
        events: Vec<(NodeId, bool)>,
    ) -> Result<(), String> {
        if !events.is_empty() && (runtime.is_some() || self.wants_realm(page)) {
            self.ensure_realm(page, runtime, viewport)?;
            let realm = runtime.as_mut().unwrap();
            for (node, accepted) in events {
                realm.lifecycle_event(Some(node), if accepted { "load" } else { "error" }, false);
            }
            self.dirty = true;
        }
        Ok(())
    }
}

pub(super) fn install_parser_methods(
    vm: &mut Vm,
    state: &Shared,
    document: Value,
    errors: &dom_api::Errors,
) -> Result<(), JsError> {
    parser_host::install(vm, state, document, errors)
}
