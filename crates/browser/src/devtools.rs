//! The native dock belongs to its window. Each top-level navigation owns
//! its logs, editor state, selections and measurements. A failed navigation
//! keeps its network observations through the recovery page. Capture never
//! creates or reopens a tab, and stale observer packets are ignored.

mod application;
mod console;
mod console_interaction;
mod console_view;
mod elements;
mod network;
mod network_view;
mod panel;
mod performance;
mod sources;

use self::console::{Console, ConsoleFilter, ConsoleInput, ConsoleOrigin, ConsoleSeverity};
use self::console_view::{ConsoleRow, Failure, Severity};
use self::network::{
    DisplayMethod, DisplayUrl, NetworkEvent, NetworkLog, NetworkOutcome, NetworkPhase,
    NetworkScope, RequestKey, RequestKind, TerminalOutcome,
};
use self::panel::{Layout, Section};
use crate::app::{DevtoolsDock, DockFrame, DockInput};
use crate::jobs::devtools as observer;
use rust_browser_engine::{
    Fonts, Rect,
    paint::Canvas,
    script::{
        ConsoleLevel, ConsoleOrigin as RuntimeOrigin, ConsolePhase, ConsoleSnapshot, ScriptRuntime,
    },
};
use std::collections::{HashMap, hash_map::Entry};
use winit::keyboard::{Key, NamedKey};

// Match App's MAX_TABS. Never evict a live tab because another scope appeared.
const MAX_DOCUMENTS: usize = 24;

#[derive(Default)]
pub(crate) struct DevTools {
    section: Section,
    documents: HashMap<u64, Document>,
    observer_dropped: usize,
    pub(crate) picking: bool,
}

struct Document {
    elements: elements::Elements,
    application: application::Application,
    sources: sources::Sources,
    performance: performance::Performance,
    navigation: u64,
    console: Console,
    console_source_dropped: u64,
    console_scroll: f32,
    console_controls: console_interaction::ConsoleControls,
    network: NetworkLog,
    network_inspector: network_view::Inspector,
}

impl Document {
    fn new(tab: u64, navigation: u64) -> Self {
        Self {
            navigation,
            elements: Default::default(),
            application: Default::default(),
            sources: Default::default(),
            performance: Default::default(),
            console: Console::default(),
            console_source_dropped: 0,
            console_scroll: f32::INFINITY,
            console_controls: console_interaction::ConsoleControls::default(),
            network: NetworkLog::new(NetworkScope { tab, navigation }),
            network_inspector: network_view::Inspector::default(),
        }
    }

    fn click(&mut self, section: Section, bounds: Rect, x: f32, y: f32) {
        match section {
            Section::Application => {
                self.application.click(bounds, x, y);
            }
            Section::Elements => {
                self.elements.click(bounds, x, y);
            }
            Section::Sources => self.sources.click(bounds, x, y),
            Section::Performance => self.performance.click(bounds, x, y),
            Section::Console => self.console_click(bounds, x, y),
            Section::Network => self
                .network_inspector
                .click(&mut self.network, bounds, x, y),
        }
    }

    fn scroll(&mut self, section: Section, bounds: Rect, x: f32, y: f32, delta_y: f32) {
        match section {
            Section::Application => {
                self.application.scroll(bounds, x, y, delta_y);
            }
            Section::Elements => {
                self.elements.scroll(bounds, x, y, delta_y);
            }
            Section::Sources => self.sources.scroll(bounds, x, delta_y),
            Section::Performance => {}
            Section::Console => {
                let layout = self.console_layout(bounds);
                if layout.list.contains(x, y) {
                    if self.console_controls.detail.is_some() {
                        let lines = (delta_y / 19.).round();
                        if self.console_detail_scroll(
                            layout.list,
                            if lines == 0. {
                                delta_y.signum() as isize
                            } else {
                                lines as isize
                            },
                        ) {
                            return;
                        }
                    }
                    let next = layout.scroll_by(delta_y);
                    // Keep following new rows while the reader is at the bottom.
                    self.console_scroll = if next == layout.scroll_by(f32::MAX) {
                        f32::INFINITY
                    } else {
                        next
                    };
                }
            }
            Section::Network => self
                .network_inspector
                .scroll(&self.network, bounds, x, y, delta_y),
        }
    }

