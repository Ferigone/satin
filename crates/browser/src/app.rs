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
mod address_popup;
mod bookmark_menu;
mod bookmarks;
mod chrome_menu;
mod chrome_motion;
mod downloads_ui;
mod favorites_strip;
mod history_ui;
mod icons;
mod navigation_error;
mod quiet_cinder;
mod scroll;
mod settings;
mod tab_behavior;
mod tab_strip;
mod visit_store;
#[cfg(all(windows, target_arch = "x86_64"))]
mod update_ui;
use self::address_bar::{
    AddressBar, Movement,
    suggestions::{Direction as SuggestionDirection, Suggestions},
};
use self::bookmark_menu::Menu;
use crate::testing::{
    Checkpoint, CheckpointDocument, CheckpointObservation, CheckpointOutputs, CheckpointSample,
    SmokeAction,
};
use chrome_motion::{Button as MotionButton, ChromeMotion, TabInk};
use navigation_error::Recovery;
use tab_behavior::{ClosedTabs, FreshTab, TabId, TabMove};
use tab_strip::TabStrip;

const TAB_H: f32 = tab_strip::TAB_ROW_HEIGHT;
#[cfg(test)]
const TOP: f32 = quiet_cinder::CONTENT_TOP;
const MAX_TABS: usize = 24;
const FOOT: f32 = 23.;
const DOCK_MIN: f32 = 96.;
const DOCK_MAX: f32 = 320.;
const PAGE_MIN_WITH_DOCK: f32 = 64.;
const WINDOW_FRAME: bool = cfg!(windows);
const TAB_STRIP: Color = Color(0x22, 0x21, 0x24, 255);
const NAVIGATION_BAR: Color = Color(0x25, 0x24, 0x29, 255);
const CHROME_PANEL: Color = Color(0x29, 0x28, 0x2c, 255);
const CHROME_HOVER: Color = Color(0x33, 0x30, 0x38, 255);
const CHROME_LINE: Color = Color(0x3e, 0x3a, 0x42, 255);
const CHROME_TEXT: Color = Color(0xf0, 0xed, 0xf0, 255);
const CHROME_MUTED: Color = Color(0xb8, 0xaf, 0xb5, 255);
const CHROME_ICON: Color = Color(0xc6, 0xbb, 0xc3, 255);
const CHROME_ACCENT: Color = Color(0xc4, 0xa3, 0xf4, 255);
const CHROME_ACCENT_SOFT: Color = Color(0x44, 0x3d, 0x50, 255);

struct WindowRegions {
    page: Rect,
    dock: Option<Rect>,
}

