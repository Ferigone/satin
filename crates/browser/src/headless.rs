use crate::jobs::{Job, Message, Payload, Task, Workers};
use rust_browser_engine::{
    LoadedPage,
    cookies::{CookieContext, CookieStore},
    history::{HistoryScroll, NavigationMode, ScriptNavigation, SessionHistory, fragment_offset},
    layout::{ScrollOffset, Viewport},
    loader::{NavigationPart, NavigationRequest, OpenedPage},
    network::{CancelSignal, FetchError, FetchErrorKind},
    script::{DocumentSession, ScriptRuntime},
    web_storage::{StorageProfile, TabStorage},
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub(crate) struct Network {
    cookies: CookieStore,
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    workers: Option<Workers>,
    document_session: Option<DocumentSession>,
    history: SessionHistory,
    storage: TabStorage,
    scroll: Option<HistoryScroll>,
    generation: u64,
    cancel: CancelSignal,
}
impl Network {
    fn new(cookies: CookieStore, address: url::Url, storage: StorageProfile) -> Self {
        let (tx, rx) = mpsc::channel(16);
        Self {
            cookies,
            tx,
            rx,
            workers: None,
            document_session: None,
            history: SessionHistory::new(address),
            storage: TabStorage::new(storage),
            scroll: None,
            generation: 0,
            cancel: CancelSignal::new(),
        }
    }
    fn job(&self, task: Task) -> Job {
        Job {
            tab: 1,
            navigation: self.generation,
            task,
            cancel: self.cancel.clone(),
        }
    }
    fn begin(
        &mut self,
        request: NavigationRequest,
    ) -> Result<Option<(LoadedPage, DocumentSession)>, String> {
        self.cancel.cancel();
        self.cancel = CancelSignal::new();
        self.generation = self.generation.wrapping_add(1);
        if request.url.scheme() == "about" {
            let opened = OpenedPage::internal(&request.url, self.cookies.clone());
            return DocumentSession::start_in_tab(
                opened,
                self.history.clone(),
                self.storage.clone(),
            )
            .map(Some);
        }
        let job = self.job(Task::Navigate(Box::new(request)));
        self.workers()?.submit(job)?;
        Ok(None)
    }

    fn next_navigation(
        &mut self,
        runtime: &mut ScriptRuntime,
    ) -> Result<Option<NavigationRequest>, String> {
        let command = runtime.take_navigation_request();
        let viewport = runtime.viewport();
        let scroll = ScrollOffset {
            x: viewport.scroll_x,
            y: viewport.scroll_y,
        };
        let request = match command {
            Some(ScriptNavigation::Traverse(delta)) => {
                let Some(transition) = self.history.traverse(delta, scroll) else {
                    return Ok(None);
                };
                if transition.same_document {
                    runtime
                        .apply_history_transition(transition)
                        .map_err(|error| error.to_string())?;
                    return Ok(None);
                }
                self.scroll = Some(transition.scroll);
                NavigationRequest::get(transition.url.as_str())?
            }
            Some(ScriptNavigation::Reload) => NavigationRequest::get(self.history.url().as_str())?,
            Some(ScriptNavigation::Navigate { url, mode }) => {
                let initiator = self.history.url();
                self.history.visit(url.clone(), mode, scroll);
                self.scroll = url.fragment().map(|_| HistoryScroll::Fragment);
                NavigationRequest::from_document(url, initiator)
            }
            None => {
                let Some(submission) = runtime.take_form_submissions().pop() else {
                    return Ok(None);
                };
                let initiator = self.history.url();
                let url = url::Url::parse(&submission.url).map_err(|error| error.to_string())?;
                self.history
                    .visit(url.clone(), NavigationMode::Push, scroll);
                self.scroll = url.fragment().map(|_| HistoryScroll::Fragment);
                NavigationRequest {
                    context: CookieContext::navigation(Some(&initiator), &url, &submission.method),
                    url,
                    method: submission.method,
                    body: submission.body,
                    content_type: submission.content_type,
                    initiator: Some(initiator),
                }
            }
        };
        Ok(Some(request))
    }

    fn scroll_document(&mut self, runtime: &ScriptRuntime, loaded: bool) -> Result<(), String> {
        let action = runtime
            .take_history_scroll()
            .or_else(|| if loaded { self.scroll.take() } else { None });
        let Some(action) = action else { return Ok(()) };
        let mut viewport = runtime.viewport();
        match action {
            HistoryScroll::Keep => return Ok(()),
            HistoryScroll::Restore(offset) => {
                viewport.scroll_x = offset.x;
                viewport.scroll_y = offset.y;
            }
            HistoryScroll::Fragment => {
                let layout = runtime
                    .layout_for_viewport(viewport)
                    .map_err(|error| error.to_string())?;
                if let Some(offset) = fragment_offset(self.history.url().as_str(), &layout) {
                    viewport.scroll_x = 0.;
                    viewport.scroll_y = offset.min((layout.height - viewport.height).max(0.));
                }
            }
        }
        runtime.set_viewport(viewport);
        Ok(())
    }
    fn workers(&mut self) -> Result<&Workers, String> {
        if self.workers.is_none() {
            self.workers = Some(Workers::new(self.cookies.clone(), self.tx.clone(), || {})?);
        }
        Ok(self.workers.as_ref().unwrap())
    }
    fn receive_until(&mut self, deadline: Instant) -> Option<Message> {
        if let Some(workers) = &self.workers {
            workers.receive_until(&mut self.rx, deadline)
        } else {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            None
        }
    }
    fn dispatch_fetch(&mut self, realm: &mut ScriptRuntime) -> Result<(), String> {
        for request in realm.take_network_jobs() {
            let id = request.id;
            let job = self.job(Task::Fetch(Box::new(request)));
            if let Err(error) = self.workers()?.submit(job) {
                realm
                    .complete_fetch(id, Err(FetchError::new(FetchErrorKind::Network, error)))
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }
    fn dispatch_resources(
        &mut self,
        page: &mut LoadedPage,
        session: &mut DocumentSession,
        realm: &mut Option<ScriptRuntime>,
        viewport: Viewport,
    ) -> Result<(), String> {
        use std::sync::mpsc::TrySendError;
        while let Some(load) = session.next_resource() {
            let job = self.job(Task::Resource(Box::new(load)));
            match self.workers()?.try_submit(job) {
                Ok(()) => {}
                Err(TrySendError::Full(Job {
                    task: Task::Resource(load),
                    ..
                })) => {
                    session.retry_resource(*load);
                    break;
                }
                Err(TrySendError::Disconnected(Job {
                    task: Task::Resource(load),
                    ..
                })) => {
                    session.complete_resource(
                        load.id,
                        Err("Resource worker disconnected".into()),
                        page,
                        realm,
                        viewport,
                    )?;
                }
                _ => unreachable!("the submitted job loads a document resource"),
            }
        }
        Ok(())
    }
}
impl Drop for Network {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(crate) fn load(
    url: &str,
    cookies: CookieStore,
    viewport: Viewport,
    storage: StorageProfile,
) -> Result<(LoadedPage, Option<ScriptRuntime>, Network), String> {
    let request = NavigationRequest::get(url)?;
    let mut network = Network::new(cookies, request.url.clone(), storage);
    network.scroll = request.url.fragment().map(|_| HistoryScroll::Fragment);
    let hard_end = Instant::now() + Duration::from_secs(42);
    let (page, realm, session) = load_document(&mut network, request, viewport, hard_end)?;
    network.document_session = Some(session);
    Ok((page, realm, network))
}

fn load_document(
    network: &mut Network,
    request: NavigationRequest,
    viewport: Viewport,
    hard_end: Instant,
) -> Result<(LoadedPage, Option<ScriptRuntime>, DocumentSession), String> {
    let mut document = network.begin(request)?;
    let mut realm: Option<ScriptRuntime> = None;
    loop {
        let now = Instant::now();
        if now >= hard_end {
            return Err("Document load timed out".into());
        }
        let mut wake = hard_end;
        if let Some((page, session)) = &mut document {
            if session.needs_advance(realm.as_ref()) {
                session.advance(page, &mut realm, viewport)?;
            }
            network.dispatch_resources(page, session, &mut realm, viewport)?;
            if let Some(runtime) = &mut realm {
                if let Some(request) = network.next_navigation(runtime)? {
                    document = network.begin(request)?;
                    realm = None;
                    continue;
                }
                network.scroll_document(runtime, session.loaded())?;
                if runtime
                    .next_deadline()
                    .is_some_and(|next| next <= Instant::now())
                {
                    runtime.pump();
                }
                network.dispatch_fetch(runtime)?;
                if let Some(next) = runtime.next_deadline() {
                    wake = wake.min(next);
                }
            }
            if session.done()
                && session.next_deadline().is_none()
                && !session.needs_advance(realm.as_ref())
                && !realm
                    .as_ref()
                    .is_some_and(ScriptRuntime::has_navigation_tasks)
            {
                let (mut page, session) = document.take().unwrap();
                if let Some(runtime) = &realm {
                    runtime.snapshot(&mut page);
                }
                return Ok((page, realm, session));
            }
            if let Some(next) = session.next_deadline() {
                wake = wake.min(next);
            }
        }
        let Some(message) = network.receive_until(wake) else {
            continue;
        };
        if message.navigation != network.generation {
            continue;
        }
        match message.payload {
            Payload::Navigation(NavigationPart::Open(opened)) => {
                document = Some(DocumentSession::start_in_tab(
                    *opened,
                    network.history.clone(),
                    network.storage.clone(),
                )?)
            }
            Payload::Navigation(NavigationPart::Text(text, bytes)) => {
                let (page, session) = document
                    .as_mut()
                    .ok_or("Received input before navigation opened")?;
                page.stats.bytes += bytes;
                session.feed(&text)?;
            }
            Payload::Navigation(NavigationPart::Finish) => {
                document
                    .as_mut()
                    .ok_or("Received EOF before navigation opened")?
                    .1
                    .finish_input()?;
            }
            Payload::NavigationError(error) => return Err(error),
            Payload::Resource { id, result } => {
                let (page, session) = document
                    .as_mut()
                    .ok_or("Received resource before navigation opened")?;
                session.complete_resource(id, result, page, &mut realm, viewport)?;
            }
            Payload::Fetch { id, result } => {
                if let Some(runtime) = &mut realm {
                    runtime
                        .complete_fetch(id, result)
                        .map_err(|error| error.to_string())?;
                }
            }
            Payload::FetchBodyProgress(id) => {
                if let Some(runtime) = &mut realm {
                    runtime
                        .progress_network(id)
                        .map_err(|error| error.to_string())?;
                }
            }
            Payload::FetchUploadDemand(id) => {
                if let Some(runtime) = &mut realm {
                    runtime
                        .progress_network_upload(id)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
    }
}

pub(crate) fn run_tasks(
    network: &mut Network,
    runtime: &mut Option<ScriptRuntime>,
    page: &mut LoadedPage,
    duration: Option<u64>,
    file_picker_responses: &mut Option<crate::file_picker::Responses>,
) -> Result<(), String> {
    let mut viewport = runtime
        .as_ref()
        .map_or_else(Viewport::default, ScriptRuntime::viewport);
    let mut session = network
        .document_session
        .take()
        .ok_or("No loaded document")?;
    let started = Instant::now();
    let hard_end = started + Duration::from_millis(duration.unwrap_or(5000).min(20000));
    let timer_end = started + Duration::from_millis(duration.unwrap_or(0).min(20000));
    let mut received = None;
    loop {
        for _ in 0..16 {
            let Some(message) = received.take().or_else(|| network.rx.try_recv().ok()) else {
                break;
            };
            if message.navigation != network.generation {
                continue;
            }
            match message.payload {
                Payload::Resource { id, result } => {
                    session.complete_resource(id, result, page, runtime, viewport)?;
                }
                Payload::Fetch { id, result } => {
                    if let Some(realm) = runtime {
                        realm
                            .complete_fetch(id, result)
                            .map_err(|error| error.to_string())?;
                    }
                }
                Payload::FetchBodyProgress(id) => {
                    if let Some(realm) = runtime {
                        realm
                            .progress_network(id)
                            .map_err(|error| error.to_string())?;
                    }
                }
                Payload::FetchUploadDemand(id) => {
                    if let Some(realm) = runtime {
                        realm
                            .progress_network_upload(id)
                            .map_err(|error| error.to_string())?;
                    }
                }
                _ => {}
            }
        }
        if let Some(realm) = runtime {
            if let Some(request) = network.next_navigation(realm)? {
                let navigation_viewport = Viewport {
                    scroll_x: 0.,
                    scroll_y: 0.,
                    ..viewport
                };
                let (next_page, next_runtime, next_session) =
                    load_document(network, request, navigation_viewport, hard_end)?;
                *page = next_page;
                *runtime = next_runtime;
                session = next_session;
                viewport = navigation_viewport;
                continue;
            }
            network.scroll_document(realm, session.loaded())?;
            if realm
                .next_deadline()
                .is_some_and(|next| next <= Instant::now())
            {
                realm.pump();
            }
            if crate::file_picker::answer_headless(realm, file_picker_responses)? {
                continue;
            }
            network.dispatch_fetch(realm)?;
        }
        if session.needs_advance(runtime.as_ref()) {
            session.advance(page, runtime, viewport)?;
        }
        network.dispatch_resources(page, &mut session, runtime, viewport)?;
        let now = Instant::now();
        if now >= hard_end {
            break;
        }
        if session.needs_advance(runtime.as_ref())
            || runtime
                .as_ref()
                .is_some_and(ScriptRuntime::has_navigation_tasks)
        {
            continue;
        }
        let next = runtime.as_ref().and_then(ScriptRuntime::next_deadline);
        let network_pending = runtime
            .as_ref()
            .is_some_and(|realm| realm.pending_network() > 0);
        let observations_pending = runtime
            .as_ref()
            .is_some_and(ScriptRuntime::has_pending_intersection_observations);
        let wake = if network_pending || !session.done() || observations_pending {
            next.into_iter()
                .chain(session.next_deadline())
                .chain(Some(hard_end))
                .min()
                .unwrap()
        } else if let Some(next) = next.filter(|next| *next <= timer_end) {
            next
        } else {
            break;
        };
        received = network.receive_until(wake);
    }
    if let Some(realm) = runtime {
        realm.snapshot(page);
    }
    network.document_session = Some(session);
    Ok(())
}