    fn paint(
        &self,
        section: Section,
        canvas: &mut Canvas,
        fonts: &mut Fonts,
        frame: DockFrame,
        bounds: Rect,
        observer_dropped: usize,
    ) {
        let scale = frame.scale;
        match section {
            Section::Application => self.application.draw(canvas, fonts, bounds, scale),
            Section::Elements => self.elements.draw(canvas, fonts, bounds, scale),
            Section::Sources => self.sources.draw(canvas, fonts, bounds, scale),
            Section::Performance => self.performance.draw(canvas, fonts, bounds, scale),
            Section::Console => self.draw_console(canvas, fonts, frame, bounds),
            Section::Network => self.network_inspector.draw(
                &self.network,
                canvas,
                fonts,
                frame,
                bounds,
                observer_dropped,
            ),
        }
    }

    fn network_copy(&self) -> Option<String> {
        self.network_inspector.copy(&self.network)
    }

    fn network_key(&mut self, key: &Key, bounds: Rect) -> bool {
        self.network_inspector.key(&self.network, key, bounds)
    }

    fn ingest_console(&mut self, snapshot: ConsoleSnapshot) -> bool {
        let changed = !snapshot.records.is_empty() || snapshot.dropped != 0;
        self.console_source_dropped = self.console_source_dropped.saturating_add(snapshot.dropped);
        for record in snapshot.records {
            let (origin, phase) = match record.origin {
                RuntimeOrigin::ConsoleApi => (ConsoleOrigin::ConsoleApi, None),
                RuntimeOrigin::BrowserReported(phase) => {
                    (ConsoleOrigin::BrowserReported, Some(console_phase(phase)))
                }
            };
            let location = record.line.map(|line| {
                let base = record.source.as_deref().unwrap_or("line");
                match record.column {
                    Some(column) => format!("{base}:{line}:{column}"),
                    None => format!("{base}:{line}"),
                }
            });
            let diagnostic = record
                .error_kind
                .map(|kind| format!("{} · {}", phase.unwrap_or("JavaScript"), kind.name()));
            // Engine snapshots drain in order and expose aggregate source loss
            // separately; model sequence numbers only order the received records.
            self.console.record_with_context(
                self.console.last_sequence().saturating_add(1),
                ConsoleInput {
                    severity: match record.level {
                        ConsoleLevel::Debug => ConsoleSeverity::Debug,
                        ConsoleLevel::Log => ConsoleSeverity::Log,
                        ConsoleLevel::Info => ConsoleSeverity::Info,
                        ConsoleLevel::Warn => ConsoleSeverity::Warn,
                        ConsoleLevel::Error => ConsoleSeverity::Error,
                    },
                    origin,
                    message: &record.text,
                    source: location.as_deref().or(record.source.as_deref()),
                    phase: diagnostic.as_deref().or(phase),
                    capture_truncated: record.truncated,
                },
                console::ConsoleContext {
                    script_node: record.script_node.map(|id| id.0),
                    line: record.line,
                    calls: record.calls,
                },
            );
        }
        changed
    }

    fn observe_network(&mut self, event: observer::Event) -> bool {
        use observer::{Phase, Request};

        let (kind, id) = match event.key.request {
            Request::Navigate => (RequestKind::Navigation, 0),
            Request::Resource(id) => (RequestKind::Resource, id),
            Request::Fetch(id) => (RequestKind::FetchOrXhr, id),
        };
        let key = RequestKey {
            scope: NetworkScope {
                tab: event.key.tab,
                navigation: event.key.navigation,
            },
            kind,
            id,
        };
        let mut at = event.started.checked_add(event.elapsed);
        let phase = match event.phase {
            Phase::Started {
                method,
                url,
                resource_type,
            } => {
                let Ok(url) = url::Url::parse(&url) else {
                    return false;
                };
                NetworkPhase::Started {
                    method: DisplayMethod::from_method(method.as_deref().unwrap_or_default()),
                    url: DisplayUrl::from_url(&url),
                    resource_type,
                }
            }
            Phase::Response {
                status,
                headers_at,
                details,
            } => {
                at = headers_at;
                NetworkPhase::ResponseHeaders { status, details }
            }
            Phase::Finished { body_bytes } => {
                NetworkPhase::Terminal(TerminalOutcome::Completed, body_bytes)
            }
            Phase::Failed => NetworkPhase::Terminal(TerminalOutcome::Failed, None),
            Phase::Cancelled => NetworkPhase::Terminal(TerminalOutcome::Cancelled, None),
        };
        self.network.observe(NetworkEvent { key, at, phase });
        true
    }
}

