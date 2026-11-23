use crate::jobs::{Job, Message, Payload, Task, Workers};
use crate::profile::{Profile, Session};
use rust_browser_engine::{
    Fonts, Layout, LoadedPage, Rect,
    cookies::CookieContext,
    css::Color,
    history::{
        HistoryScroll, HistoryTransition, NavigationMode, ScriptNavigation, SessionHistory,
        fragment_offset,
    },
    layout::{ScrollOffset, Viewport},
    loader::{DocumentResource, NavigationPart, NavigationRequest, OpenedPage},
    network::CancelSignal,
    normalize_url,
    paint::Canvas,
    script::{DocumentSession, FormKey, ScriptRuntime},
};
use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroU32,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};
use tokio::sync::mpsc::{self, Receiver};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Icon, Window, WindowId},
};

const TAB_H: f32 = 36.;
const TOP: f32 = 90.;
const FOOT: f32 = 25.;

fn page_viewport(window: Option<&Window>, scale: f32, zoom: f32, x: f32, y: f32) -> Viewport {
    let (width, height) = window.map_or((1280., 685.), |w| {
        (
            w.inner_size().width as f32 / scale,
            w.inner_size().height as f32 / scale - TOP - FOOT,
        )
    });
    Viewport {
        width: (width / zoom).max(120.),
        height: (height / zoom).max(1.),
        scroll_x: x,
        scroll_y: y,
    }
}

#[derive(Clone)]
enum Action {
    Address,
    Back,
    Forward,
    Reload,
    Home,
    NewTab,
    Select(usize),
    Close(usize),
    Version,
}
struct Tab {
    id: u64,
    url: String,
    title: String,
    page: Option<Arc<LoadedPage>>,
    runtime: Option<ScriptRuntime>,
    document_session: Option<DocumentSession>,
    layout: Option<Arc<Layout>>,
    history: SessionHistory,
    storage: rust_browser_engine::web_storage::TabStorage,
    history_scroll: Option<HistoryScroll>,
    scroll: f32,
    scroll_x: f32,
    nested_scroll: HashMap<rust_browser_engine::dom::NodeId, ScrollOffset>,
    zoom: f32,
    loading: bool,
    navigation: u64,
    cancel: CancelSignal,
}
impl Tab {
    fn new(
        id: u64,
        url: String,
        storage: rust_browser_engine::web_storage::StorageProfile,
    ) -> Self {
        let address =
            url::Url::parse(&url).unwrap_or_else(|_| url::Url::parse("about:home").unwrap());
        Self {
            id,
            title: "Nowa karta".into(),
            url: address.as_str().into(),
            page: None,
            runtime: None,
            document_session: None,
            layout: None,
            history: SessionHistory::new(address),
            storage: rust_browser_engine::web_storage::TabStorage::new(storage),
            history_scroll: None,
            scroll: 0.,
            scroll_x: 0.,
            nested_scroll: HashMap::new(),
            zoom: 1.,
            loading: false,
            navigation: 0,
            cancel: CancelSignal::new(),
        }
    }
}
struct Graphics {
    _context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}
#[derive(Clone, Copy)]
struct SelectPopup {
    node: rust_browser_engine::dom::NodeId,
    rect: Rect,
    row_height: f32,
    first: usize,
}

pub fn run(
    path: PathBuf,
    url: Option<String>,
    smoke_output: Option<PathBuf>,
    smoke_click: Option<String>,
    smoke_actions: Option<PathBuf>,
    file_picker_responses: Option<crate::file_picker::Responses>,
) -> Result<(), String> {
    let (profile, session) = Profile::open(path)?;
    let fonts = Fonts::new()?;
    let events = EventLoop::<()>::with_user_event()
        .build()
        .map_err(|e| e.to_string())?;
    events.set_control_flow(ControlFlow::Wait);
    let proxy = events.create_proxy();
    let (results, incoming) = mpsc::channel(16);
    let jobs = Workers::new(profile.cookies(), results, move || {
        let _ = proxy.send_event(());
    })?;
    let mut tabs = session
        .tabs
        .into_iter()
        .take(16)
        .enumerate()
        .map(|(i, u)| Tab::new(i as u64 + 1, u, profile.storage()))
        .collect::<Vec<_>>();
    if tabs.is_empty() {
        tabs.push(Tab::new(1, "about:home".into(), profile.storage()));
    }
    let active = session.active.min(tabs.len() - 1);
    if let Some(url) = url {
        tabs[active].url = normalize_url(&url)?;
        tabs[active].history =
            SessionHistory::new(url::Url::parse(&tabs[active].url).map_err(|e| e.to_string())?);
    }
    let next_id = tabs.len() as u64 + 1;
    let smoke_requested = smoke_output.is_some();
    let mut app = App {
        window: None,
        graphics: None,
        canvas: None,
        fonts,
        profile,
        tabs,
        active,
        next_id,
        jobs,
        incoming,
        mouse: (0., 0.),
        modifiers: ModifiersState::empty(),
        address: String::new(),
        cursor: 0,
        address_focus: false,
        select_all: false,
        preedit: false,
        select_popup: None,
        hits: vec![],
        hover: None,
        status: String::new(),
        scale: 1.,
        clipboard: arboard::Clipboard::new().ok(),
        smoke_output,
        smoke_click,
        smoke_actions: smoke_actions
            .as_deref()
            .map(crate::testing::read_actions)
            .transpose()?
            .unwrap_or_default(),
        file_picker_responses,
        smoke_result: None,
        presented_frames: 0,
        last_present_error: None,
    };
    events.run_app(&mut app).map_err(|e| e.to_string())?;
    if smoke_requested {
        app.smoke_result
            .take()
            .ok_or("Window check did not finish")??;
        println!(
            "{}",
            serde_json::json!({"window_smoke":true,"version":env!("CARGO_PKG_VERSION"),"frames_presented":app.presented_frames,"url":app.tabs[app.active].url,"title":app.tabs[app.active].title,"layout_limited":app.tabs[app.active].layout.as_ref().is_some_and(|l|l.limited),"paint_limited":app.canvas.as_ref().is_some_and(|c|c.limited),"javascript":{"diagnostics":app.tabs[app.active].runtime.as_ref().map(ScriptRuntime::diagnostics).unwrap_or_default()}})
        );
    }
    Ok(())
}

struct App {
    smoke_output: Option<PathBuf>,
    smoke_click: Option<String>,
    smoke_actions: VecDeque<crate::testing::SmokeAction>,
    file_picker_responses: Option<crate::file_picker::Responses>,
    smoke_result: Option<Result<(), String>>,
    presented_frames: u64,
    last_present_error: Option<String>,
    window: Option<Rc<Window>>,
    graphics: Option<Graphics>,
    canvas: Option<Canvas>,
    fonts: Fonts,
    profile: Profile,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    jobs: Workers,
    incoming: Receiver<Message>,
    mouse: (f32, f32),
    modifiers: ModifiersState,
    address: String,
    cursor: usize,
    address_focus: bool,
    select_all: bool,
    preedit: bool,
    select_popup: Option<SelectPopup>,
    hits: Vec<(Rect, Action)>,
    hover: Option<String>,
    status: String,
    scale: f32,
    clipboard: Option<arboard::Clipboard>,
}
impl App {
    fn tab_viewport(&self, index: usize) -> Viewport {
        let tab = &self.tabs[index];
        page_viewport(
            self.window.as_deref(),
            self.scale,
            tab.zoom,
            tab.scroll_x,
            tab.scroll,
        )
    }

    fn start_page(&mut self, index: usize, opened: OpenedPage) {
        let history = self.tabs[index].history.clone();
        let storage = self.tabs[index].storage.clone();
        match DocumentSession::start_in_tab(opened, history, storage) {
            Ok((page, session)) => {
                let tab = &mut self.tabs[index];
                tab.url = page.url.clone();
                tab.title = page.title();
                tab.page = Some(Arc::new(page));
                tab.runtime = None;
                tab.document_session = Some(session);
                tab.layout = None;
                tab.loading = true;
            }
            Err(error) => self.fail_page(index, &error),
        }
    }

    fn fail_page(&mut self, index: usize, error: &str) {
        let tab = &mut self.tabs[index];
        tab.cancel.cancel();
        tab.navigation = tab.navigation.wrapping_add(1);
        let page = LoadedPage::error(&tab.url, error);
        tab.title = page.title();
        tab.page = Some(Arc::new(page));
        tab.layout = None;
        tab.runtime = None;
        tab.document_session = None;
        tab.loading = false;
    }

    fn advance_page(&mut self, index: usize) -> bool {
        let viewport = self.tab_viewport(index);
        let tab = &mut self.tabs[index];
        let (Some(session), Some(page)) = (&mut tab.document_session, &mut tab.page) else {
            return false;
        };
        if !session.needs_advance(tab.runtime.as_ref()) {
            return false;
        }
        match session.advance(Arc::make_mut(page), &mut tab.runtime, viewport) {
            Ok(changed) => {
                let was_loading = tab.loading;
                tab.loading = !session.loaded();
                if changed {
                    tab.title = page.title();
                    tab.layout = None;
                }
                changed || was_loading != tab.loading
            }
            Err(error) => {
                self.fail_page(index, &error);
                true
            }
        }
    }

