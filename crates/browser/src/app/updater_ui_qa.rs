//! Ignored offline QA: build and present the real Winit App, without OS input injection.
use super::*;
use crate::updater::{UpdateAction, Updates};
use std::{cell::RefCell, fs};
use winit::keyboard::{KeyCode, PhysicalKey};

const LINK: &str = "about:version";
const PAGE_TITLE: &str = "Offline updater fixture";
const HEIGHT: u32 = 800;
const DOWNLOAD: Action = Action::UpdatePrimary(UpdateAction::Download);
const RESTART: Action = Action::UpdatePrimary(UpdateAction::Restart);
const FIRST_FAVORITE: Action = Action::Favorite(0);
const LAST_FAVORITE: Action = Action::Favorite(5);

#[derive(Clone, Copy, Debug)]
enum Step {
    Baseline,
    Badge,
    Offer,
    Escaped,
    OutsideOpen,
    OutsideDismissed,
    DownloadOpen,
    Progress,
    Ready,
    Dismissed,
    LinkReached,
    ResizeWide,
    ResizeNarrow,
    FavoritesScrolled,
    QuietPage,
    QuietScrolled,
    ResizeRestored,
    TabsWide,
    TabsContext,
    TabsPinned,
    TabsDragging,
    TabsDropped,
    TabsNarrow,
    TabsNarrowRevealed,
    TabsNarrowContext,
    TabsUnpinned,
}

struct Driver {
    output: PathBuf,
    url: String,
    size: PhysicalSize<u32>,
    step: Step,
    deadline: Instant,
    previous_pixels: Option<Vec<u32>>,
    result: Option<Result<(), String>>,
}

thread_local! {
    static DRIVER: RefCell<Option<Driver>> = const { RefCell::new(None) };
}

pub(super) fn active() -> bool {
    size().is_some()
}

pub(super) fn size() -> Option<PhysicalSize<u32>> {
    DRIVER.with(|cell| cell.borrow().as_ref().map(|driver| driver.size))
}

pub(super) fn deadline(app: &mut App, events: &ActiveEventLoop) -> Option<Instant> {
    DRIVER.with(|cell| {
        let mut slot = cell.borrow_mut();
        let driver = slot.as_mut()?;
        if driver.result.is_none() && Instant::now() >= driver.deadline {
            let actual = app.window.as_ref().map(|window| window.inner_size());
            driver.result = Some(Err(format!(
                "Timed out in {:?}: expected {:?}, actual {actual:?}, scale {}, frames {}",
                driver.step, driver.size, app.scale, app.presented_frames
            )));
            app.finish_close(events);
        }
        Some(driver.deadline)
    })
}

pub(super) fn after_draw(app: &mut App, events: &ActiveEventLoop) {
    DRIVER.with(|cell| {
        let mut slot = cell.borrow_mut();
        let Some(driver) = slot.as_mut() else { return };
        if driver.result.is_some() {
            return;
        }
        match driver.after_frame(app, events) {
            Ok(false) => app.redraw(),
            finished => {
                driver.result = Some(finished.map(|_| ()).map_err(|error| {
                    format!("{:?} at {:?}: {error}", driver.step, driver.size)
                }));
                app.finish_close(events);
            }
        }
    });
}

fn require(ok: bool, failure: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(failure.into()) }
}

fn rect(rect: Rect) -> [f32; 4] {
    [rect.x, rect.y, rect.width, rect.height]
}

fn center([x, y, w, h]: [f32; 4]) -> (f32, f32) {
    (x + w / 2., y + h / 2.)
}

fn hit(app: &App, action: Action) -> Option<[f32; 4]> {
    app.hits
        .iter()
        .rev()
        .find_map(|(bounds, item)| (*item == action).then(|| rect(*bounds)))
}

fn exact_hit(app: &App, action: Action, name: &str, bounds: [f32; 4]) -> Result<(), String> {
    let actual = hit(app, action);
    require(actual == Some(bounds), &format!("{name}: expected {bounds:?}, got {actual:?}"))
}