fn console_phase(phase: ConsolePhase) -> &'static str {
    match phase {
        ConsolePhase::ClassicScript => "Classic script",
        ConsolePhase::DomListener => "DOM listener",
        ConsolePhase::EventHandlerCompile => "Event handler compilation",
        ConsolePhase::EventHandler => "Event handler",
        ConsolePhase::EventTargetListener => "EventTarget listener",
        ConsolePhase::Microtask => "Microtask",
        ConsolePhase::UnhandledPromiseRejection => "Unhandled promise rejection",
        ConsolePhase::HostTask => "Host task",
    }
}

fn console_row(row: &console::ConsoleRow) -> ConsoleRow<'_> {
    ConsoleRow {
        severity: match row.severity {
            ConsoleSeverity::Debug => Severity::Debug,
            ConsoleSeverity::Log => Severity::Log,
            ConsoleSeverity::Info => Severity::Info,
            ConsoleSeverity::Warn => Severity::Warn,
            ConsoleSeverity::Error => Severity::Error,
        },
        message: &row.message,
        failure: match row.origin {
            ConsoleOrigin::ConsoleApi | ConsoleOrigin::Command | ConsoleOrigin::Evaluation => None,
            ConsoleOrigin::BrowserReported => Some(Failure::BrowserReported),
        },
        source: row.source.as_deref(),
        phase: row
            .phase
            .as_deref()
            .or_else(|| console_interaction::origin_label(row.origin)),
        capture_truncated: row.capture_truncated,
        repeat: 1, // Each call is a separate record; never infer grouping or caught errors.
    }
}