    fn deliver_resource(
        &mut self,
        index: usize,
        id: u64,
        result: Result<DocumentResource, String>,
    ) {
        let viewport = self.tab_viewport(index);
        let tab = &mut self.tabs[index];
        if let (Some(session), Some(page)) = (&mut tab.document_session, &mut tab.page)
            && let Err(error) = session.complete_resource(
                id,
                result,
                Arc::make_mut(page),
                &mut tab.runtime,
                viewport,
            )
        {
            self.fail_page(index, &error);
        }
    }

    fn dispatch_resources(&mut self, index: usize) {
        use std::sync::mpsc::TrySendError;
        while let Some(load) = self.tabs[index]
            .document_session
            .as_mut()
            .and_then(DocumentSession::next_resource)
        {
            let tab = &self.tabs[index];
            let job = Job {
                tab: tab.id,
                navigation: tab.navigation,
                cancel: tab.cancel.clone(),
                task: Task::Resource(Box::new(load)),
            };
            match self.jobs.try_submit(job) {
                Ok(()) => {}
                Err(TrySendError::Full(Job {
                    task: Task::Resource(load),
                    ..
                })) => {
                    self.tabs[index]
                        .document_session
                        .as_mut()
                        .unwrap()
                        .retry_resource(*load);
                    break;
                }
                Err(TrySendError::Disconnected(Job {
                    task: Task::Resource(load),
                    ..
                })) => {
                    self.deliver_resource(
                        index,
                        load.id,
                        Err("Resource worker disconnected".into()),
                    );
                }
                _ => unreachable!("the submitted job loads a document resource"),
            }
        }
    }

    fn sync_runtime(&mut self, index: usize) -> bool {
        let tab = &mut self.tabs[index];
        let mut changed = tab
            .runtime
            .as_ref()
            .is_some_and(ScriptRuntime::publish_canvas_frames);
        if let (Some(runtime), Some(page)) = (&tab.runtime, &mut tab.page)
            && runtime.snapshot(Arc::make_mut(page))
        {
            tab.title = page.title();
            tab.layout = None;
            changed = true;
        }
        if let Some(scroll) = tab
            .runtime
            .as_ref()
            .and_then(ScriptRuntime::take_history_scroll)
        {
            tab.history_scroll = Some(scroll);
            changed = true;
        }
        if tab.document_session.is_some()
            && let Some(page) = &tab.page
            && tab.url != page.url
        {
            tab.url.clone_from(&page.url);
            if index == self.active && !self.address_focus {
                self.sync_address();
            }
            self.save();
            changed = true;
        }
        changed
    }
    fn dispatch_fetches(&mut self, index: usize) {
        let tab = &self.tabs[index];
        let requests = tab
            .runtime
            .as_ref()
            .map(ScriptRuntime::take_network_jobs)
            .unwrap_or_default();
        let id = tab.id;
        let navigation = tab.navigation;
        let cancel = tab.cancel.clone();
        let had_requests = !requests.is_empty();
        for request in requests {
            let request_id = request.id;
            if let Err(error) = self.jobs.submit(Job {
                tab: id,
                navigation,
                task: Task::Fetch(Box::new(request)),
                cancel: cancel.clone(),
            }) && let Some(runtime) = &mut self.tabs[index].runtime
            {
                let _ = runtime.complete_fetch(
                    request_id,
                    Err(rust_browser_engine::network::FetchError::new(
                        rust_browser_engine::network::FetchErrorKind::Network,
                        error,
                    )),
                );
            }
        }
        if had_requests && self.sync_runtime(index) && index == self.active {
            self.ensure_layout();
            self.set_title();
            self.redraw();
        }
    }
    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    fn save(&self) {
        let session = Session {
            schema: 1,
            tabs: self.tabs.iter().map(|t| t.url.clone()).collect(),
            active: self.active,
        };
        let _ = self.profile.save(&session);
    }
    fn sync_address(&mut self) {
        self.address = self.tabs[self.active].url.clone();
        self.cursor = self.address.len();
    }
    fn set_title(&self) {
        if let Some(w) = &self.window {
            w.set_title(&format!(
                "{} — Rust Browser · Alfa 0.1",
                self.tabs[self.active].title
            ));
        }
    }
    fn ensure_layout(&mut self) {
        let Some(w) = &self.window else {
            return;
        };
        let tab = &mut self.tabs[self.active];
        match tab.history_scroll {
            Some(HistoryScroll::Restore(offset)) if !tab.loading => {
                tab.scroll = offset.y;
                tab.scroll_x = offset.x;
                tab.history_scroll = None;
            }
            Some(HistoryScroll::Keep) => tab.history_scroll = None,
            _ => {}
        }
        let width = (w.inner_size().width as f32 / (self.scale * tab.zoom)).max(120.);
        let height = (w.inner_size().height as f32 - TOP * self.scale - FOOT * self.scale)
            / (self.scale * tab.zoom);
        if let Some(runtime) = &tab.runtime
            && let Some(request) = runtime.take_scroll_request()
        {
            tab.scroll = request.y;
            tab.scroll_x = request.x;
        }
        let mut viewport = Viewport {
            width,
            height: height.max(1.),
            scroll_x: tab.scroll_x,
            scroll_y: tab.scroll,
        };
        if let Some(runtime) = &tab.runtime {
            match runtime.layout_for_viewport(viewport) {
                Ok(layout) => tab.layout = Some(layout),
                Err(error) => self.status = error.to_string(),
            }
        } else if tab.layout.as_ref().is_none_or(|l| {
            (l.width - width).abs() > 0.5
                || (l.viewport.height - height).abs() > 0.5
                || (l.viewport.scroll_x - tab.scroll_x).abs() > 0.1
                || (l.depends_on_viewport_scroll && (l.viewport.scroll_y - tab.scroll).abs() > 0.1)
        }) && let Some(page) = &tab.page
        {
            tab.layout = Some(Arc::new(Layout::build_with_scroll(
                page,
                &self.fonts,
                viewport,
                &tab.nested_scroll,
            )));
        }
        if matches!(tab.history_scroll, Some(HistoryScroll::Fragment))
            && tab.document_session.is_some()
        {
            if let Some(y) = tab
                .layout
                .as_deref()
                .and_then(|layout| fragment_offset(&tab.url, layout))
            {
                tab.scroll = y;
                tab.scroll_x = 0.;
                tab.history_scroll = None;
            } else if !tab.loading {
                tab.history_scroll = None;
            }
        }
        tab.scroll = tab.scroll.clamp(
            0.,
            tab.layout
                .as_ref()
                .map_or(0., |l| (l.height - height).max(0.)),
        );
        tab.scroll_x = tab.scroll_x.clamp(
            0.,
            tab.layout
                .as_ref()
                .map_or(0., |layout| (layout.content_width - width).max(0.)),
        );
        viewport.scroll_x = tab.scroll_x;
        viewport.scroll_y = tab.scroll;
        if let Some(runtime) = &tab.runtime {
            runtime.set_viewport(viewport);
        }
        // Clamping after a shrink can change viewport-dependent positioning.
        if tab.layout.as_ref().is_some_and(|l| {
            (l.viewport.scroll_x - tab.scroll_x).abs() > 0.1
                || (l.depends_on_viewport_scroll && (l.viewport.scroll_y - tab.scroll).abs() > 0.1)
        }) {
            if let Some(runtime) = &tab.runtime {
                if let Ok(layout) = runtime.layout_for_viewport(viewport) {
                    tab.layout = Some(layout);
                }
            } else if let Some(page) = &tab.page {
                tab.layout = Some(Arc::new(Layout::build_with_scroll(
                    page,
                    &self.fonts,
                    viewport,
                    &tab.nested_scroll,
                )));
            }
        }
    }
    fn navigate(&mut self, input: String, push: bool) {
        self.navigate_with_request(input, push, None);
    }
    fn navigate_with_request(
        &mut self,
        input: String,
        push: bool,
        request: Option<NavigationRequest>,
    ) {
        let mode = if push {
            NavigationMode::Push
        } else {
            NavigationMode::Reload
        };
        self.navigate_tab(self.active, input, mode, request, None);
    }
    fn navigate_tab(
        &mut self,
        index: usize,
        input: String,
        mode: NavigationMode,
        request: Option<NavigationRequest>,
        restore: Option<HistoryScroll>,
    ) {
        if index == self.active {
            self.select_popup = None;
        }
        let url = match normalize_url(&input) {
            Ok(u) => u,
            Err(e) => {
                self.status = e;
                self.redraw();
                return;
            }
        };
        let parsed = url::Url::parse(&url).expect("normalize_url returns a serialized URL");
        let tab = &self.tabs[index];
        let scroll = ScrollOffset {
            x: tab.scroll_x,
            y: tab.scroll,
        };
        if tab.document_session.is_some()
            && request
                .as_ref()
                .is_none_or(|request| request.method == "GET")
            && let Some(transition) = tab.history.fragment(parsed.clone(), mode, scroll)
        {
            self.apply_history_transition(index, transition);
            return;
        }
        let tab = &mut self.tabs[index];
        let mode = if mode == NavigationMode::Push && tab.history.url() == parsed {
            NavigationMode::Reload
        } else {
            mode
        };
        tab.history.visit(parsed.clone(), mode, scroll);
        tab.history_scroll = restore.or_else(|| parsed.fragment().map(|_| HistoryScroll::Fragment));
        tab.cancel.cancel();
        tab.cancel = CancelSignal::new();
        tab.runtime = None;
        tab.document_session = None;
        tab.navigation += 1;
        tab.loading = true;
        tab.scroll = 0.;
        tab.scroll_x = 0.;
        tab.nested_scroll.clear();
        tab.url = url.clone();
        if index == self.active {
            self.address_focus = false;
            self.select_all = false;
            self.status.clear();
        }
        if url.starts_with("about:") {
            self.start_page(index, OpenedPage::internal(&parsed, self.profile.cookies()));
            self.advance_page(index);
        } else {
            let job = Job {
                tab: tab.id,
                navigation: tab.navigation,
                task: Task::Navigate(Box::new(
                    request.unwrap_or_else(|| NavigationRequest::get(&url).unwrap()),
                )),
                cancel: tab.cancel.clone(),
            };
            if self.jobs.submit(job).is_err() {
                tab.loading = false;
                self.status =
                    "Wczytywanie innych stron jeszcze trwa. Spróbuj ponownie za chwilę.".into();
            }
        }
        if index == self.active {
            self.sync_address();
            self.ensure_layout();
            self.set_title();
            self.redraw();
        }
        self.save();
    }
    fn apply_history_transition(&mut self, index: usize, transition: HistoryTransition) {
        let tab = &mut self.tabs[index];
        if let Some(runtime) = &mut tab.runtime {
            if let Err(error) = runtime.apply_history_transition(transition) {
                self.status = error.to_string();
            }
            self.sync_runtime(index);
        } else if let Some(page) = &mut tab.page {
            tab.url = transition.url.as_str().into();
            Arc::make_mut(page).url.clone_from(&tab.url);
            tab.history_scroll = Some(transition.scroll);
        }
        if index == self.active {
            self.address_focus = false;
            self.sync_address();
            self.ensure_layout();
            self.set_title();
            self.redraw();
        }
        self.save();
    }