fn click(app: &mut App, events: &ActiveEventLoop, point: (f32, f32)) {
    app.mouse = point;
    app.pointer_inside = true;
    app.left_press(events);
    app.left_release(events);
}

fn move_pointer(app: &mut App, events: &ActiveEventLoop, point: (f32, f32)) {
    let window = app.window.as_ref().unwrap().id();
    let device_id = winit::event::DeviceId::dummy();
    app.window_event(events, window, WindowEvent::CursorMoved {
        device_id,
        position: winit::dpi::PhysicalPosition::new(f64::from(point.0), f64::from(point.1)),
    });
}

fn mouse_button(app: &mut App, events: &ActiveEventLoop, button: MouseButton, state: ElementState) {
    let window = app.window.as_ref().unwrap().id();
    app.window_event(events, window, WindowEvent::MouseInput {
        button,
        state,
        device_id: winit::event::DeviceId::dummy(),
    });
}

fn wheel(app: &mut App, events: &ActiveEventLoop, point: (f32, f32), dx: f64, dy: f64) {
    let window = app.window.as_ref().unwrap().id();
    let device_id = winit::event::DeviceId::dummy();
    move_pointer(app, events, point);
    app.window_event(events, window, WindowEvent::MouseWheel {
        device_id,
        delta: winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(-dx, -dy)),
        phase: winit::event::TouchPhase::Moved,
    });
}

// Winit's KeyEvent has platform-private fields. Use the same guard and App
// key handler with explicit physical identity; never fake OS keyboard input.
fn feed_key(
    app: &mut App,
    events: &ActiveEventLoop,
    code: KeyCode,
    key: Key,
    state: ElementState,
    repeat: bool,
    synthetic: bool,
) -> bool {
    let owns_key = app.site_info.is_some() || app.update_owns_key(&key);
    let blocked = app.chrome_keys.blocks(PhysicalKey::Code(code), state, repeat, synthetic, owns_key);
    if !blocked && state == ElementState::Pressed {
        app.key(key, None, repeat, events);
    }
    !blocked
}

fn named(
    app: &mut App,
    events: &ActiveEventLoop,
    code: KeyCode,
    key: NamedKey,
    state: ElementState,
    repeat: bool,
    synthetic: bool,
) -> bool {
    feed_key(app, events, code, Key::Named(key), state, repeat, synthetic)
}

fn tap(app: &mut App, events: &ActiveEventLoop, code: KeyCode, key: NamedKey) -> bool {
    let accepted = named(app, events, code, key, ElementState::Pressed, false, false);
    named(app, events, code, key, ElementState::Released, false, false);
    accepted
}

fn alt_u(app: &mut App, events: &ActiveEventLoop) -> bool {
    let prior = std::mem::replace(&mut app.modifiers, ModifiersState::ALT);
    let accepted = feed_key(
        app, events, KeyCode::KeyU, Key::Character("u".into()), ElementState::Pressed, false, false,
    );
    app.modifiers = prior;
    feed_key(
        app, events, KeyCode::KeyU, Key::Character("u".into()), ElementState::Released, false, false,
    );
    accepted
}

fn updates(app: &App) -> Result<&Updates, String> {
    app.updater.as_ref().ok_or_else(|| "Missing offline test transport".into())
}

fn commands(app: &App, expected: [usize; 3]) -> Result<(), String> {
    let actual = updates(app)?.ui_qa_commands();
    require(app.update_restart.is_none() && actual == expected,
        &format!("Check / Download / Restart: expected {expected:?}, got {actual:?}"))
}

fn rgb_at(canvas: &Canvas, x: usize, y: usize, Color(r, g, b, _): Color) -> bool {
    canvas.pixels[y * canvas.width as usize + x] & 0x00ff_ffff
        == (u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b))
}

impl Driver {
    fn request_width(&mut self, app: &App, width: u32) {
        self.size = PhysicalSize::new(width, HEIGHT);
        let _ = app.window.as_ref().unwrap().request_inner_size(self.size);
    }