fn network_row(row: &network::NetworkRow) -> network_view::NetworkRow<'_> {
    use network_view::NetworkOutcome as ViewOutcome;
    network_view::NetworkRow {
        method: Some(row.method.as_str()),
        display_url: Some(row.url.as_str()),
        category: Some(row.category()),
        http_status: row.status(),
        outcome: match row.outcome() {
            NetworkOutcome::Started => Some(ViewOutcome::Pending),
            NetworkOutcome::Terminal(TerminalOutcome::Failed) => Some(ViewOutcome::Failed),
            NetworkOutcome::Terminal(TerminalOutcome::Cancelled) => Some(ViewOutcome::Cancelled),
            NetworkOutcome::HeadersReceived
            | NetworkOutcome::Terminal(TerminalOutcome::Completed) => None,
        },
        elapsed: row.elapsed(),
        started_at: row.started_at,
        headers_elapsed: row.headers_elapsed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn log(message: &str) -> ConsoleInput<'_> {
        ConsoleInput {
            severity: ConsoleSeverity::Log,
            origin: ConsoleOrigin::ConsoleApi,
            message,
            source: None,
            phase: None,
            capture_truncated: false,
        }
    }

    fn dock(tab: u64, pointer: Option<(f32, f32)>) -> DockFrame {
        DockFrame {
            bounds: Rect {
                x: 0.,
                y: 120.,
                width: 400.,
                height: 230.,
            },
            scale: 1.,
            focused: true,
            pointer,
            tab,
            document_navigation: 5,
        }
    }

    #[test]
    fn navigation_replaces_only_its_own_tab_and_never_accepts_stale_events() {
        let mut devtools = DevTools::default();
        assert!(devtools.document_mut(3, 1).is_none());
        devtools.begin_navigation(1, 1);
        devtools.begin_navigation(2, 1);
        devtools
            .document_mut(1, 1)
            .unwrap()
            .console
            .record(1, log("first tab"));
        devtools
            .document_mut(2, 1)
            .unwrap()
            .console
            .record(1, log("background tab"));

        devtools.begin_navigation(1, 1);
        assert_eq!(devtools.documents[&1].console.last_sequence(), 1);
        devtools.begin_navigation(1, 3);
        assert!(devtools.document_mut(1, 1).is_none());
        assert_eq!(devtools.documents[&1].console.last_sequence(), 0);
        assert_eq!(
            devtools.documents[&2]
                .console
                .rows(ConsoleFilter::ALL)
                .next()
                .unwrap()
                .message
                .as_ref(),
            "background tab"
        );
        devtools.close_tab(2);
        assert!(devtools.document_mut(2, 1).is_none());
    }

    #[test]
    fn full_document_table_keeps_live_tabs_and_reuses_closed_slots() {
        let mut devtools = DevTools::default();
        for tab in 0..MAX_DOCUMENTS as u64 {
            devtools.begin_navigation(tab, 1);
        }
        let extra = MAX_DOCUMENTS as u64;
        devtools.begin_navigation(extra, 1);
        assert!(devtools.document_mut(extra, 1).is_none());
        devtools.begin_navigation(0, 2);
        assert!(devtools.document_mut(0, 2).is_some());
        devtools.close_tab(0);
        devtools.begin_navigation(extra, 1);
        assert!(devtools.document_mut(extra, 1).is_some());
    }

    #[test]
    fn console_clear_and_scrolling_are_local_and_do_not_reset_the_source_cursor() {
        let mut devtools = DevTools::default();
        devtools.begin_navigation(4, 5);
        devtools.begin_navigation(6, 7);
        for tab in [4, 6] {
            let console = &mut devtools.documents.get_mut(&tab).unwrap().console;
            for sequence in 2..=20 {
                console.record(sequence, log("message"));
            }
        }
        assert!(
            devtools
                .documents
                .get_mut(&4)
                .unwrap()
                .ingest_console(ConsoleSnapshot {
                    dropped: 9,
                    ..Default::default()
                })
        );
        let body = Layout::new(dock(4, None).bounds).unwrap().body;
        let layout = devtools.documents[&4].console_layout(body);
        let frame = dock(4, Some((layout.list.x, layout.list.y - 120.)));
        devtools.input(frame, DockInput::Wheel(-30.));
        assert!(devtools.documents[&4].console_scroll.is_finite());
        assert_eq!(devtools.documents[&6].console_scroll, f32::INFINITY);
        let clear = layout.clear.unwrap();
        devtools.input(dock(4, Some((clear.x, clear.y - 120.))), DockInput::Click);
        let console = &mut devtools.documents.get_mut(&4).unwrap().console;
        assert_eq!(console.rows(ConsoleFilter::ALL).count(), 0);
        assert_eq!(console.discarded(), 0);
        assert!(!console.record(20, log("old snapshot")));
        assert_eq!(devtools.documents[&4].console_scroll, f32::INFINITY);
        assert_eq!(devtools.documents[&4].console_source_dropped, 0);
        assert_eq!(devtools.documents[&6].console.discarded(), 1);
    }

    #[test]
    fn a_logged_error_is_not_labeled_a_browser_report_or_caught_throw() {
        let mut console = Console::default();
        console.record(
            1,
            ConsoleInput {
                severity: ConsoleSeverity::Error,
                ..log("explicit console.error")
            },
        );
        console.record(
            2,
            ConsoleInput {
                severity: ConsoleSeverity::Error,
                origin: ConsoleOrigin::BrowserReported,
                phase: Some("Event"),
                ..log("uncaught event callback")
            },
        );
        let mut rows = console.rows(ConsoleFilter::ALL).map(console_row);
        let logged = rows.next().unwrap();
        assert_eq!(logged.severity, Severity::Error);
        assert_eq!(logged.failure, None);
        assert_eq!(logged.phase, None);
        let reported = rows.next().unwrap();
        assert_eq!(reported.failure, Some(Failure::BrowserReported));
        assert_eq!(reported.phase, Some("Event"));
    }

    #[test]
    fn dock_input_is_relative_to_the_frame_and_tab_switches_do_not_register_documents() {
        let mut devtools = DevTools::default();
        devtools.begin_navigation(4, 5);
        devtools.input(dock(4, Some((100., -1.))), DockInput::Click);
        assert_eq!(devtools.section, Section::Console);
        devtools.input(dock(4, Some((100., 20.))), DockInput::Click);
        assert_eq!(devtools.section, Section::Network);
        devtools.input(dock(7, Some((399., 20.))), DockInput::Click);
        assert_eq!(devtools.section, Section::Network);
        assert!(devtools.document_mut(4, 5).is_some());
        devtools.input(dock(7, Some((20., 20.))), DockInput::Click);
        assert_eq!(devtools.section, Section::Console);
        assert!(!devtools.documents.contains_key(&7));
    }

    #[test]
    fn network_adaptation_keeps_real_elapsed_time_without_inventing_a_fetch_completion() {
        let mut devtools = DevTools::default();
        devtools.begin_navigation(4, 5);
        let started = std::time::Instant::now();
        let event = |navigation, elapsed_ms, phase| observer::Event {
            started,
            key: observer::Key {
                tab: 4,
                navigation,
                request: observer::Request::Fetch(1),
            },
            elapsed: Duration::from_millis(elapsed_ms),
            phase,
        };
        assert!(!devtools.observe_network(event(4, 8, observer::Phase::Failed)));
        assert!(devtools.observe_network(event(
            5,
            0,
            observer::Phase::Started {
                method: Some("GET".into()),
                url: "https://name:pass@example.test/main?private=1#frag".into(),
                resource_type: None,
            },
        )));
        assert!(devtools.observe_network(event(
            5,
            23,
            observer::Phase::Response {
                status: Some(404),
                headers_at: Some(started + Duration::from_millis(23)),
                details: observer::ResponseDetails::default()
            },
        )));
        let row = devtools.documents[&4].network.rows().next().unwrap();
        assert_eq!(row.outcome(), NetworkOutcome::HeadersReceived);
        let view = network_row(row);
        assert_eq!(view.display_url, Some("https://example.test/main"));
        assert_eq!(view.http_status, Some(404));
        assert_eq!(view.elapsed, Some(Duration::from_millis(23)));
        devtools.close_tab(4);
        assert!(!devtools.observe_network(event(5, 30, observer::Phase::Failed)));
    }
}

