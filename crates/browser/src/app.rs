use crate::jobs::{Job, Message, Payload, Task, Workers};
use crate::media::Bridge;
use crate::profile::{Profile, Session};
use crate::shortcuts::{self, Command};
use rust_browser_engine::{
    Fonts, Layout, LoadedPage, Rect,
    cookies::CookieContext,
    css::Color,
    dom::Document,
    history::{
        HistoryScroll, HistoryTransition, NavigationMode, ScriptNavigation, SessionHistory,
        fragment_offset,
    },
    layout::{ScrollOffset, Viewport},
    loader::{DocumentResource, NavigationPart, NavigationRequest, OpenedPage, built_in_site_icon},
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
    time::{Duration, Instant},
};
use tokio::sync::mpsc::{self, Receiver};
#[cfg(windows)]
use winit::platform::windows::WindowAttributesExtWindows;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize, PhysicalSize, Size},
    event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Icon, ResizeDirection, Theme, Window, WindowId},
};

pub(crate) mod address_bar;
mod bookmark_menu;
mod bookmarks;
mod chrome_menu;
mod history_ui;
mod icons;
mod navigation_error;
mod scroll;
mod settings;
mod tab_strip;
use self::address_bar::{AddressBar, Movement};
use self::bookmark_menu::Menu;
use crate::testing::{
    Checkpoint, CheckpointDocument, CheckpointObservation, CheckpointOutputs, CheckpointSample,
    SmokeAction,
};
use navigation_error::Recovery;
use tab_strip::{NEW_TAB_WIDTH, TabStrip};

const TAB_H: f32 = 58.;
const TOP: f32 = TAB_H + 64.;
const MAX_TABS: usize = 24;
const FOOT: f32 = 23.;
const DOCK_MIN: f32 = 96.;
const DOCK_MAX: f32 = 320.;
const PAGE_MIN_WITH_DOCK: f32 = 64.;
const WINDOW_FRAME: bool = cfg!(windows);
const CAPTION_BUTTON_WIDTH: f32 = 59.;
const CAPTION_BUTTON_HEIGHT: f32 = 52.;
const TAB_STRIP: Color = Color(17, 16, 18, 255);
const NAVIGATION_BAR: Color = Color(26, 26, 28, 255);
const CHROME_PANEL: Color = Color(42, 41, 43, 255);
const CHROME_HOVER: Color = Color(54, 53, 55, 255);
const CHROME_LINE: Color = Color(55, 54, 55, 255);
const CHROME_TEXT: Color = Color(242, 241, 241, 255);
const CHROME_MUTED: Color = Color(169, 165, 167, 255);

struct WindowRegions {
    page: Rect,
    dock: Option<Rect>,
}

impl WindowRegions {
    #[cfg(test)]
    fn new(width: f32, height: f32, footer: f32, dock_open: bool) -> Self {
        Self::with_dock_height(width, height, footer, dock_open, None)
    }
    fn with_dock_height(
        width: f32,
        height: f32,
        footer: f32,
        dock_open: bool,
        preferred: Option<f32>,
    ) -> Self {
        // A mounted dock sits above a stable footer, including on the home page
        // where the status footer would otherwise overlay the dock.
        let dock_space = (height - TOP - footer.max(FOOT)).max(0.);
        let dock =
            (dock_open && width > 0. && dock_space >= DOCK_MIN + PAGE_MIN_WITH_DOCK).then(|| {
                let height = preferred
                    .unwrap_or((dock_space * 0.4).min(DOCK_MAX))
                    .clamp(DOCK_MIN, dock_space - PAGE_MIN_WITH_DOCK);
                Rect {
                    x: 0.,
                    y: TOP + dock_space - height,
                    width,
                    height,
                }
            });
        let page_bottom = dock.map_or(height - footer, |bounds| bounds.y);
        Self {
            page: Rect {
                x: 0.,
                y: TOP,
                width,
                height: (page_bottom - TOP).max(0.),
            },
            dock,
        }
    }

    fn viewport(&self, tab: &Tab) -> Viewport {
        Viewport {
            width: (self.page.width / tab.zoom).max(120.),
            height: (self.page.height / tab.zoom).max(1.),
            scroll_x: tab.scroll_x,
            scroll_y: tab.scroll,
        }
    }
}

/// Lane 09 supplies the dock shell via `run`. App owns visibility, reserved
/// bounds, and dispatch. It does not own panel controls, rows or data.
pub(crate) trait DevtoolsDock {
    fn draw(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, frame: DockFrame);
    fn input(&mut self, frame: DockFrame, input: DockInput);
}

#[derive(Clone, Copy)]
pub(crate) struct DockFrame {
    /// Logical window pixels, independent of document zoom. Canvas is clipped
    /// to these bounds × scale; any additional clips must stay within them.
    pub bounds: Rect,
    pub scale: f32,
    pub focused: bool,
    /// Logical pixels relative to the dock origin; absent outside or occluded.
    pub pointer: Option<(f32, f32)>,
    pub tab: u64,
    pub document_navigation: u64,
}

pub(crate) enum DockInput {
    /// Left press; position is in `frame.pointer`. App owns release routing.
    Click,
    /// Positive logical window pixels scroll down, independent of page zoom.
    Wheel(f32),
    /// Non-browser key press while the dock owns keyboard input; no IME/text.
    Key {
        key: Key,
        modifiers: ModifiersState,
        repeat: bool,
    },
}

fn physical_rect(scale: f32, rect: Rect) -> Rect {
    scaled_rect(scale, rect.x, rect.y, rect.width, rect.height)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Address,
    Back,
    Forward,
    Reload,
    Home,
    NewTab,
    Select(usize),
    Close(usize),
    Settings,
    Version,
    Bookmarks,
    ShortcutHelp,
    Menu,
    TabHistory,
    MenuItem(usize),
    BookmarkItem(usize),
    Minimize,
    Maximize,
    CloseWindow,
}

impl Action {
    fn caption(self) -> bool {
        matches!(self, Self::Minimize | Self::Maximize | Self::CloseWindow)
    }

    fn in_menu(self) -> bool {
        matches!(self, Self::Menu | Self::MenuItem(_) | Self::BookmarkItem(_))
    }
}

const MENU_ITEMS: &[(Action, &str)] = &[
    (Action::Home, "Start page"),
    (Action::NewTab, "New tab"),
    (Action::Settings, "Settings"),
    (Action::Bookmarks, "Bookmarks"),
    (Action::TabHistory, "Tab history"),
    (Action::Version, "About Satin"),
    (Action::ShortcutHelp, "Keyboard shortcuts"),
];