    fn capture(&self, app: &App, name: &str) -> Result<(), String> {
        app.canvas.as_ref().ok_or("Missing presented canvas")?
            .save_png(&self.output.join(name))
            .map_err(|error| error.to_string())
    }

    fn after_frame(&mut self, app: &mut App, events: &ActiveEventLoop) -> Result<bool, String> {
        if let Some(error) = &app.last_present_error {
            return Err(format!("Winit surface did not present: {error}"));
        }
        let window = app.window.as_ref().ok_or("Missing Winit window")?;
        require(app.scale == 1. && window.scale_factor() == 1., "Fixture requires 100% scale")?;
        let tab = &app.tabs[app.active];
        let canvas = app.canvas.as_ref().ok_or("Missing presented canvas")?;
        if !app.tab_hits_current || window.inner_size() != self.size
            || (canvas.width, canvas.height) != (self.size.width, self.size.height)
            || tab.loading || tab.layout.is_none()
            || tab.document_session.as_ref().is_some_and(|session| {
                !session.done() || session.needs_advance(tab.runtime.as_ref())
            })
        {
            self.previous_pixels = None;
            return Ok(false);
        }
        require(!canvas.limited && tab.layout.as_ref().is_some_and(|layout| !layout.limited),
            "The frame hit a paint or layout limit")?;
        if self.previous_pixels.as_deref() != Some(canvas.pixels.as_slice()) {
            self.previous_pixels = Some(canvas.pixels.clone());
            return Ok(false);
        }
        self.previous_pixels = None;
        self.advance(app, events)
    }

    fn advance_resize(&mut self, app: &mut App, events: &ActiveEventLoop) -> Result<bool, String> {
        let page = app.window_regions(app.active).page;
        let scroll = app.tabs[app.active].scroll;
        let canvas = app.canvas.as_ref().ok_or("Missing resize canvas")?;
        require(app.tabs[app.active].url == self.url, "Unexpected fixture navigation")?;
        commands(app, [0, 0, 0])?;
        self.step = match self.step {
            Step::ResizeWide => {
                require(page.y == 121. && scroll == 0. && hit(app, LAST_FAVORITE).is_some(),
                    "Wide frame should show all six favorites above unscrolled content")?;
                self.capture(app, "00-wide.png")?;
                self.request_width(app, 390);
                Step::ResizeNarrow
            }
            Step::ResizeNarrow => {
                let [x, y, w, h] = hit(app, FIRST_FAVORITE).ok_or("Missing first favorite")?;
                require(page.y == 121. && scroll == 0. && hit(app, LAST_FAVORITE).is_none()
                    && x + w <= 390., "Narrow layout did not clip the overflow")?;
                self.capture(app, "01-narrow.png")?;
                wheel(app, events, center([x, y, w, h]), 2000., 0.);
                require(hit(app, FIRST_FAVORITE).is_none(),
                    "Wheel retained a stale hit before the new paint")?;
                Step::FavoritesScrolled
            }
            Step::FavoritesScrolled => {
                let [x, y, w, h] = hit(app, LAST_FAVORITE).ok_or("Last favorite did not scroll into view")?;
                require(scroll == 0. && x >= 0. && x + w <= 390. && hit(app, FIRST_FAVORITE).is_none(),
                    "Favorites overflow changed page scroll or exposed an invalid hit")?;
                app.mouse = center([x, y, w, h]);
                require(app.chrome_action_at_pointer() == Some(LAST_FAVORITE),
                    "Painted final favorite is not actionable at its new position")?;
                self.capture(app, "02-favorites-scrolled.png")?;
                let quiet = hit(app, Action::QuietMode).ok_or("Missing Quiet button")?;
                click(app, events, center(quiet));
                Step::QuietPage
            }
            Step::QuietPage => {
                require(page.y == 91. && scroll == 0. && hit(app, LAST_FAVORITE).is_none()
                    && rgb_at(canvas, 4, 105, Color(237, 240, 245, 255)),
                    "Quiet did not expose page content at the former favorites bar")?;
                self.capture(app, "03-quiet.png")?;
                wheel(app, events, (4., 105.), 0., 480.);
                Step::QuietScrolled
            }
            Step::QuietScrolled => {
                if scroll < 400. {
                    return Ok(false);
                }
                require(page.y == 91. && rgb_at(canvas, 4, 105, Color(37, 88, 116, 255)),
                    "Wheel over the hidden favorites bar did not visibly scroll the page")?;
                self.capture(app, "04-page-scrolled.png")?;
                let quiet = hit(app, Action::QuietMode).ok_or("Missing Quiet button")?;
                click(app, events, center(quiet));
                self.request_width(app, 1280);
                Step::ResizeRestored
            }
            Step::ResizeRestored => {
                require(page.y == 121. && scroll >= 400. && hit(app, FIRST_FAVORITE).is_some()
                    && hit(app, LAST_FAVORITE).is_some(),
                    "Restored wide layout did not clamp favorites scroll and keep document scroll")?;
                self.capture(app, "05-wide-restored.png")?;
                fs::write(self.output.join("passed.txt"), format!(
                    "1280x800 -> 390x800 -> 1280x800 @ 100%, {} presented frames; \
                     clipped favorites scroll and hit invalidation, Quiet routing and page scroll passed.\n",
                    app.presented_frames
                )).map_err(|error| error.to_string())?;
                return Ok(true);
            }
            _ => unreachable!("updater steps use the regular driver"),
        };
        Ok(false)
    }