impl DevTools {
    pub(crate) fn note_observer_drops(&mut self, dropped: usize) {
        self.observer_dropped = self.observer_dropped.saturating_add(dropped);
    }

    pub(crate) fn begin_navigation(&mut self, tab: u64, navigation: u64) {
        let at_capacity = self.documents.len() >= MAX_DOCUMENTS;
        match self.documents.entry(tab) {
            Entry::Occupied(mut slot) if slot.get().navigation != navigation => {
                slot.insert(Document::new(tab, navigation));
            }
            Entry::Vacant(slot) if !at_capacity => {
                slot.insert(Document::new(tab, navigation));
            }
            _ => {}
        }
    }

    pub(crate) fn close_tab(&mut self, tab: u64) {
        self.documents.remove(&tab);
    }

    fn document_mut(&mut self, tab: u64, navigation: u64) -> Option<&mut Document> {
        self.documents
            .get_mut(&tab)
            .filter(|document| document.navigation == navigation)
    }

    /// Returns whether a live inspection scope accepted the packet.
    pub(crate) fn observe_network(&mut self, event: observer::Event) -> bool {
        self.document_mut(event.key.tab, event.key.navigation)
            .is_some_and(|document| document.observe_network(event))
    }

    /// True when the active inspection scope received new rows or source loss.
    pub(crate) fn sync_console(
        &mut self,
        tab: u64,
        navigation: u64,
        runtime: &ScriptRuntime,
    ) -> bool {
        self.document_mut(tab, navigation)
            .is_some_and(|document| document.ingest_console(runtime.take_console_snapshot()))
    }
}