struct ActiveDownload {
    id: u64,
    cancel: CancelSignal,
    name: String,
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
    recovery: Recovery,
    navigation: u64,
    document_navigation: u64,
    // The viewport can still paint the previous page until the next response opens.
    displayed_navigation: u64,
    cancel: CancelSignal,
    download: Option<ActiveDownload>,
    download_status: String,
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
            title: "New tab".into(),
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
            recovery: Recovery::default(),
            navigation: 0,
            document_navigation: 0,
            displayed_navigation: 0,
            cancel: CancelSignal::new(),
            download: None,
            download_status: String::new(),
        }
    }

    fn install_page(&mut self, page: LoadedPage) {
        self.title = page.title();
        self.page = Some(Arc::new(page));
        self.layout = None;
        self.displayed_navigation = self.document_navigation;
    }

    fn inspected_page(&self) -> Option<&LoadedPage> {
        self.page
            .as_deref()
            .filter(|_| self.displayed_navigation == self.document_navigation)
    }

    fn sync_inspector(&self, dock: &mut crate::devtools::DevTools, frame: DockFrame) {
        if let Some(page) = self.inspected_page() {
            dock.sync_inspector(
                frame,
                Some(page),
                self.layout.as_ref(),
                self.runtime.as_ref(),
                self.document_session.as_ref(),
                &self.storage,
            );
        }
    }

    fn footer_height(&self) -> f32 {
        if self.is_home() { 0. } else { FOOT }
    }

    fn is_home(&self) -> bool {
        self.url.split(['#', '?']).next() == Some("about:home")
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

pub(crate) fn run(
    path: PathBuf,
    url: Option<String>,
    smoke_output: Option<PathBuf>,
    smoke_click: Option<String>,
    smoke_actions: Option<PathBuf>,
    file_picker_responses: Option<crate::file_picker::Responses>,
    smoke_size: Option<PhysicalSize<u32>>,
    devtools: Option<crate::devtools::DevTools>,
) -> Result<(), String> {
    let (profile, session) = Profile::open(path.clone())?;
    let settings = settings::Store::open(&path);
    let session = settings.initial_session(session);
    let bookmarks_path = path.join(bookmarks::FILE_NAME);
    let bookmarks = bookmarks::Bookmarks::open(bookmarks_path);
    let fonts = Fonts::new()?;
    let events = EventLoop::<()>::with_user_event()
        .build()
        .map_err(|e| e.to_string())?;
    events.set_control_flow(ControlFlow::Wait);
    let proxy = events.create_proxy();
    let (results, incoming) = mpsc::channel(16);
    let (jobs, network_observations) =
        Workers::new_observed(profile.cookies(), results, move || {
            let _ = proxy.send_event(());
        })?;
    let media_proxy = events.create_proxy();
    let media = Bridge::new(move || {
        let _ = media_proxy.send_event(());
    });
    let worker_proxy = events.create_proxy();
    let worker_wake = Arc::new(move || {
        let _ = worker_proxy.send_event(());
    });
    let mut tabs = session
        .tabs
        .into_iter()
        .take(MAX_TABS)
        .enumerate()
        .map(|(i, u)| Tab::new(i as u64 + 1, u, profile.storage()))
        .collect::<Vec<_>>();
    if tabs.is_empty() {
        tabs.push(Tab::new(1, "about:home".into(), profile.storage()));
    }
    let active = session.active.min(tabs.len() - 1);
    if let Some(url) = url {
        tabs[active].url = url;
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
        bookmarks,
        settings,
        settings_panel: None,
        tabs,
        active,
        next_id,
        jobs,
        media,
        worker_wake,
        incoming,
        network_observations,
        mouse: (0., 0.),
        wheel: None,
        pointer_inside: false,
        devtools,
        devtools_open: false,
        dock_height: None,
        resizing_dock: false,
        devtools_focused: false,
        modifiers: ModifiersState::empty(),
        address: AddressBar::default(),
        address_focus: false,
        preedit: false,
        select_popup: None,
        hits: vec![],
        tab_hits_current: false,
        chrome_hover: None,
        chrome_menu: None,
        history_menu: None,
        caption_pressed: None,
        last_title_press: None,
        hover: None,
        status: String::new(),
        download_id: 0,
        scale: 1.,
        clipboard: arboard::Clipboard::new().ok(),
        smoke_output,
        smoke_size,
        smoke_click,
        smoke_actions: smoke_actions
            .as_deref()
            .map(crate::testing::read_actions)
            .transpose()?
            .unwrap_or_default(),
        smoke_wait_deadline: None,
        smoke_checkpoint_document: None,
        smoke_checkpoint_sample: None,
        smoke_checkpoint_last: None,
        smoke_checkpoint_outputs: CheckpointOutputs::default(),
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
        let tab = &app.tabs[app.active];
        let mut summary = serde_json::json!({
                "window_smoke": true,
                "version": env!("CARGO_PKG_VERSION"),
                "frames_presented": app.presented_frames,
                "url": tab.url,
                "title": tab.title,
                "layout_limited": tab.layout.as_ref().is_some_and(|layout| layout.limited),
                "paint_limited": app.canvas.as_ref().is_some_and(|canvas| canvas.limited),
                "media": tab.runtime.as_ref().map(ScriptRuntime::media_status).unwrap_or_default(),
                "javascript": {
                    "diagnostics": tab.runtime.as_ref().map(ScriptRuntime::diagnostics).unwrap_or_default()
                }
        });
        if let Some(checkpoints) = app.smoke_checkpoint_outputs.summary() {
            summary["checkpoints"] = checkpoints;
        }
        println!("{summary}");
        app.smoke_checkpoint_outputs.commit();
    }
    Ok(())
}

struct App {
    smoke_output: Option<PathBuf>,
    smoke_size: Option<PhysicalSize<u32>>,
    smoke_click: Option<String>,
    smoke_actions: VecDeque<crate::testing::SmokeAction>,
    smoke_wait_deadline: Option<Instant>,
    smoke_checkpoint_document: Option<CheckpointDocument>,
    smoke_checkpoint_sample: Option<CheckpointSample>,
    smoke_checkpoint_last: Option<Checkpoint>,
    smoke_checkpoint_outputs: CheckpointOutputs,
    file_picker_responses: Option<crate::file_picker::Responses>,
    smoke_result: Option<Result<(), String>>,
    presented_frames: u64,
    last_present_error: Option<String>,
    window: Option<Rc<Window>>,
    graphics: Option<Graphics>,
    canvas: Option<Canvas>,
    fonts: Fonts,
    profile: Profile,
    bookmarks: Result<bookmarks::Bookmarks, String>,
    settings: settings::Store,
    settings_panel: Option<settings::Panel>,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    jobs: Workers,
    media: Bridge,
    worker_wake: Arc<dyn Fn() + Send + Sync>,
    incoming: Receiver<Message>,
    network_observations: crate::jobs::devtools::Receiver,
    mouse: (f32, f32),
    wheel: Option<scroll::WheelGesture>,
    pointer_inside: bool,
    devtools: Option<crate::devtools::DevTools>,
    devtools_open: bool,
    dock_height: Option<f32>,
    resizing_dock: bool,
    devtools_focused: bool,
    modifiers: ModifiersState,
    address: AddressBar,
    address_focus: bool,
    preedit: bool,
    select_popup: Option<SelectPopup>,
    hits: Vec<(Rect, Action)>,
    tab_hits_current: bool,
    chrome_hover: Option<Action>,
    chrome_menu: Option<Menu>,
    history_menu: Option<history_ui::HistoryMenu>,
    caption_pressed: Option<Action>,
    last_title_press: Option<(Instant, f32, f32)>,
    hover: Option<String>,
    status: String,
    download_id: u64,
    scale: f32,
    clipboard: Option<arboard::Clipboard>,
}
impl App {
    fn window_regions(&self, index: usize) -> WindowRegions {
        let (width, height) = self.window.as_ref().map_or((1280., 800.), |window| {
            let size = window.inner_size();
            (
                size.width as f32 / self.scale,
                size.height as f32 / self.scale,
            )
        });
        WindowRegions::with_dock_height(
            width,
            height,
            self.tabs[index].footer_height(),
            self.devtools_open,
            self.dock_height,
        )
    }

    fn tab_viewport(&self, index: usize) -> Viewport {
        self.window_regions(index).viewport(&self.tabs[index])
    }

    fn pointer_in(&self, rect: Rect) -> bool {
        self.pointer_inside && rect.contains(self.mouse.0, self.mouse.1)
    }

    fn dock_frame(&self, bounds: Rect) -> DockFrame {
        let tab = &self.tabs[self.active];
        let pointer = (self.pointer_in(bounds)
            && self.settings_panel.is_none()
            && self.history_menu.is_none()
            && self.chrome_menu.is_none()
            && self.frame_resize_at_pointer().is_none())
        .then_some((self.mouse.0 - bounds.x, self.mouse.1 - bounds.y));
        DockFrame {
            bounds,
            scale: self.scale,
            focused: self.devtools_focused,
            pointer,
            tab: tab.id,
            document_navigation: tab.document_navigation,
        }
    }

    fn send_dock_input(&mut self, frame: DockFrame, input: DockInput) {
        // Clear must not leave queued observations that immediately refill the view.
        self.sync_console(self.active);
        self.drain_network_observations();
        self.sync_inspector(frame);
        if let Some(dock) = &mut self.devtools {
            dock.input(frame, input);
            self.run_inspector_action(frame);
            self.redraw();
        }
    }

    fn sync_inspector(&mut self, frame: DockFrame) {
        if let Some(dock) = &mut self.devtools {
            self.tabs[self.active].sync_inspector(dock, frame);
        }
    }

    fn run_inspector_action(&mut self, frame: DockFrame) {
        let edit = self
            .devtools
            .as_mut()
            .and_then(|d| d.take_style_edit(frame));
        if edit.is_none()
            && !self
                .devtools
                .as_ref()
                .is_some_and(|d| d.console_has_command(frame))
        {
            return;
        }
        let index = self.active;
        let viewport = self.tab_viewport(index);
        let tab = &mut self.tabs[index];
        let has_page = tab.inspected_page().is_some();
        if has_page
            && tab.runtime.is_none()
            && let (Some(page), Some(session)) = (&tab.page, &mut tab.document_session)
        {
            if let Err(error) = session.ensure_realm(page, &mut tab.runtime, viewport) {
                self.status = error;
            }
            if let Some(runtime) = &mut tab.runtime {
                runtime.set_worker_wake(self.worker_wake.clone());
            }
        }
        let Some(dock) = self.devtools.as_mut() else {
            return;
        };
        let runtime = tab.runtime.as_mut().filter(|_| has_page);
        let updated = if let Some((node, key, value, important)) = edit {
            let result = runtime
                .ok_or_else(|| String::from("Document is unavailable"))
                .and_then(|r| r.set_inspected_style(node, &key, &value, important));
            let ok = result.is_ok();
            dock.style_edit_result(frame, result);
            ok
        } else {
            dock.run_console_command(frame, runtime)
        };
        if updated {
            self.sync_runtime(index);
            self.dispatch_fetches(index);
            self.dispatch_forms(index);
            self.ensure_layout();
            self.set_title();
        }
    }

    fn dock_text(&mut self, frame: DockFrame, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(dock) = &mut self.devtools {
            if dock.is_console() {
                dock.console_text(frame, text);
            } else {
                dock.input(
                    frame,
                    DockInput::Key {
                        key: Key::Character(text.into()),
                        modifiers: ModifiersState::empty(),
                        repeat: false,
                    },
                );
            }
        }
        self.redraw();
    }

    fn dock_key(&mut self, frame: DockFrame, key: Key, text: Option<&str>, repeat: bool) {
        let ctrl = self.modifiers == ModifiersState::CONTROL;
        if ctrl && key == Key::Named(NamedKey::Enter) {
            let tab = &self.tabs[self.active];
            if let Some(page) = tab.inspected_page()
                && self.devtools.as_mut().is_some_and(|d| {
                    d.reveal_console_source(
                        frame,
                        Some(page),
                        tab.runtime.as_ref(),
                        tab.document_session.as_ref(),
                    )
                })
            {
                self.redraw();
                return;
            }
        }
        if ctrl && let Key::Character(c) = &key {
            if c.eq_ignore_ascii_case("v") {
                if let Some(value) = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
                    self.dock_text(frame, &value);
                }
                return;
            }
            if (c.eq_ignore_ascii_case("c") || c.eq_ignore_ascii_case("x"))
                && let (Some(dock), Some(cb)) = (&mut self.devtools, &mut self.clipboard)
                && let Some(value) = dock.clipboard_selection(frame, c.eq_ignore_ascii_case("x"))
            {
                let _ = cb.set_text(value);
                self.redraw();
                return;
            }
        }
        let printable = matches!(key, Key::Character(_) | Key::Named(NamedKey::Space))
            && !self.modifiers.super_key()
            && (!self.modifiers.control_key() && !self.modifiers.alt_key()
                || self.modifiers.control_key() && self.modifiers.alt_key());
        if self.devtools.as_ref().is_some_and(|d| d.is_console()) && printable {
            if !self.preedit
                && let Some(value) = text
            {
                self.dock_text(frame, value);
            }
            return;
        }
        self.send_dock_input(
            frame,
            DockInput::Key {
                key,
                modifiers: self.modifiers,
                repeat,
            },
        );
    }

    fn page_node_under_pointer(&self) -> Option<rust_browser_engine::dom::NodeId> {
        let tab = &self.tabs[self.active];
        tab.inspected_page()?;
        if !self.pointer_in(self.window_regions(self.active).page) {
            return None;
        }
        tab.layout.as_ref()?.element_at(
            self.mouse.0 / tab.zoom + tab.scroll_x,
            (self.mouse.1 - TOP) / tab.zoom + tab.scroll,
        )
    }

    fn sync_console(&mut self, index: usize) -> bool {
        let tab = &self.tabs[index];
        let (Some(dock), Some(runtime)) = (&mut self.devtools, &tab.runtime) else {
            return false;
        };
        dock.sync_console(tab.id, tab.document_navigation, runtime)
    }

    fn drain_network_observations(&mut self) -> bool {
        let Some(dock) = &mut self.devtools else {
            return false;
        };
        let active = self.tabs[self.active].id;
        let dropped = self.network_observations.take_dropped();
        dock.note_observer_drops(dropped);
        let mut redraw = dropped != 0;
        for _ in 0..crate::jobs::devtools::CAPACITY {
            let Some(event) = self.network_observations.try_recv() else {
                break;
            };
            let visible = event.key.tab == active;
            redraw |= dock.observe_network(event) && visible;
        }
        redraw && self.devtools_open
    }

    fn set_dock_focus(&mut self, focused: bool) {
        if self.devtools_focused != focused {
            self.devtools_focused = focused;
            self.preedit = false;
            if let Some(window) = &self.window {
                window.set_ime_allowed(self.settings_panel.is_none());
            }
            self.redraw();
        }
    }

    fn toggle_devtools(&mut self) {
        if self.devtools.is_none() {
            return;
        }
        self.devtools_open = !self.devtools_open;
        self.resizing_dock = false;
        if !self.devtools_open
            && let Some(dock) = &mut self.devtools
        {
            dock.picking = false;
        }
        self.dismiss_history_menu();
        self.chrome_menu = None;
        self.select_popup = None;
        self.hover = None;
        self.chrome_hover = None;
        self.caption_pressed = None;
        self.last_title_press = None;
        let visible = self.window_regions(self.active).dock.is_some();
        self.set_dock_focus(visible && self.settings_panel.is_none());
        self.ensure_layout();
        if self.pointer_inside {
            self.update_pointer(false);
        }
        self.redraw();
    }

    fn window_geometry_changed(&mut self) {
        self.wheel = None;
        self.tab_hits_current = false;
        self.select_popup = None;
        self.hover = None;
        if self.window_regions(self.active).dock.is_none() {
            self.set_dock_focus(false);
        }
        self.ensure_layout();
        self.redraw();
    }

    fn start_page(&mut self, index: usize, opened: OpenedPage) {
        let history = self.tabs[index].history.clone();
        let storage = self.tabs[index].storage.clone();
        match DocumentSession::start_in_tab(opened, history, storage) {
            Ok((page, session)) => {
                let tab = &mut self.tabs[index];
                tab.url = page.url.clone();
                tab.install_page(page);
                tab.runtime = None;
                tab.document_session = Some(session);
                tab.loading = true;
            }
            Err(error) => self.fail_page(index, &error),
        }
    }

    fn fail_page(&mut self, index: usize, error: &str) {
        self.sync_console(index);
        if index == self.active {
            self.select_popup = None;
            self.hover = None;
        }
        let tab = &mut self.tabs[index];
        self.media.close_page(tab.id, tab.document_navigation);
        tab.cancel.cancel();
        tab.navigation = tab.navigation.wrapping_add(1);
        tab.document_navigation = tab.navigation;
        tab.recovery = tab.recovery.failed();
        let page = tab.recovery.page(&tab.url, error);
        tab.install_page(page);
        tab.runtime = None;
        tab.document_session = None;
        tab.loading = false;
        tab.history_scroll = None;
        tab.scroll = 0.;
        tab.scroll_x = 0.;
        tab.nested_scroll.clear();
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
        let new_realm = tab.runtime.is_none();
        match session.advance(Arc::make_mut(page), &mut tab.runtime, viewport) {
            Ok(changed) => {
                if new_realm && let Some(runtime) = &mut tab.runtime {
                    runtime.set_worker_wake(self.worker_wake.clone());
                }
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

    fn apply_media_updates(&mut self) -> bool {
        let mut redraw = false;
        for update in self.media.drain() {
            if let Some((index, tab)) = self.tabs.iter_mut().enumerate().find(|(_, tab)| {
                tab.id == update.key.tab && tab.document_navigation == update.key.navigation
            }) && let Some(runtime) = &mut tab.runtime
            {
                redraw |= Bridge::apply(runtime, update) && index == self.active;
            }
        }
        redraw
    }

    fn sync_runtime(&mut self, index: usize) -> bool {
        let console_changed = self.sync_console(index);
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
        changed || (self.devtools_open && console_changed)
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
        let tab = &self.tabs[self.active];
        self.address.set_text(if tab.is_home() {
            String::new()
        } else {
            tab.url.clone()
        });
    }
    fn set_title(&self) {
        if let Some(w) = &self.window {
            w.set_title(&format!(
                "{} — Satin · Alpha 0.1",
                self.tabs[self.active].title
            ));
        }
    }
    fn ensure_layout(&mut self) {
        if self.window.is_none() {
            return;
        }
        let regions = self.window_regions(self.active);
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
        if let Some(runtime) = &tab.runtime
            && let Some(request) = runtime.take_scroll_request()
        {
            tab.scroll = request.y;
            tab.scroll_x = request.x;
        }
        let mut viewport = regions.viewport(tab);
        let (width, height) = (viewport.width, viewport.height);
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
            tab.layout = Some(Arc::new(Layout::for_scrolling(
                page,
                &self.fonts,
                viewport,
                &tab.nested_scroll,
                tab.layout.as_deref(),
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
                tab.layout = Some(Arc::new(Layout::for_scrolling(
                    page,
                    &self.fonts,
                    viewport,
                    &tab.nested_scroll,
                    tab.layout.as_deref(),
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
        self.media.close_page(tab.id, tab.document_navigation);
        let mode = if mode == NavigationMode::Push && tab.history.url() == parsed {
            NavigationMode::Reload
        } else {
            mode
        };
        tab.history.visit(parsed.clone(), mode, scroll);
        tab.history_scroll = restore.or_else(|| parsed.fragment().map(|_| HistoryScroll::Fragment));
        if tab.recovery.is_error_page() {
            // Do not leave a stale retry action in the viewport during a new load.
            tab.page = None;
            tab.layout = None;
            tab.title.clone_from(&url);
            if index == self.active {
                self.hover = None;
            }
        }
        tab.recovery = Recovery::for_navigation(&parsed, request.as_ref());
        tab.cancel.cancel();
        tab.cancel = CancelSignal::new();
        tab.runtime = None;
        tab.document_session = None;
        tab.navigation += 1;
        tab.document_navigation = tab.navigation;
        if let Some(dock) = &mut self.devtools {
            dock.begin_navigation(tab.id, tab.document_navigation);
        }
        tab.loading = true;
        tab.scroll = 0.;
        tab.scroll_x = 0.;
        tab.nested_scroll.clear();
        tab.url = url.clone();
        if index == self.active {
            self.address_focus = false;
            self.status.clear();
        }
        if let Some(page) = shortcuts::help_page(&parsed) {
            // Browser-owned help is static: do not create a script realm for it.
            tab.install_page(page);
            tab.loading = false;
        } else if url.starts_with("about:") {
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
                self.status = "Other pages are still loading. Try again in a moment.".into();
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
        if index != self.active
            && let Some(dock) = &mut self.devtools
        {
            dock.picking = false;
        }
        if index >= self.tabs.len() {
            return;
        }
        self.tab_hits_current = false;
        self.active = index;
        self.select_popup = None;
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
        self.wheel = None;
        if !action.in_menu() {
            self.chrome_menu = None;
        }
        match action {
            Action::Address => {
                self.set_dock_focus(false);
                if let Some(runtime) = &mut self.tabs[self.active].runtime
                    && let Err(error) = runtime.focus_control(None)
                {
                    self.status = error.to_string();
                }
                self.sync_runtime(self.active);
                self.select_popup = None;
                self.address_focus = true;
                self.address.select_all();
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
                    // Stopping the download retains the document and its active media players.
                    t.loading = false;
                    if let (Some(session), Some(page)) = (&mut t.document_session, &mut t.page) {
                        session.stop(Arc::make_mut(page), t.runtime.as_mut());
                        t.title = page.title();
                        t.layout = None;
                    }
                    self.status = "Loading stopped".into();
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
            Action::Settings => {
                self.set_dock_focus(false);
                self.open_settings();
            }
            Action::Version => self.navigate("about:version".into(), true),
            Action::Bookmarks => self.show_bookmarks(),
            Action::ShortcutHelp => self.navigate(shortcuts::HELP_URL.into(), true),
            Action::TabHistory => self.open_history_menu(),
            Action::Menu => {
                self.select_popup = None;
                self.chrome_menu = self.chrome_menu.is_none().then_some(Menu::Main(0));
                self.redraw();
            }
            Action::MenuItem(index) => {
                if matches!(self.chrome_menu, Some(Menu::Main(_)))
                    && let Some((action, _)) = MENU_ITEMS.get(index).copied()
                {
                    self.action(action, event_loop);
                }
            }
            Action::BookmarkItem(index) => {
                if matches!(self.chrome_menu, Some(Menu::Bookmarks(_))) {
                    self.pick_bookmark_row(index);
                }
            }
            Action::Minimize => {
                if let Some(window) = &self.window {
                    window.set_minimized(true);
                }
            }
            Action::Maximize => {
                if let Some(window) = &self.window {
                    window.set_maximized(!window.is_maximized());
                }
            }
            Action::CloseWindow => self.close_window(event_loop),
            Action::NewTab => {
                if self.tabs.len() >= MAX_TABS {
                    self.status = "The alpha supports up to 24 tabs.".into();
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
                let tab = &self.tabs[i];
                self.media.close_page(tab.id, tab.document_navigation);
                tab.cancel.cancel();
                if let Some(dock) = &mut self.devtools {
                    dock.close_tab(tab.id);
                }
                if let Some(d) = self.tabs[i].download.take() {
                    d.cancel.cancel();
                }
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

    fn close_window(&mut self, event_loop: &ActiveEventLoop) {
        self.save();
        for tab in &self.tabs {
            tab.cancel.cancel();
        }
        event_loop.exit();
    }
    fn insert(&mut self, text: &str) {
        if self.devtools_focused
            && self.settings_panel.is_none()
            && let Some(bounds) = self.window_regions(self.active).dock
        {
            self.dock_text(self.dock_frame(bounds), text);
            return;
        }
        if self.settings_panel.is_some()
            || self.devtools_focused
            || self.history_menu.is_some()
            || matches!(self.chrome_menu, Some(Menu::Bookmarks(_)))
        {
            return;
        }
        if !self.address_focus {
            if let Some(runtime) = &mut self.tabs[self.active].runtime {
                if let Err(error) = runtime.insert_form_text(text) {
                    self.status = error.to_string();
                }
                self.after_form_interaction();
            }
            return;
        }
        if self.address.insert(text) {
            self.redraw();
        }
    }
    fn run_shortcut(&mut self, command: Command, event_loop: &ActiveEventLoop) {
        match command {
            Command::Address => self.action(Action::Address, event_loop),
            Command::NewTab => self.action(Action::NewTab, event_loop),
            Command::CloseTab => self.action(Action::Close(self.active), event_loop),
            Command::NextTab => self.activate((self.active + 1) % self.tabs.len()),
            Command::PreviousTab => {
                self.activate((self.active + self.tabs.len() - 1) % self.tabs.len())
            }
            // The keyboard reloads even if a load is in progress. The chrome
            // button's Action::Reload can instead stop the current load.
            Command::Reload => self.navigate(self.tabs[self.active].url.clone(), false),
            Command::Back => self.action(Action::Back, event_loop),
            Command::Forward => self.action(Action::Forward, event_loop),
            Command::Menu => self.action(Action::Menu, event_loop),
            Command::BookmarkPage => self.save_current_bookmark(),
            Command::ToggleDevtools => self.toggle_devtools(),
            Command::InspectElement => {
                if !self.devtools_open {
                    self.toggle_devtools();
                }
                if let Some(d) = &mut self.devtools {
                    d.toggle_picker();
                }
                self.set_dock_focus(true);
                self.redraw();
            }
            Command::ZoomIn | Command::ZoomOut | Command::ResetZoom => {
                self.select_popup = None;
                let tab = &mut self.tabs[self.active];
                tab.zoom = match command {
                    Command::ZoomIn => tab.zoom * 1.1,
                    Command::ZoomOut => tab.zoom / 1.1,
                    _ => 1.,
                }
                .clamp(0.5, 3.);
                tab.layout = None;
                self.ensure_layout();
                self.redraw();
            }
        }
    }

    fn key(&mut self, key: Key, text: Option<&str>, repeat: bool, event_loop: &ActiveEventLoop) {
        self.wheel = None;
        if key == Key::Named(NamedKey::Escape)
            && let Some(d) = &self.tabs[self.active].download
        {
            d.cancel.cancel();
            self.tabs[self.active].download_status = "Cancelling download…".into();
            self.redraw();
            return;
        }
        // F12 is window chrome, not a panel, address, or document key. Settings
        // can remain modal above the dock when it opens or closes underneath.
        if let Some(toggle) = shortcuts::devtools_toggle(&key, self.modifiers, repeat) {
            if toggle {
                self.run_shortcut(Command::ToggleDevtools, event_loop);
            }
            return;
        }
        if self.settings_panel.is_some() {
            self.settings_key(key);
            return;
        }
        if self.handle_history_key(&key) || self.bookmark_panel_key(&key) {
            return;
        }
        if let Some(Menu::Main(selected)) = self.chrome_menu {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.chrome_menu = None;
                    self.redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp | NamedKey::Tab) => {
                    let previous = key == Key::Named(NamedKey::ArrowUp)
                        || (key == Key::Named(NamedKey::Tab) && self.modifiers.shift_key());
                    let step = if previous { MENU_ITEMS.len() - 1 } else { 1 };
                    self.chrome_menu = Some(Menu::Main((selected + step) % MENU_ITEMS.len()));
                    self.redraw();
                    return;
                }
                Key::Named(NamedKey::Enter | NamedKey::Space) => {
                    self.action(Action::MenuItem(selected), event_loop);
                    return;
                }
                _ => {}
            }
        }
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
        if let Some(command @ (Command::InspectElement | Command::ToggleDevtools)) =
            shortcuts::command(&key, self.modifiers)
        {
            if !repeat {
                self.run_shortcut(command, event_loop);
            }
            return;
        }
        if self.devtools_focused
            && let Some(bounds) = self.window_regions(self.active).dock
            && (shortcuts::command(&key, self.modifiers).is_none()
                || self.devtools.as_ref().is_some_and(|d| d.is_console())
                    && self.modifiers == ModifiersState::CONTROL
                    && matches!(&key,Key::Character(c) if c.eq_ignore_ascii_case("l")))
        {
            self.dock_key(self.dock_frame(bounds), key, text, repeat);
            return;
        }
        if let Some(command) = shortcuts::command(&key, self.modifiers) {
            self.run_shortcut(command, event_loop);
            return;
        }
        if self.modifiers.alt_key() && WINDOW_FRAME && key == Key::Named(NamedKey::Space) {
            if let Some(window) = &self.window {
                window.show_window_menu(LogicalPosition::new(0., TAB_H));
            }
            return;
        }
        let ctrl = self.modifiers.control_key() && !self.modifiers.alt_key();
        if ctrl {
            if let Key::Character(c) = &key {
                match c.to_lowercase().as_str() {
                    "a" if self.address_focus => {
                        self.address.select_all();
                        self.redraw();
                        return;
                    }
                    "a" if !self.address_focus => {
                        self.send_form_key(FormKey::SelectAll);
                        return;
                    }
                    "c" => {
                        let value = if self.address_focus {
                            self.address.selected_text().map(str::to_owned)
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
                    "x" if self.address_focus => {
                        if let (Some(cb), Some(value)) =
                            (&mut self.clipboard, self.address.selected_text())
                            && cb.set_text(value).is_ok()
                        {
                            self.address.delete_selection();
                            self.redraw();
                        }
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
                    _ => {}
                }
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
                self.navigate(omnibox_target(self.address.text()), true)
            }
            Key::Named(edit @ (NamedKey::Backspace | NamedKey::Delete)) if self.address_focus => {
                self.address.delete(edit == NamedKey::Backspace);
                self.redraw();
            }
            Key::Named(
                direction @ (NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::Home
                | NamedKey::End),
            ) if self.address_focus => {
                let movement = match direction {
                    NamedKey::ArrowLeft => Movement::Left,
                    NamedKey::ArrowRight => Movement::Right,
                    NamedKey::Home => Movement::Start,
                    NamedKey::End => Movement::End,
                    _ => unreachable!(),
                };
                self.address
                    .move_caret(movement, self.modifiers.shift_key());
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
                let h = self.tab_viewport(self.active).height;
                self.scroll(h * 0.85);
            }
            Key::Named(NamedKey::PageUp) => {
                let h = self.tab_viewport(self.active).height;
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
        let monitoring =
            self.devtools_open && self.devtools.as_ref().is_some_and(|d| d.monitoring());
        let layout_started = monitoring.then(Instant::now);
        self.ensure_layout();
        let layout_time = layout_started.map(|t| t.elapsed());
        let mut canvas = self
            .canvas
            .take()
            .filter(|c| c.width == size.width && c.height == size.height)
            .unwrap_or_else(|| Canvas::new(size.width, size.height, Color::WHITE));
        let regions = self.window_regions(self.active);
        let dock_frame = regions.dock.map(|bounds| self.dock_frame(bounds));
        if let Some(frame) = dock_frame {
            self.sync_inspector(frame);
        }
        let picked_node = if self.devtools.as_ref().is_some_and(|d| d.picking) {
            self.page_node_under_pointer()
        } else {
            None
        };
        let tab = &self.tabs[self.active];
        if let Some(runtime) = &tab.runtime {
            runtime.publish_canvas_frames();
        }
        canvas.clear(tab.layout.as_ref().map_or(Color::WHITE, |l| l.background));
        let u = self.scale;
        let width = size.width as f32 / u;
        let height = size.height as f32 / u;
        let content_clip = physical_rect(u, regions.page);
        canvas.clip(content_clip);
        let paint_started = monitoring.then(Instant::now);
        if regions.page.height > 0.
            && let Some(layout) = &tab.layout
        {
            canvas.document_scaled(layout, &mut self.fonts, TOP * u, tab.scroll, u * tab.zoom);
        }
        if let (Some(frame), Some(dock)) = (dock_frame, &mut self.devtools) {
            let page = tab.inspected_page();
            if let (Some(page), Some(start), Some(layout_time)) = (page, paint_started, layout_time)
            {
                dock.sample(
                    tab.id,
                    layout_time,
                    start.elapsed(),
                    Some(page),
                    tab.layout.as_deref(),
                    tab.runtime.as_ref(),
                );
            }
            let highlight = if page.is_none() {
                None
            } else if dock.picking {
                picked_node.and_then(|n| tab.layout.as_ref()?.rect_for(n))
            } else {
                dock.selected_rect(tab.id)
            };
            if let Some(r) = highlight {
                let scale = u * tab.zoom;
                let rect = Rect {
                    x: (r.x - tab.scroll_x) * scale,
                    y: TOP * u + (r.y - tab.scroll) * scale,
                    width: r.width * scale,
                    height: r.height * scale,
                };
                canvas.fill_rect(rect, Color(111, 169, 252, 48));
                for line in [
                    Rect {
                        height: 2. * u,
                        ..rect
                    },
                    Rect {
                        width: 2. * u,
                        ..rect
                    },
                    Rect {
                        y: rect.y + rect.height - 2. * u,
                        height: 2. * u,
                        ..rect
                    },
                    Rect {
                        x: rect.x + rect.width - 2. * u,
                        width: 2. * u,
                        ..rect
                    },
                ] {
                    canvas.fill_rect(line, Color(126, 178, 253, 230));
                }
            }
            canvas.clip(physical_rect(u, frame.bounds));
            dock.draw(&mut canvas, &mut self.fonts, frame);
        }
        let full_canvas = Rect {
            x: 0.,
            y: 0.,
            width: size.width as f32,
            height: size.height as f32,
        };
        canvas.clip(full_canvas);
        let rect = |x: f32, y: f32, w: f32, h: f32| Rect {
            x: x * u,
            y: y * u,
            width: w * u,
            height: h * u,
        };
        self.draw_chrome(
            &mut canvas,
            width,
            height,
            window.is_maximized(),
            dock_frame.is_some(),
        );
        if let Some(popup) = self.select_popup
            && let Some(control) = self.tabs[self.active]
                .page
                .as_ref()
                .and_then(|page| page.form_controls.get(&popup.node))
        {
            canvas.clip(content_clip);
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
            canvas.clip(full_canvas);
        }
        if let Some(panel) = &self.settings_panel {
            panel.draw(&mut canvas, &mut self.fonts, u, &self.settings);
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
                self.tab_hits_current = true;
                self.presented_frames += 1;
                self.last_present_error = None;
            }
            Err(e) => {
                self.tab_hits_current = false;
                self.last_present_error = Some(e);
            }
        }
        self.canvas = Some(canvas);
    }

    fn draw_chrome(
        &mut self,
        canvas: &mut Canvas,
        width: f32,
        height: f32,
        maximized: bool,
        dock_visible: bool,
    ) {
        let u = self.scale;
        canvas.fill_rect(scaled_rect(u, 0., 0., width, TAB_H), TAB_STRIP);
        canvas.fill_rect(
            scaled_rect(u, 0., TAB_H, width, TOP - TAB_H),
            NAVIGATION_BAR,
        );
        canvas.fill_rect(scaled_rect(u, 0., TAB_H - 1., width, 1.), CHROME_LINE);
        canvas.fill_rect(
            scaled_rect(u, 0., TOP - 1., width, 1.),
            Color(36, 35, 36, 255),
        );
        if WINDOW_FRAME && !maximized {
            canvas.fill_rect(scaled_rect(u, 0., 0., width, 1.), Color(39, 37, 39, 255));
        }
        self.hits.clear();
        self.draw_tabs(canvas, width);
        if WINDOW_FRAME {
            self.draw_caption_buttons(canvas, width, maximized);
        }
        self.draw_toolbar(canvas, width);
        self.draw_footer(canvas, width, height, dock_visible);
        self.draw_chrome_menu(canvas, width);
        self.draw_history_menu(canvas, width);
    }

    fn draw_tabs(&mut self, canvas: &mut Canvas, width: f32) {
        let u = self.scale;
        let caption = if WINDOW_FRAME {
            CAPTION_BUTTON_WIDTH * 3.
        } else {
            0.
        };
        let strip = TabStrip::new(width, caption, self.tabs.len(), self.active);
        let compact = strip.slot < 62.;
        let full_canvas = scaled_rect(1., 0., 0., canvas.width as f32, canvas.height as f32);
        let pointer = self.chrome_hover;

        // The normal active shoulders join the toolbar and overlap adjacent edges.
        for index in 0..self.tabs.len() {
            if index != self.active {
                let tab = strip.tab(index);
                let hover =
                    matches!(pointer, Some(Action::Select(i) | Action::Close(i)) if i == index);
                draw_tab_face(canvas, tab.x, tab.face, u, false, hover);
            }
        }
        let active_slot = strip.tab(self.active);
        if compact {
            // Don't paint the active shoulders over narrow neighbours' targets.
            canvas.clip(scaled_rect(
                u,
                active_slot.x - 2.,
                0.,
                active_slot.face + 4.,
                TAB_H,
            ));
        }
        draw_tab_face(canvas, active_slot.x, active_slot.face, u, true, true);
        if compact {
            canvas.clip(full_canvas);
        }

        for (index, tab) in self.tabs.iter().enumerate() {
            let slot = strip.tab(index);
            let select = slot.select();
            let x = slot.x;
            let close = Action::Close(index);
            let active = index == self.active;
            let hovered =
                matches!(pointer, Some(Action::Select(i) | Action::Close(i)) if i == index);
            let icon = if strip.slot >= 94. {
                built_in_site_icon(&tab.url)
            } else {
                None
            };
            let padding = if let Some(icon) = icon {
                canvas.image(scaled_rect(u, x + 14., 22., 24., 24.), icon);
                48.
            } else if compact {
                5.
            } else if active {
                24.
            } else {
                16.
            };
            let font = if compact { 12. } else { 17. } * u;
            let text_width = (select.end - x - padding - 3.) * u;
            if text_width >= 7. * u {
                let title = if tab.is_home() { "New tab" } else { &tab.title };
                let title = elide(&self.fonts, title, text_width, font);
                if !compact
                    || title != "…"
                    || self.fonts.shape(&title, font, false, false, false).advance <= text_width
                {
                    if compact {
                        canvas.clip(scaled_rect(u, x + padding, 0., text_width / u, TAB_H));
                    }
                    canvas.label(
                        &title,
                        (x + padding) * u,
                        40. * u,
                        font,
                        CHROME_TEXT,
                        false,
                        &mut self.fonts,
                    );
                    if compact {
                        canvas.clip(full_canvas);
                    }
                }
            }
            self.hits.push((
                Rect {
                    x,
                    y: 8.,
                    width: select.end - select.start,
                    height: TAB_H - 8.,
                },
                Action::Select(index),
            ));
            if let Some(target) = slot.close() {
                let close_x = target.start;
                let close_width = target.end - target.start;
                if pointer == Some(close) {
                    rounded_chrome_rect(
                        canvas,
                        scaled_rect(u, close_x, 21., close_width, 25.),
                        8. * u,
                        CHROME_HOVER,
                    );
                }
                let mark = 18.;
                let icon_scale = mark / 20.;
                let offset = if strip.slot >= 90. { 5. } else { 0. };
                chrome_icon(
                    canvas,
                    (close_x + (close_width - mark) / 2. - offset) / icon_scale,
                    (34. - mark / 2.) / icon_scale,
                    u * icon_scale,
                    close,
                    false,
                    if active || hovered {
                        CHROME_TEXT
                    } else {
                        CHROME_MUTED
                    },
                );
                self.hits.push((
                    Rect {
                        x: close_x,
                        y: 13.,
                        width: close_width,
                        height: TAB_H - 13.,
                    },
                    close,
                ));
            }
        }
        let plus_x = strip.new_tab_x;
        let button = Rect {
            x: plus_x,
            y: 12.,
            width: NEW_TAB_WIDTH,
            height: 43.,
        };
        if pointer == Some(Action::NewTab) {
            rounded_chrome_rect(
                canvas,
                scaled_rect(u, plus_x, 12., NEW_TAB_WIDTH, 43.),
                10. * u,
                CHROME_HOVER,
            );
        }
        chrome_icon(
            canvas,
            plus_x + 11.,
            23.,
            u,
            Action::NewTab,
            false,
            CHROME_TEXT,
        );
        self.hits.push((button, Action::NewTab));
    }

    fn draw_caption_buttons(&mut self, canvas: &mut Canvas, width: f32, maximized: bool) {
        let u = self.scale;
        for (index, action) in [Action::Minimize, Action::Maximize, Action::CloseWindow]
            .into_iter()
            .enumerate()
        {
            let x = width - CAPTION_BUTTON_WIDTH * (3 - index) as f32;
            let hovered = self.chrome_hover == Some(action);
            let pressed = self.caption_pressed == Some(action);
            if hovered || pressed {
                let color = match (action, pressed) {
                    (Action::CloseWindow, true) => Color(169, 20, 37, 255),
                    (Action::CloseWindow, false) => Color(194, 26, 46, 255),
                    (_, true) => CHROME_HOVER,
                    _ => CHROME_PANEL,
                };
                canvas.fill_rect(
                    scaled_rect(u, x, 1., CAPTION_BUTTON_WIDTH, CAPTION_BUTTON_HEIGHT - 1.),
                    color,
                );
            }
            chrome_icon(
                canvas,
                x + (CAPTION_BUTTON_WIDTH - 20.) / 2.,
                16.,
                u,
                action,
                maximized,
                CHROME_TEXT,
            );
            self.hits.push((
                Rect {
                    x,
                    y: 1.,
                    width: CAPTION_BUTTON_WIDTH,
                    height: CAPTION_BUTTON_HEIGHT - 1.,
                },
                action,
            ));
        }
    }

    fn draw_toolbar(&mut self, canvas: &mut Canvas, width: f32) {
        let u = self.scale;
        let tab = &self.tabs[self.active];
        let row_y = TAB_H + 9.;
        let row_height = 46.;
        let pointer = self.chrome_hover;
        for (x, action, enabled) in [
            (18., Action::Back, tab.history.can_traverse(-1)),
            (70., Action::Forward, tab.history.can_traverse(1)),
            (124., Action::Reload, true),
            (width - 162., Action::Home, true),
            (width - 110., Action::Version, true),
            (width - 59., Action::Menu, true),
        ] {
            if (enabled && pointer == Some(action))
                || (action == Action::Menu && self.chrome_menu.is_some())
            {
                rounded_chrome_rect(
                    canvas,
                    scaled_rect(u, x, row_y, 46., row_height),
                    10. * u,
                    CHROME_HOVER,
                );
            }
            chrome_icon(
                canvas,
                x + 13.,
                row_y + 13.,
                u,
                action,
                tab.loading,
                if enabled {
                    CHROME_TEXT
                } else {
                    Color(141, 137, 139, 255)
                },
            );
            self.hits.push((
                Rect {
                    x,
                    y: row_y,
                    width: 46.,
                    height: row_height,
                },
                action,
            ));
        }

        let address_x = 185.;
        let address_width = (width - 176. - address_x).max(80.);
        let address_rect = Rect {
            x: address_x,
            y: row_y,
            width: address_width,
            height: row_height,
        };
        let address_focus = self.address_focus && !self.devtools_focused;
        let border = if address_focus { 2. } else { 1. };
        let border_color = if address_focus {
            Color(132, 130, 132, 255)
        } else if pointer == Some(Action::Address) {
            Color(96, 94, 96, 255)
        } else {
            Color(68, 67, 69, 255)
        };
        rounded_chrome_rect(
            canvas,
            scaled_rect(u, address_x, row_y, address_width, row_height),
            23. * u,
            border_color,
        );
        rounded_chrome_rect(
            canvas,
            scaled_rect(
                u,
                address_x + border,
                row_y + border,
                address_width - 2. * border,
                row_height - 2. * border,
            ),
            (23. - border) * u,
            Color(25, 25, 27, 255),
        );
        chrome_icon(
            canvas,
            address_x + 23.,
            row_y + 13.,
            u,
            Action::Address,
            false,
            CHROME_TEXT,
        );

        let text_x = address_x + 62.;
        let available = (address_width - 81.).max(12.);
        let font_size = if available < 225. { 16. } else { 19. } * u;
        canvas.clip(scaled_rect(
            u,
            text_x,
            row_y + 4.,
            available,
            row_height - 8.,
        ));
        let address = self.address.text();
        let measure = |index| {
            self.fonts
                .shape(&address[..index], font_size, false, false, false)
                .advance
                / u
        };
        let caret = self.address.caret();
        let cursor_width = if address_focus { measure(caret) } else { 0. };
        let shift = (cursor_width - available + 8.).max(0.);
        let selected = self.address.selection();
        if address_focus && !selected.is_empty() {
            let start = if selected.start == caret {
                cursor_width
            } else {
                measure(selected.start)
            };
            let end = if selected.end == caret {
                cursor_width
            } else {
                measure(selected.end)
            };
            canvas.fill_rect(
                scaled_rect(u, text_x + start - shift, row_y + 10., end - start, 26.),
                Color(67, 87, 105, 255),
            );
        }
        let empty = address.is_empty();
        canvas.label(
            if empty {
                "Search or enter an address"
            } else {
                address
            },
            (text_x - shift) * u,
            (row_y + 30.) * u,
            font_size,
            if empty && address_focus {
                CHROME_MUTED
            } else {
                CHROME_TEXT
            },
            false,
            &mut self.fonts,
        );
        if address_focus && selected.is_empty() {
            canvas.fill_rect(
                scaled_rect(u, text_x + cursor_width - shift, row_y + 12., 1., 22.),
                CHROME_TEXT,
            );
        }
        canvas.clip(Rect {
            x: 0.,
            y: 0.,
            width: canvas.width as f32,
            height: canvas.height as f32,
        });
        self.hits.push((address_rect, Action::Address));
    }

    fn draw_footer(&mut self, canvas: &mut Canvas, width: f32, height: f32, dock_visible: bool) {
        let tab = &self.tabs[self.active];
        if !dock_visible
            && tab.footer_height() == 0.
            && self.status.is_empty()
            && tab.download_status.is_empty()
            && self.hover.is_none()
            && !tab.loading
        {
            return;
        }
        let u = self.scale;
        canvas.fill_rect(
            scaled_rect(u, 0., height - FOOT, width, FOOT),
            NAVIGATION_BAR,
        );
        canvas.fill_rect(scaled_rect(u, 0., height - FOOT, width, 1.), CHROME_LINE);
        let status = if tab.download.is_some() {
            tab.download_status.clone()
        } else if let Some(link) = &self.hover {
            link.clone()
        } else if !tab.download_status.is_empty() {
            tab.download_status.clone()
        } else if !self.status.is_empty() {
            self.status.clone()
        } else if tab.loading {
            "Loading…".into()
        } else if tab.layout.as_ref().is_some_and(|layout| layout.limited) {
            "Layout simplified for a deeply nested page.".into()
        } else if let Some(message) = tab
            .runtime
            .as_ref()
            .and_then(ScriptRuntime::last_diagnostic)
        {
            format!("JavaScript: {message}")
        } else {
            format!("Satin {}", env!("CARGO_PKG_VERSION"))
        };
        let text = elide(&self.fonts, &status, (width - 100.) * u, 11. * u);
        canvas.label(
            &text,
            12. * u,
            (height - 8.) * u,
            11. * u,
            CHROME_MUTED,
            false,
            &mut self.fonts,
        );
        let zoom = format!("{}%", (tab.zoom * 100.).round() as u32);
        canvas.label(
            &zoom,
            (width - 53.) * u,
            (height - 8.) * u,
            11. * u,
            CHROME_MUTED,
            false,
            &mut self.fonts,
        );
    }

    fn draw_chrome_menu(&mut self, canvas: &mut Canvas, width: f32) {
        let selected = match self.chrome_menu {
            Some(Menu::Main(selected)) => selected,
            Some(Menu::Bookmarks(_)) => return self.draw_bookmarks(canvas, width),
            None => return,
        };
        let u = self.scale;
        let x = width - 212.;
        let y = TOP + 4.;
        let height = MENU_ITEMS.len() as f32 * 35. + 12.;
        rounded_chrome_rect(
            canvas,
            scaled_rect(u, x, y, 200., height),
            10. * u,
            CHROME_LINE,
        );
        rounded_chrome_rect(
            canvas,
            scaled_rect(u, x + 1., y + 1., 198., height - 2.),
            9. * u,
            CHROME_PANEL,
        );
        for (index, (item, label)) in MENU_ITEMS.iter().enumerate() {
            let item_y = y + 6. + index as f32 * 35.;
            let action = Action::MenuItem(index);
            let bounds = Rect {
                x: x + 6.,
                y: item_y,
                width: 188.,
                height: 35.,
            };
            chrome_menu::draw_row_highlight(
                canvas,
                bounds,
                u,
                self.chrome_hover == Some(action),
                index == selected,
            );
            canvas.label(
                label,
                (x + 17.) * u,
                (item_y + 23.) * u,
                15. * u,
                CHROME_TEXT,
                false,
                &mut self.fonts,
            );
            if *item == Action::NewTab {
                let hint = shortcuts::label(Command::NewTab);
                let size = 13. * u;
                let advance = self.fonts.shape(hint, size, false, false, false).advance;
                canvas.label(
                    hint,
                    (x + 183.) * u - advance,
                    (item_y + 23.) * u,
                    size,
                    CHROME_MUTED,
                    false,
                    &mut self.fonts,
                );
            }
            self.hits.push((bounds, action));
        }
    }

    fn checkpoint_document(&self) -> CheckpointDocument {
        let tab = &self.tabs[self.active];
        CheckpointDocument {
            tab_id: tab.id,
            document_navigation: tab.document_navigation,
        }
    }

    fn checkpoint_snapshot(&self, checkpoint: &Checkpoint) -> Result<(&Document, &Layout), String> {
        let tab = &self.tabs[self.active];
        let (Some(page), Some(layout)) = (&tab.page, &tab.layout) else {
            return Err(checkpoint.error("no current document layout"));
        };
        if tab.recovery.is_error_page()
            || page.document.truncated
            || layout.limited
            || tab
                .runtime
                .as_ref()
                .is_some_and(|runtime| !runtime.layout_is_current(layout))
        {
            return Err(checkpoint.error("document or layout is incomplete"));
        }
        Ok((&page.document, layout))
    }

    fn checkpoint_observation(
        &self,
        checkpoint: &Checkpoint,
    ) -> Result<CheckpointObservation, String> {
        let (document, layout) = self.checkpoint_snapshot(checkpoint)?;
        checkpoint.observe(document, layout, self.tab_viewport(self.active))
    }

    fn arm_smoke_checkpoint(&mut self) -> Result<(), String> {
        if !matches!(self.smoke_actions.get(1), Some(SmokeAction::Checkpoint(_))) {
            return Ok(());
        }
        self.sync_runtime(self.active);
        self.ensure_layout();
        let Some(SmokeAction::Checkpoint(checkpoint)) = self.smoke_actions.get(1) else {
            unreachable!("refresh does not modify the smoke queue");
        };
        let identity = self.checkpoint_document();
        if let Some(expected) = self.smoke_checkpoint_document {
            checkpoint.require_document(expected, identity)?;
        }
        checkpoint.require_unmet(self.checkpoint_observation(checkpoint)?.is_match())?;
        self.smoke_checkpoint_document = Some(identity);
        self.smoke_checkpoint_sample = None;
        self.smoke_wait_deadline =
            Some(Instant::now() + Duration::from_millis(checkpoint.timeout_ms));
        Ok(())
    }

    fn finish_smoke_checkpoint(&mut self, event_loop: &ActiveEventLoop) {
        if !self.tabs[self.active].loading
            && self.smoke_checkpoint_document == Some(self.checkpoint_document())
            && self
                .smoke_wait_deadline
                .is_some_and(|deadline| Instant::now() < deadline)
        {
            self.sync_runtime(self.active);
            self.ensure_layout();
        }
        let result = (|| {
            let Some(SmokeAction::Checkpoint(checkpoint)) = self.smoke_actions.front() else {
                unreachable!("only called for checkpoints");
            };
            let identity = self.checkpoint_document();
            let expected = self
                .smoke_checkpoint_document
                .ok_or_else(|| checkpoint.error("checkpoint was not armed before input"))?;
            checkpoint.require_document(expected, identity)?;
            let deadline = self
                .smoke_wait_deadline
                .ok_or_else(|| checkpoint.error("checkpoint has no input deadline"))?;
            if Instant::now() >= deadline {
                return Err(checkpoint.timeout_error(self.smoke_checkpoint_sample));
            }
            let tab = &self.tabs[self.active];
            let sample = if tab.loading
                || tab.document_session.as_ref().is_some_and(|session| {
                    !session.done() || session.needs_advance(tab.runtime.as_ref())
                }) {
                CheckpointObservation::DocumentNotReady.into()
            } else {
                let (document, layout) = self.checkpoint_snapshot(checkpoint)?;
                checkpoint.sample_after_click(document, layout, self.tab_viewport(self.active))?
            };
            self.smoke_checkpoint_sample = Some(sample);
            if !sample.is_match() {
                return Ok(Some(deadline));
            }
            if Instant::now() >= deadline {
                return Err(checkpoint.timeout_error(self.smoke_checkpoint_sample));
            }
            checkpoint.capture(
                self.smoke_output
                    .as_deref()
                    .expect("native smoke is active"),
                &mut self.smoke_checkpoint_outputs,
                identity,
                &tab.url,
                &tab.title,
                &tab.page
                    .as_ref()
                    .expect("checkpoint matched a page")
                    .document,
            )?;
            Ok(None)
        })();
        match result {
            Ok(None) => {
                let Some(SmokeAction::Checkpoint(checkpoint)) = self.smoke_actions.pop_front()
                else {
                    unreachable!("completed a checkpoint");
                };
                self.smoke_checkpoint_last = Some(checkpoint);
                self.smoke_wait_deadline = None;
                event_loop.set_control_flow(ControlFlow::Poll);
            }
            Ok(Some(deadline)) => match event_loop.control_flow() {
                ControlFlow::Wait => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
                ControlFlow::WaitUntil(next) if deadline < next => {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
                _ => {}
            },
            Err(error) => {
                self.smoke_output = None;
                self.smoke_result = Some(Err(error));
                event_loop.set_control_flow(ControlFlow::Poll);
                event_loop.exit();
            }
        }
    }

    fn finish_smoke(&mut self, event_loop: &ActiveEventLoop) {
        if self.smoke_output.is_none() {
            return;
        }
        if matches!(self.smoke_actions.front(), Some(SmokeAction::Checkpoint(_))) {
            self.finish_smoke_checkpoint(event_loop);
            return;
        }
        let tab = &self.tabs[self.active];
        if let Some(SmokeAction::WaitTitle { value, timeout_ms }) = self.smoke_actions.front() {
            if &tab.title == value {
                self.smoke_actions.pop_front();
                self.smoke_wait_deadline = None;
                event_loop.set_control_flow(ControlFlow::Poll);
            } else {
                let now = Instant::now();
                let deadline = *self.smoke_wait_deadline.get_or_insert_with(|| {
                    now + Duration::from_millis((*timeout_ms).clamp(1, 30_000))
                });
                if now >= deadline {
                    self.smoke_output = None;
                    self.smoke_result = Some(Err(format!(
                        "Timed out waiting for native title {value:?}, got {:?}",
                        tab.title
                    )));
                    event_loop.set_control_flow(ControlFlow::Poll);
                    event_loop.exit();
                } else {
                    match event_loop.control_flow() {
                        ControlFlow::Wait => {
                            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                        }
                        ControlFlow::WaitUntil(next) if deadline < next => {
                            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                        }
                        _ => {}
                    }
                }
            }
            return;
        }
        if let Some(SmokeAction::ClickInRow(target)) = self.smoke_actions.front() {
            let timeout_ms = target.timeout_ms;
            let found = tab
                .page
                .as_ref()
                .map_or(Ok(None), |page| target.find(&page.document));
            match found {
                Ok(None) => {
                    let deadline = *self
                        .smoke_wait_deadline
                        .get_or_insert_with(|| Instant::now() + Duration::from_millis(timeout_ms));
                    if Instant::now() < deadline {
                        if matches!(event_loop.control_flow(), ControlFlow::Wait) {
                            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                        }
                        return;
                    }
                    self.smoke_output = None;
                    self.smoke_result =
                        Some(Err("Timed out waiting for the requested row control".into()));
                    event_loop.exit();
                    return;
                }
                Err(error) => {
                    self.smoke_output = None;
                    self.smoke_result = Some(Err(error));
                    event_loop.exit();
                    return;
                }
                Ok(Some(_)) => {
                    self.smoke_wait_deadline = None;
                }
            }
        }
        if tab.loading
            || self.media.has_pending(tab.id, tab.document_navigation)
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
        if !self.smoke_actions.is_empty() {
            let result = self.arm_smoke_checkpoint().and_then(|()| {
                let action = self.smoke_actions.pop_front().expect("action is pending");
                self.smoke_action(action, event_loop)
            });
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
            if let Some(checkpoint) = &self.smoke_checkpoint_last {
                let expected = self
                    .smoke_checkpoint_document
                    .expect("a completed checkpoint has a document");
                checkpoint.require_document(expected, self.checkpoint_document())?;
                if !self.checkpoint_observation(checkpoint)?.is_match() {
                    return Err(checkpoint.error("condition no longer matched at final frame"));
                }
            }
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
    fn chrome_action_at_pointer(&self) -> Option<Action> {
        self.hits.iter().rev().find_map(|(rect, action)| {
            let current = match action {
                Action::MenuItem(_) => matches!(self.chrome_menu, Some(Menu::Main(_))),
                Action::BookmarkItem(_) => matches!(self.chrome_menu, Some(Menu::Bookmarks(_))),
                _ => true,
            };
            (current && self.pointer_in(*rect)).then_some(*action)
        })
    }
    fn frame_resize_at_pointer(&self) -> Option<ResizeDirection> {
        let window = self.window.as_ref()?;
        if !self.pointer_inside || !WINDOW_FRAME || window.is_maximized() || !window.is_resizable()
        {
            return None;
        }
        let size = window.inner_size();
        let width = size.width as f32 / self.scale;
        let height = size.height as f32 / self.scale;
        let (x, y) = self.mouse;
        let horizontal = if x < 5. {
            -1
        } else if x >= width - 5. {
            1
        } else {
            0
        };
        let vertical = if y < 5. {
            -1
        } else if y >= height - 5. {
            1
        } else {
            0
        };
        use ResizeDirection::*;
        match (horizontal, vertical) {
            (-1, -1) => Some(NorthWest),
            (0, -1) => Some(North),
            (1, -1) => Some(NorthEast),
            (-1, 0) => Some(West),
            (1, 0) => Some(East),
            (-1, 1) => Some(SouthWest),
            (0, 1) => Some(South),
            (1, 1) => Some(SouthEast),
            _ => None,
        }
    }

    fn left_press(&mut self, event_loop: &ActiveEventLoop) {
        self.wheel = None;
        if !self.pointer_inside {
            return;
        }
        if self.settings_panel.is_some() {
            self.settings_press();
            return;
        }
        if self.handle_history_click() {
            return;
        }
        if let Some(edge) = self.frame_resize_at_pointer() {
            if let Some(window) = &self.window {
                let _ = window.drag_resize_window(edge);
            }
            return;
        }
        let action = self.chrome_action_at_pointer();
        // A keyboard tab switch/close or resize can precede the next frame.
        // Don't apply an old tab hit to the new layout or tab indices.
        if self.mouse.1 < TAB_H && !self.tab_hits_current && !action.is_some_and(Action::caption) {
            return;
        }
        let dismissed_menu = self.chrome_menu.is_some() && !action.is_some_and(Action::in_menu);
        if dismissed_menu {
            self.chrome_menu = None;
            self.redraw();
        }
        let regions = self.window_regions(self.active);
        if let Some(bounds) = regions.dock
            && self.pointer_in(bounds)
            && !action.is_some_and(Action::in_menu)
        {
            // An outside click dismisses an overlapping browser menu; it must
            // not also activate a dock control underneath.
            if dismissed_menu {
                return;
            }
            self.select_popup = None;
            self.last_title_press = None;
            self.caption_pressed = None;
            self.set_dock_focus(true);
            if self.mouse.1 - bounds.y < 5. {
                self.resizing_dock = true;
                return;
            }
            let frame = self.dock_frame(bounds);
            self.send_dock_input(frame, DockInput::Click);
            return;
        }
        if let Some(action) = action {
            self.last_title_press = None;
            if action.caption() {
                self.caption_pressed = Some(action);
                self.redraw();
            } else {
                self.action(action, event_loop);
            }
        } else if WINDOW_FRAME && self.mouse.1 < TAB_H {
            let (x, y) = self.mouse;
            let now = Instant::now();
            let double_click = self.last_title_press.is_some_and(|(time, left, top)| {
                now.duration_since(time) <= Duration::from_millis(500)
                    && (left - x).abs() <= 4.
                    && (top - y).abs() <= 4.
            });
            self.last_title_press = (!double_click).then_some((now, x, y));
            if double_click {
                self.action(Action::Maximize, event_loop);
            } else if let Some(window) = &self.window {
                let _ = window.drag_window();
            }
        } else {
            self.last_title_press = None;
            if self.pointer_in(regions.page) {
                if self.devtools.as_ref().is_some_and(|d| d.picking)
                    && let Some(bounds) = regions.dock
                {
                    let frame = self.dock_frame(bounds);
                    self.sync_inspector(frame);
                    if let Some(n) = self.page_node_under_pointer()
                        && let Some(dock) = &mut self.devtools
                    {
                        dock.inspect(frame, n);
                    }
                    self.set_dock_focus(true);
                    self.redraw();
                    return;
                }
                self.set_dock_focus(false);
                self.address_focus = false;
                self.content_click();
            } else if self.select_popup.take().is_some() {
                self.redraw();
            }
        }
    }

    fn left_release(&mut self, event_loop: &ActiveEventLoop) {
        if self.resizing_dock {
            self.resizing_dock = false;
            self.update_pointer(true);
            return;
        }
        if self.settings_panel.is_some() {
            self.settings_release();
            return;
        }
        if let Some(action) = self.caption_pressed.take() {
            if self.chrome_action_at_pointer() == Some(action) {
                self.action(action, event_loop);
            }
            self.redraw();
        }
    }

    fn pointer_link(&self) -> Option<String> {
        if self.settings_panel.is_some()
            || self.history_menu.is_some()
            || !self.pointer_in(self.window_regions(self.active).page)
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

    fn update_pointer(&mut self, redraw_dock: bool) {
        if self.settings_panel.is_some() {
            self.settings_cursor();
            return;
        }
        if self.update_history_pointer() {
            return;
        }
        let regions = self.window_regions(self.active);
        let divider = regions
            .dock
            .is_some_and(|r| self.pointer_in(r) && self.mouse.1 - r.y < 5.);
        let inspecting = regions.dock.is_some()
            && self.devtools.as_ref().is_some_and(|d| d.picking)
            && self.pointer_in(regions.page);
        if self.resizing_dock || divider || inspecting {
            self.hover = None;
            self.chrome_hover = None;
            if let Some(window) = &self.window {
                window.set_cursor(if inspecting {
                    CursorIcon::Crosshair
                } else {
                    CursorIcon::RowResize
                });
            }
            self.redraw();
            return;
        }
        let chrome = self.chrome_action_at_pointer();
        let link = if chrome.is_none() {
            self.pointer_link()
        } else {
            None
        };
        if self.chrome_hover != chrome || self.hover != link || redraw_dock {
            self.chrome_hover = chrome;
            self.hover = link;
            self.redraw();
        }
        if let Some(window) = &self.window {
            let history = &self.tabs[self.active].history;
            window.set_cursor(self.frame_resize_at_pointer().map_or_else(
                || match chrome {
                    Some(Action::Address) => CursorIcon::Text,
                    Some(Action::Back) if !history.can_traverse(-1) => CursorIcon::Default,
                    Some(Action::Forward) if !history.can_traverse(1) => CursorIcon::Default,
                    Some(action) if action.caption() => CursorIcon::Default,
                    Some(_) => CursorIcon::Pointer,
                    None if self.hover.is_some() => CursorIcon::Pointer,
                    None => CursorIcon::Default,
                },
                resize_cursor,
            ));
        }
    }

    fn smoke_target(&mut self, name: &str) -> Result<(), String> {
        self.ensure_layout();
        let tab = &self.tabs[self.active];
        let id = tab
            .page
            .as_ref()
            .and_then(|page| {
                page.document
                    .connected_nodes()
                    .find(|&id| page.document.attr(id, "id") == Some(name))
            })
            .ok_or_else(|| format!("Smoke target #{name} is missing"))?;
        self.smoke_click_node(id, &format!("#{name}"), false)
    }

    fn smoke_click_node(
        &mut self,
        id: rust_browser_engine::dom::NodeId,
        label: &str,
        check_hit: bool,
    ) -> Result<(), String> {
        let height = self.tab_viewport(self.active).height;
        let tab = &mut self.tabs[self.active];
        let layout = tab.layout.as_ref().ok_or("Missing native layout")?;
        let rect = layout
            .rect_for(id)
            .ok_or_else(|| format!("Smoke target {label} has no rendered box"))?;
        if rect.y < tab.scroll || rect.y + rect.height > tab.scroll + height {
            tab.scroll = rect.y.max(0.);
        }
        let x = rect.x + rect.width / 2.;
        let y = rect.y + rect.height.min(height) / 2.;
        if check_hit {
            let document = &tab.page.as_ref().ok_or("Missing native page")?.document;
            let hit = layout.element_at(x, y);
            if !std::iter::successors(hit, |n| document.nodes[n.0].parent).any(|n| n == id) {
                return Err(format!("Smoke target {label} is covered"));
            }
        }
        self.mouse = (
            (x - tab.scroll_x) * tab.zoom,
            TOP + (y - tab.scroll) * tab.zoom,
        );
        self.pointer_inside = true;
        self.set_dock_focus(false);
        self.content_click();
        Ok(())
    }
    fn smoke_action(
        &mut self,
        action: crate::testing::SmokeAction,
        event_loop: &ActiveEventLoop,
    ) -> Result<(), String> {
        match action {
            SmokeAction::Click { id } => self.smoke_target(&id),
            SmokeAction::ClickInRow(target) => {
                self.ensure_layout();
                let page = self.tabs[self.active]
                    .page
                    .as_ref()
                    .ok_or("Missing native page")?;
                let node = target
                    .find(&page.document)?
                    .ok_or("Smoke row disappeared before click")?;
                self.smoke_click_node(node, "row control", true)
            }
            SmokeAction::ClickAt { x, y } => {
                let window = self.window.as_ref().ok_or("Missing native window")?;
                let bounds = window.inner_size();
                if !(0. ..bounds.width as f32 / self.scale).contains(&x)
                    || !(0. ..bounds.height as f32 / self.scale).contains(&y)
                {
                    return Err("Click coordinates are outside the native window".into());
                }
                self.draw();
                self.mouse = (x, y);
                self.pointer_inside = true;
                self.left_press(event_loop);
                self.left_release(event_loop);
                Ok(())
            }
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
            SmokeAction::WaitTitle { .. } | SmokeAction::Checkpoint(_) => {
                unreachable!("the event loop handles waiting actions")
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
                    "F12" => NamedKey::F12,
                    "F2" => NamedKey::F2,
                    _ => {
                        let prior = self.modifiers;
                        self.modifiers = ModifiersState::empty();
                        self.modifiers.set(ModifiersState::CONTROL, control);
                        self.modifiers.set(ModifiersState::SHIFT, shift);
                        self.modifiers.set(ModifiersState::ALT, alt);
                        self.key(Key::Character(value.into()), None, false, event_loop);
                        self.modifiers = prior;
                        return Ok(());
                    }
                };
                let prior = self.modifiers;
                self.modifiers = ModifiersState::empty();
                self.modifiers.set(ModifiersState::CONTROL, control);
                self.modifiers.set(ModifiersState::SHIFT, shift);
                self.modifiers.set(ModifiersState::ALT, alt);
                self.key(Key::Named(named), None, false, event_loop);
                self.modifiers = prior;
                Ok(())
            }
        }
    }
    fn accept_download(
        &mut self,
        index: usize,
        response: crate::jobs::OfferedDownload,
        from_link: bool,
    ) {
        let tab = &mut self.tabs[index];
        if !from_link {
            tab.loading = false;
            // A response sent as an attachment must never acquire an executable document.
            tab.install_page(LoadedPage::from_html(
                "about:downloads",
                "<title>Download</title>",
            ));
        }
        if tab.download.is_some() {
            tab.download_status =
                "A download is already running in this tab. Esc cancels it.".into();
            return;
        }
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let path = match crate::downloads::destination(window, &response.response.suggested_name) {
            Ok(Some(path)) => path,
            Ok(None) => {
                tab.download_status = "Download cancelled".into();
                return;
            }
            Err(e) => {
                tab.download_status = format!("Could not save the download: {e}");
                return;
            }
        };
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.download_id += 1;
        let cancel = self
            .jobs
            .save_download(tab.id, self.download_id, path, response);
        tab.download_status = format!("Downloading {name} · Esc to cancel");
        tab.download = Some(ActiveDownload {
            id: self.download_id,
            cancel,
            name,
        });
    }

    fn content_click(&mut self) {
        if self.settings_panel.is_some() || !self.pointer_in(self.window_regions(self.active).page)
        {
            return;
        }
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
        let mut link = self.pointer_link();
        let tab = &mut self.tabs[self.active];
        if tab.recovery.is_retry_link(link.as_deref(), &tab.url) {
            let url = tab.url.clone();
            self.navigate(url, false);
            return;
        }
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
                let doc = self.tabs[self.active].page.as_ref().map(|p| &p.document);
                let hint = doc.and_then(|doc| {
                    std::iter::successors(target, |n| doc.nodes.get(n.0).and_then(|v| v.parent))
                        .find(|&n| matches!(doc.tag(n), "a" | "area"))
                        .and_then(|n| doc.attr(n, "download"))
                        .map(str::to_owned)
                });
                if let Some(hint) = hint
                    && matches!(request.url.scheme(), "http" | "https")
                    && request
                        .initiator
                        .as_ref()
                        .is_some_and(|source| source.origin() == request.url.origin())
                {
                    let t = &self.tabs[self.active];
                    if let Err(e) = self.jobs.submit(Job {
                        tab: t.id,
                        navigation: t.navigation,
                        cancel: t.cancel.clone(),
                        task: Task::DownloadLink(Box::new(request), hint),
                    }) {
                        self.status = e;
                    }
                    return;
                }
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
        let content = self.window_regions(self.active).page;
        let bottom = content.y + content.height;
        let width = content.width;
        let row_height = 24.;
        let rows = control
            .options
            .len()
            .min(12)
            .min((content.height / row_height) as usize);
        if rows == 0 || width <= 0. {
            self.select_popup = None;
            return;
        }
        let y = TOP + (element.y + element.height - tab.scroll) * tab.zoom;
        let popup_height = row_height * rows as f32;
        let y = if y + popup_height > bottom {
            y - element.height * tab.zoom - popup_height
        } else {
            y
        }
        .clamp(TOP, bottom - popup_height);
        let x = ((element.x - tab.scroll_x) * tab.zoom).clamp(0., (width - 120.).max(0.));
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
                self.status = format!("File selection failed. {error}");
                None
            }
        };
        let Some(runtime) = self.tabs[index].runtime.as_mut() else {
            return false;
        };
        if let Err(error) = runtime.complete_file_picker(&request, files) {
            self.status = format!("File selection failed. {error}");
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
            self.status = "Invalid form address".into();
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
            let height = self.tab_viewport(active).height;
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
        let mut redraw = self.apply_media_updates() | self.drain_network_observations();
        let mut deadline = self.tick_wheel(Instant::now());
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
            let tab = &mut self.tabs[index];
            if let Some(runtime) = &mut tab.runtime
                && self
                    .media
                    .dispatch(tab.id, tab.document_navigation, runtime)
            {
                redraw |= self.sync_runtime(index) && index == self.active;
            }
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
            self.status = format!("Site data could not be saved. {error}");
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
            .with_title("Satin · Alpha 0.1")
            .with_theme(Some(Theme::Dark))
            .with_inner_size(self.smoke_size.map_or_else(
                || {
                    if self.smoke_output.is_some() {
                        LogicalSize::new(1180., 840.).into()
                    } else {
                        initial_window_size(event_loop)
                    }
                },
                Size::Physical,
            ))
            .with_min_inner_size(LogicalSize::new(640., 400.));
        #[cfg(windows)]
        {
            attributes = attributes
                .with_decorations(false)
                .with_undecorated_shadow(true);
        }
        if self.smoke_output.is_some() {
            attributes = attributes.with_visible(false);
        }
        if let Ok(img) = image::load_from_memory(include_bytes!("../../../assets/satin.png")) {
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
        if self.drain_network_observations() {
            self.redraw();
        }
        for _ in 0..16 {
            let Ok(message) = self.incoming.try_recv() else {
                break;
            };
            if matches!(
                message.payload,
                Payload::DownloadProgress(_) | Payload::DownloadDone(_)
            ) {
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == message.tab)
                    && let Some(download) =
                        tab.download.as_ref().filter(|d| d.id == message.navigation)
                {
                    match message.payload {
                        Payload::DownloadProgress(n) => {
                            tab.download_status = format!(
                                "Downloading {} · {} bytes · Esc to cancel",
                                download.name, n
                            )
                        }
                        Payload::DownloadDone(result) => {
                            tab.download_status = match result {
                                Ok(n) => format!("Saved {} · {} bytes", download.name, n),
                                Err(e) => e,
                            };
                            tab.download = None;
                        }
                        _ => unreachable!(),
                    }
                    self.redraw();
                }
                continue;
            }
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
                Payload::DownloadOffer {
                    response,
                    from_link,
                } => self.accept_download(index, response, from_link),
                Payload::DownloadLinkError(error) => self.tabs[index].download_status = error,
                Payload::DownloadProgress(_) | Payload::DownloadDone(_) => unreachable!(),
                Payload::NavigationError(error) => self.fail_page(index, &error),
                Payload::MediaWake | Payload::WorkerWake => continue,
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
            WindowEvent::CloseRequested => self.close_window(event_loop),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(_) => self.window_geometry_changed(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let new_scale = scale_factor as f32;
                if self.pointer_inside {
                    self.mouse.0 *= self.scale / new_scale;
                    self.mouse.1 *= self.scale / new_scale;
                }
                self.scale = new_scale;
                self.window_geometry_changed();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::Focused(false) => {
                self.wheel = None;
                self.resizing_dock = false;
                self.preedit = false;
                self.dismiss_history_menu();
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } if event.state == ElementState::Pressed => {
                if is_synthetic && event.logical_key == Key::Named(NamedKey::F12) {
                    return;
                }
                let bookmarks_open = matches!(self.chrome_menu, Some(Menu::Bookmarks(_)));
                let changes_panel = match event.logical_key {
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        self.settings_panel.is_some()
                            || self.history_menu.is_some()
                            || bookmarks_open
                    }
                    Key::Named(NamedKey::Delete) => bookmarks_open,
                    _ => false,
                };
                if event.repeat && changes_panel {
                    return;
                }
                self.key(
                    event.logical_key,
                    event.text.as_deref(),
                    event.repeat,
                    event_loop,
                );
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.preedit = false;
                self.insert(&text);
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                self.preedit = self.settings_panel.is_none() && !text.is_empty();
            }
            WindowEvent::Ime(Ime::Disabled) => self.preedit = false,
            WindowEvent::CursorMoved { position, .. } => {
                let dock = self.window_regions(self.active).dock;
                let was_in_dock = dock.is_some_and(|bounds| self.pointer_in(bounds));
                self.mouse = (
                    position.x as f32 / self.scale,
                    position.y as f32 / self.scale,
                );
                self.pointer_inside = true;
                if self.resizing_dock
                    && let Some(rect) = dock
                {
                    self.dock_height = Some((rect.y + rect.height - self.mouse.1).max(DOCK_MIN));
                    self.ensure_layout();
                    self.redraw();
                }
                let in_dock = dock.is_some_and(|bounds| self.pointer_in(bounds));
                self.update_pointer(was_in_dock || in_dock);
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer_inside = false;
                self.mouse = (f32::NEG_INFINITY, f32::NEG_INFINITY);
                self.clear_history_hover();
                self.chrome_hover = None;
                self.caption_pressed = None;
                self.hover = None;
                self.redraw();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                if button == MouseButton::Left {
                    self.left_press(event_loop);
                } else if button == MouseButton::Right
                    && self.pointer_inside
                    && self.history_menu.is_none()
                    && WINDOW_FRAME
                    && self.settings_panel.is_none()
                    && self.mouse.1 < TAB_H
                    && self.chrome_action_at_pointer().is_none()
                    && let Some(window) = &self.window
                {
                    window.show_window_menu(LogicalPosition::new(self.mouse.0, self.mouse.1));
                } else if button == MouseButton::Middle
                    && let Some(url) = self.pointer_link()
                {
                    self.action(Action::NewTab, event_loop);
                    self.navigate(url, true);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.left_release(event_loop),
            WindowEvent::MouseWheel { delta, .. } => {
                if self.settings_panel.is_some() || self.history_menu.is_some() {
                    return;
                }
                let (delta, detent) = scroll::wheel_delta(delta, self.scale);
                let regions = self.window_regions(self.active);
                if let Some(bounds) = regions.dock
                    && self.pointer_in(bounds)
                {
                    let frame = self.dock_frame(bounds);
                    if frame.pointer.is_some() {
                        self.send_dock_input(frame, DockInput::Wheel(delta.y));
                    }
                } else if self.pointer_in(regions.page) {
                    self.queue_wheel(delta, detent);
                }
            }
            WindowEvent::DroppedFile(path) if self.settings_panel.is_none() => {
                self.dismiss_history_menu();
                if let Ok(url) = url::Url::from_file_path(path) {
                    self.navigate(url.into(), true);
                }
            }
            _ => {}
        }
    }
}

fn initial_window_size(event_loop: &ActiveEventLoop) -> Size {
    let factor = event_loop.primary_monitor().map_or(1., |monitor| {
        let size = monitor.size().to_logical::<f64>(monitor.scale_factor());
        ((size.width - 48.) / 1586.)
            .min((size.height - 64.) / 992.)
            .clamp(0.41, 1.)
    });
    LogicalSize::new(1586. * factor, 992. * factor).into()
}

fn resize_cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::NsResize,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::EwResize,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::NwseResize,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::NeswResize,
    }
}

fn scaled_rect(scale: f32, x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x: x * scale,
        y: y * scale,
        width: width * scale,
        height: height * scale,
    }
}

fn draw_tab_face(canvas: &mut Canvas, x: f32, width: f32, scale: f32, active: bool, hovered: bool) {
    // Shoulder offsets in logical pixels from the reference's 48 px tab face.
    const SHOULDER: [(f32, f32); 14] = [
        (0., 19.),
        (2., 13.),
        (4., 10.),
        (8., 6.),
        (12., 3.),
        (17., 2.),
        (22., 0.),
        (28., -1.),
        (32., -3.),
        (36., -5.),
        (40., -9.),
        (44., -14.),
        (46., -20.),
        (48., -24.),
    ];
    let first = (10. * scale).round() as u32;
    let end = ((TAB_H - if active { 0. } else { 1. }) * scale).round() as u32;
    let radius = (width * 0.45).min(19.);
    let shoulder_scale = (width / 100.).min(1.);
    for y in first..end {
        let row = (y as f32 - first as f32) / scale;
        let inset = if active {
            let next = SHOULDER
                .partition_point(|&(position, _)| position <= row)
                .clamp(1, SHOULDER.len() - 1);
            let (start, left) = SHOULDER[next - 1];
            let (stop, right) = SHOULDER[next];
            (left + (right - left) * ((row - start) / (stop - start)).clamp(0., 1.))
                * shoulder_scale
        } else {
            let rise = (radius - row - 0.5).max(0.);
            radius - (radius * radius - rise * rise).sqrt()
        };
        let left = ((x + inset) * scale).round();
        let right = ((x + width - inset) * scale).round();
        if right <= left {
            continue;
        }
        let fill = if active {
            let shade = 42 + (7. * (1. - ((row - 18.) / 19.).abs()).max(0.)).round() as u8;
            Color(shade, shade, shade + 1, 255)
        } else if hovered {
            CHROME_PANEL
        } else {
            Color(24, 23, 24, 255)
        };
        let outline = if active {
            let shade = (123. - row * 1.37).round() as u8;
            Color(shade, shade, shade + 1, 255)
        } else {
            Color(40, 38, 40, 255)
        };
        let strip = Rect {
            x: left,
            y: y as f32,
            width: right - left,
            height: 1.,
        };
        canvas.fill_rect(strip, outline);
        let line = scale.round().max(1.);
        if y as f32 - first as f32 >= line && strip.width > line * 2. {
            canvas.fill_rect(
                Rect {
                    x: left + line,
                    width: strip.width - line * 2.,
                    ..strip
                },
                fill,
            );
        }
    }
}

fn omnibox_target(input: &str) -> String {
    let input = input.trim();
    let authority = input.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.split(':').next().unwrap_or_default();
    let explicit = input.contains("://")
        || input
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
        || input.starts_with("about:")
        || input.starts_with("file:")
        || std::path::Path::new(input).is_absolute();
    let host_like = !authority.chars().any(char::is_whitespace)
        && (host.eq_ignore_ascii_case("localhost")
            || host.contains('.')
            || authority.starts_with('['));
    if input.is_empty() || explicit || host_like {
        return input.into();
    }
    let mut search = url::Url::parse("https://search.brave.com/search").expect("search URL");
    search.query_pairs_mut().append_pair("q", input);
    search.into()
}

fn rounded_chrome_rect(canvas: &mut Canvas, rect: Rect, radius: f32, color: Color) {
    let radius = radius
        .round()
        .clamp(0., (rect.width.min(rect.height) * 0.5).floor());
    canvas.fill_rect(
        Rect {
            y: rect.y + radius,
            height: rect.height - 2. * radius,
            ..rect
        },
        color,
    );
    for row in 0..radius as u32 {
        let row = row as f32;
        let dy = radius - row - 0.5;
        let inset = (radius - (radius * radius - dy * dy).sqrt()).round();
        let scanline = Rect {
            x: rect.x + inset,
            y: rect.y + row,
            width: rect.width - 2. * inset,
            height: 1.,
        };
        canvas.fill_rect(scanline, color);
        canvas.fill_rect(
            Rect {
                y: rect.y + rect.height - row - 1.,
                ..scanline
            },
            color,
        );
    }
}

fn chrome_icon(
    canvas: &mut Canvas,
    x: f32,
    y: f32,
    scale: f32,
    action: Action,
    loading: bool,
    color: Color,
) {
    let mut line = |a: f32, b: f32, c: f32, d: f32| {
        canvas.stroke_line(
            (x + a) * scale,
            (y + b) * scale,
            (x + c) * scale,
            (y + d) * scale,
            1.3 * scale,
            color,
        )
    };
    match action {
        Action::Back => paint_glyph(canvas, x, y, scale, icons::Glyph::Back, color),
        Action::Forward => paint_glyph(canvas, x, y, scale, icons::Glyph::Forward, color),
        Action::Address => paint_glyph(canvas, x, y, scale, icons::Glyph::Search, color),
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
        Action::Reload => paint_glyph(canvas, x, y, scale, icons::Glyph::Reload, color),
        Action::Close(_) => {
            line(4.5, 4.5, 15.5, 15.5);
            line(15.5, 4.5, 4.5, 15.5);
        }
        Action::NewTab => {
            line(2., 10., 18., 10.);
            line(10., 2., 10., 18.);
        }
        Action::Version => {
            for step in 0..24 {
                let a = step as f32 * std::f32::consts::TAU / 24.;
                let b = (step + 1) as f32 * std::f32::consts::TAU / 24.;
                line(
                    10. + 8. * a.cos(),
                    10. + 8. * a.sin(),
                    10. + 8. * b.cos(),
                    10. + 8. * b.sin(),
                );
            }
            line(10., 9., 10., 14.);
            line(10., 6.5, 10., 6.75);
        }
        Action::Minimize => line(4., 10., 16., 10.),
        Action::Maximize if loading => {
            line(7., 4., 16., 4.);
            line(16., 4., 16., 13.);
            line(4., 7., 13., 7.);
            line(13., 7., 13., 16.);
            line(13., 16., 4., 16.);
            line(4., 16., 4., 7.);
        }
        Action::Maximize => {
            line(4., 4., 16., 4.);
            line(16., 4., 16., 16.);
            line(16., 16., 4., 16.);
            line(4., 16., 4., 4.);
        }
        Action::CloseWindow => {
            line(4., 4., 16., 16.);
            line(16., 4., 4., 16.);
        }
        Action::Menu => {
            for offset in [3., 10., 17.] {
                line(offset, 9., offset, 10.);
            }
        }
        Action::Settings
        | Action::Select(_)
        | Action::Bookmarks
        | Action::ShortcutHelp
        | Action::TabHistory
        | Action::MenuItem(_)
        | Action::BookmarkItem(_) => {}
    }
}

fn paint_glyph(canvas: &mut Canvas, x: f32, y: f32, scale: f32, glyph: icons::Glyph, color: Color) {
    glyph.rasterize(
        x * scale,
        y * scale,
        scale,
        canvas.width,
        canvas.height,
        |x, y, coverage| {
            let alpha = (u16::from(color.3) * u16::from(coverage) + 127) / 255;
            canvas.fill_rect(
                Rect {
                    x: x as f32,
                    y: y as f32,
                    width: 1.,
                    height: 1.,
                },
                Color(color.0, color.1, color.2, alpha as u8),
            );
        },
    );
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

#[cfg(test)]
mod dock_tests {
    use super::*;

    #[test]
    fn sources_wait_for_the_new_document_even_when_reloading_the_same_url() {
        let url = "https://example.test/";
        let page = |css: &str| {
            let mut page = LoadedPage::from_html(url, "<main></main>");
            page.stylesheets = Arc::new(vec![Arc::from(css)]);
            page
        };
        let mut tab = Tab::new(1, url.into(), Default::default());
        let mut dock = crate::devtools::DevTools::default();
        let mut frame = DockFrame {
            bounds: WindowRegions::new(800., 600., FOOT, true).dock.unwrap(),
            scale: 1.,
            focused: true,
            pointer: None,
            tab: tab.id,
            document_navigation: tab.document_navigation,
        };
        dock.begin_navigation(tab.id, tab.document_navigation);
        tab.install_page(page(".previous {}"));
        dock.input(
            frame,
            DockInput::Key {
                key: Key::Character("4".into()),
                modifiers: ModifiersState::ALT,
                repeat: false,
            },
        );
        tab.sync_inspector(&mut dock, frame);
        assert_eq!(
            dock.clipboard_selection(frame, false).as_deref(),
            Some(".previous {}")
        );

        tab.navigation += 1;
        tab.document_navigation = tab.navigation;
        frame.document_navigation = tab.document_navigation;
        dock.begin_navigation(tab.id, tab.document_navigation);
        tab.loading = true;
        assert_eq!(
            tab.page.as_ref().unwrap().stylesheets[0].as_ref(),
            ".previous {}"
        );
        tab.sync_inspector(&mut dock, frame);
        assert!(dock.clipboard_selection(frame, false).is_none());
        assert!(tab.inspected_page().is_none());

        // A stopped or rejected download can leave the previous page painted indefinitely.
        tab.loading = false;
        tab.sync_inspector(&mut dock, frame);
        assert!(dock.clipboard_selection(frame, false).is_none());

        // A newly opened document is inspectable while its body is still loading.
        tab.loading = true;
        tab.install_page(page(".current {}"));
        tab.sync_inspector(&mut dock, frame);
        assert_eq!(
            dock.clipboard_selection(frame, false).as_deref(),
            Some(".current {}")
        );
    }

    #[test]
    fn page_and_dock_share_a_half_open_boundary_above_the_footer() {
        for footer in [0., FOOT] {
            let closed = WindowRegions::new(640., 400., footer, false);
            assert!(closed.dock.is_none());
            assert_eq!(closed.page.height, 400. - TOP - footer);

            let open = WindowRegions::new(640., 400., footer, true);
            let dock = open.dock.unwrap();
            assert_eq!(open.page.y + open.page.height, dock.y);
            assert_eq!(dock.y + dock.height, 400. - FOOT);
            assert!(open.page.contains(10., dock.y - 1.));
            assert!(!dock.contains(10., dock.y - 1.));
            assert!(!open.page.contains(10., dock.y));
            assert!(dock.contains(10., dock.y));
            assert!(!dock.contains(10., 400. - FOOT));
        }
    }

    #[test]
    fn dock_caps_height_and_suspends_when_the_page_would_be_too_small() {
        let large = WindowRegions::new(1180., 1600., FOOT, true);
        assert_eq!(large.dock.unwrap().height, DOCK_MAX);
        let minimum = TOP + FOOT + DOCK_MIN + PAGE_MIN_WITH_DOCK;
        let visible = WindowRegions::new(640., minimum, FOOT, true);
        assert_eq!(visible.page.height, PAGE_MIN_WITH_DOCK);
        assert_eq!(visible.dock.unwrap().height, DOCK_MIN);
        let small = WindowRegions::new(640., minimum - 1., FOOT, true);
        assert!(small.dock.is_none());
        assert_eq!(small.page.height, DOCK_MIN + PAGE_MIN_WITH_DOCK - 1.);
        let minimized = WindowRegions::new(0., 0., FOOT, true);
        assert!(minimized.dock.is_none());
        assert_eq!(minimized.page.height, 0.);
    }

    #[test]
    fn viewport_uses_page_bounds_at_nondefault_zoom_without_scaling_the_dock() {
        let mut tab = Tab::new(1, "about:home".into(), Default::default());
        tab.zoom = 2.;
        tab.scroll_x = 10.;
        tab.scroll = 20.;
        let open = WindowRegions::new(640., 400., tab.footer_height(), true);
        let viewport = open.viewport(&tab);
        assert_eq!(viewport.width, open.page.width / 2.);
        assert_eq!(viewport.height, open.page.height / 2.);
        assert_eq!((viewport.scroll_x, viewport.scroll_y), (10., 20.));
        let scale = 1.25;
        let clip = physical_rect(scale, open.page);
        let dock = physical_rect(scale, open.dock.unwrap());
        assert_eq!(clip.y + clip.height, dock.y);
    }
}