    fn advance_tabs(&mut self, app: &mut App, events: &ActiveEventLoop) -> Result<bool, String> {
        use ElementState::{Pressed, Released};
        const PIN: Action = Action::TabContextRow(2, tab_context::ContextCommand::Pin);
        let ids: Vec<_> = app.tabs.iter().map(|tab| tab.id).collect();
        let target = |app: &App, action| hit(app, action).map(center).ok_or("Tab control is not visible");
        require(app.tabs[app.active].id == 1 && app.tabs[app.active].url == self.url,
            "Context or drag unexpectedly replaced or activated the fixture tab")?;
        commands(app, [0, 0, 0])?;
        self.step = match self.step {
            Step::TabsWide => {
                require(ids == [1, 2, 3, 4], "Wrong initial tabs")?;
                let point = target(app, Action::Select(2))?;
                move_pointer(app, events, point);
                mouse_button(app, events, MouseButton::Right, Pressed);
                mouse_button(app, events, MouseButton::Right, Released);
                Step::TabsContext
            }
            Step::TabsContext => {
                let point = target(app, PIN)?;
                self.capture(app, "01-tab-context.png")?;
                move_pointer(app, events, point);
                mouse_button(app, events, MouseButton::Left, Pressed);
                require(!app.tabs[1].pinned && app.tabs[1].id == 2, "Pin ran before release")?;
                mouse_button(app, events, MouseButton::Left, Released);
                Step::TabsPinned
            }
            Step::TabsPinned => {
                require(ids == [2, 1, 3, 4] && app.tabs[0].pinned && app.chrome_menu.is_none(),
                    "The right-click target was not pinned and compacted")?;
                let pinned = hit(app, Action::Select(2)).ok_or("Missing pinned hit")?;
                let destination = hit(app, Action::Select(1)).ok_or("Missing unpinned target")?;
                require(pinned[2] < destination[2], "Pin did not yield a compact tab")?;
                self.capture(app, "02-pinned.png")?;
                let point = target(app, Action::Select(4))?;
                move_pointer(app, events, point);
                mouse_button(app, events, MouseButton::Left, Pressed);
                move_pointer(app, events, (destination[0] + 4., destination[1] + destination[3] / 2.));
                Step::TabsDragging
            }
            Step::TabsDragging => {
                require(ids == [2, 1, 3, 4] && app.tab_press.is_some(),
                    "Drag preview committed before release")?;
                self.capture(app, "03-drag-preview.png")?;
                mouse_button(app, events, MouseButton::Left, Released);
                Step::TabsDropped
            }
            Step::TabsDropped => {
                require(ids == [2, 4, 1, 3] && app.tab_press.is_none(),
                    "Valid drop did not commit within the unpinned group")?;
                self.capture(app, "04-drag-released.png")?;
                self.request_width(app, 390);
                Step::TabsNarrow
            }
            Step::TabsNarrow => {
                let strip = app.tab_strip(app.window_regions(app.active).page.width);
                wheel(app, events, center(rect(strip.list)), -1000., 0.);
                Step::TabsNarrowRevealed
            }
            Step::TabsNarrowRevealed => {
                let point = target(app, Action::Select(2))?;
                move_pointer(app, events, point);
                mouse_button(app, events, MouseButton::Right, Pressed);
                mouse_button(app, events, MouseButton::Right, Released);
                Step::TabsNarrowContext
            }
            Step::TabsNarrowContext => {
                let point = target(app, PIN)?;
                self.capture(app, "05-narrow-context.png")?;
                move_pointer(app, events, point);
                mouse_button(app, events, MouseButton::Left, Pressed);
                mouse_button(app, events, MouseButton::Left, Released);
                Step::TabsUnpinned
            }
            Step::TabsUnpinned => {
                require(ids == [2, 4, 1, 3] && !app.tabs[0].pinned && app.chrome_menu.is_none(),
                    "Narrow context menu did not unpin the intended tab")?;
                self.capture(app, "06-narrow-unpinned.png")?;
                fs::write(self.output.join("passed.txt"), format!(
                    "Right-click, release-only pin, wide pointer reorder, and narrow unpin, \
                     {} presented frames at 100%.\n", app.presented_frames
                )).map_err(|error| error.to_string())?;
                return Ok(true);
            }
            _ => unreachable!("other fixtures use their own step handlers"),
        };
        Ok(false)
    }