fn window_pointer(frame: DockFrame) -> Option<(f32, f32)> {
    let (x, y) = frame.pointer?;
    let point = (frame.bounds.x + x, frame.bounds.y + y);
    panel::contains(frame.bounds, point.0, point.1).then_some(point)
}

impl DevtoolsDock for DevTools {
    fn draw(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, frame: DockFrame) {
        let scale = frame.scale;
        if !scale.is_finite() || scale <= 0. {
            return;
        }
        let Some(layout) = Layout::new(frame.bounds) else {
            return;
        };
        layout.paint(canvas, fonts, scale, self.section, self.picking);
        if panel::has_area(layout.body) {
            canvas.clip(panel::scaled(layout.body, scale));
            // The last registered inspection attempt remains visible after
            // App increments its own generation to install a recovery page.
            if let Some(document) = self.documents.get(&frame.tab) {
                document.paint(
                    self.section,
                    canvas,
                    fonts,
                    frame,
                    layout.body,
                    self.observer_dropped,
                );
            } else {
                // A restored but not-yet-loaded tab (or an internal blank tab).
                // Render the normal empty view without registering a navigation.
                Document::new(frame.tab, frame.document_navigation).paint(
                    self.section,
                    canvas,
                    fonts,
                    frame,
                    layout.body,
                    self.observer_dropped,
                );
            }
        }
        canvas.clip(Rect {
            x: frame.bounds.x * scale,
            y: frame.bounds.y * scale,
            width: frame.bounds.width * scale,
            height: frame.bounds.height * scale,
        });
    }