    fn traverse_history(&mut self, index: usize, delta: i32) {
        let tab = &self.tabs[index];
        let scroll = ScrollOffset {
            x: tab.scroll_x,
            y: tab.scroll,
        };
        if let Some(transition) = tab.history.traverse(delta, scroll) {
            if transition.same_document && tab.document_session.is_some() {
                self.apply_history_transition(index, transition);
            } else {
                self.navigate_tab(
                    index,
                    transition.url.into(),
                    NavigationMode::Reload,
                    None,
                    Some(transition.scroll),
                );
            }
        }
    }

    fn dispatch_history(&mut self, index: usize) {
        let tab = &self.tabs[index];
        match tab
            .runtime
            .as_ref()
            .and_then(ScriptRuntime::take_navigation_request)
        {
            Some(ScriptNavigation::Traverse(delta)) => self.traverse_history(index, delta),
            Some(ScriptNavigation::Reload) => {
                self.navigate_tab(index, tab.url.clone(), NavigationMode::Reload, None, None)
            }
            Some(ScriptNavigation::Navigate { url, mode }) => {
                let address = url.as_str().to_owned();
                let request = NavigationRequest::from_document(url, tab.history.url());
                self.navigate_tab(index, address, mode, Some(request), None);
            }
            None => {}
        }
    }
    fn activate(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        self.address_focus = false;
        self.sync_address();
        if self.tabs[index].page.is_none() && !self.tabs[index].loading {
            self.navigate(self.tabs[index].url.clone(), false);
        }
        self.ensure_layout();
        self.set_title();
        self.save();
        self.redraw();
    }
    fn action(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        match action {
            Action::Address => {
                if let Some(runtime) = &mut self.tabs[self.active].runtime
                    && let Err(error) = runtime.focus_control(None)
                {
                    self.status = error.to_string();
                }
                self.sync_runtime(self.active);
                self.select_popup = None;
                self.address_focus = true;
                self.select_all = true;
                self.cursor = self.address.len();
                self.redraw();
            }
            Action::Back => self.traverse_history(self.active, -1),
            Action::Forward => self.traverse_history(self.active, 1),
            Action::Reload => {
                let t = &mut self.tabs[self.active];
                if t.loading {
                    t.cancel.cancel();
                    t.cancel = CancelSignal::new();
                    t.navigation += 1;
                    t.loading = false;
                    if let (Some(session), Some(page)) = (&mut t.document_session, &mut t.page) {
                        session.stop(Arc::make_mut(page), t.runtime.as_ref());
                        t.title = page.title();
                        t.layout = None;
                    }
                    self.status = "Wczytywanie zatrzymane".into();
                    if let Some(runtime) = &mut t.runtime
                        && let Err(error) = runtime.cancel_pending_network()
                    {
                        self.status = error.to_string();
                    }
                    self.sync_runtime(self.active);
                    self.ensure_layout();
                    self.set_title();
                    self.redraw();
                } else {
                    let url = t.url.clone();
                    self.navigate(url, false);
                }
            }
            Action::Home => self.navigate("about:home".into(), true),
            Action::Version => self.navigate("about:version".into(), true),
            Action::NewTab => {
                if self.tabs.len() >= 24 {
                    self.status = "Wersja alfa obsługuje do 24 kart.".into();
                    self.redraw();
                    return;
                }
                let index = self.tabs.len();
                self.tabs.push(Tab::new(
                    self.next_id,
                    "about:home".into(),
                    self.profile.storage(),
                ));
                self.next_id += 1;
                self.activate(index);
                self.action(Action::Address, event_loop);
            }
            Action::Select(i) => self.activate(i),
            Action::Close(i) => {
                if i >= self.tabs.len() {
                    return;
                }
                self.tabs[i].cancel.cancel();
                self.tabs.remove(i);
                if self.tabs.is_empty() {
                    self.save();
                    event_loop.exit();
                    return;
                }
                if i < self.active {
                    self.active -= 1;
                }
                self.active = self.active.min(self.tabs.len() - 1);
                self.activate(self.active);
            }
        }
    }
    fn scroll(&mut self, dy: f32) {
        self.address_focus = false;
        self.ensure_layout();
        let tab = &mut self.tabs[self.active];
        let x = self.mouse.0 / tab.zoom + tab.scroll_x;
        let y = (self.mouse.1 - TOP) / tab.zoom + tab.scroll;
        let mut consumed = false;
        if self.mouse.1 >= TOP
            && let (Some(layout), Some(page)) = (&tab.layout, &tab.page)
        {
            let mut current = layout.element_at(x, y);
            while let Some(node) = current {
                if let Some(region) = layout.scroll_regions.iter().find(|r| r.node == node)
                    && layout.computed_styles[node.0].overflow[1].user_scrollable()
                    && ((dy > 0. && region.offset.y < region.max_y)
                        || (dy < 0. && region.offset.y > 0.))
                {
                    if let Some(runtime) = &mut tab.runtime {
                        match runtime.scroll_element(node, 0., dy) {
                            Ok(changed) => consumed = changed,
                            Err(e) => self.status = e.to_string(),
                        }
                    } else {
                        tab.nested_scroll.insert(
                            node,
                            ScrollOffset {
                                x: region.offset.x,
                                y: (region.offset.y + dy).clamp(0., region.max_y),
                            },
                        );
                        consumed = true;
                    }
                    if consumed {
                        break;
                    }
                }
                current = page.document.nodes[node.0].parent;
            }
        }
        if consumed {
            tab.layout = None;
        } else if tab
            .layout
            .as_ref()
            .is_none_or(|layout| layout.viewport_overflow[1].user_scrollable())
        {
            tab.scroll += dy;
        }
        self.ensure_layout();
        self.redraw();
    }
    fn insert(&mut self, text: &str) {
        if !self.address_focus {
            if let Some(runtime) = &mut self.tabs[self.active].runtime {
                if let Err(error) = runtime.insert_form_text(text) {
                    self.status = error.to_string();
                }
                self.after_form_interaction();
            }
            return;
        }
        if self.select_all {
            self.address.clear();
            self.cursor = 0;
            self.select_all = false;
        }
        let filtered = text.chars().filter(|c| !c.is_control()).collect::<String>();
        if self.address.len() + filtered.len() > 16384 {
            return;
        }
        self.address.insert_str(self.cursor, &filtered);
        self.cursor += filtered.len();
        self.redraw();
    }
    fn key(&mut self, key: Key, text: Option<&str>, event_loop: &ActiveEventLoop) {
        if self.select_popup.is_some()
            && matches!(
                key,
                Key::Named(NamedKey::Escape)
                    | Key::Named(NamedKey::Enter)
                    | Key::Named(NamedKey::Tab)
            )
        {
            self.select_popup = None;
            self.redraw();
            if key != Key::Named(NamedKey::Tab) {
                return;
            }
        }
        let ctrl = self.modifiers.control_key() && !self.modifiers.alt_key();
        if ctrl {
            if let Key::Character(c) = &key {
                match c.to_lowercase().as_str() {
                    "l" => {
                        self.action(Action::Address, event_loop);
                        return;
                    }
                    "t" => {
                        self.action(Action::NewTab, event_loop);
                        return;
                    }
                    "w" => {
                        self.action(Action::Close(self.active), event_loop);
                        return;
                    }
                    "r" => {
                        self.navigate(self.tabs[self.active].url.clone(), false);
                        return;
                    }
                    "a" if self.address_focus => {
                        self.select_all = true;
                        self.redraw();
                        return;
                    }
                    "a" if !self.address_focus => {
                        self.send_form_key(FormKey::SelectAll);
                        return;
                    }
                    "c" => {
                        let value = if self.address_focus {
                            Some(self.address.clone())
                        } else {
                            self.tabs[self.active]
                                .runtime
                                .as_ref()
                                .and_then(ScriptRuntime::form_selected_text)
                        };
                        if let Some(cb) = &mut self.clipboard
                            && let Some(value) = value
                        {
                            let _ = cb.set_text(value);
                        }
                        return;
                    }
                    "x" if self.address_focus && self.select_all => {
                        if let Some(cb) = &mut self.clipboard {
                            let _ = cb.set_text(self.address.clone());
                        }
                        self.address.clear();
                        self.cursor = 0;
                        self.select_all = false;
                        self.redraw();
                        return;
                    }
                    "x" if !self.address_focus => {
                        let value = self.tabs[self.active]
                            .runtime
                            .as_ref()
                            .and_then(ScriptRuntime::form_selected_text);
                        if let (Some(cb), Some(value)) = (&mut self.clipboard, value)
                            && cb.set_text(value).is_ok()
                        {
                            if let Some(runtime) = &mut self.tabs[self.active].runtime
                                && let Err(error) = runtime.cut_form_selection()
                            {
                                self.status = error.to_string();
                            }
                            self.after_form_interaction();
                        }
                        return;
                    }
                    "v" => {
                        if let Some(value) =
                            self.clipboard.as_mut().and_then(|cb| cb.get_text().ok())
                        {
                            self.insert(&value);
                        }
                        return;
                    }
                    "+" | "=" | "-" | "0" => {
                        let t = &mut self.tabs[self.active];
                        t.zoom = match c.as_str() {
                            "-" => t.zoom / 1.1,
                            "0" => 1.,
                            _ => t.zoom * 1.1,
                        }
                        .clamp(0.5, 3.);
                        t.layout = None;
                        self.ensure_layout();
                        self.redraw();
                        return;
                    }
                    _ => {}
                }
            }
            if key == Key::Named(NamedKey::Tab) {
                let direction = if self.modifiers.shift_key() {
                    self.tabs.len() - 1
                } else {
                    1
                };
                self.activate((self.active + direction) % self.tabs.len());
                return;
            }
        }
        if self.modifiers.alt_key() {
            if key == Key::Named(NamedKey::ArrowLeft) {
                self.action(Action::Back, event_loop);
                return;
            }
            if key == Key::Named(NamedKey::ArrowRight) {
                self.action(Action::Forward, event_loop);
                return;
            }
        }
        if !self.address_focus {
            if key == Key::Named(NamedKey::Tab)
                && let Some(runtime) = &mut self.tabs[self.active].runtime
            {
                if let Err(error) = runtime.form_keydown(
                    FormKey::Tab,
                    self.modifiers.shift_key(),
                    self.modifiers.control_key(),
                ) {
                    self.status = error.to_string();
                }
                self.after_form_interaction();
                return;
            }
            let form_key = match key {
                Key::Named(NamedKey::ArrowLeft) => Some(FormKey::ArrowLeft),
                Key::Named(NamedKey::ArrowRight) => Some(FormKey::ArrowRight),
                Key::Named(NamedKey::ArrowUp) => Some(FormKey::ArrowUp),
                Key::Named(NamedKey::ArrowDown) => Some(FormKey::ArrowDown),
                Key::Named(NamedKey::Home) => Some(FormKey::Home),
                Key::Named(NamedKey::End) => Some(FormKey::End),
                Key::Named(NamedKey::Backspace) => Some(FormKey::Backspace),
                Key::Named(NamedKey::Delete) => Some(FormKey::Delete),
                Key::Named(NamedKey::Enter) => Some(FormKey::Enter),
                Key::Named(NamedKey::Space) => Some(FormKey::Space),
                _ => None,
            };
            if form_key.is_some_and(|key| self.send_form_key(key)) {
                return;
            }
        }
        match key {
            Key::Named(NamedKey::F5) => self.navigate(self.tabs[self.active].url.clone(), false),
            Key::Named(NamedKey::Escape) => {
                if self.address_focus {
                    self.address_focus = false;
                    self.sync_address();
                } else if self.tabs[self.active].loading {
                    self.action(Action::Reload, event_loop);
                }
                self.redraw();
            }
            Key::Named(NamedKey::Enter) if self.address_focus => {
                self.navigate(self.address.clone(), true)
            }
            Key::Named(NamedKey::Backspace) if self.address_focus => {
                if self.select_all {
                    self.address.clear();
                    self.cursor = 0;
                } else if self.cursor > 0 {
                    let previous = self.address[..self.cursor]
                        .char_indices()
                        .last()
                        .map_or(0, |(i, _)| i);
                    self.address.drain(previous..self.cursor);
                    self.cursor = previous;
                }
                self.select_all = false;
                self.redraw();
            }
            Key::Named(NamedKey::Delete) if self.address_focus => {
                if self.select_all {
                    self.address.clear();
                    self.cursor = 0;
                } else if self.cursor < self.address.len() {
                    let next = self.cursor
                        + self.address[self.cursor..]
                            .chars()
                            .next()
                            .unwrap()
                            .len_utf8();
                    self.address.drain(self.cursor..next);
                }
                self.select_all = false;
                self.redraw();
            }
            Key::Named(NamedKey::ArrowLeft) if self.address_focus => {
                self.cursor = if self.select_all {
                    0
                } else {
                    self.address[..self.cursor]
                        .char_indices()
                        .last()
                        .map_or(0, |(i, _)| i)
                };
                self.select_all = false;
                self.redraw();
            }
            Key::Named(NamedKey::ArrowRight) if self.address_focus => {
                self.cursor = if self.select_all {
                    self.address.len()
                } else {
                    self.cursor
                        + self.address[self.cursor..]
                            .chars()
                            .next()
                            .map_or(0, char::len_utf8)
                };
                self.select_all = false;
                self.redraw();
            }
            Key::Named(NamedKey::Home) if self.address_focus => {
                self.cursor = 0;
                self.select_all = false;
                self.redraw();
            }
            Key::Named(NamedKey::End) if self.address_focus => {
                self.cursor = self.address.len();
                self.select_all = false;
                self.redraw();
            }
            Key::Named(NamedKey::Home | NamedKey::End) => {
                self.ensure_layout();
                let tab = &mut self.tabs[self.active];
                if tab
                    .layout
                    .as_ref()
                    .is_none_or(|l| l.viewport_overflow[1].user_scrollable())
                {
                    tab.scroll = if matches!(key, Key::Named(NamedKey::Home)) {
                        0.
                    } else {
                        f32::MAX
                    };
                    self.ensure_layout();
                    self.redraw();
                }
            }
            Key::Named(NamedKey::ArrowDown) => self.scroll(45.),
            Key::Named(NamedKey::ArrowUp) => self.scroll(-45.),
            Key::Named(NamedKey::PageDown) | Key::Named(NamedKey::Space) if !self.address_focus => {
                let h = self
                    .window
                    .as_ref()
                    .map_or(600., |w| w.inner_size().height as f32 / self.scale)
                    - TOP;
                self.scroll(h * 0.85);
            }
            Key::Named(NamedKey::PageUp) => {
                let h = self
                    .window
                    .as_ref()
                    .map_or(600., |w| w.inner_size().height as f32 / self.scale)
                    - TOP;
                self.scroll(-h * 0.85);
            }
            _ => {
                if !ctrl
                    && !self.preedit
                    && let Some(text) = text
                {
                    self.insert(text);
                }
            }
        }
    }