    fn advance(&mut self, app: &mut App, events: &ActiveEventLoop) -> Result<bool, String> {
        use ElementState::{Pressed, Released};
        if matches!(self.step, Step::TabsWide | Step::TabsContext | Step::TabsPinned | Step::TabsDragging
            | Step::TabsDropped | Step::TabsNarrow | Step::TabsNarrowRevealed
            | Step::TabsNarrowContext | Step::TabsUnpinned)
        {
            return self.advance_tabs(app, events);
        }
        if matches!(self.step, Step::ResizeWide | Step::ResizeNarrow | Step::FavoritesScrolled
            | Step::QuietPage | Step::QuietScrolled | Step::ResizeRestored)
        {
            return self.advance_resize(app, events);
        }
        let narrow = self.size.width == 390;
        // Independent expected frame geometry at DPR 1 for the shared Toolbar.
        let (badge, address_before, address, quiet, menu, panel, downloads) = if narrow {
            ([310., 51.5, 25., 31.], [81., 50., 254., 34.], [81., 50., 228., 34.],
                [336., 51.5, 25., 31.], [362., 51.5, 25., 31.], [12., 90.5, 350., 137.], None)
        } else {
            ([1121., 51.5, 31., 31.], [167., 50., 985., 34.], [167., 50., 946., 34.],
                [1160., 51.5, 31., 31.], [1238., 51.5, 31., 31.], [802., 90.5, 350., 137.],
                Some([1199., 51.5, 31., 31.]))
        };
        let download = [panel[0] + 14., 161.5, 150., 34.];
        let restart = [panel[0] + 186., 161.5, 150., 34.];
        let outside = (24., app.window_regions(app.active).page.y + 260.);
        let settled_open = |app: &App| exact_hit(app, Action::UpdateSurface, "flyout", panel);
        let settled_closed = |app: &App| require(!app.update_panel
            && hit(app, Action::UpdateSurface).is_none()
            && hit(app, DOWNLOAD).is_none() && hit(app, RESTART).is_none(),
            "Dismissed flyout still has a presented surface or action");

        if !matches!(self.step, Step::LinkReached) {
            let tab = &app.tabs[app.active];
            require(tab.url == self.url && tab.title == PAGE_TITLE,
                "Unexpected navigation or fixture title")?;
            exact_hit(app, Action::QuietMode, "Quiet", quiet)?;
            exact_hit(app, Action::Menu, "ellipsis", menu)?;
            require(hit(app, Action::Downloads) == downloads, "Downloads moved or overlapped the badge")?;
            if !matches!(self.step, Step::Baseline) {
                let toolbar = app.toolbar_layout(self.size.width as f32);
                require(toolbar.update.map(chrome_box).map(rect) == Some(badge),
                    "Shared toolbar badge geometry differs")?;
                exact_hit(app, Action::UpdatePanel, "badge", badge)?;
                exact_hit(app, Action::Address, "address", address)?;
            }
        }
        self.step = match self.step {
            Step::Baseline => {
                settled_closed(app)?;
                require(hit(app, Action::UpdatePanel).is_none(), "A pending check painted the badge")?;
                exact_hit(app, Action::Address, "address before offer", address_before)?;
                self.capture(app, "00-no-offer.png")?;
                app.updater.as_mut().ok_or("Missing fixture updater")?.ui_qa_offer()?;
                app.poll_updates(events);
                require(!app.tab_hits_current, "Offer did not invalidate the old toolbar hits")?;
                click(app, events, center(badge)); // Was in the wider address field.
                require(!app.address_focus && !app.update_panel, "Old address hit handled the future badge click")?;
                commands(app, [0, 0, 0])?;
                Step::Badge
            }
            Step::Badge => {
                settled_closed(app)?;
                let canvas = app.canvas.as_ref().ok_or("Missing badge pixels")?;
                require(rgb_at(canvas, (badge[0] + badge[2] - 5.5) as usize, 59, CHROME_ACCENT),
                    "Badge accent dot was not painted")?;
                app.mouse = outside;
                app.pointer_inside = true;
                require(app.pointer_link().as_deref() == Some(LINK), "Missing offline underneath link")?;
                self.capture(app, "01-badge.png")?;
                click(app, events, center(badge));
                // The panel is modal immediately, even without a presented surface.
                click(app, events, outside);
                require(app.update_panel && app.tabs[app.active].url == self.url,
                    "Unpresented flyout leaked an outside click")?;
                Step::Offer
            }
            Step::Offer => {
                settled_open(app)?;
                exact_hit(app, DOWNLOAD, "Download", download)?;
                require(app.update_view().and_then(|view| view.button)
                    == Some((UpdateAction::Download, "Download update")) && hit(app, RESTART).is_none(),
                    "Offer did not expose only Download")?;
                self.capture(app, "02-download-offer.png")?;
                require(tap(app, events, KeyCode::Escape, NamedKey::Escape), "Escape was not routed")?;
                Step::Escaped
            }
            Step::Escaped => {
                settled_closed(app)?;
                require(alt_u(app, events), "Alt+U was not routed")?;
                Step::OutsideOpen
            }
            Step::OutsideOpen => {
                settled_open(app)?;
                app.mouse = outside;
                require(app.chrome_action_at_pointer().is_none(), "Outside probe is inside a chrome hit")?;
                click(app, events, outside);
                require(!app.update_panel && app.tabs[app.active].url == self.url,
                    "Outside dismissal clicked through to the page")?;
                // Before the dismissal repaint the old chrome/popup cannot leak.
                click(app, events, outside);
                require(app.tabs[app.active].url == self.url, "Stale dismissal frame clicked through")?;
                Step::OutsideDismissed
            }
            Step::OutsideDismissed => {
                settled_closed(app)?;
                click(app, events, center(badge));
                Step::DownloadOpen
            }
            Step::DownloadOpen => {
                exact_hit(app, DOWNLOAD, "Download", download)?;
                click(app, events, center(download));
                click(app, events, center(download));
                commands(app, [0, 1, 0])?;
                require(hit(app, DOWNLOAD).is_none(), "Download left an actionable hit before repaint")?;
                require(named(app, events, KeyCode::Space, NamedKey::Space, Pressed, false, false),
                    "Fresh Space did not reach the modal guard while there was no button")?;
                updates(app)?.ui_qa_progress()?;
                app.poll_updates(events);
                Step::Progress
            }
            Step::Progress => {
                settled_open(app)?;
                let view = app.update_view().ok_or("Missing progress view")?;
                require(view.progress == Some(0.37) && view.detail.contains("37%")
                    && view.button.is_none() && hit(app, DOWNLOAD).is_none() && hit(app, RESTART).is_none(),
                    "Progress failed to replace the Download button")?;
                let canvas = app.canvas.as_ref().ok_or("Missing progress pixels")?;
                let x = panel[0] as usize + 16;
                require(rgb_at(canvas, x + 50, 162, CHROME_ACCENT)
                    && rgb_at(canvas, x + 200, 162, CHROME_HOVER), "Progress track fill or remainder is missing")?;
                self.capture(app, "03-progress-37.png")?;
                app.updater.as_mut().ok_or("Missing fixture updater")?.ui_qa_ready()?;
                app.poll_updates(events);
                require(tap(app, events, KeyCode::Enter, NamedKey::Enter), "Fresh Enter did not reach the modal guard")?;
                commands(app, [0, 1, 0])?; // Ready is not actionable until presented.
                Step::Ready
            }
            Step::Ready => {
                settled_open(app)?;
                exact_hit(app, RESTART, "Restart", restart)?;
                require(app.update_view().and_then(|view| view.button)
                    == Some((UpdateAction::Restart, "Restart and update")) && hit(app, DOWNLOAD).is_none(),
                    "Ready did not expose only Restart")?;
                self.capture(app, "04-ready-restart.png")?;
                click(app, events, center(download));
                require(app.update_panel, "Stale Download point dismissed the flyout instead of hitting its surface")?;
                require(!named(app, events, KeyCode::Space, NamedKey::Space, Pressed, true, false),
                    "Held Space repeated into Ready")?;
                require(!named(app, events, KeyCode::Enter, NamedKey::Enter, Pressed, false, true),
                    "Synthetic Enter reached a presented Restart")?;
                commands(app, [0, 1, 0])?;
                // Positive control: a fresh click on the distinct, presented
                // Restart hits the test sink. No Restart object can be created.
                click(app, events, center(restart));
                click(app, events, center(restart));
                commands(app, [0, 1, 1])?;
                require(tap(app, events, KeyCode::Escape, NamedKey::Escape), "Could not dismiss Ready")?;
                Step::Dismissed
            }
            Step::Dismissed => {
                settled_closed(app)?;
                self.capture(app, "05-dismissed.png")?;
                app.mouse = outside;
                let scroll = app.tabs[app.active].scroll;
                require(!named(app, events, KeyCode::Space, NamedKey::Space, Pressed, true, false)
                    && app.tabs[app.active].scroll == scroll, "Held Space leaked into the dismissed page")?;
                named(app, events, KeyCode::Space, NamedKey::Space, Released, false, false);
                require(tap(app, events, KeyCode::Space, NamedKey::Space)
                    && app.tabs[app.active].scroll > scroll, "Fresh Space did not scroll the underlying page")?;
                tap(app, events, KeyCode::Home, NamedKey::Home);
                require(app.tabs[app.active].scroll == 0. && app.pointer_link().as_deref() == Some(LINK),
                    "Could not restore the positive-control link after scrolling")?;
                click(app, events, outside);
                require(app.tabs[app.active].url == LINK, "Dismissed outside point was not actionable")?;
                Step::LinkReached
            }
            Step::LinkReached => {
                commands(app, [0, 1, 1])?;
                require(app.tabs[app.active].url == LINK, "Offline navigation did not settle")?;
                fs::write(self.output.join("passed.txt"), format!(
                    "{}x{} @ 100%, {} presented frames. Exact toolbar/flyout hits and 37% bar. \
                    Escape / Alt+U / outside, stale hits and keys, fresh click and Space passed. \
                    Offline effect counts: Check 0, Download 1, Restart 1.\n",
                    self.size.width, self.size.height, app.presented_frames
                )).map_err(|error| error.to_string())?;
                return Ok(true);
            }
            _ => unreachable!("resize steps routed separately"),
        };
        Ok(false)
    }
}