impl WindowRegions {
    #[cfg(test)]
    fn new(width: f32, height: f32, footer: f32, dock_open: bool) -> Self {
        Self::with_dock_height(width, height, footer, dock_open, None, TOP)
    }
    fn with_dock_height(
        width: f32,
        height: f32,
        footer: f32,
        dock_open: bool,
        preferred: Option<f32>,
        content_top: f32,
    ) -> Self {
        // A mounted dock sits above a stable footer, including on the home page
        // where the status footer would otherwise overlay the dock.
        let dock_space = (height - content_top - footer.max(FOOT)).max(0.);
        let dock =
            (dock_open && width > 0. && dock_space >= DOCK_MIN + PAGE_MIN_WITH_DOCK).then(|| {
                let height = preferred
                    .unwrap_or((dock_space * 0.4).min(DOCK_MAX))
                    .clamp(DOCK_MIN, dock_space - PAGE_MIN_WITH_DOCK);
                Rect {
                    x: 0.,
                    y: content_top + dock_space - height,
                    width,
                    height,
                }
            });
        let page_bottom = dock.map_or(height - footer, |bounds| bounds.y);
        Self {
            page: Rect {
                x: 0.,
                y: content_top,
                width,
                height: (page_bottom - content_top).max(0.),
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
    Suggestion(usize),
    SuggestionBackground,
    Back,
    Forward,
    Reload,
    Home,
    NewTab,
    ReopenTab,
    DuplicateTab(TabId),
    MoveTab(TabId, TabMove),
    TabList,
    TabListItem(Option<TabId>),
    Select(TabId),
    Close(TabId),
    Settings,
    Version,
    Bookmarks,
    QuietMode,
    Downloads,
    ToggleBookmark,
    FavoriteManager,
    Favorite(usize),
    ShortcutHelp,
    StartShortcuts,
    Menu,
    TabHistory,
    VisitHistory,
    MenuItem(usize),
    BookmarkItem(usize),
    Minimize,
    Maximize,
    CloseWindow,
    #[cfg(all(windows, target_arch = "x86_64"))]
    UpdatePanel,
    #[cfg(all(windows, target_arch = "x86_64"))]
    UpdatePrimary(crate::updater::UpdateAction),
    #[cfg(all(windows, target_arch = "x86_64"))]
    UpdateSurface,
}

impl Action {
    fn address_suggestion(self) -> bool {
        matches!(self, Self::Suggestion(_) | Self::SuggestionBackground)
    }

    fn in_favorites(self) -> bool {
        matches!(self, Self::FavoriteManager | Self::Favorite(_))
    }

    fn caption(self) -> bool {
        matches!(self, Self::Minimize | Self::Maximize | Self::CloseWindow)
    }

    fn in_menu(self) -> bool {
        matches!(
            self,
            Self::Menu
                | Self::MenuItem(_)
                | Self::BookmarkItem(_)
                | Self::TabList
                | Self::TabListItem(_)
        )
    }
}

const MENU_ITEMS: &[(Action, &str)] = &[
    (Action::Home, "Start page"),
    (Action::NewTab, "New tab"),
    (Action::Settings, "Settings"),
    (Action::Bookmarks, "Bookmarks"),
    (Action::TabHistory, "Tab history"),
    (Action::VisitHistory, "Historia"),
    (Action::Version, "About Satin"),
    (Action::ShortcutHelp, "Keyboard shortcuts"),
    (Action::StartShortcuts, "Skróty startowe"),
    (Action::Downloads, "Pobrane pliki"),
    (Action::QuietMode, "Tryb skupienia"),
];

struct ActiveDownload {
    id: u64,
    cancel: CancelSignal,
    name: String,
}

enum DownloadUpdate {
    Progress(u64),
    Done(Result<u64, String>),
}

fn cancel_download(downloads: &mut downloads_ui::Downloads, tabs: &mut [Tab], id: u64) -> bool {
    for tab in tabs {
        if let Some(download) = &tab.download
            && download.id == id
            && !download.cancel.is_cancelled()
        {
            downloads.request_cancel(id);
            download.cancel.cancel();
            tab.download_status = "Cancelling download…".into();
            return true;
        }
    }
    false
}

fn apply_download_update(
    downloads: &mut downloads_ui::Downloads,
    tabs: &mut [Tab],
    tab_id: u64,
    transfer_id: u64,
    update: DownloadUpdate,
) -> bool {
    // The worker uses `Message.navigation` as a transfer ID for these two
    // events, not the current page's navigation ID. The session row survives
    // navigation, tab switches, and even tab closure while cancellation settles.
    let row_changed = match &update {
        DownloadUpdate::Progress(bytes) => downloads.progress(transfer_id, *bytes),
        DownloadUpdate::Done(result) => downloads.finish(transfer_id, result),
    };
    let Some(tab) = tabs.iter_mut().find(|tab| tab.id == tab_id) else {
        return row_changed;
    };
    let Some(download) = tab
        .download
        .as_ref()
        .filter(|download| download.id == transfer_id)
    else {
        return row_changed;
    };
    match update {
        DownloadUpdate::Progress(n) if !download.cancel.is_cancelled() => {
            tab.download_status = format!(
                "Downloading {} · {} bytes · Esc to cancel",
                download.name, n
            );
        }
        DownloadUpdate::Progress(_) => return row_changed,
        DownloadUpdate::Done(result) => {
            tab.download_status = match result {
                Ok(n) => format!("Saved {} · {} bytes", download.name, n),
                Err(error) => error,
            };
            tab.download = None;
        }
    }
    true
}

struct Tab {
    id: TabId,
    url: String,
    // Keep the initiating method's eligibility through redirects.
    get_navigation: bool,
    visit_pending: bool,
    visit_confirm: bool,
    title: String,
    page: Option<Arc<LoadedPage>>,
    runtime: Option<ScriptRuntime>,
    document_session: Option<DocumentSession>,
    layout: Option<Arc<Layout>>,
    history: SessionHistory,
    storage: rust_browser_engine::web_storage::TabStorage,
    history_scroll: Option<HistoryScroll>,
    reveal_form_focus: Option<(u64, rust_browser_engine::dom::NodeId)>,
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
        id: TabId,
        url: String,
        storage: rust_browser_engine::web_storage::StorageProfile,
    ) -> Self {
        let address =
            url::Url::parse(&url).unwrap_or_else(|_| url::Url::parse("about:home").unwrap());
        Self {
            id,
            title: "New tab".into(),
            url: address.as_str().into(),
            get_navigation: true,
            visit_pending: false,
            visit_confirm: false,
            page: None,
            runtime: None,
            document_session: None,
            layout: None,
            history: SessionHistory::new(address),
            storage: rust_browser_engine::web_storage::TabStorage::new(storage),
            history_scroll: None,
            reveal_form_focus: None,
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

    fn fresh_address(&self) -> Option<FreshTab> {
        FreshTab::capture(&self.url, self.get_navigation)
    }

    fn visit_history_ready(&self) -> bool {
        !self.loading
            && self.url == visit_store::PAGE_URL
            && self.document_navigation == self.navigation
            && self.displayed_navigation == self.navigation
            && self.runtime.is_none()
            && self.document_session.is_none()
            && self.page.as_ref().is_some_and(|page| page.url == visit_store::PAGE_URL)
    }

    fn install_page(&mut self, page: LoadedPage) {
        self.title = page.title();
        self.page = Some(Arc::new(page));
        self.layout = None;
        self.displayed_navigation = self.document_navigation;
    }

    fn queue_form_reveal(&mut self) {
        self.reveal_form_focus = self
            .runtime
            .as_ref()
            .and_then(ScriptRuntime::focused_control)
            .map(|node| (self.document_navigation, node));
    }

    fn reveal_edited_control(&mut self, height: f32) {
        if let Some((document, node)) = self.reveal_form_focus.take()
            && document == self.document_navigation
            && self
                .runtime
                .as_ref()
                .and_then(ScriptRuntime::focused_control)
                == Some(node)
            && let Some(rect) = self
                .layout
                .as_ref()
                .and_then(|layout| layout.rect_for(node))
        {
            if rect.y < self.scroll {
                self.scroll = rect.y;
            } else if rect.y + rect.height > self.scroll + height {
                self.scroll = (rect.y + rect.height - height).max(0.);
            }
        }
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
    #[cfg(all(windows, target_arch = "x86_64"))] enable_updates: bool,
) -> Result<(), String> {
    #[cfg(all(windows, target_arch = "x86_64"))]
    let startup_signal = if enable_updates && url.is_none() {
        crate::updater::StartupSignal::from_parent()?
    } else {
        None
    };
    let (profile, session) = Profile::open(path.clone())?;
    let settings = settings::Store::open(&path);
    #[cfg(all(windows, target_arch = "x86_64"))]
    let restore_upgrade_session = startup_signal.is_some();
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    let restore_upgrade_session = false;
    let session = if restore_upgrade_session {
        session
    } else {
        settings.initial_session(session)
    };
    let bookmarks_path = path.join(bookmarks::FILE_NAME);
    let bookmarks = bookmarks::Bookmarks::open(bookmarks_path);
    let visits = visit_store::Visits::open(path.join(visit_store::FILE_NAME));
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
    let chrome_motion_forced_off =
        smoke_requested || std::env::var("SATIN_REDUCED_MOTION").is_ok_and(|value| value == "1");
    let mut app = App {
        window: None,
        graphics: None,
        canvas: None,
        fonts,
        profile,
        bookmarks,
        visits,
        settings,
        settings_panel: None,
        tabs,
        closed_tabs: ClosedTabs::default(),
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
        address_suggestions: Suggestions::default(),
        suggestion_scroll: 0.,
        address_focus: false,
        preedit: false,
        select_popup: None,
        hits: vec![],
        tab_hits_current: false,
        tab_scroll: 0.,
        favorites_scroll: favorites_strip::Scroll::default(),
        chrome_hover: None,
        chrome_motion: ChromeMotion::new(
            Instant::now(),
            !chrome_motion_forced_off && scroll::animations_enabled(),
        ),
        chrome_motion_forced_off,
        chrome_menu: None,
        history_menu: None,
        caption_pressed: None,
        last_title_press: None,
        hover: None,
        status: String::new(),
        download_id: 0,
        downloads: downloads_ui::Downloads::default(),
        downloads_panel: None,
        downloads_frame: None,
        downloads_hover: None,
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
        #[cfg(all(windows, target_arch = "x86_64"))]
        // A trial handoff intentionally starts before current.json is changed.
        // Do not discover/offer another update in that entire browser session.
        updater: (enable_updates && startup_signal.is_none())
            .then(|| {
                crate::updater::Updates::start(
                    &path,
                    Arc::new({
                        let proxy = events.create_proxy();
                        move || {
                            let _ = proxy.send_event(());
                        }
                    }),
                )
            })
            .flatten(),
        #[cfg(all(windows, target_arch = "x86_64"))]
        update_panel: false,
        #[cfg(all(windows, target_arch = "x86_64"))]
        update_keys: update_ui::OwnedKeys::default(),
        #[cfg(all(windows, target_arch = "x86_64"))]
        update_ime_blocked: false,
        #[cfg(all(windows, target_arch = "x86_64"))]
        update_restart: None,
        #[cfg(all(windows, target_arch = "x86_64"))]
        startup_signal,
    };
    let event_result = events.run_app(&mut app);
    if event_result.is_ok() && smoke_requested {
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
    #[cfg(all(windows, target_arch = "x86_64"))]
    let restart = app.update_restart.take();
    drop(app);
    #[cfg(all(windows, target_arch = "x86_64"))]
    if let Some(restart) = restart {
        // Profile::drop flushes data and drops its exclusive lock. Starting the
        // next browser any earlier would race the child for the same profile.
        return match event_result {
            Ok(()) => restart.launch(),
            Err(error) => restart.recover(&format!("Could not close the old window: {error}")),
        };
    }
    event_result.map_err(|error| error.to_string())
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
    bookmarks: Result<bookmarks::Bookmarks, String>,
    visits: Result<visit_store::Visits, String>,
    settings: settings::Store,
    settings_panel: Option<settings::Panel>,
    tabs: Vec<Tab>,
    closed_tabs: ClosedTabs,
    active: usize,
    next_id: TabId,
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
    address_suggestions: Suggestions,
    suggestion_scroll: f32,
    address_focus: bool,
    preedit: bool,
    select_popup: Option<SelectPopup>,
    hits: Vec<(Rect, Action)>,
    tab_hits_current: bool,
    tab_scroll: f32,
    favorites_scroll: favorites_strip::Scroll,
    chrome_hover: Option<Action>,
    chrome_motion: ChromeMotion<MAX_TABS>,
    chrome_motion_forced_off: bool,
    chrome_menu: Option<Menu>,
    history_menu: Option<history_ui::HistoryMenu>,
    caption_pressed: Option<Action>,
    last_title_press: Option<(Instant, f32, f32)>,
    hover: Option<String>,
    status: String,
    download_id: u64,
    downloads: downloads_ui::Downloads,
    downloads_panel: Option<downloads_ui::Panel>,
    downloads_frame: Option<downloads_ui::HitFrame>,
    downloads_hover: Option<downloads_ui::Action>,
    scale: f32,
    clipboard: Option<arboard::Clipboard>,
    #[cfg(all(windows, target_arch = "x86_64"))]
    updater: Option<crate::updater::Updates>,
    #[cfg(all(windows, target_arch = "x86_64"))]
    update_panel: bool,
    #[cfg(all(windows, target_arch = "x86_64"))]
    update_keys: update_ui::OwnedKeys,
    #[cfg(all(windows, target_arch = "x86_64"))]
    update_ime_blocked: bool,
    #[cfg(all(windows, target_arch = "x86_64"))]
    update_restart: Option<crate::updater::Restart>,
    #[cfg(all(windows, target_arch = "x86_64"))]
    startup_signal: Option<crate::updater::StartupSignal>,
    // Keep the profile lock held until the page and background workers drop.
    profile: Profile,
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
            self.settings.chrome().content_top(),
        )
    }

    fn tab_viewport(&self, index: usize) -> Viewport {
        self.window_regions(index).viewport(&self.tabs[index])
    }

    fn tab_strip(&self, width: f32) -> TabStrip {
        let caption = if WINDOW_FRAME {
            3. * tab_strip::caption_button_width(width)
        } else {
            0.
        };
        TabStrip::new(width, caption, self.tabs.len()).scroll_to(self.tab_scroll)
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
            && self.downloads_panel.is_none()
            && self.frame_resize_at_pointer().is_none())
        .then_some((self.mouse.0 - bounds.x, self.mouse.1 - bounds.y));
        DockFrame {
            bounds,
            scale: self.scale,
            focused: self.devtools_focused && self.downloads_panel.is_none(),
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
            (self.mouse.1 - self.window_regions(self.active).page.y) / tab.zoom + tab.scroll,
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
            if focused {
                self.dismiss_address_suggestions();
            }
            if let Some(window) = &self.window {
                window.set_ime_allowed(
                    self.settings_panel.is_none()
                        && self.downloads_panel.is_none()
                        && !self.update_panel_open(),
                );
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
        self.dismiss_downloads();
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
        self.hits.retain(|(_, action)| !action.address_suggestion());
        self.invalidate_favorites_hits();
        self.tab_hits_current = false;
        self.downloads_frame = None;
        self.downloads_hover = None;
        self.caption_pressed = None;
        self.last_title_press = None;
        self.reveal_active_tab();
        self.select_popup = None;
        self.hover = None;
        if self.window_regions(self.active).dock.is_none() {
            self.set_dock_focus(false);
        }
        self.ensure_layout();
        if self.settings_panel.is_some() {
            self.settings_cursor();
        }
        self.redraw();
    }

    fn start_page(&mut self, index: usize, opened: OpenedPage) {
        if !opened.http_status().is_some_and(|code| (200..300).contains(&code)) {
            self.tabs[index].visit_pending = false;
        }
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
        tab.visit_pending = false;
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
                let loading_changed = was_loading != tab.loading;
                if !tab.loading && std::mem::take(&mut tab.visit_pending)
                    && let Ok(visits) = &mut self.visits
                {
                    let time = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if let Err(error) = visits.record(&page.url, &tab.title, time) {
                        self.status = format!("Nie zapisano historii: {error}");
                    }
                }
                changed || loading_changed
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
        let _ = self.save_session();
    }
    fn save_session(&self) -> Result<(), String> {
        let session = Session {
            schema: 1,
            tabs: self.tabs.iter().map(|t| t.url.clone()).collect(),
            active: self.active,
        };
        self.profile.save(&session)
    }
    fn sync_address(&mut self) {
        self.dismiss_address_suggestions();
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
            match runtime.layout_for_paint(viewport) {
                Ok(layout) => tab.layout = Some(layout),
                Err(error) => self.status = error.to_string(),
            }
        } else if tab.layout.as_ref().is_none_or(|l| {
            (l.width - width).abs() > 0.5
                || (l.viewport.height - height).abs() > 0.5
                || (l.depends_on_viewport_scroll
                    && (l.viewport.scroll_x != tab.scroll_x || l.viewport.scroll_y != tab.scroll))
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
        tab.reveal_edited_control(height);
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
            l.depends_on_viewport_scroll
                && (l.viewport.scroll_x != tab.scroll_x || l.viewport.scroll_y != tab.scroll)
        }) {
            if let Some(runtime) = &tab.runtime {
                if let Ok(layout) = runtime.layout_for_paint(viewport) {
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
        tab.get_navigation = request
            .as_ref()
            .is_none_or(|request| request.method == "GET");
        let mode = if mode == NavigationMode::Push && tab.history.url() == parsed {
            NavigationMode::Reload
        } else {
            mode
        };
        tab.visit_pending = mode == NavigationMode::Push && tab.get_navigation;
        tab.visit_confirm = false;
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
        } else if url == visit_store::PAGE_URL {
            tab.install_page(visit_store::page(self.visits.as_ref().ok(), false));
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
                if url.as_str() == rust_browser_engine::loader::HOME_FOCUS_ADDRESS_URL
                    && index == self.active
                    && tab.url == "about:home"
                    && tab.document_navigation == tab.navigation
                    && tab.displayed_navigation == tab.navigation
                    && tab
                        .page
                        .as_ref()
                        .is_some_and(|page| page.url == "about:home")
                {
                    self.focus_address();
                    return;
                }
                let address = url.as_str().to_owned();
                let request = NavigationRequest::from_document(url, tab.history.url());
                self.navigate_tab(index, address, mode, Some(request), None);
            }
            None => {}
        }
    }
    fn focus_address(&mut self) {
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
        self.refresh_address_suggestions();
    }

    fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    fn reveal_active_tab(&mut self) {
        let width = self.window_regions(self.active).page.width;
        self.tab_scroll = self.tab_strip(width).reveal(self.active).scroll();
    }

    fn can_add_tab(&mut self) -> bool {
        if self.tabs.len() < MAX_TABS {
            return true;
        }
        self.status = "The alpha supports up to 24 tabs.".into();
        self.redraw();
        false
    }

    fn append_tab(&mut self, address: String) {
        debug_assert!(self.tabs.len() < MAX_TABS);
        let index = self.tabs.len();
        // A new browsing context always gets a new ID; late events from a
        // cancelled tab must not reach a reopened tab, even at the same URL.
        self.tabs
            .push(Tab::new(self.next_id, address, self.profile.storage()));
        self.next_id += 1;
        self.activate(index);
    }

    fn move_active_tab(&mut self, id: TabId, direction: TabMove) {
        if self.tabs[self.active].id != id {
            return;
        }
        if let Some(index) = tab_behavior::move_tab(&mut self.tabs, id, direction, |tab| tab.id) {
            // The selected browsing context hasn't changed. Keep its document,
            // media, download, inspector and focus rather than activating it again.
            self.active = index;
            self.tab_hits_current = false;
            self.reveal_active_tab();
            self.save();
        }
        // The common action path can also dismiss a menu at the row boundary.
        self.redraw();
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
        self.reveal_active_tab();
        self.select_popup = None;
        self.address_focus = false;
        self.sync_address();
        if self.tabs[index].page.is_none() && !self.tabs[index].loading {
            self.navigate(self.tabs[index].url.clone(), false);
        } else if self.tabs[index].visit_history_ready() {
            // The profile-wide store can change while a history tab is in
            // the background. Never re-expose its old list or confirmation.
            let tab = &mut self.tabs[index];
            tab.visit_confirm = false;
            tab.install_page(visit_store::page(self.visits.as_ref().ok(), false));
        }
        self.ensure_layout();
        self.set_title();
        self.save();
        self.redraw();
    }
    fn action(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        self.wheel = None;
        if action != Action::Address && !action.address_suggestion() {
            self.dismiss_address_suggestions();
        }
        if action != Action::Downloads {
            self.dismiss_downloads();
        }
        if !action.in_menu() {
            self.chrome_menu = None;
        }
        match action {
            #[cfg(all(windows, target_arch = "x86_64"))]
            Action::UpdatePanel => self.toggle_update_panel(),
            #[cfg(all(windows, target_arch = "x86_64"))]
            Action::UpdatePrimary(command) => self.update_command(command),
            #[cfg(all(windows, target_arch = "x86_64"))]
            Action::UpdateSurface => {}
            Action::Address => self.focus_address(),
            Action::Suggestion(index) => {
                if self.suggestions_visible()
                    && let Some(row) = self.address_suggestions.rows().get(index)
                {
                    self.navigate(row.target().to_owned(), true);
                }
            }
            Action::SuggestionBackground => {}
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
            Action::Bookmarks | Action::FavoriteManager => self.show_bookmarks(),
            Action::QuietMode => self.toggle_quiet_mode(),
            Action::Downloads => self.toggle_downloads(false),
            Action::ToggleBookmark => {
                let tab = &self.tabs[self.active];
                let result = self.bookmarks.as_mut().ok().and_then(|store| {
                    let url = bookmarks::page_url(&tab.url)?;
                    if store.contains(&url) {
                        Some(store.remove(&url))
                    } else if tab.document_session.is_some()
                        && tab.document_navigation == tab.navigation
                    {
                        Some(store.add(&tab.url, &tab.title))
                    } else {
                        None
                    }
                });
                if let Some(Err(error)) = result {
                    self.status = error;
                }
                self.invalidate_favorites_hits();
                self.redraw();
            }
            Action::Favorite(index) => {
                if let Some(bookmark) = self
                    .bookmarks
                    .as_ref()
                    .ok()
                    .and_then(|store| store.entries().get(index))
                {
                    self.navigate(bookmark.url.to_string(), true);
                }
            }
            Action::ShortcutHelp => self.navigate(shortcuts::HELP_URL.into(), true),
            Action::StartShortcuts => self.navigate(
                rust_browser_engine::loader::START_SHORTCUTS_URL.into(),
                true,
            ),
            Action::TabHistory => self.open_history_menu(),
            Action::VisitHistory => self.navigate(visit_store::PAGE_URL.into(), true),
            Action::Menu => {
                self.select_popup = None;
                self.chrome_menu = (!matches!(self.chrome_menu, Some(Menu::Main(_))))
                    .then(|| Menu::Main(chrome_menu::first_item()));
                self.redraw();
            }
            Action::MenuItem(index) => {
                if matches!(self.chrome_menu, Some(Menu::Main(_))) {
                    self.activate_main_menu_item(index, event_loop);
                }
            }
            Action::TabList => {
                self.select_popup = None;
                self.chrome_menu = (!matches!(self.chrome_menu, Some(Menu::Tabs(_))))
                    .then_some(Menu::Tabs(self.active));
                self.redraw();
            }
            Action::TabListItem(id) => {
                if matches!(self.chrome_menu, Some(Menu::Tabs(_)))
                    && let Some(index) = id.and_then(|id| self.tab_index(id))
                {
                    self.chrome_menu = None;
                    self.activate(index);
                }
            }
            Action::BookmarkItem(index) => {
                if matches!(self.chrome_menu, Some(Menu::Bookmarks(_))) {
                    self.pick_bookmark_row(index);
                    self.invalidate_favorites_hits();
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
                if self.can_add_tab() {
                    self.append_tab("about:home".into());
                    self.action(Action::Address, event_loop);
                }
            }
            Action::ReopenTab => {
                if self.can_add_tab() {
                    if let Some(tab) = self.closed_tabs.reopen() {
                        self.append_tab(tab.into_address());
                    } else {
                        self.status = "No recently closed eligible tab.".into();
                        self.redraw();
                    }
                }
            }
            Action::DuplicateTab(id) => {
                if self.can_add_tab()
                    && let Some(index) = self.tab_index(id)
                {
                    if let Some(tab) = self.tabs[index].fresh_address() {
                        self.append_tab(tab.into_address());
                    } else {
                        self.status = "This address cannot be duplicated.".into();
                        self.redraw();
                    }
                }
            }
            Action::MoveTab(id, direction) => self.move_active_tab(id, direction),
            Action::Select(id) => {
                if let Some(index) = self.tab_index(id) {
                    self.activate(index);
                }
            }
            Action::Close(id) => {
                let Some(i) = self.tab_index(id) else {
                    return;
                };
                #[cfg(all(windows, target_arch = "x86_64"))]
                if self.tabs.len() == 1 && self.update_applying() {
                    return;
                }
                let tab = &self.tabs[i];
                let reopen = if self.tabs.len() > 1 {
                    tab.fresh_address()
                } else {
                    None
                };
                self.media.close_page(tab.id, tab.document_navigation);
                tab.cancel.cancel();
                if let Some(dock) = &mut self.devtools {
                    dock.close_tab(tab.id);
                }
                if let Some(id) = self.tabs[i].download.as_ref().map(|download| download.id) {
                    self.cancel_download(id);
                }
                self.tabs.remove(i);
                if self.tabs.is_empty() {
                    self.save();
                    event_loop.exit();
                    return;
                }
                self.closed_tabs.remember(reopen);
                if i < self.active {
                    self.active -= 1;
                }
                self.active = self.active.min(self.tabs.len() - 1);
                self.activate(self.active);
            }
        }
    }

    fn close_window(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(all(windows, target_arch = "x86_64"))]
        if self.update_applying() {
            return;
        }
        self.finish_close(event_loop);
    }

    fn finish_close(&mut self, event_loop: &ActiveEventLoop) {
        self.save();
        for tab in &self.tabs {
            tab.cancel.cancel();
            if let Some(download) = &tab.download {
                download.cancel.cancel();
            }
        }
        event_loop.set_control_flow(ControlFlow::Poll);
        event_loop.exit();
    }
    fn insert(&mut self, text: &str) {
        if self.downloads_panel.is_some() || self.update_panel_open() {
            return;
        }
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
                // Input events and their effects are synchronous. Consecutive text edits can
                // share the next layout for painting and scrolling the control into view.
                self.publish_form_interaction();
            }
            return;
        }
        if self.address.insert(text) {
            self.refresh_address_suggestions();
        }
    }
    fn run_shortcut(&mut self, command: Command, event_loop: &ActiveEventLoop) {
        match command {
            Command::Address => self.action(Action::Address, event_loop),
            Command::NewTab => self.action(Action::NewTab, event_loop),
            Command::CloseTab => self.action(Action::Close(self.tabs[self.active].id), event_loop),
            Command::ReopenTab => self.action(Action::ReopenTab, event_loop),
            Command::DuplicateTab => {
                self.action(Action::DuplicateTab(self.tabs[self.active].id), event_loop)
            }
            Command::MoveTabLeft | Command::MoveTabRight => {
                let direction = if command == Command::MoveTabLeft {
                    TabMove::Left
                } else {
                    TabMove::Right
                };
                self.action(
                    Action::MoveTab(self.tabs[self.active].id, direction),
                    event_loop,
                );
            }
            Command::NextTab => self.activate((self.active + 1) % self.tabs.len()),
            Command::VisitHistory => self.action(Action::VisitHistory, event_loop),
            Command::PreviousTab => {
                self.activate((self.active + self.tabs.len() - 1) % self.tabs.len())
            }
            // The keyboard reloads even if a load is in progress. The chrome
            // button's Action::Reload can instead stop the current load.
            Command::Reload => self.navigate(self.tabs[self.active].url.clone(), false),
            Command::Back => self.action(Action::Back, event_loop),
            Command::Forward => self.action(Action::Forward, event_loop),
            Command::Menu => self.action(Action::Menu, event_loop),
            Command::Downloads => self.toggle_downloads(true),
            Command::BookmarkPage => {
                self.save_current_bookmark();
                self.invalidate_favorites_hits();
            }
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
        #[cfg(all(windows, target_arch = "x86_64"))]
        if self.update_key(&key, repeat) {
            return;
        }
        if WINDOW_FRAME
            && self.modifiers == ModifiersState::ALT
            && key == Key::Named(NamedKey::Space)
        {
            if !repeat && let Some(window) = &self.window {
                window.show_window_menu(LogicalPosition::new(0., TAB_H));
            }
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
        let mut command = shortcuts::command(&key, self.modifiers);
        let editor_arrow = matches!(command, Some(Command::MoveTabLeft | Command::MoveTabRight))
            && (self.address_focus
                || self.devtools_focused
                || self.preedit
                || self.tabs[self.active]
                    .runtime
                    .as_ref()
                    .and_then(ScriptRuntime::focused_control)
                    .is_some());
        // A tab edit must not dismiss an open Downloads or page select popup.
        if matches!(
            command,
            Some(
                Command::ReopenTab
                    | Command::DuplicateTab
                    | Command::MoveTabLeft
                    | Command::MoveTabRight
            )
        ) && (self.downloads_panel.is_some()
            || self.select_popup.is_some()
            || (repeat && !editor_arrow))
        {
            return;
        }
        if editor_arrow {
            // Keep Ctrl+Shift+arrows in the focused editor or document control.
            command = None;
        }
        if command == Some(Command::Downloads) {
            if !repeat {
                self.run_shortcut(Command::Downloads, event_loop);
            }
            return;
        }
        if self.downloads_panel.is_some() {
            if command.is_some() {
                self.dismiss_downloads();
            } else {
                self.downloads_key(&key, repeat);
                return;
            }
        }
        if self.handle_history_key(&key) {
            return;
        }
        if self.bookmark_panel_key(&key) {
            self.invalidate_favorites_hits();
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
                    self.chrome_menu = Some(Menu::Main(self.step_main_menu(selected, previous)));
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
        if let Some(Menu::Tabs(selected)) = self.chrome_menu {
            match key {
                Key::Named(NamedKey::Escape) => self.chrome_menu = None,
                Key::Named(NamedKey::Enter | NamedKey::Space) => {
                    if !repeat && let Some(tab) = self.tabs.get(selected) {
                        self.action(Action::TabListItem(Some(tab.id)), event_loop);
                    }
                    return;
                }
                Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp | NamedKey::Tab) => {
                    let previous = key == Key::Named(NamedKey::ArrowUp)
                        || (key == Key::Named(NamedKey::Tab) && self.modifiers.shift_key());
                    let count = self.tabs.len();
                    self.chrome_menu = Some(Menu::Tabs(
                        (selected + if previous { count - 1 } else { 1 }) % count,
                    ));
                }
                _ => return,
            }
            self.redraw();
            return;
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
        if key == Key::Named(NamedKey::Escape)
            && !self.suggestions_visible()
            && let Some(id) = self.tabs[self.active]
                .download
                .as_ref()
                .map(|download| download.id)
        {
            if !repeat {
                self.cancel_download(id);
            }
            return;
        }
        if let Some(command @ (Command::InspectElement | Command::ToggleDevtools)) = command {
            if !repeat {
                self.run_shortcut(command, event_loop);
            }
            return;
        }
        if self.devtools_focused
            && let Some(bounds) = self.window_regions(self.active).dock
            && (command.is_none()
                || self.devtools.as_ref().is_some_and(|d| d.is_console())
                    && self.modifiers == ModifiersState::CONTROL
                    && matches!(&key,Key::Character(c) if c.eq_ignore_ascii_case("l")))
        {
            self.dock_key(self.dock_frame(bounds), key, text, repeat);
            return;
        }
        if let Some(command) = command {
            self.run_shortcut(command, event_loop);
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
                            self.refresh_address_suggestions();
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
            if self.preedit && key == Key::Named(NamedKey::Space) {
                // The input method will deliver the committed text separately.
                return;
            }
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
                    if self.dismiss_address_suggestions() {
                        return;
                    }
                    self.address_focus = false;
                    self.sync_address();
                } else if self.tabs[self.active].loading {
                    self.action(Action::Reload, event_loop);
                }
                self.redraw();
            }
            Key::Named(NamedKey::Enter) if self.address_focus => {
                if self.preedit || repeat {
                    return;
                }
                let target = self
                    .address_suggestions
                    .selected_target()
                    .filter(|_| self.suggestions_visible())
                    .map_or_else(|| omnibox_target(self.address.text()), str::to_owned);
                self.navigate(target, true);
            }
            Key::Named(edit @ (NamedKey::Backspace | NamedKey::Delete)) if self.address_focus => {
                if !self.preedit {
                    self.address.delete(edit == NamedKey::Backspace);
                    self.refresh_address_suggestions();
                }
            }
            Key::Named(direction @ (NamedKey::ArrowUp | NamedKey::ArrowDown))
                if self.address_focus =>
            {
                if self.modifiers.is_empty() && !self.preedit {
                    self.step_address_suggestions(if direction == NamedKey::ArrowUp {
                        SuggestionDirection::Previous
                    } else {
                        SuggestionDirection::Next
                    });
                }
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
        let picked_node = if self.downloads_panel.is_none()
            && self.devtools.as_ref().is_some_and(|d| d.picking)
        {
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
            canvas.document_at(
                layout,
                &mut self.fonts,
                regions.page.y * u,
                ScrollOffset {
                    x: tab.scroll_x,
                    y: tab.scroll,
                },
                u * tab.zoom,
            );
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
                    y: regions.page.y * u + (r.y - tab.scroll) * scale,
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
        self.chrome_motion.begin_frame(Instant::now());
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
            panel.draw_at(
                &mut canvas,
                &mut self.fonts,
                u,
                &self.settings,
                regions.page.y,
            );
        }
        let downloads_frame = self.draw_downloads(&mut canvas, width, height);
        #[cfg(all(windows, target_arch = "x86_64"))]
        self.draw_update_panel(&mut canvas, width);
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
        self.chrome_motion.finish_frame(presented.is_ok());
        match presented {
            Ok(()) => {
                self.downloads_frame = downloads_frame;
                self.tab_hits_current = true;
                self.presented_frames += 1;
                self.last_present_error = None;
                #[cfg(all(windows, target_arch = "x86_64"))]
                if let Some(signal) = self.startup_signal.take() {
                    signal.ready();
                }
            }
            Err(e) => {
                self.downloads_frame = None;
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
        let strip = scaled_rect(u, 0., 0., width, TAB_H);
        canvas.fill_rect(strip, TAB_STRIP);
        canvas.fill_linear_gradient(
            strip,
            110.,
            [
                (0., Color(0x26, 0x23, 0x28, 255)),
                (0.69, TAB_STRIP),
                (1., Color(0x36, 0x31, 0x3e, 255)),
            ],
        );
        let content_top = self.settings.chrome().content_top();
        canvas.fill_rect(
            scaled_rect(u, 0., TAB_H, width, content_top - TAB_H),
            NAVIGATION_BAR,
        );
        canvas.fill_rect(scaled_rect(u, 0., TAB_H - 1., width, 1.), CHROME_LINE);
        canvas.fill_rect(
            scaled_rect(u, 0., quiet_cinder::TOOLBAR_END - 1., width, 1.),
            CHROME_LINE,
        );
        if content_top > quiet_cinder::TOOLBAR_END {
            canvas.fill_rect(scaled_rect(u, 0., content_top - 1., width, 1.), CHROME_LINE);
        }
        self.hits.clear();
        self.draw_tabs(canvas, width);
        let compact = width <= tab_strip::NARROW_WIDTH;
        let mark = if compact { 18. } else { 16. };
        let mark_x = if compact { 14. } else { 19. };
        paint_centered_glyph(
            canvas,
            scaled_rect(1., mark_x, (TAB_H - mark) / 2., mark, mark),
            mark,
            u,
            icons::Glyph::Brand,
            CHROME_ACCENT,
        );
        if !compact {
            let wordmark = chrome_shape(&self.fonts, "Satin", 11. * u, 700);
            canvas.text(41. * u, 26. * u, &wordmark, CHROME_TEXT, &mut self.fonts);
        }
        if WINDOW_FRAME {
            self.draw_caption_buttons(canvas, width, maximized);
        }
        self.draw_toolbar(canvas, width);
        if self.settings.chrome().bookmarks_bar_visible() {
            self.draw_favorites(canvas, width);
        }
        self.draw_footer(canvas, width, height, dock_visible);
        self.draw_address_suggestions(canvas, width, height);
        self.draw_chrome_menu(canvas, width);
        self.draw_history_menu(canvas, width);
    }

    fn draw_tabs(&mut self, canvas: &mut Canvas, width: f32) {
        let u = self.scale;
        let strip = self.tab_strip(width);
        self.tab_scroll = strip.scroll();
        let pointer = self.chrome_hover;
        let over_tabs = matches!(pointer, Some(Action::Select(_) | Action::Close(_)));
        canvas.clip(physical_rect(u, strip.list));
        for slot in strip.visible_tabs() {
            let index = slot.index;
            let tab = &self.tabs[index];
            let close = Action::Close(tab.id);
            let active = index == self.active;
            // An old hit can be under a different tab after a reorder or
            // scroll. Painting takes hover from the new geometry.
            let hovered = over_tabs && self.pointer_in(slot.visible_face);
            let close_hovered = slot
                .close
                .as_ref()
                .map(|button| hovered && self.pointer_in(button.hit));
            let ink = self
                .chrome_motion
                .tab(tab.id, active, hovered, close_hovered);
            draw_tab_face(canvas, slot.face, u, active, &ink);
            if let Some(icon) = built_in_site_icon(&tab.url) {
                canvas.image(physical_rect(u, slot.favicon), icon);
            } else {
                rounded_chrome_rect(
                    canvas,
                    physical_rect(u, slot.favicon),
                    5. * u,
                    CHROME_ACCENT_SOFT,
                );
                paint_centered_glyph(
                    canvas,
                    slot.favicon,
                    13.,
                    u,
                    if tab.is_home() {
                        icons::Glyph::Compass
                    } else {
                        icons::Glyph::Globe
                    },
                    CHROME_ACCENT,
                );
            }
            let title = if tab.is_home() {
                "Nowa karta"
            } else {
                &tab.title
            };
            let weight = if active { 620 } else { 400 };
            let size = tab_strip::TAB_FONT_SIZE * u;
            let title = chrome_elide(&self.fonts, title, slot.title.width * u, size, weight);
            let run = chrome_shape(&self.fonts, &title, size, weight);
            if run.advance <= slot.title.width * u {
                canvas.text(
                    slot.title.x * u,
                    (slot.title.y + 20.) * u,
                    &run,
                    chrome_mix(CHROME_MUTED, CHROME_TEXT, ink.text),
                    &mut self.fonts,
                );
            }
            self.hits.push((slot.visible_face, Action::Select(tab.id)));
            if let Some(button) = slot.close {
                if ink.close_hover > 0. && ink.close > 0. {
                    rounded_chrome_rect(
                        canvas,
                        physical_rect(u, button.hover_circle),
                        8. * u,
                        chrome_alpha(
                            Color(CHROME_TEXT.0, CHROME_TEXT.1, CHROME_TEXT.2, 38),
                            ink.close_hover * ink.close,
                        ),
                    );
                }
                if ink.close > 0. {
                    paint_centered_glyph(
                        canvas,
                        button.glyph,
                        10.,
                        u,
                        icons::Glyph::X,
                        chrome_alpha(
                            if close_hovered == Some(true) {
                                CHROME_TEXT
                            } else {
                                CHROME_MUTED
                            },
                            ink.close,
                        ),
                    );
                }
                self.hits.push((button.hit, close));
            }
        }
        canvas.clip(scaled_rect(
            1.,
            0.,
            0.,
            canvas.width as f32,
            canvas.height as f32,
        ));
        for (button, action, motion, glyph) in [
            (
                strip.new_tab,
                Action::NewTab,
                MotionButton::NewTab,
                icons::Glyph::Plus,
            ),
            (
                strip.tab_list,
                Action::TabList,
                MotionButton::TabList,
                icons::Glyph::ChevronDown,
            ),
        ] {
            let Some(button) = button else { continue };
            let on = pointer == Some(action)
                || action == Action::TabList && matches!(self.chrome_menu, Some(Menu::Tabs(_)));
            let hot = self.chrome_motion.button(motion, on);
            if hot > 0. {
                rounded_chrome_rect(
                    canvas,
                    physical_rect(u, button.bounds),
                    tab_strip::CONTROL_RADIUS * u,
                    chrome_alpha(CHROME_HOVER, hot),
                );
            }
            paint_centered_glyph(
                canvas,
                button.glyph,
                17.,
                u,
                glyph,
                chrome_mix(CHROME_ICON, CHROME_TEXT, hot),
            );
            self.hits.push((button.bounds, action));
        }
    }

    fn draw_caption_buttons(&mut self, canvas: &mut Canvas, width: f32, maximized: bool) {
        let u = self.scale;
        for (index, action) in [Action::Minimize, Action::Maximize, Action::CloseWindow]
            .into_iter()
            .enumerate()
        {
            let bounds = chrome_box(quiet_cinder::caption(width, index));
            let hovered = self.chrome_hover == Some(action);
            let pressed = self.caption_pressed == Some(action);
            let (hot, press) = self.chrome_motion.caption(index, hovered, pressed);
            if hot > 0. {
                let color = if action == Action::CloseWindow {
                    chrome_mix(
                        Color(0xc9, 0x38, 0x46, 255),
                        Color(0xa9, 0x14, 0x25, 255),
                        press,
                    )
                } else {
                    CHROME_HOVER
                };
                canvas.fill_rect(physical_rect(u, bounds), chrome_alpha(color, hot));
            }
            centered_chrome_icon(
                canvas,
                bounds,
                13.,
                u,
                action,
                maximized,
                if action == Action::CloseWindow {
                    chrome_mix(CHROME_ICON, Color::WHITE, hot)
                } else {
                    CHROME_ICON
                },
            );
            self.hits.push((bounds, action));
        }
    }

    fn toolbar_layout(&self, width: f32) -> quiet_cinder::Toolbar {
        #[cfg(all(windows, target_arch = "x86_64"))]
        let show_update = self.update_view().is_some();
        #[cfg(not(all(windows, target_arch = "x86_64")))]
        let show_update = false;
        self.settings.chrome().toolbar(width, show_update)
    }

    fn update_panel_open(&self) -> bool {
        #[cfg(all(windows, target_arch = "x86_64"))]
        return self.update_panel;
        #[cfg(not(all(windows, target_arch = "x86_64")))]
        false
    }

    fn draw_toolbar(&mut self, canvas: &mut Canvas, width: f32) {
        let u = self.scale;
        let layout = self.toolbar_layout(width);
        #[cfg(all(windows, target_arch = "x86_64"))]
        if let Some(bounds) = layout.update {
            self.draw_update_button(canvas, chrome_box(bounds));
        }
        let quiet = self.settings.chrome().quiet_button(layout.quiet);
        quiet.paint(canvas, u, self.chrome_hover == Some(Action::QuietMode));
        if let Some(hit) = quiet.hit_rect() {
            self.hits.push((hit, Action::QuietMode));
        }
        let tab = &self.tabs[self.active];
        let pointer = self.chrome_hover;
        for (bounds, action, motion, enabled) in [
            (
                Some(layout.back),
                Action::Back,
                MotionButton::Back,
                tab.history.can_traverse(-1),
            ),
            (
                Some(layout.forward),
                Action::Forward,
                MotionButton::Forward,
                tab.history.can_traverse(1),
            ),
            (
                Some(layout.reload),
                Action::Reload,
                MotionButton::Reload,
                true,
            ),
            (layout.home, Action::Home, MotionButton::Home, true),
            (
                layout.downloads,
                Action::Downloads,
                MotionButton::Downloads,
                true,
            ),
            (Some(layout.menu), Action::Menu, MotionButton::Menu, true),
        ] {
            let Some(bounds) = bounds.map(chrome_box) else {
                continue;
            };
            let on = enabled
                && (pointer == Some(action)
                    || action == Action::Downloads && self.downloads_panel.is_some()
                    || action == Action::Menu && self.chrome_menu.is_some());
            let hot = self.chrome_motion.button(motion, on);
            if hot > 0. {
                rounded_chrome_rect(
                    canvas,
                    physical_rect(u, bounds),
                    6. * u,
                    chrome_alpha(CHROME_HOVER, hot),
                );
            }
            centered_chrome_icon(
                canvas,
                bounds,
                17.,
                u,
                action,
                tab.loading,
                if enabled {
                    chrome_mix(CHROME_ICON, CHROME_TEXT, hot)
                } else {
                    Color(CHROME_ICON.0, CHROME_ICON.1, CHROME_ICON.2, 84)
                },
            );
            if action == Action::Downloads && self.downloads.has_active() {
                let dot = scaled_rect(1., bounds.x + bounds.width - 12., bounds.y + 4., 7., 7.);
                rounded_chrome_rect(canvas, physical_rect(u, dot), 3.5 * u, NAVIGATION_BAR);
                let fill = scaled_rect(1., dot.x + 1., dot.y + 1., 5., 5.);
                rounded_chrome_rect(canvas, physical_rect(u, fill), 2.5 * u, CHROME_ACCENT);
            }
            if enabled {
                self.hits.push((bounds, action));
            }
        }

        let address_rect = chrome_box(layout.address);
        let address_focus = self.address_focus
            && !self.devtools_focused
            && self.downloads_panel.is_none()
            && !self.update_panel_open();
        address_bar::chrome::paint_address_field(canvas, address_rect, u, address_focus);
        paint_centered_glyph(
            canvas,
            chrome_box(layout.location),
            17.,
            u,
            icons::Glyph::Search,
            CHROME_ICON,
        );

        let url = bookmarks::page_url(&tab.url);
        let marked = url.as_ref().is_some_and(|url| {
            self.bookmarks
                .as_ref()
                .is_ok_and(|store| store.contains(url))
        });
        let can_bookmark = marked
            || url.is_some()
                && self.bookmarks.is_ok()
                && tab.document_session.is_some()
                && tab.document_navigation == tab.navigation;
        let star_rect = chrome_box(layout.bookmark);
        let star_hot = self.chrome_motion.button(
            MotionButton::Bookmark,
            pointer == Some(Action::ToggleBookmark) && can_bookmark,
        );
        if star_hot > 0. {
            rounded_chrome_rect(
                canvas,
                physical_rect(u, star_rect),
                6. * u,
                chrome_alpha(CHROME_HOVER, star_hot),
            );
        }
        if marked {
            paint_bookmarked_star(canvas, star_rect, u);
        } else {
            paint_centered_glyph(
                canvas,
                star_rect,
                17.,
                u,
                icons::Glyph::Star,
                if can_bookmark {
                    chrome_mix(CHROME_ICON, CHROME_TEXT, star_hot)
                } else {
                    Color(CHROME_ICON.0, CHROME_ICON.1, CHROME_ICON.2, 84)
                },
            );
        }

        let text_x = layout.edit.x;
        let available = layout.edit.w;
        let center = layout.edit.y + layout.edit.h / 2.;
        let font_size = 13. * u;
        canvas.clip(physical_rect(u, chrome_box(layout.edit)));
        let address = self.address.text();
        let measure =
            |index| chrome_shape(&self.fonts, &address[..index], font_size, 400).advance / u;
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
                scaled_rect(u, text_x + start - shift, center - 9., end - start, 18.),
                CHROME_ACCENT_SOFT,
            );
        }
        let empty = address.is_empty();
        let address_run = chrome_shape(
            &self.fonts,
            if empty {
                "Wyszukaj lub wpisz adres"
            } else {
                address
            },
            font_size,
            400,
        );
        canvas.text(
            (text_x - shift) * u,
            (center + 4.5) * u,
            &address_run,
            if empty { CHROME_MUTED } else { CHROME_TEXT },
            &mut self.fonts,
        );
        if address_focus && selected.is_empty() {
            canvas.fill_rect(
                scaled_rect(u, text_x + cursor_width - shift, center - 8., 1., 16.),
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
        if can_bookmark {
            self.hits.push((star_rect, Action::ToggleBookmark));
        }
    }

    fn draw_downloads(
        &mut self,
        canvas: &mut Canvas,
        width: f32,
        height: f32,
    ) -> Option<downloads_ui::HitFrame> {
        if self.settings_panel.is_some() || self.update_panel_open() {
            return None;
        }
        let panel = self.downloads_panel.as_ref()?;
        let (window, anchor) = downloads_geometry(&self.toolbar_layout(width), width, height);
        let layout = panel.layout(&self.downloads, window, anchor)?;
        downloads_ui::draw(
            canvas,
            &mut self.fonts,
            &layout,
            &downloads_ui::Colors {
                card: CHROME_PANEL,
                line: CHROME_LINE,
                text: CHROME_TEXT,
                muted: CHROME_MUTED,
                icon: CHROME_ICON,
                hover: CHROME_HOVER,
                accent: CHROME_ACCENT,
            },
            self.scale,
            self.pointer_inside.then_some(self.mouse),
            panel.focused(&layout),
        );
        Some(layout.hit_frame())
    }

    fn invalidate_favorites_hits(&mut self) {
        self.hits.retain(|(_, action)| !action.in_favorites());
        if self.chrome_hover.is_some_and(Action::in_favorites) {
            self.chrome_hover = None;
        }
    }

    fn favorites_occluded(&self) -> bool {
        self.chrome_menu.is_some()
            || self.suggestions_visible()
            || self.history_menu.is_some()
            || self.settings_panel.is_some()
            || self.downloads_panel.is_some()
            || self.update_panel_open()
    }

    fn pointer_in_favorites(&self, width: f32) -> bool {
        self.settings.chrome().bookmarks_bar_visible()
            && self.pointer_in(scaled_rect(
                1.,
                0.,
                quiet_cinder::TOOLBAR_END,
                width,
                quiet_cinder::FAVORITES_ROW,
            ))
    }

    fn scroll_favorites_at_pointer(&mut self, width: f32, delta: ScrollOffset) -> bool {
        if !self.pointer_in_favorites(width) {
            return false;
        }
        // Don't hand a bar gesture back to the document or dock at either end.
        self.wheel = None;
        if !self.favorites_occluded() && self.favorites_scroll.wheel(delta.x, delta.y) {
            self.invalidate_favorites_hits();
            self.redraw();
        }
        true
    }

    fn draw_favorites(&mut self, canvas: &mut Canvas, width: f32) {
        let u = self.scale;
        let y = quiet_cinder::TOOLBAR_END;
        let inner_height = quiet_cinder::FAVORITES_ROW - 1.;
        let manager_open = matches!(self.chrome_menu, Some(Menu::Bookmarks(_)));
        let interactive = !self.favorites_occluded();
        let entries = self
            .bookmarks
            .as_ref()
            .map_or(&[][..], |store| store.entries());
        let runs: Vec<_> = entries
            .iter()
            .take(favorites_strip::MAX_FAVORITES)
            .map(|bookmark| {
                // These titles are bounded by the store. Empty titles use only the host.
                let text = if bookmark.title.is_empty() {
                    bookmark.url.host_str().unwrap_or_default()
                } else {
                    &bookmark.title
                };
                chrome_shape(&self.fonts, text, 11., 400)
            })
            .collect();
        let strip = favorites_strip::Strip::layout(
            width,
            runs.iter().map(|run| run.advance),
            &mut self.favorites_scroll,
        );
        canvas.clip(scaled_rect(u, 0., y, width, inner_height));
        for (slot, paint, visible) in strip.visible() {
            let (action, top, height) = if slot == 0 {
                (Action::FavoriteManager, y, inner_height)
            } else {
                (Action::Favorite(slot - 1), y + 2.25, 24.5)
            };
            let bounds = scaled_rect(1., paint.x, top, paint.width, height);
            let hit = scaled_rect(1., visible.x, top, visible.width, height);
            let hovered = interactive && self.pointer_in(hit);
            self.hits.push((hit, action));
            if slot == 0 {
                if hovered || manager_open {
                    rounded_chrome_rect(canvas, physical_rect(u, bounds), 6. * u, CHROME_HOVER);
                }
                centered_chrome_icon(
                    canvas,
                    bounds,
                    17.,
                    u,
                    Action::Bookmarks,
                    false,
                    CHROME_ICON,
                );
                continue;
            }
            if hovered {
                rounded_chrome_rect(canvas, physical_rect(u, bounds), 4. * u, CHROME_HOVER);
            }
            let icon = scaled_rect(1., paint.x + 8., y + 7., 15., 15.);
            if let Some(bitmap) = built_in_site_icon(entries[slot - 1].url.as_str()) {
                canvas.image(physical_rect(u, icon), bitmap);
            } else {
                rounded_chrome_rect(canvas, physical_rect(u, icon), 4. * u, CHROME_ACCENT_SOFT);
                paint_centered_glyph(canvas, icon, 11., u, icons::Glyph::Globe, CHROME_ACCENT);
            }
            canvas.text_scaled(
                (paint.x + favorites_strip::LABEL_INSET) * u,
                (y + 18.) * u,
                &runs[slot - 1],
                if hovered { CHROME_TEXT } else { CHROME_ICON },
                &mut self.fonts,
                u,
            );
        }
        canvas.clip(scaled_rect(
            1.,
            0.,
            0.,
            canvas.width as f32,
            canvas.height as f32,
        ));
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
        match self.chrome_menu {
            Some(Menu::Main(selected)) => self.draw_main_menu(canvas, selected),
            Some(Menu::Bookmarks(_)) => self.draw_bookmarks(canvas, width),
            Some(Menu::Tabs(selected)) => self.draw_tab_list(canvas, width, selected),
            None => {}
        }
    }

    fn draw_tab_list(&mut self, canvas: &mut Canvas, width: f32, selected: usize) {
        let u = self.scale;
        let Some(button) = self.tab_strip(width).tab_list else {
            return;
        };
        let panel_width = 352_f32.min(width - 18.);
        let x = button.bounds.x.clamp(8., width - panel_width - 8.);
        let y = button.bounds.y + button.bounds.height + 5.;
        let count = (((canvas.height as f32 / u - y - 54.) / 35.).floor() as usize)
            .clamp(1, self.tabs.len());
        let start = selected.saturating_sub(count - 1);
        let panel = scaled_rect(1., x, y, panel_width, 43. + count as f32 * 35.);
        let physical = physical_rect(u, panel);
        canvas.fill_rounded_rect(physical, [10. * u; 4], CHROME_PANEL);
        canvas.outline_rounded_rect(physical, [10. * u; 4], -u, u, CHROME_LINE);
        self.hits.push((panel, Action::TabListItem(None)));
        let title = chrome_shape(&self.fonts, "Otwarte karty", 13. * u, 700);
        canvas.text(
            (x + 14.) * u,
            (y + 26.) * u,
            &title,
            CHROME_TEXT,
            &mut self.fonts,
        );
        for (offset, tab) in self.tabs.iter().skip(start).take(count).enumerate() {
            let index = start + offset;
            let action = Action::TabListItem(Some(tab.id));
            let row = scaled_rect(
                1.,
                x + 7.,
                y + 36. + 35. * offset as f32,
                panel_width - 14.,
                35.,
            );
            chrome_menu::draw_row_highlight(
                canvas,
                row,
                u,
                self.pointer_in(row),
                index == selected,
            );
            let name = if tab.is_home() {
                "Nowa karta"
            } else {
                &tab.title
            };
            let name = chrome_elide(&self.fonts, name, (row.width - 16.) * u, 13. * u, 400);
            let run = chrome_shape(&self.fonts, &name, 13. * u, 400);
            canvas.text(
                (row.x + 8.) * u,
                (row.y + 22.) * u,
                &run,
                CHROME_TEXT,
                &mut self.fonts,
            );
            self.hits.push((row, action));
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
        let content = self.hits.iter().rev().find_map(|(rect, action)| {
            #[cfg(all(windows, target_arch = "x86_64"))]
            if self.update_panel
                && !matches!(
                    action,
                    Action::UpdatePanel | Action::UpdatePrimary(_) | Action::UpdateSurface
                )
            {
                return None;
            }
            if !self.tab_hits_current
                && ((TAB_H..quiet_cinder::TOOLBAR_END).contains(&rect.y)
                    || action.address_suggestion())
            {
                return None;
            }
            let current = match action {
                action if action.caption() => false,
                action if action.in_favorites() => {
                    self.tab_hits_current
                        && self.settings.chrome().bookmarks_bar_visible()
                        && !self.favorites_occluded()
                }
                Action::MenuItem(_) => matches!(self.chrome_menu, Some(Menu::Main(_))),
                Action::BookmarkItem(_) => matches!(self.chrome_menu, Some(Menu::Bookmarks(_))),
                Action::TabListItem(_) => matches!(self.chrome_menu, Some(Menu::Tabs(_))),
                action if action.address_suggestion() => self.suggestions_visible(),
                #[cfg(all(windows, target_arch = "x86_64"))]
                Action::UpdatePanel => self.update_view().is_some(),
                #[cfg(all(windows, target_arch = "x86_64"))]
                Action::UpdatePrimary(_) => self.update_panel && self.tab_hits_current,
                #[cfg(all(windows, target_arch = "x86_64"))]
                Action::UpdateSurface => self.update_panel,
                _ => true,
            };
            (current && self.pointer_in(*rect)).then_some(*action)
        });
        self.caption_action_at_pointer().or(content)
    }

    fn caption_action_at_pointer(&self) -> Option<Action> {
        if !WINDOW_FRAME || !self.pointer_inside {
            return None;
        }
        let width = self.window.as_ref()?.inner_size().width as f32 / self.scale;
        [Action::Minimize, Action::Maximize, Action::CloseWindow]
            .into_iter()
            .enumerate()
            .find_map(|(i, action)| {
                self.pointer_in(chrome_box(quiet_cinder::caption(width, i)))
                    .then_some(action)
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
        #[cfg(all(windows, target_arch = "x86_64"))]
        if self.update_panel {
            if self.caption_action_at_pointer().is_some() {
                self.dismiss_update_panel();
            } else {
                self.update_press();
                return;
            }
        }
        // The native frame remains operable above browser overlays. Other
        // outside presses dismiss Downloads without reaching the page/dock.
        if self.downloads_panel.is_some() {
            if self.caption_action_at_pointer().is_some() {
                self.dismiss_downloads();
            } else {
                self.downloads_press();
                return;
            }
        }
        let action = self.chrome_action_at_pointer();
        // Geometry can change before its presentation (including an updater
        // offer resizing the omnibox). Don't fall through from a stale chrome
        // hit or an old suggestion row to the page.
        if !self.tab_hits_current && !action.is_some_and(Action::caption) {
            return;
        }
        if !action.is_some_and(|action| action == Action::Address || action.address_suggestion()) {
            self.dismiss_address_suggestions();
        }
        let dismissed_menu = self.chrome_menu.is_some() && !action.is_some_and(Action::in_menu);
        if dismissed_menu {
            self.chrome_menu = None;
            self.redraw();
        }
        let regions = self.window_regions(self.active);
        if action.is_none() && self.pointer_in_favorites(regions.page.width) {
            // Blank bar and invalidated targets do not pass the click onward.
            return;
        }
        if let Some(bounds) = regions.dock
            && self.pointer_in(bounds)
            && !action.is_some_and(|a| a.in_menu() || a.address_suggestion())
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
            || self.downloads_panel.is_some()
            || self.update_panel_open()
            || !self.pointer_in(self.window_regions(self.active).page)
        {
            return None;
        }
        let tab = &self.tabs[self.active];
        let x = self.mouse.0 / tab.zoom + tab.scroll_x;
        let y = (self.mouse.1 - self.window_regions(self.active).page.y) / tab.zoom + tab.scroll;
        tab.layout
            .as_ref()
            .and_then(|l| l.link_at(x, y))
            .map(str::to_owned)
    }

    fn visit_history_click(&mut self, link: &str) -> bool {
        use visit_store::HistoryClick;
        let Some(command) = HistoryClick::from_link(link) else {
            return false;
        };
        let tab = &self.tabs[self.active];
        if !tab.visit_history_ready() {
            return false;
        }
        let Ok(visits) = &mut self.visits else {
            return true;
        };
        let mut confirm = tab.visit_confirm;
        let result = match command {
            HistoryClick::Confirm => {
                confirm = !visits.entries().is_empty();
                Ok(())
            }
            HistoryClick::Cancel => {
                confirm = false;
                Ok(())
            }
            HistoryClick::Clear if confirm => {
                confirm = false;
                visits.clear().map(|()| {
                    for tab in &mut self.tabs {
                        tab.visit_pending = false;
                        tab.visit_confirm = false;
                    }
                })
            }
            HistoryClick::Clear => return true,
            HistoryClick::Remove(url) => visits.remove(&url),
        };
        if let Err(error) = result {
            self.status = format!("Nie można zmienić historii: {error}");
        }
        let tab = &mut self.tabs[self.active];
        tab.visit_confirm = confirm;
        tab.install_page(visit_store::page(Some(visits), confirm));
        self.hover = None;
        self.ensure_layout();
        self.redraw();
        true
    }

    fn update_pointer(&mut self, redraw_dock: bool) {
        if self.settings_panel.is_some() {
            self.settings_cursor();
            return;
        }
        if self.update_history_pointer() {
            return;
        }
        if self.downloads_panel.is_some() {
            self.update_downloads_pointer();
            return;
        }
        #[cfg(all(windows, target_arch = "x86_64"))]
        if self.update_panel {
            self.update_panel_pointer();
            return;
        }
        let regions = self.window_regions(self.active);
        let chrome = self.chrome_action_at_pointer();
        let over_suggestions = chrome.is_some_and(Action::address_suggestion);
        let divider = !over_suggestions
            && regions
                .dock
                .is_some_and(|r| self.pointer_in(r) && self.mouse.1 - r.y < 5.);
        let inspecting = !over_suggestions
            && regions.dock.is_some()
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
                    Some(Action::SuggestionBackground) => CursorIcon::Default,
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
        let regions = self.window_regions(self.active);
        let height = regions.viewport(&self.tabs[self.active]).height;
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
            regions.page.y + (y - tab.scroll) * tab.zoom,
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
    fn toggle_downloads(&mut self, keyboard: bool) {
        if self.settings_panel.is_some() || self.update_panel_open() {
            return;
        }
        if self.downloads_panel.is_some() {
            self.dismiss_downloads();
            return;
        }
        self.dismiss_history_menu();
        self.chrome_menu = None;
        self.select_popup = None;
        self.chrome_hover = None;
        self.hover = None;
        self.caption_pressed = None;
        self.last_title_press = None;
        self.preedit = false;
        self.wheel = None;
        self.resizing_dock = false;
        self.downloads_panel = Some(downloads_ui::Panel::open(keyboard));
        if let Some(window) = &self.window {
            window.set_ime_allowed(false);
        }
        self.redraw();
    }

    fn dismiss_downloads(&mut self) {
        if self.downloads_panel.take().is_some() {
            self.downloads_frame = None;
            self.downloads_hover = None;
            if let Some(window) = &self.window {
                window.set_ime_allowed(self.settings_panel.is_none() && !self.update_panel_open());
                window.set_cursor(CursorIcon::Default);
            }
            self.redraw();
        }
    }

    fn downloads_viewport(&self) -> Option<(Rect, Rect)> {
        let size = self.window.as_ref()?.inner_size();
        let width = size.width as f32 / self.scale;
        Some(downloads_geometry(
            &self.toolbar_layout(width),
            width,
            size.height as f32 / self.scale,
        ))
    }

    fn downloads_key(&mut self, key: &Key, repeat: bool) {
        if self.modifiers.control_key() || self.modifiers.alt_key() || self.modifiers.super_key() {
            return;
        }
        if key == &Key::Named(NamedKey::Escape) {
            self.dismiss_downloads();
            return;
        }
        let Some((window, anchor)) = self.downloads_viewport() else {
            return;
        };
        let Some(panel) = &mut self.downloads_panel else {
            return;
        };
        let Some(layout) = panel.layout(&self.downloads, window, anchor) else {
            return;
        };
        match key {
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::Tab) => {
                let previous = *key == Key::Named(NamedKey::ArrowUp)
                    || (*key == Key::Named(NamedKey::Tab) && self.modifiers.shift_key());
                panel.move_focus(&layout, previous);
                self.redraw();
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) if !repeat => {
                if let Some(action) = panel.focused(&layout) {
                    self.downloads_action(action);
                }
            }
            Key::Named(NamedKey::PageUp | NamedKey::PageDown) => {
                if panel.scroll_page(&layout, *key == Key::Named(NamedKey::PageUp)) {
                    self.redraw();
                }
            }
            _ => {}
        }
    }

    fn downloads_press(&mut self) {
        let Some(frame) = &self.downloads_frame else {
            // Nothing is presented yet (or viewport coordinates are stale).
            return;
        };
        if let Some(action) = frame.hit(self.mouse.0, self.mouse.1) {
            self.downloads_action(action);
        } else if !self.pointer_in(frame.bounds) {
            self.dismiss_downloads();
        }
    }

    fn downloads_action(&mut self, action: downloads_ui::Action) {
        use downloads_ui::Action::*;
        match action {
            Dismiss => self.dismiss_downloads(),
            Cancel(id) => self.cancel_download(id),
            ShowAll | ShowRecent => {
                if let Some(panel) = &mut self.downloads_panel {
                    panel.show_all(action == ShowAll);
                    self.redraw();
                }
            }
        }
    }

    fn update_downloads_pointer(&mut self) {
        let target = self
            .downloads_frame
            .as_ref()
            .and_then(|frame| frame.hit(self.mouse.0, self.mouse.1));
        if let Some(window) = &self.window {
            let button = matches!(self.chrome_action_at_pointer(), Some(Action::Downloads));
            window.set_cursor(self.frame_resize_at_pointer().map_or_else(
                || {
                    if target.is_some() || button {
                        CursorIcon::Pointer
                    } else {
                        CursorIcon::Default
                    }
                },
                resize_cursor,
            ));
        }
        if self.downloads_hover != target {
            self.downloads_hover = target;
            self.redraw();
        }
    }

    fn downloads_wheel(&mut self, delta: f32) {
        if !self
            .downloads_frame
            .as_ref()
            .is_some_and(|frame| self.pointer_in(frame.bounds))
        {
            return;
        }
        if let Some((window, anchor)) = self.downloads_viewport()
            && let Some(panel) = &mut self.downloads_panel
            && let Some(layout) = panel.layout(&self.downloads, window, anchor)
            && panel.wheel(&layout, delta)
        {
            self.redraw();
        }
    }

    fn cancel_download(&mut self, id: u64) {
        if cancel_download(&mut self.downloads, &mut self.tabs, id) {
            self.redraw();
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
        self.download_id += 1;
        let id = self.download_id;
        let suggested = &response.response.suggested_name;
        let path = match crate::downloads::destination(window, suggested) {
            Ok(Some(path)) => path,
            Ok(None) => {
                tab.download_status = "Download cancelled".into();
                self.downloads
                    .record(id, suggested, downloads_ui::Start::Cancelled);
                self.redraw();
                return;
            }
            Err(e) => {
                tab.download_status = format!("Could not save the download: {e}");
                self.downloads
                    .record(id, suggested, downloads_ui::Start::DestinationError);
                self.redraw();
                return;
            }
        };
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let cancel = self.jobs.save_download(tab.id, id, path, response);
        self.downloads
            .record(id, &name, downloads_ui::Start::Transfer);
        tab.download_status = format!("Downloading {name} · Esc to cancel");
        tab.download = Some(ActiveDownload { id, cancel, name });
        self.redraw();
    }

    fn content_click(&mut self) {
        if self.settings_panel.is_some()
            || self.downloads_panel.is_some()
            || !self.pointer_in(self.window_regions(self.active).page)
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
        let content_top = self.window_regions(self.active).page.y;
        let tab = &mut self.tabs[self.active];
        if tab.recovery.is_retry_link(link.as_deref(), &tab.url) {
            let url = tab.url.clone();
            self.navigate(url, false);
            return;
        }
        let x = self.mouse.0 / tab.zoom + tab.scroll_x;
        let y = (self.mouse.1 - content_top) / tab.zoom + tab.scroll;
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
            if self.visit_history_click(&url) {
                return;
            }
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
        let y = content.y + (element.y + element.height - tab.scroll) * tab.zoom;
        let popup_height = row_height * rows as f32;
        let y = if y + popup_height > bottom {
            y - element.height * tab.zoom - popup_height
        } else {
            y
        }
        .clamp(content.y, bottom - popup_height);
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
        self.publish_form_interaction();
        self.ensure_layout();
    }
    fn publish_form_interaction(&mut self) {
        let active = self.active;
        self.sync_runtime(active);
        self.dispatch_forms(active);
        self.dispatch_fetches(active);
        self.tabs[active].queue_form_reveal();
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
        let motion_redraw = self.chrome_motion.take_due(Instant::now());
        if let Some(next) = self.chrome_motion.deadline() {
            deadline = Some(deadline.map_or(next, |current| current.min(next)));
        }
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
        if let Err(error) = self.profile.save_storage() {
            self.status = format!("Site data could not be saved. {error}");
        }
        if redraw {
            self.ensure_layout();
            self.set_title();
            self.redraw();
        } else if motion_redraw {
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
            .with_min_inner_size(LogicalSize::new(390., 400.));
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
        #[cfg(all(windows, target_arch = "x86_64"))]
        if self.poll_updates(event_loop) {
            return;
        }
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
                let update = match message.payload {
                    Payload::DownloadProgress(bytes) => DownloadUpdate::Progress(bytes),
                    Payload::DownloadDone(result) => DownloadUpdate::Done(result),
                    _ => unreachable!(),
                };
                if apply_download_update(
                    &mut self.downloads,
                    &mut self.tabs,
                    message.tab,
                    message.navigation,
                    update,
                ) {
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
        if matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        ) && self
            .tabs
            .get(self.active)
            .is_some_and(|tab| tab.runtime.is_some() && tab.layout.is_none())
        {
            // Hit testing can precede the redraw requested by an input handler.
            self.ensure_layout();
        }
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
            WindowEvent::Focused(true) => {
                if self
                    .chrome_motion
                    .set_enabled(!self.chrome_motion_forced_off && scroll::animations_enabled())
                {
                    self.redraw();
                }
            }
            WindowEvent::Focused(false) => {
                let caption_changed = self.caption_pressed.take().is_some();
                self.settings_cancel_pointer();
                self.dismiss_address_suggestions();
                self.wheel = None;
                self.resizing_dock = false;
                self.preedit = false;
                self.dismiss_history_menu();
                self.dismiss_downloads();
                #[cfg(all(windows, target_arch = "x86_64"))]
                self.dismiss_update_panel();
                if caption_changed {
                    self.redraw();
                }
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                #[cfg(all(windows, target_arch = "x86_64"))]
                {
                    let owns_key = self.update_owns_key(&event.logical_key);
                    if self.update_keys.blocks(
                        event.physical_key,
                        event.state,
                        event.repeat,
                        is_synthetic,
                        owns_key,
                    ) {
                        return;
                    }
                }
                if event.state != ElementState::Pressed {
                    return;
                }
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
            // Windows sends Enabled at the start of a new composition. A
            // pending Commit from disabling IME belongs to the old surface,
            // even if Escape or a click has since closed the flyout.
            #[cfg(all(windows, target_arch = "x86_64"))]
            WindowEvent::Ime(Ime::Enabled) if !self.update_panel => {
                self.update_ime_blocked = false;
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.preedit = false;
                #[cfg(all(windows, target_arch = "x86_64"))]
                if self.update_ime_blocked {
                    return;
                }
                self.insert(&text);
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                #[cfg(all(windows, target_arch = "x86_64"))]
                if self.update_ime_blocked {
                    return;
                }
                self.preedit = self.settings_panel.is_none()
                    && self.downloads_panel.is_none()
                    && !text.is_empty();
                if self.preedit && self.address_focus {
                    self.dismiss_address_suggestions();
                }
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
                self.settings_cancel_pointer();
                self.clear_history_hover();
                self.chrome_hover = None;
                self.downloads_hover = None;
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
                    && self.downloads_panel.is_none()
                    && !self.update_panel_open()
                    && WINDOW_FRAME
                    && self.settings_panel.is_none()
                    && self.tab_hits_current
                    && self.mouse.1 < TAB_H
                    && self.chrome_action_at_pointer().is_none()
                    && let Some(window) = &self.window
                {
                    window.show_window_menu(LogicalPosition::new(self.mouse.0, self.mouse.1));
                } else if button == MouseButton::Middle
                    && self.settings_panel.is_none()
                    && self.history_menu.is_none()
                    && self.downloads_panel.is_none()
                    && !self.update_panel_open()
                    && self.chrome_menu.is_none()
                    && self.tab_hits_current
                    && !self
                        .chrome_action_at_pointer()
                        .is_some_and(Action::address_suggestion)
                {
                    if let Some(Action::Select(id) | Action::Close(id)) =
                        self.chrome_action_at_pointer()
                    {
                        self.action(Action::Close(id), event_loop);
                    } else if let Some(url) = self.pointer_link() {
                        self.action(Action::NewTab, event_loop);
                        self.navigate(url, true);
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.left_release(event_loop),
            WindowEvent::MouseWheel { delta, .. } => {
                if self.settings_panel.is_some()
                    || self.history_menu.is_some()
                    || self.update_panel_open()
                {
                    return;
                }
                if self
                    .chrome_action_at_pointer()
                    .is_some_and(Action::address_suggestion)
                {
                    return;
                }
                let (delta, detent) = scroll::wheel_delta(delta, self.scale);
                if self.downloads_panel.is_some() {
                    self.downloads_wheel(delta.y);
                    return;
                }
                if !self.tab_hits_current {
                    return;
                }
                let regions = self.window_regions(self.active);
                if let Some(Menu::Tabs(selected)) = self.chrome_menu {
                    if matches!(
                        self.chrome_action_at_pointer(),
                        Some(Action::TabListItem(_))
                    ) {
                        let next = if delta.y > 0. {
                            (selected + 1).min(self.tabs.len() - 1)
                        } else if delta.y < 0. {
                            selected.saturating_sub(1)
                        } else {
                            selected
                        };
                        self.chrome_menu = Some(Menu::Tabs(next));
                        self.redraw();
                    }
                    return;
                }
                if self.chrome_menu.is_some()
                    && self.chrome_action_at_pointer().is_some_and(Action::in_menu)
                {
                    return;
                }
                if self.scroll_favorites_at_pointer(regions.page.width, delta) {
                    return;
                }
                let strip = self.tab_strip(regions.page.width);
                if self.pointer_in(strip.list) {
                    let dx = if delta.x != 0. { delta.x } else { delta.y };
                    let next = strip.scroll_to(self.tab_scroll + dx).scroll();
                    if self.tab_scroll != next {
                        self.tab_scroll = next;
                        self.tab_hits_current = false;
                        self.redraw();
                    }
                    return;
                }
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
            WindowEvent::DroppedFile(path)
                if self.settings_panel.is_none()
                    && !self.update_panel_open()
                    && self.tab_hits_current =>
            {
                self.dismiss_history_menu();
                self.dismiss_downloads();
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

fn chrome_box(bounds: quiet_cinder::Box2) -> Rect {
    scaled_rect(1., bounds.x, bounds.y, bounds.w, bounds.h)
}

fn downloads_geometry(toolbar: &quiet_cinder::Toolbar, width: f32, height: f32) -> (Rect, Rect) {
    // At short window sizes the popover can slide upward, but not over the
    // native title/caption buttons which retain pointer priority.
    let top = quiet_cinder::TAB_ROW;
    (
        scaled_rect(1., 0., top, width, (height - top).max(0.)),
        chrome_box(toolbar.downloads_anchor()),
    )
}

fn centered_chrome_icon(
    canvas: &mut Canvas,
    bounds: Rect,
    size: f32,
    scale: f32,
    action: Action,
    loading: bool,
    color: Color,
) {
    use icons::Glyph;
    let glyph = match action {
        Action::Back => Glyph::ArrowLeft,
        Action::Forward => Glyph::ArrowRight,
        Action::Home => Glyph::House,
        Action::Reload if loading => Glyph::X,
        Action::Reload => Glyph::RotateCw,
        Action::Bookmarks => Glyph::BookMarked,
        Action::Downloads => Glyph::Download,
        Action::Menu => Glyph::Ellipsis,
        Action::Minimize => Glyph::Minus,
        Action::Maximize if loading => Glyph::Copy,
        Action::Maximize => Glyph::Square,
        Action::CloseWindow => Glyph::X,
        _ => return,
    };
    paint_centered_glyph(canvas, bounds, size, scale, glyph, color);
}

fn paint_centered_glyph(
    canvas: &mut Canvas,
    bounds: Rect,
    size: f32,
    scale: f32,
    glyph: icons::Glyph,
    color: Color,
) {
    let zoom = size / 20.;
    paint_glyph(
        canvas,
        (bounds.x + (bounds.width - size) / 2.) / zoom,
        (bounds.y + (bounds.height - size) / 2.) / zoom,
        zoom * scale,
        glyph,
        color,
    );
}

fn paint_bookmarked_star(canvas: &mut Canvas, bounds: Rect, scale: f32) {
    let size = 17.;
    icons::Glyph::rasterize_bookmarked_star(
        (bounds.x + (bounds.width - size) / 2.) * scale,
        (bounds.y + (bounds.height - size) / 2.) * scale,
        size * scale,
        canvas.width,
        canvas.height,
        |x, y, stroke, fill| {
            let (stroke, fill) = (u32::from(stroke), u32::from(fill));
            let coverage = stroke + fill;
            if coverage != 0 {
                let mix = |a: u8, b: u8| {
                    ((u32::from(a) * stroke + u32::from(b) * fill + coverage / 2) / coverage) as u8
                };
                canvas.fill_rect(
                    scaled_rect(1., x as f32, y as f32, 1., 1.),
                    Color(
                        mix(CHROME_ACCENT.0, CHROME_ACCENT_SOFT.0),
                        mix(CHROME_ACCENT.1, CHROME_ACCENT_SOFT.1),
                        mix(CHROME_ACCENT.2, CHROME_ACCENT_SOFT.2),
                        coverage.min(255) as u8,
                    ),
                );
            }
        },
    );
}

fn chrome_mix(Color(r, g, b, a): Color, Color(r2, g2, b2, a2): Color, amount: f32) -> Color {
    let mix = |from: u8, to: u8| {
        let from = f32::from(from);
        (from + (f32::from(to) - from) * amount).round() as u8
    };
    Color(mix(r, r2), mix(g, g2), mix(b, b2), mix(a, a2))
}

fn chrome_alpha(Color(r, g, b, a): Color, amount: f32) -> Color {
    Color(r, g, b, (f32::from(a) * amount).round() as u8)
}

fn draw_tab_face(canvas: &mut Canvas, bounds: Rect, u: f32, active: bool, ink: &TabInk) {
    let radii = [tab_strip::TAB_RADIUS * u; 4];
    let bounds = physical_rect(u, bounds);
    if active {
        canvas.fill_rounded_linear_gradient(
            bounds,
            radii,
            110.,
            [
                (0., Color(0x30, 0x2e, 0x34, 255)),
                (0.73, Color(0x3b, 0x35, 0x42, 255)),
                (1., Color(0x48, 0x3c, 0x51, 255)),
            ],
        );
    } else if ink.hover > 0. {
        canvas.fill_rounded_rect(bounds, radii, chrome_alpha(CHROME_HOVER, ink.hover));
    }
    if ink.border > 0. {
        canvas.outline_rounded_rect(
            bounds,
            radii,
            -2. * u,
            u,
            chrome_alpha(CHROME_ACCENT_SOFT, ink.border),
        );
        canvas.outline_rounded_rect(
            bounds,
            radii,
            -u,
            u,
            chrome_alpha(Color(125, 107, 150, 255), ink.border),
        );
    }
}

fn chrome_shape(
    fonts: &Fonts,
    text: &str,
    size: f32,
    weight: u16,
) -> rust_browser_engine::fonts::Run {
    use rust_browser_engine::fonts::{FontFamily, FontRequest, FontSet, FontStyle, GenericFamily};
    use std::sync::OnceLock;
    static FAMILIES: OnceLock<Arc<[FontFamily]>> = OnceLock::new();
    static EMPTY: OnceLock<FontSet> = OnceLock::new();
    let families = FAMILIES.get_or_init(|| {
        [
            FontFamily::Named("Segoe UI Variable Text".into()),
            FontFamily::Named("Segoe UI".into()),
            FontFamily::Generic(GenericFamily::SystemUi),
            FontFamily::Generic(GenericFamily::SansSerif),
        ]
        .into()
    });
    fonts.shape_text(
        text,
        size,
        &FontRequest {
            families: families.clone(),
            weight,
            style: FontStyle::Normal,
        },
        EMPTY.get_or_init(FontSet::default),
    )
}

fn chrome_elide(fonts: &Fonts, text: &str, width: f32, size: f32, weight: u16) -> String {
    if chrome_shape(fonts, text, size, weight).advance <= width {
        return text.into();
    }
    let mut prefix: String = text.chars().take(80).collect();
    while !prefix.is_empty()
        && chrome_shape(fonts, &format!("{prefix}…"), size, weight).advance > width
    {
        prefix.pop();
    }
    prefix.push('…');
    prefix
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
    canvas.fill_rounded_rect(rect, [radius; 4], color);
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
mod download_wiring_tests {
    use super::*;

    fn download_tab(downloads: &mut downloads_ui::Downloads, tab_id: u64, transfer_id: u64) -> Tab {
        let mut tab = Tab::new(tab_id, "about:home".into(), Default::default());
        tab.download = Some(ActiveDownload {
            id: transfer_id,
            cancel: CancelSignal::new(),
            name: format!("file-{transfer_id}.txt"),
        });
        downloads.record(
            transfer_id,
            &tab.download.as_ref().unwrap().name,
            downloads_ui::Start::Transfer,
        );
        tab
    }

    #[test]
    fn cancellation_targets_the_live_transfer_id_across_tabs() {
        let mut downloads = downloads_ui::Downloads::default();
        let foreground = download_tab(&mut downloads, 1, 7);
        let mut background = download_tab(&mut downloads, 2, 8);
        background.navigation = 42;
        let mut tabs = [foreground, background];

        assert!(!cancel_download(&mut downloads, &mut tabs, 2));
        assert!(!apply_download_update(
            &mut downloads,
            &mut tabs,
            2,
            42,
            DownloadUpdate::Done(Ok(100))
        ));
        assert!(cancel_download(&mut downloads, &mut tabs, 8));
        assert!(!cancel_download(&mut downloads, &mut tabs, 8));
        assert!(!tabs[0].download.as_ref().unwrap().cancel.is_cancelled());
        assert!(tabs[1].download.as_ref().unwrap().cancel.is_cancelled());
        assert!(!apply_download_update(
            &mut downloads,
            &mut tabs,
            2,
            8,
            DownloadUpdate::Progress(768)
        ));
        assert_eq!(tabs[1].download_status, "Cancelling download…");

        assert!(apply_download_update(
            &mut downloads,
            &mut tabs,
            2,
            8,
            DownloadUpdate::Done(Ok(1024))
        ));
        assert_eq!(tabs[1].download_status, "Saved file-8.txt · 1024 bytes");
        assert!(tabs[1].download.is_none());
        assert!(!cancel_download(&mut downloads, &mut tabs, 8));
        assert!(downloads.has_active());
    }

    #[test]
    fn terminal_event_settles_the_session_row_after_source_tab_closes() {
        let mut downloads = downloads_ui::Downloads::default();
        let source = download_tab(&mut downloads, 9, 12);
        let mut tabs = vec![
            Tab::new(10, "about:home".into(), Default::default()),
            source,
        ];
        assert!(cancel_download(&mut downloads, &mut tabs, 12));
        tabs.pop();
        assert!(downloads.has_active());
        assert!(apply_download_update(
            &mut downloads,
            &mut tabs,
            9,
            12,
            DownloadUpdate::Done(Err("Download cancelled".into()))
        ));
        assert!(!downloads.has_active());
        assert!(tabs[0].download_status.is_empty());
        assert!(!apply_download_update(
            &mut downloads,
            &mut tabs,
            9,
            12,
            DownloadUpdate::Done(Ok(100))
        ));
    }

    #[test]
    fn reordered_source_keeps_its_transfer_but_a_reopened_tab_does_not_inherit_it() {
        let mut downloads = downloads_ui::Downloads::default();
        let mut source = download_tab(&mut downloads, 9, 12);
        source.navigation = 7;
        let mut tabs = vec![
            Tab::new(10, "about:shortcuts".into(), Default::default()),
            source,
        ];

        let active = tab_behavior::move_tab(&mut tabs, 9, TabMove::Left, |tab| tab.id).unwrap();
        assert_eq!(active, 0);
        assert_eq!(tabs[active].navigation, 7);
        assert!(apply_download_update(
            &mut downloads,
            &mut tabs,
            9,
            12,
            DownloadUpdate::Progress(120)
        ));
        let mut closed = ClosedTabs::default();
        closed.remember(tabs[active].fresh_address());
        assert!(cancel_download(&mut downloads, &mut tabs, 12));
        tabs.remove(active);
        let fresh = closed.reopen().unwrap().into_address();
        tabs.push(Tab::new(11, fresh, Default::default()));
        assert_eq!(tabs[1].url, "about:home");
        assert_eq!(tabs[1].navigation, 0);
        assert!(tabs[1].download.is_none());
        assert!(apply_download_update(
            &mut downloads,
            &mut tabs,
            9,
            12,
            DownloadUpdate::Done(Err("Download cancelled".into()))
        ));
        assert!(!downloads.has_active());
        assert!(tabs[1].download_status.is_empty());
    }

    #[test]
    fn toolbar_slot_and_popup_anchor_share_geometry() {
        for width in [320., 390., 410., 640., 660., 661., 880., 881., 1280.] {
            for quiet in [false, true] {
                let normal = quiet_cinder::Toolbar::new(width, false, quiet);
                for show_update in [false, true] {
                    let toolbar = quiet_cinder::Toolbar::new(width, show_update, quiet);
                    let (viewport, anchor) = downloads_geometry(&toolbar, width, 400.);
                    assert_eq!(anchor, chrome_box(normal.downloads_anchor()));
                    assert_eq!(viewport.y, quiet_cinder::TAB_ROW);
                    assert_eq!(viewport.y + viewport.height, 400.);
                }
            }
        }
    }
}

#[cfg(test)]
mod form_frame_tests {
    use super::*;

    #[test]
    fn successive_edits_share_layout_and_only_reveal_the_still_focused_control() {
        let url = "https://forms.test/";
        let page = LoadedPage::from_html(
            url,
            "<style>input{display:block;margin-top:600px}</style><input id=q>",
        );
        let field = page
            .document
            .connected_nodes()
            .find(|&id| page.document.tag(id) == "input")
            .unwrap();
        let mut tab = Tab::new(1, url.into(), Default::default());
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let viewport = Viewport {
            width: 360.,
            height: 200.,
            ..Default::default()
        };
        runtime.focus_control(Some(field)).unwrap();
        tab.layout = Some(runtime.layout_for_paint(viewport).unwrap());
        tab.runtime = Some(runtime);
        for key in ["a", "b"] {
            tab.runtime.as_mut().unwrap().insert_form_text(key).unwrap();
            tab.queue_form_reveal();
            assert!(tab.runtime.as_ref().unwrap().layout_needs_refresh());
            assert_eq!(tab.scroll, 0.);
        }
        let runtime = tab.runtime.as_mut().unwrap();
        assert_eq!(
            runtime
                .eval("document.getElementById('q').value === 'ab'")
                .unwrap(),
            true.into()
        );
        tab.layout = Some(runtime.layout_for_paint(viewport).unwrap());
        let rect = tab.layout.as_ref().unwrap().rect_for(field).unwrap();
        tab.reveal_edited_control(viewport.height);
        assert_eq!(tab.scroll, rect.y + rect.height - viewport.height);
        tab.scroll = 0.;
        tab.reveal_edited_control(viewport.height);
        assert_eq!(tab.scroll, 0.);

        tab.queue_form_reveal();
        tab.runtime.as_mut().unwrap().focus_control(None).unwrap();
        tab.reveal_edited_control(viewport.height);
        assert_eq!(tab.scroll, 0.);
    }
}

#[cfg(test)]
mod dock_tests {
    use super::*;

    #[test]
    fn quiet_viewport_and_hits_start_after_the_toolbar_not_the_hidden_bar() {
        let top = quiet_cinder::TOOLBAR_END;
        let quiet = WindowRegions::with_dock_height(390., 400., 0., false, None, top);
        assert_eq!(quiet.page.y, 91.);
        assert_eq!(quiet.page.height, 309.);
        assert!(!quiet.page.contains(195., top - 0.5));
        assert!(quiet.page.contains(195., top));
        let quiet_dock = WindowRegions::with_dock_height(390., 400., 0., true, None, top);
        assert_eq!(
            quiet_dock.dock.unwrap().y,
            quiet_dock.page.y + quiet_dock.page.height
        );
    }

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