    fn input(&mut self, frame: DockFrame, input: DockInput) {
        let Some(layout) = Layout::new(frame.bounds) else {
            return;
        };
        match input {
            DockInput::Click => {
                let Some((x, y)) = window_pointer(frame) else {
                    return;
                };
                if layout.picker_at(x, y) {
                    self.toggle_picker();
                } else if let Some(section) = layout.section_at(x, y) {
                    self.section = section;
                    self.picking = false;
                } else if panel::contains(layout.body, x, y)
                    && let Some(document) = self.documents.get_mut(&frame.tab)
                {
                    document.click(self.section, layout.body, x, y);
                }
            }
            DockInput::Wheel(delta) if delta != 0. && delta.is_finite() => {
                if let Some((x, y)) = window_pointer(frame)
                    && panel::contains(layout.body, x, y)
                    && let Some(document) = self.documents.get_mut(&frame.tab)
                {
                    document.scroll(self.section, layout.body, x, y, delta);
                }
            }
            DockInput::Key {
                key,
                modifiers,
                repeat,
            } if frame.focused => {
                if !repeat
                    && modifiers.alt_key()
                    && !modifiers.control_key()
                    && !modifiers.super_key()
                    && let Key::Character(c) = &key
                    && let Some(i) = c.parse::<usize>().ok().and_then(|i| i.checked_sub(1))
                    && let Some(section) = Section::ALL.get(i)
                {
                    self.section = *section;
                    self.picking = false;
                    return;
                }
                if self.picking && key == Key::Named(NamedKey::Escape) {
                    self.picking = false;
                    return;
                }
                if self.section == Section::Console {
                    self.console_key(frame, &key, modifiers, repeat);
                    return;
                }
                if let Some(doc) = self.documents.get_mut(&frame.tab) {
                    match self.section {
                        Section::Application => {
                            doc.application.key(layout.body, &key, modifiers);
                        }
                        Section::Elements => {
                            doc.elements.key(layout.body, &key, modifiers);
                        }
                        Section::Sources => {
                            doc.sources.key(layout.body, &key, modifiers);
                        }
                        Section::Network if modifiers.is_empty() => {
                            doc.network_key(&key, layout.body);
                        }
                        Section::Performance if key == Key::Named(NamedKey::Space) && !repeat => {
                            doc.performance.click(
                                layout.body,
                                layout.body.x + 1.,
                                layout.body.y + 1.,
                            )
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

impl DevTools {
    pub(crate) fn toggle_picker(&mut self) {
        self.picking = !self.picking;
        self.section = Section::Elements;
    }
    pub(crate) fn is_console(&self) -> bool {
        self.section == Section::Console
    }

    pub(crate) fn sync_inspector(
        &mut self,
        frame: DockFrame,
        page: Option<&rust_browser_engine::LoadedPage>,
        layout: Option<&std::sync::Arc<rust_browser_engine::Layout>>,
        runtime: Option<&ScriptRuntime>,
        session: Option<&rust_browser_engine::script::DocumentSession>,
        storage: &rust_browser_engine::web_storage::TabStorage,
    ) {
        let Some(doc) = self.documents.get_mut(&frame.tab) else {
            return;
        };
        match self.section {
            Section::Elements => doc.elements.sync_page(page, layout),
            Section::Sources => doc.sources.sync(page, runtime, session),
            Section::Application => doc.application.sync(page, storage),
            _ => {}
        }
    }
    pub(crate) fn inspect(&mut self, frame: DockFrame, node: rust_browser_engine::dom::NodeId) {
        self.section = Section::Elements;
        self.picking = false;
        if let (Some(layout), Some(doc)) = (
            Layout::new(frame.bounds),
            self.documents.get_mut(&frame.tab),
        ) {
            doc.elements.inspect(node, layout.body);
        }
    }
    pub(crate) fn selected_rect(&self, tab: u64) -> Option<Rect> {
        if self.section != Section::Elements {
            return None;
        }
        self.documents.get(&tab)?.elements.selected_rect()
    }
    pub(crate) fn monitoring(&self) -> bool {
        self.section == Section::Performance
    }
    pub(crate) fn sample(
        &mut self,
        tab: u64,
        layout_time: std::time::Duration,
        paint_time: std::time::Duration,
        page: Option<&rust_browser_engine::LoadedPage>,
        layout: Option<&rust_browser_engine::Layout>,
        runtime: Option<&ScriptRuntime>,
    ) {
        if let Some(doc) = self.documents.get_mut(&tab) {
            doc.performance
                .sample(layout_time, paint_time, page, layout, runtime);
        }
    }
}

impl DevTools {
    pub(crate) fn take_style_edit(
        &mut self,
        frame: DockFrame,
    ) -> Option<(rust_browser_engine::dom::NodeId, String, String, bool)> {
        self.document_mut(frame.tab, frame.document_navigation)?
            .elements
            .take_style_edit()
    }
    pub(crate) fn style_edit_result(&mut self, frame: DockFrame, result: Result<(), String>) {
        if let Some(doc) = self.document_mut(frame.tab, frame.document_navigation) {
            doc.elements.style_edit_result(result);
        }
    }
}

impl DevTools {
    pub(crate) fn clipboard_selection(&mut self, frame: DockFrame, cut: bool) -> Option<String> {
        if self.section == Section::Elements {
            return self
                .document_mut(frame.tab, frame.document_navigation)?
                .elements
                .clipboard_selection(cut);
        }
        if !cut && self.section == Section::Sources {
            return self
                .document_mut(frame.tab, frame.document_navigation)?
                .sources
                .copy_line();
        }
        if !cut && self.section == Section::Network {
            return self.documents.get(&frame.tab)?.network_copy();
        }
        self.console_selection(frame, cut)
    }
}

impl DevTools {
    pub(crate) fn reveal_console_source(
        &mut self,
        frame: DockFrame,
        page: Option<&rust_browser_engine::LoadedPage>,
        runtime: Option<&ScriptRuntime>,
        session: Option<&rust_browser_engine::script::DocumentSession>,
    ) -> bool {
        let Some((node, line)) = self.console_source_target(frame) else {
            return false;
        };
        let Some(doc) = self.document_mut(frame.tab, frame.document_navigation) else {
            return false;
        };
        doc.sources.sync(page, runtime, session);
        if !doc.sources.reveal(node, line) {
            return false;
        }
        self.section = Section::Sources;
        true
    }
}