#[test]
#[ignore = "requires an interactive Windows x64 desktop at 100% DPI"]
fn capture_installed_updater_chrome_offline() {
    let width = match std::env::var("SATIN_QA_UPDATER_WIDTH").as_deref() {
        Ok("390") => 390,
        Ok("1280") => 1280,
        _ => panic!("Set SATIN_QA_UPDATER_WIDTH to 390 or 1280"),
    };
    let output = PathBuf::from(std::env::var_os("SATIN_QA_UPDATER_OUTPUT").expect("Set output directory"));
    run_fixture(output, width, Step::Baseline);
}

#[test]
#[ignore = "requires an interactive Windows x64 desktop at 100% DPI"]
fn capture_favorites_resize_and_scroll_offline() {
    let output = PathBuf::from(std::env::var_os("SATIN_QA_FAVORITES_OUTPUT").expect("Set output directory"));
    run_fixture(output, 1280, Step::ResizeWide);
}

#[test]
#[ignore = "requires an interactive Windows x64 desktop at 100% DPI"]
fn capture_tab_context_pin_and_drag_offline() {
    let output = PathBuf::from(std::env::var_os("SATIN_QA_TAB_OUTPUT").expect("Set output directory"));
    run_fixture(output, 1280, Step::TabsWide);
}