    fn draw(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.ensure_layout();
        let mut canvas = self
            .canvas
            .take()
            .filter(|c| c.width == size.width && c.height == size.height)
            .unwrap_or_else(|| Canvas::new(size.width, size.height, Color::WHITE));
        let tab = &self.tabs[self.active];
        if let Some(runtime) = &tab.runtime {
            runtime.publish_canvas_frames();
        }
        canvas.clear(tab.layout.as_ref().map_or(Color::WHITE, |l| l.background));
        let u = self.scale;
        let width = size.width as f32 / u;
        let height = size.height as f32 / u;
        canvas.clip(Rect {
            x: 0.,
            y: TOP * u,
            width: size.width as f32,
            height: ((height - TOP - FOOT) * u).max(0.),
        });
        if let Some(layout) = &tab.layout {
            canvas.document_scaled(layout, &mut self.fonts, TOP * u, tab.scroll, u * tab.zoom);
        }
        canvas.clip(Rect {
            x: 0.,
            y: 0.,
            width: size.width as f32,
            height: size.height as f32,
        });
        let rect = |x: f32, y: f32, w: f32, h: f32| Rect {
            x: x * u,
            y: y * u,
            width: w * u,
            height: h * u,
        };
        canvas.fill_rect(rect(0., 0., width, TAB_H), Color(18, 29, 37, 255));
        canvas.fill_rect(
            rect(0., TAB_H, width, TOP - TAB_H),
            Color(233, 239, 241, 255),
        );
        self.hits.clear();
        let tab_width = ((width - 96.) / self.tabs.len() as f32).clamp(65., 225.);
        for (i, t) in self.tabs.iter().enumerate() {
            let x = 8. + i as f32 * tab_width;
            let active = i == self.active;
            canvas.fill_rect(
                rect(x, 6., tab_width - 4., 30.),
                if active {
                    Color(233, 239, 241, 255)
                } else {
                    Color(34, 49, 58, 255)
                },
            );
            let label = elide(&self.fonts, &t.title, (tab_width - 44.) * u, 12. * u);
            canvas.label(
                &label,
                (x + 12.) * u,
                25. * u,
                12. * u,
                if active {
                    Color(26, 45, 51, 255)
                } else {
                    Color(201, 216, 222, 255)
                },
                active,
                &mut self.fonts,
            );
            canvas.label(
                "×",
                (x + tab_width - 23.) * u,
                25. * u,
                17. * u,
                if active {
                    Color(62, 80, 88, 255)
                } else {
                    Color(174, 194, 203, 255)
                },
                false,
                &mut self.fonts,
            );
            self.hits.push((
                Rect {
                    x,
                    y: 0.,
                    width: tab_width - 27.,
                    height: TAB_H,
                },
                Action::Select(i),
            ));
            self.hits.push((
                Rect {
                    x: x + tab_width - 28.,
                    y: 0.,
                    width: 26.,
                    height: TAB_H,
                },
                Action::Close(i),
            ));
        }
        let plus_x = 8. + self.tabs.len() as f32 * tab_width;
        canvas.label(
            "+",
            (plus_x + 9.) * u,
            26. * u,
            22. * u,
            Color(205, 227, 225, 255),
            false,
            &mut self.fonts,
        );
        self.hits.push((
            Rect {
                x: plus_x,
                y: 0.,
                width: 32.,
                height: TAB_H,
            },
            Action::NewTab,
        ));
        for (x, action, enabled) in [
            (10., Action::Back, tab.history.can_traverse(-1)),
            (48., Action::Forward, tab.history.can_traverse(1)),
            (86., Action::Reload, true),
            (124., Action::Home, true),
        ] {
            navigation_icon(
                &mut canvas,
                x + 7.,
                53.,
                u,
                &action,
                tab.loading,
                if enabled {
                    Color(49, 72, 82, 255)
                } else {
                    Color(160, 174, 181, 255)
                },
            );
            self.hits.push((
                Rect {
                    x,
                    y: TAB_H + 5.,
                    width: 34.,
                    height: 43.,
                },
                action,
            ));
        }
        let address_rect = Rect {
            x: 166.,
            y: 46.,
            width: (width - 220.).max(80.),
            height: 34.,
        };
        canvas.fill_rect(
            rect(
                address_rect.x - 1.,
                address_rect.y - 1.,
                address_rect.width + 2.,
                address_rect.height + 2.,
            ),
            if self.address_focus {
                Color(27, 144, 119, 255)
            } else {
                Color(205, 216, 222, 255)
            },
        );
        canvas.fill_rect(
            rect(
                address_rect.x,
                address_rect.y,
                address_rect.width,
                address_rect.height,
            ),
            Color::WHITE,
        );
        canvas.label(
            if self.tabs[self.active].url.starts_with("https:") {
                "HTTPS"
            } else if self.tabs[self.active].url.starts_with("http:") {
                "HTTP"
            } else if self.tabs[self.active].url.starts_with("file:") {
                "FILE"
            } else {
                "APP"
            },
            174. * u,
            68. * u,
            10. * u,
            Color(76, 114, 110, 255),
            true,
            &mut self.fonts,
        );
        let text_x = 211.;
        let available = (address_rect.width - 53.).max(12.);
        canvas.clip(rect(text_x, 48., available, 29.));
        let mut display = self.address.clone();
        let mut cursor_prefix = self.address[..self.cursor.min(self.address.len())].to_owned();
        let cursor_width = self
            .fonts
            .shape(&cursor_prefix, 14. * u, false, false, false)
            .advance
            / u;
        let shift = if self.address_focus {
            (cursor_width - available + 8.).max(0.)
        } else {
            0.
        };
        if self.select_all && self.address_focus {
            let run = self.fonts.shape(&display, 14. * u, false, false, false);
            canvas.fill_rect(
                rect(text_x - shift, 51., run.advance / u, 23.),
                Color(198, 231, 222, 255),
            );
        }
        canvas.label(
            &display,
            (text_x - shift) * u,
            69. * u,
            14. * u,
            Color(29, 47, 57, 255),
            false,
            &mut self.fonts,
        );
        if self.address_focus && !self.select_all {
            canvas.fill_rect(
                rect(text_x + cursor_width - shift, 53., 1., 20.),
                Color(26, 108, 91, 255),
            );
        }
        display.clear();
        cursor_prefix.clear();
        canvas.clip(Rect {
            x: 0.,
            y: 0.,
            width: size.width as f32,
            height: size.height as f32,
        });
        self.hits.push((address_rect, Action::Address));
        canvas.label(
            "i",
            (width - 35.) * u,
            69. * u,
            18. * u,
            Color(62, 91, 100, 255),
            true,
            &mut self.fonts,
        );
        self.hits.push((
            Rect {
                x: width - 45.,
                y: 43.,
                width: 35.,
                height: 39.,
            },
            Action::Version,
        ));
        canvas.fill_rect(
            rect(0., height - FOOT, width, FOOT),
            Color(237, 242, 244, 255),
        );
        let text = if let Some(link) = &self.hover {
            link.clone()
        } else if !self.status.is_empty() {
            self.status.clone()
        } else if self.tabs[self.active].loading {
            "Ładowanie…".into()
        } else if self.tabs[self.active]
            .layout
            .as_ref()
            .is_some_and(|layout| layout.limited)
        {
            "Limit złożoności: uproszczony układ głęboko zagnieżdżonej strony.".into()
        } else if let Some(message) = self.tabs[self.active]
            .runtime
            .as_ref()
            .and_then(ScriptRuntime::last_diagnostic)
        {
            format!("JavaScript: {message}")
        } else {
            format!("Rust Browser {}", env!("CARGO_PKG_VERSION"))
        };
        let text = elide(&self.fonts, &text, (width - 100.) * u, 11. * u);
        canvas.label(
            &text,
            12. * u,
            (height - 8.) * u,
            11. * u,
            Color(88, 110, 120, 255),
            false,
            &mut self.fonts,
        );
        let zoom = format!("{}%", (self.tabs[self.active].zoom * 100.).round() as u32);
        canvas.label(
            &zoom,
            (width - 53.) * u,
            (height - 8.) * u,
            11. * u,
            Color(88, 110, 120, 255),
            false,
            &mut self.fonts,
        );
        if let Some(popup) = self.select_popup
            && let Some(control) = self.tabs[self.active]
                .page
                .as_ref()
                .and_then(|page| page.form_controls.get(&popup.node))
        {
            canvas.clip(rect(0., TOP, width, height - TOP - FOOT));
            canvas.fill_rect(
                rect(
                    popup.rect.x + 3.,
                    popup.rect.y + 3.,
                    popup.rect.width,
                    popup.rect.height,
                ),
                Color(40, 50, 60, 55),
            );
            canvas.fill_rect(
                rect(
                    popup.rect.x,
                    popup.rect.y,
                    popup.rect.width,
                    popup.rect.height,
                ),
                Color::WHITE,
            );
            let rows = (popup.rect.height / popup.row_height).floor() as usize;
            for (row, option) in control
                .options
                .iter()
                .skip(popup.first)
                .take(rows)
                .enumerate()
            {
                let y = popup.rect.y + row as f32 * popup.row_height;
                if option.selected {
                    canvas.fill_rect(
                        rect(popup.rect.x, y, popup.rect.width, popup.row_height),
                        Color(17, 121, 173, 255),
                    );
                }
                let color = if option.disabled {
                    Color(140, 140, 140, 255)
                } else if option.selected {
                    Color::WHITE
                } else {
                    Color(20, 24, 28, 255)
                };
                canvas.label(
                    &option.label,
                    (popup.rect.x + 7.) * u,
                    (y + popup.row_height * 0.75) * u,
                    14. * u,
                    color,
                    false,
                    &mut self.fonts,
                );
            }
            for border in [
                Rect {
                    height: 1.,
                    ..popup.rect
                },
                Rect {
                    y: popup.rect.y + popup.rect.height - 1.,
                    height: 1.,
                    ..popup.rect
                },
                Rect {
                    width: 1.,
                    ..popup.rect
                },
                Rect {
                    x: popup.rect.x + popup.rect.width - 1.,
                    width: 1.,
                    ..popup.rect
                },
            ] {
                canvas.fill_rect(
                    rect(border.x, border.y, border.width, border.height),
                    Color(93, 106, 118, 255),
                );
            }
        }
        let presented = (|| -> Result<(), String> {
            let graphics = self
                .graphics
                .as_mut()
                .ok_or("Presentation surface is missing")?;
            graphics
                .surface
                .resize(
                    NonZeroU32::new(size.width).unwrap(),
                    NonZeroU32::new(size.height).unwrap(),
                )
                .map_err(|e| e.to_string())?;
            let mut buffer = graphics.surface.buffer_mut().map_err(|e| e.to_string())?;
            buffer.copy_from_slice(&canvas.pixels);
            buffer.present().map_err(|e| e.to_string())
        })();
        match presented {
            Ok(()) => {
                self.presented_frames += 1;
                self.last_present_error = None;
            }
            Err(e) => self.last_present_error = Some(e),
        }
        self.canvas = Some(canvas);
    }
    fn finish_smoke(&mut self, event_loop: &ActiveEventLoop) {
        let tab = &self.tabs[self.active];
        if self.smoke_output.is_none()
            || tab.loading
            || tab.document_session.as_ref().is_some_and(|session| {
                !session.done() || session.needs_advance(tab.runtime.as_ref())
            })
            || tab.runtime.as_ref().is_some_and(|runtime| {
                runtime.pending_network() > 0
                    || runtime.has_pending_image_decodes()
                    || runtime.has_pending_intersection_observations()
                    || runtime.has_queued_script_tasks()
                    || runtime.has_navigation_tasks()
            })
        {
            return;
        }
        if let Some(action) = self.smoke_actions.pop_front() {
            let result = self.smoke_action(action, event_loop);
            if let Err(error) = result {
                self.smoke_output = None;
                self.smoke_result = Some(Err(error));
                event_loop.set_control_flow(ControlFlow::Poll);
                event_loop.exit();
                return;
            }
            self.redraw();
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        if let Some(name) = self.smoke_click.take() {
            if let Err(error) = self.smoke_target(&name) {
                self.smoke_output = None;
                self.smoke_result = Some(Err(error));
                event_loop.set_control_flow(ControlFlow::Poll);
                event_loop.exit();
                return;
            }
            self.redraw();
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        let path = self.smoke_output.take().unwrap();
        self.draw();
        let result = (|| -> Result<(), String> {
            if let Some(error) = &self.last_present_error {
                return Err(error.clone());
            }
            if self.presented_frames == 0 {
                return Err("Native window did not present a frame".into());
            }
            if self.tabs[self.active]
                .layout
                .as_ref()
                .is_none_or(|l| l.commands.len() < 10)
            {
                return Err("Document was not rendered".into());
            }
            self.canvas
                .as_ref()
                .ok_or("Missing frame")?
                .save_png(&path)
                .map_err(|e| e.to_string())
        })();
        self.smoke_result = Some(result);
        // Winit's Windows AboutToWait path can enter Wait before observing its
        // exit flag; Poll guarantees a wake after an idle hidden-window check.
        event_loop.set_control_flow(ControlFlow::Poll);
        event_loop.exit();
    }
    fn pointer_link(&self) -> Option<String> {
        if self.mouse.1 < TOP
            || self.window.as_ref().is_some_and(|window| {
                self.mouse.1 >= window.inner_size().height as f32 / self.scale - FOOT
            })
        {
            return None;
        }
        let tab = &self.tabs[self.active];
        let x = self.mouse.0 / tab.zoom + tab.scroll_x;
        let y = (self.mouse.1 - TOP) / tab.zoom + tab.scroll;
        tab.layout
            .as_ref()
            .and_then(|l| l.link_at(x, y))
            .map(str::to_owned)
    }
    fn smoke_target(&mut self, name: &str) -> Result<(), String> {
        self.ensure_layout();
        let tab = &mut self.tabs[self.active];
        let id = tab
            .page
            .as_ref()
            .and_then(|page| {
                page.document
                    .connected_nodes()
                    .find(|&id| page.document.attr(id, "id") == Some(name))
            })
            .ok_or_else(|| format!("Smoke target #{name} is missing"))?;
        let rect = tab
            .layout
            .as_ref()
            .and_then(|layout| layout.rect_for(id))
            .ok_or_else(|| format!("Smoke target #{name} has no rendered box"))?;
        let height = self.window.as_ref().map_or(600., |window| {
            window.inner_size().height as f32 / self.scale - TOP - FOOT
        }) / tab.zoom;
        if rect.y < tab.scroll || rect.y + rect.height > tab.scroll + height {
            tab.scroll = rect.y.max(0.);
        }
        self.mouse = (
            (rect.x + rect.width / 2. - tab.scroll_x) * tab.zoom,
            TOP + (rect.y + rect.height.min(height) / 2. - tab.scroll) * tab.zoom,
        );
        self.content_click();
        Ok(())
    }
    fn smoke_action(
        &mut self,
        action: crate::testing::SmokeAction,
        event_loop: &ActiveEventLoop,
    ) -> Result<(), String> {
        use crate::testing::SmokeAction;
        match action {
            SmokeAction::Click { id } => self.smoke_target(&id),
            SmokeAction::Text { value } => {
                self.insert(&value);
                Ok(())
            }
            SmokeAction::Title { value } => {
                let actual = &self.tabs[self.active].title;
                if actual == &value {
                    Ok(())
                } else {
                    Err(format!("Expected native title {value:?}, got {actual:?}"))
                }
            }
            SmokeAction::Key {
                value,
                control,
                shift,
                alt,
            } => {
                let named = match value.as_str() {
                    "Tab" => NamedKey::Tab,
                    "Enter" => NamedKey::Enter,
                    "Space" => NamedKey::Space,
                    "Escape" => NamedKey::Escape,
                    "Backspace" => NamedKey::Backspace,
                    "Delete" => NamedKey::Delete,
                    "ArrowLeft" => NamedKey::ArrowLeft,
                    "ArrowRight" => NamedKey::ArrowRight,
                    "ArrowUp" => NamedKey::ArrowUp,
                    "ArrowDown" => NamedKey::ArrowDown,
                    "Home" => NamedKey::Home,
                    "End" => NamedKey::End,
                    _ => {
                        let prior = self.modifiers;
                        self.modifiers = ModifiersState::empty();
                        self.modifiers.set(ModifiersState::CONTROL, control);
                        self.modifiers.set(ModifiersState::SHIFT, shift);
                        self.modifiers.set(ModifiersState::ALT, alt);
                        self.key(Key::Character(value.into()), None, event_loop);
                        self.modifiers = prior;
                        return Ok(());
                    }
                };
                let prior = self.modifiers;
                self.modifiers = ModifiersState::empty();
                self.modifiers.set(ModifiersState::CONTROL, control);
                self.modifiers.set(ModifiersState::SHIFT, shift);
                self.modifiers.set(ModifiersState::ALT, alt);
                self.key(Key::Named(named), None, event_loop);
                self.modifiers = prior;
                Ok(())
            }
        }
    }
    fn content_click(&mut self) {
        if let Some(popup) = self.select_popup.take() {
            if popup.rect.contains(self.mouse.0, self.mouse.1) {
                let index = popup.first
                    + ((self.mouse.1 - popup.rect.y) / popup.row_height).floor() as usize;
                let option = self.tabs[self.active]
                    .page
                    .as_ref()
                    .and_then(|page| page.form_controls.get(&popup.node))
                    .and_then(|control| control.options.get(index))
                    .map(|option| option.node);
                if let (Some(runtime), Some(option)) = (&mut self.tabs[self.active].runtime, option)
                    && let Err(error) = runtime.choose_form_option(popup.node, option, false, false)
                {
                    self.status = error.to_string();
                }
                self.after_form_interaction();
                return;
            }
            self.redraw();
            return;
        }
        if self.mouse.1 < TOP
            || self.window.as_ref().is_some_and(|window| {
                self.mouse.1 >= window.inner_size().height as f32 / self.scale - FOOT
            })
        {
            return;
        }
        let mut link = self.pointer_link();
        let tab = &mut self.tabs[self.active];
        let x = self.mouse.0 / tab.zoom + tab.scroll_x;
        let y = (self.mouse.1 - TOP) / tab.zoom + tab.scroll;
        let target = tab
            .layout
            .as_ref()
            .and_then(|layout| layout.element_at(x, y));
        if let (Some(runtime), Some(target)) = (&mut tab.runtime, target) {
            let focus = tab.page.as_ref().and_then(|page| {
                let mut current = Some(target);
                while let Some(node) = current {
                    if page.form_controls.contains_key(&node) {
                        return Some(node);
                    }
                    current = page.document.nodes[node.0].parent;
                }
                None
            });
            if let Err(error) = runtime.focus_control(focus) {
                self.status = error.to_string();
            }
            let (allowed, href) = runtime.click_with_default(target);
            link = href.and_then(|href| runtime.resolve_reference(&href));
            if allowed
                && let Some(option) = tab
                    .layout
                    .as_ref()
                    .and_then(|layout| layout.control_option_at(target, x, y))
                && let Err(error) = runtime.choose_form_option(
                    target,
                    option,
                    self.modifiers.control_key(),
                    self.modifiers.shift_key(),
                )
            {
                self.status = error.to_string();
            }
            if let Some(page) = &mut tab.page
                && runtime.snapshot(Arc::make_mut(page))
            {
                tab.title = page.title();
                tab.layout = None;
            }
        }
        self.answer_file_picker();
        if self.dispatch_forms(self.active) {
            return;
        }
        if let Some(url) = link {
            let initiator = self.tabs[self.active]
                .page
                .as_ref()
                .and_then(|page| url::Url::parse(&page.url).ok());
            if let Ok(mut request) = NavigationRequest::get(&url) {
                request.context =
                    CookieContext::navigation(initiator.as_ref(), &request.url, "GET");
                request.initiator = initiator;
                self.navigate_with_request(url, true, Some(request));
            }
            return;
        }
        self.ensure_layout();
        if let Some(target) = target {
            self.open_select_popup(target);
        }
        self.set_title();
        self.redraw();
    }
    fn open_select_popup(&mut self, node: rust_browser_engine::dom::NodeId) {
        let tab = &self.tabs[self.active];
        let Some(page) = &tab.page else {
            return;
        };
        let Some(control) = page.form_controls.get(&node) else {
            return;
        };
        if control.kind != "select-one"
            || control.disabled
            || control.options.is_empty()
            || page
                .document
                .attr(node, "size")
                .and_then(|value| value.parse::<usize>().ok())
                .is_some_and(|value| value > 1)
        {
            return;
        }
        let Some(element) = tab.layout.as_ref().and_then(|layout| layout.rect_for(node)) else {
            return;
        };
        let height = self.window.as_ref().map_or(840., |window| {
            window.inner_size().height as f32 / self.scale
        });
        let width = self.window.as_ref().map_or(1180., |window| {
            window.inner_size().width as f32 / self.scale
        });
        let row_height = 24.;
        let rows = control
            .options
            .len()
            .min(12)
            .min(((height - TOP - FOOT) / row_height).floor().max(1.) as usize);
        let y = TOP + (element.y + element.height - tab.scroll) * tab.zoom;
        let popup_height = row_height * rows as f32;
        let y = if y + popup_height > height - FOOT {
            (y - element.height * tab.zoom - popup_height).max(TOP)
        } else {
            y
        };
        let x = (element.x * tab.zoom).max(0.).min((width - 120.).max(0.));
        let selected = control
            .options
            .iter()
            .position(|option| option.selected)
            .unwrap_or(0);
        self.select_popup = Some(SelectPopup {
            node,
            rect: Rect {
                x,
                y,
                width: (element.width * tab.zoom).max(120.).min(width - x),
                height: popup_height,
            },
            row_height,
            first: selected
                .saturating_sub(rows / 2)
                .min(control.options.len().saturating_sub(rows)),
        });
    }
    fn send_form_key(&mut self, key: FormKey) -> bool {
        let result = self.tabs[self.active].runtime.as_mut().map(|runtime| {
            runtime.form_keydown(
                key,
                self.modifiers.shift_key(),
                self.modifiers.control_key(),
            )
        });
        let consumed = match result {
            Some(Ok(value)) => value,
            Some(Err(error)) => {
                self.status = error.to_string();
                true
            }
            None => false,
        };
        if consumed {
            self.answer_file_picker();
            self.after_form_interaction();
            if let Some(popup) = self.select_popup {
                self.open_select_popup(popup.node);
            }
        }
        consumed
    }
    fn answer_file_picker(&mut self) -> bool {
        let (tab_id, navigation, request) = {
            let tab = &self.tabs[self.active];
            let Some(request) = tab
                .runtime
                .as_ref()
                .and_then(ScriptRuntime::take_file_picker_request)
            else {
                return false;
            };
            (tab.id, tab.navigation, request)
        };
        let response = if self.smoke_output.is_some() {
            self.file_picker_responses
                .as_mut()
                .map_or(Ok(None), |answers| answers.select(&request))
        } else if let Some(window) = self.window.as_deref() {
            crate::file_picker::choose(window, &request)
        } else {
            Ok(None)
        };
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id && tab.navigation == navigation)
        else {
            return false;
        };
        let files = match response {
            Ok(files) => files,
            Err(error) => {
                self.status = format!("Nie można wybrać plików: {error}");
                None
            }
        };
        let Some(runtime) = self.tabs[index].runtime.as_mut() else {
            return false;
        };
        if let Err(error) = runtime.complete_file_picker(&request, files) {
            self.status = format!("Nie można wybrać plików: {error}");
        }
        self.sync_runtime(index);
        index == self.active
    }
    fn dispatch_forms(&mut self, index: usize) -> bool {
        let Some(submission) = self.tabs[index]
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.take_form_submissions().pop())
        else {
            return false;
        };
        let initiator = self.tabs[index]
            .page
            .as_ref()
            .and_then(|page| url::Url::parse(&page.url).ok());
        let Ok(url) = url::Url::parse(&submission.url) else {
            self.status = "Nieprawidłowy adres formularza".into();
            return false;
        };
        let request = NavigationRequest {
            context: CookieContext::navigation(initiator.as_ref(), &url, &submission.method),
            url,
            method: submission.method,
            body: submission.body,
            content_type: submission.content_type,
            initiator,
        };
        self.navigate_tab(
            index,
            submission.url,
            NavigationMode::Push,
            Some(request),
            None,
        );
        true
    }
    fn after_form_interaction(&mut self) {
        let active = self.active;
        self.sync_runtime(active);
        self.dispatch_forms(active);
        self.dispatch_fetches(active);
        self.ensure_layout();
        if let Some(node) = self.tabs[active]
            .runtime
            .as_ref()
            .and_then(ScriptRuntime::focused_control)
            && let Some(rect) = self.tabs[active]
                .layout
                .as_ref()
                .and_then(|layout| layout.rect_for(node))
        {
            let height = self.window.as_ref().map_or(600., |window| {
                window.inner_size().height as f32 / self.scale - TOP - FOOT
            }) / self.tabs[active].zoom;
            let tab = &mut self.tabs[active];
            if rect.y < tab.scroll {
                tab.scroll = rect.y;
            } else if rect.y + rect.height > tab.scroll + height {
                tab.scroll = (rect.y + rect.height - height).max(0.);
            }
        }
        self.set_title();
        self.redraw();
    }
}
impl ApplicationHandler<()> for App {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if event_loop.exiting() {
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        let mut redraw = false;
        let mut deadline = None;
        for index in 0..self.tabs.len() {
            let changed = self.advance_page(index) | self.sync_runtime(index);
            redraw |= changed && index == self.active;
            if index == self.active && self.tabs[index].history_scroll.is_some() {
                self.ensure_layout();
            }
            self.dispatch_resources(index);
        }
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let Some(runtime) = &mut tab.runtime else {
                continue;
            };
            runtime.set_foreground(index == self.active);
            if runtime
                .next_deadline()
                .is_some_and(|deadline| deadline <= std::time::Instant::now())
            {
                runtime.pump();
                let canvas_changed = runtime.publish_canvas_frames();
                redraw |= canvas_changed && index == self.active;
                if tab
                    .layout
                    .as_ref()
                    .is_none_or(|layout| !runtime.layout_is_current(layout))
                {
                    tab.layout = None;
                    redraw |= index == self.active;
                }
            }
        }
        redraw |= self.answer_file_picker();
        for index in 0..self.tabs.len() {
            let tab = &self.tabs[index];
            if tab
                .document_session
                .as_ref()
                .is_some_and(|session| session.needs_advance(tab.runtime.as_ref()))
            {
                let changed = self.advance_page(index);
                redraw |= changed && index == self.active;
                self.dispatch_resources(index);
            }
            let changed = self.sync_runtime(index);
            redraw |= changed && index == self.active;
            self.dispatch_history(index);
            self.dispatch_forms(index);
            self.dispatch_fetches(index);
        }
        // A failed submission can synchronously queue a new request through a
        // rejection reaction; retain a wake-up for that outgoing work.
        for tab in &self.tabs {
            if let Some(next) = tab
                .document_session
                .as_ref()
                .and_then(DocumentSession::next_deadline)
            {
                deadline =
                    Some(deadline.map_or(next, |current: std::time::Instant| current.min(next)));
            }
            if let Some(next) = tab.runtime.as_ref().and_then(ScriptRuntime::next_deadline) {
                deadline =
                    Some(deadline.map_or(next, |current: std::time::Instant| current.min(next)));
            }
        }
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
        if let Err(error) = self.profile.save_storage() {
            self.status = format!("Nie można zapisać danych witryny: {error}");
        }
        if redraw {
            self.ensure_layout();
            self.set_title();
            self.redraw();
        }
        self.finish_smoke(event_loop);
    }
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attributes = Window::default_attributes()
            .with_title("Rust Browser · Alfa 0.1")
            .with_inner_size(LogicalSize::new(1180., 840.))
            .with_min_inner_size(LogicalSize::new(640., 400.));
        if self.smoke_output.is_some() {
            attributes = attributes.with_visible(false);
        }
        if let Ok(img) = image::load_from_memory(include_bytes!("../../../assets/browser.png")) {
            let img = img.into_rgba8();
            if let Ok(icon) = Icon::from_rgba(img.as_raw().clone(), img.width(), img.height()) {
                attributes = attributes.with_window_icon(Some(icon));
            }
        }
        let window = Rc::new(
            event_loop
                .create_window(attributes)
                .expect("create browser window"),
        );
        window.set_ime_allowed(true);
        self.scale = window.scale_factor() as f32;
        let context =
            softbuffer::Context::new(window.clone()).expect("create presentation context");
        let surface = softbuffer::Surface::new(&context, window.clone())
            .expect("create presentation surface");
        self.window = Some(window);
        self.graphics = Some(Graphics {
            _context: context,
            surface,
        });
        let url = self.tabs[self.active].url.clone();
        self.navigate(url, false);
        self.finish_smoke(event_loop);
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, _: ()) {
        for _ in 0..16 {
            let Ok(message) = self.incoming.try_recv() else {
                break;
            };
            let Some(index) = self
                .tabs
                .iter()
                .position(|t| t.id == message.tab && t.navigation == message.navigation)
            else {
                continue;
            };
            let headers = matches!(&message.payload, Payload::Fetch { .. });
            if headers
                || matches!(
                    &message.payload,
                    Payload::FetchBodyProgress(_) | Payload::FetchUploadDemand(_)
                )
            {
                if let Some(runtime) = &mut self.tabs[index].runtime {
                    let result = match message.payload {
                        Payload::Fetch { id, result } => runtime.complete_fetch(id, result),
                        Payload::FetchBodyProgress(id) => runtime.progress_network(id),
                        Payload::FetchUploadDemand(id) => runtime.progress_network_upload(id),
                        _ => unreachable!(),
                    };
                    if let Err(error) = result {
                        self.status = error.to_string();
                    }
                }
                let changed = self.sync_runtime(index);
                self.dispatch_fetches(index);
                self.dispatch_forms(index);
                if changed && index == self.active {
                    self.ensure_layout();
                    self.set_title();
                    self.redraw();
                }
                self.finish_smoke(event_loop);
                if headers {
                    let _ = self.profile.save_cookies();
                }
                continue;
            }
            match message.payload {
                Payload::Fetch { .. }
                | Payload::FetchBodyProgress(_)
                | Payload::FetchUploadDemand(_) => unreachable!(),
                Payload::Resource { id, result } => {
                    self.deliver_resource(index, id, result);
                    if self.tabs[index].document_session.is_some() {
                        continue;
                    }
                }
                Payload::Navigation(NavigationPart::Open(opened)) => {
                    self.start_page(index, *opened)
                }
                Payload::Navigation(NavigationPart::Text(text, bytes)) => {
                    let tab = &mut self.tabs[index];
                    if let (Some(session), Some(page)) = (&mut tab.document_session, &mut tab.page)
                    {
                        Arc::make_mut(page).stats.bytes += bytes;
                        if let Err(error) = session.feed(&text) {
                            self.fail_page(index, &error);
                        } else {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
                Payload::Navigation(NavigationPart::Finish) => {
                    if let Some(session) = &mut self.tabs[index].document_session
                        && let Err(error) = session.finish_input()
                    {
                        self.fail_page(index, &error);
                    } else {
                        let _ = self.profile.save_cookies();
                        continue;
                    }
                }
                Payload::NavigationError(error) => self.fail_page(index, &error),
            }
            if index == self.active {
                if !self.address_focus {
                    self.sync_address();
                }
                self.ensure_layout();
                self.set_title();
                self.redraw();
            }
            self.save();
            self.finish_smoke(event_loop);
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.save();
                for tab in &self.tabs {
                    tab.cancel.cancel();
                }
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(_) => {
                self.ensure_layout();
                self.redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor as f32;
                self.ensure_layout();
                self.redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.key(event.logical_key, event.text.as_deref(), event_loop)
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.preedit = false;
                self.insert(&text);
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => self.preedit = !text.is_empty(),
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = (
                    position.x as f32 / self.scale,
                    position.y as f32 / self.scale,
                );
                let link = self.pointer_link();
                if self.hover != link {
                    self.hover = link;
                    self.redraw();
                }
                if let Some(window) = &self.window {
                    window.set_cursor(if self.hover.is_some() {
                        CursorIcon::Pointer
                    } else if self.address_focus {
                        CursorIcon::Text
                    } else {
                        CursorIcon::Default
                    });
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.hover = None;
                self.redraw();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                if button == MouseButton::Left {
                    if let Some(action) = self
                        .hits
                        .iter()
                        .rev()
                        .find(|(r, _)| r.contains(self.mouse.0, self.mouse.1))
                        .map(|(_, a)| a.clone())
                    {
                        self.action(action, event_loop);
                    } else {
                        self.address_focus = false;
                        self.content_click();
                    }
                } else if button == MouseButton::Middle
                    && let Some(url) = self.pointer_link()
                {
                    self.action(Action::NewTab, event_loop);
                    self.navigate(url, true);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 54.,
                    MouseScrollDelta::PixelDelta(p) => -p.y as f32 / self.scale,
                };
                self.scroll(amount / self.tabs[self.active].zoom);
            }
            WindowEvent::DroppedFile(path) => {
                if let Ok(url) = url::Url::from_file_path(path) {
                    self.navigate(url.into(), true);
                }
            }
            _ => {}
        }
    }
}

fn navigation_icon(
    canvas: &mut Canvas,
    x: f32,
    y: f32,
    scale: f32,
    action: &Action,
    loading: bool,
    color: Color,
) {
    let mut line = |a: f32, b: f32, c: f32, d: f32| {
        canvas.stroke_line(
            (x + a) * scale,
            (y + b) * scale,
            (x + c) * scale,
            (y + d) * scale,
            1.5 * scale,
            color,
        )
    };
    match action {
        Action::Back => {
            line(3., 10., 17., 10.);
            line(3., 10., 9., 4.);
            line(3., 10., 9., 16.);
        }
        Action::Forward => {
            line(3., 10., 17., 10.);
            line(17., 10., 11., 4.);
            line(17., 10., 11., 16.);
        }
        Action::Home => {
            line(2., 9., 10., 2.);
            line(10., 2., 18., 9.);
            line(4., 8., 4., 18.);
            line(4., 18., 16., 18.);
            line(16., 18., 16., 8.);
            line(8., 18., 8., 12.);
            line(8., 12., 12., 12.);
            line(12., 12., 12., 18.);
        }
        Action::Reload if loading => {
            line(5., 5., 15., 15.);
            line(15., 5., 5., 15.);
        }
        Action::Reload => {
            for i in 0..24 {
                let a = 0.25 + i as f32 / 24. * std::f32::consts::PI * 1.65;
                let b = 0.25 + (i + 1) as f32 / 24. * std::f32::consts::PI * 1.65;
                line(
                    10. + 7. * a.cos(),
                    10. + 7. * a.sin(),
                    10. + 7. * b.cos(),
                    10. + 7. * b.sin(),
                );
            }
            line(17., 2., 17., 8.);
            line(17., 8., 11., 8.);
        }
        _ => {}
    }
}

fn elide(fonts: &Fonts, text: &str, width: f32, size: f32) -> String {
    if fonts.shape(text, size, false, false, false).advance <= width {
        return text.into();
    }
    let mut out = text.chars().take(80).collect::<String>();
    while !out.is_empty() {
        if fonts
            .shape(&format!("{out}…"), size, false, false, false)
            .advance
            <= width
        {
            break;
        }
        out.pop();
    }
    out.push('…');
    out
}