fn run_fixture(output: PathBuf, width: u32, step: Step) {
    assert!(output.is_absolute(), "Use an absolute output directory in disposable scratch space");
    fs::create_dir(&output).expect("Output must not exist and its parent must exist");
    let profile = output.join("profile");
    if matches!(step, Step::ResizeWide) {
        fs::create_dir(&profile).unwrap();
        let entries: Vec<_> = (1..=6).map(|index| serde_json::json!({
            "url": format!("http://127.0.0.1:1/bookmark-{index}"),
            "title": format!("Favorite {index}: Offline fixture")
        })).collect();
        fs::write(profile.join(bookmarks::FILE_NAME),
            serde_json::json!({"schema": 1, "entries": entries}).to_string()).unwrap();
    } else if matches!(step, Step::TabsWide) {
        fs::create_dir(&profile).unwrap();
        fs::write(profile.join("session.json"), serde_json::json!({
            "schema": 1, "active": 0,
            "tabs": ["about:home", "about:version", "about:shortcuts", "about:start-shortcuts"]
        }).to_string()).unwrap();
    }
    let page = output.join("offline-click-guard.html");
    fs::write(&page, format!(r#"<!doctype html><meta charset="utf-8"><title>{PAGE_TITLE}</title>
<style>html{{overflow-y:auto}}body{{margin:0;background:#edf0f5;font-family:sans-serif}}
 a{{display:block;height:400px;padding:16px;color:#172340}}div{{height:1500px;background:#255874}}</style>
 <a href="{LINK}">Offline content click target</a><div></div>"#)).unwrap();
    let url = url::Url::from_file_path(page).expect("Absolute file URL").to_string();
    DRIVER.with(|cell| {
        *cell.borrow_mut() = Some(Driver {
            output: output.clone(),
            url: url.clone(),
            size: PhysicalSize::new(width, HEIGHT),
            step,
            deadline: Instant::now() + Duration::from_secs(30),
            previous_pixels: None,
            result: None,
        });
    });
    // Disable installed discovery and startup ACK; do not call main or the CLI.
    let run_result = run(
        profile,
        Some(url),
        None,
        None,
        None,
        None,
        None,
        None,
        crate::renderer::PageRenderer::Software,
        false,
    );
    let fixture_result = DRIVER.with(|cell| cell.borrow_mut().take().unwrap().result);
    run_result.expect("Offline Winit test window");
    fixture_result.expect("Fixture exited before completion").unwrap();
}
