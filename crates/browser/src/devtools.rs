//! Desktop DevTools contract for App integration (`mod devtools` in `main.rs`):
//!
//! - Store one `DevTools::default()` per window, using lane 08's `DevtoolsDock`
//!   `draw` / `input` methods. App owns F12, visibility, placement, viewport
//!   sizing, focus, and event routing. Prefer `Option<DevTools>` over
//!   `Option<Box<dyn DevtoolsDock>>` in App so it can also call the concrete
//!   lifecycle and capture methods below.
//! - `begin_navigation(tab_id, navigation_id)` at the real navigation boundary,
//!   before dispatching any work, clears the previous attempt. Never call it
//!   for same-document history/fragment changes, tab switches, or dock toggles.
//!   The error-page replacement advances App's job generation but is not a new
//!   inspection attempt: keep the failed attempt and its real outcome visible.
//! - `close_tab(tab_id)` drops that tab's data. Ingestion uses the originating
//!   tab and navigation IDs even for an inactive tab; unregistered, closed,
//!   and superseded scopes must not be created from events.
//! - Call `sync_console(tab_id, navigation_id, runtime)` after script work,
//!   regardless of visibility or whether the page changed. This drains only the
//!   structured Console batch, not legacy diagnostics. Sync before routing
//!   Console Clear and before discarding a failed document's runtime.
//! - Pass packets from `Workers::new_observed` to `observe_network(event)`.
//!   Drain them without waiting, independently of App's job-generation check:
//!   that check also rejects the final observation for a failed attempt. The
//!   jobs wake the existing event loop; App drains on that loop.
//!   Drain available packets before routing Network Clear. The queue overflow
//!   total is window-wide and must not be attributed to a document.

mod console;
mod console_view;
mod network;
mod network_view;
mod panel;

use self::console::{Console, ConsoleFilter, ConsoleInput, ConsoleOrigin, ConsoleSeverity};
use self::console_view::{ConsoleLayout, ConsoleRow, Failure, Severity};
use self::network::{
    DisplayMethod, DisplayUrl, NetworkEvent, NetworkLog, NetworkOutcome, NetworkPhase,
    NetworkScope, RequestKey, RequestKind, TerminalOutcome,
};
use self::network_view::{NetworkLayout, NetworkView};
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
use std::{
    collections::{HashMap, hash_map::Entry},
    time::Instant,
};
use winit::keyboard::{Key, NamedKey};

// Match App's MAX_TABS. Never evict a live tab because another scope appeared.
const MAX_DOCUMENTS: usize = 24;

#[derive(Default)]
pub(crate) struct DevTools {
    section: Section,
    documents: HashMap<u64, Document>,
    observer_dropped: usize,
}

struct Document {
    navigation: u64,
    console: Console,
    console_source_dropped: u64,
    console_scroll: f32,
    network: NetworkLog,
    network_scroll: f32,
    network_clock: Instant,
}

impl Document {
    fn new(tab: u64, navigation: u64) -> Self {
        Self {
            navigation,
            console: Console::default(),
            console_source_dropped: 0,
            console_scroll: f32::INFINITY,
            network: NetworkLog::new(NetworkScope { tab, navigation }),
            network_scroll: 0.,
            network_clock: Instant::now(),
        }
    }

    fn console_layout(&self, bounds: Rect) -> ConsoleLayout {
        ConsoleLayout::new(
            bounds,
            self.console.rows(ConsoleFilter::ALL).count(),
            self.console_scroll,
            self.console
                .discarded()
                .saturating_add(self.console_source_dropped),
        )
    }

    fn click(&mut self, section: Section, bounds: Rect, x: f32, y: f32) {
        match section {
            Section::Console => {
                if self
                    .console_layout(bounds)
                    .clear
                    .is_some_and(|rect| rect.contains(x, y))
                {
                    self.console.clear();
                    self.console_source_dropped = 0;
                    self.console_scroll = f32::INFINITY;
                }
            }
            Section::Network => {
                if NetworkLayout::new(bounds)
                    .clear_rect()
                    .is_some_and(|rect| rect.contains(x, y))
                {
                    self.network.clear();
                    self.network_scroll = 0.;
                }
            }
        }
    }

    fn scroll(&mut self, section: Section, bounds: Rect, x: f32, y: f32, delta_y: f32) {
        match section {
            Section::Console => {
                let layout = self.console_layout(bounds);
                if layout.list.contains(x, y) {
                    let next = layout.scroll_by(delta_y);
                    // Keep following new rows while the reader is at the bottom.
                    self.console_scroll = if next == layout.scroll_by(f32::MAX) {
                        f32::INFINITY
                    } else {
                        next
                    };
                }
            }
            Section::Network => {
                let layout = NetworkLayout::new(bounds);
                if layout.list_rect().contains(x, y) {
                    self.network_scroll =
                        layout.scroll_by(self.network.rows().len(), self.network_scroll, delta_y);
                }
            }
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
            Section::Console => {
                let rows: Vec<_> = self
                    .console
                    .rows(ConsoleFilter::ALL)
                    .map(console_row)
                    .collect();
                self.console_layout(bounds).draw(
                    canvas,
                    fonts,
                    &rows,
                    scale,
                    panel::scaled(bounds, scale),
                );
            }
            Section::Network => {
                let rows: Vec<_> = self.network.rows().map(network_row).collect();
                let loss = self.network.loss();
                let layout = NetworkLayout::new(bounds);
                let coverage = if observer_dropped == 0 {
                    "Partial HTTP(S): navigation final status, Fetch headers, loaded resources"
                        .into()
                } else {
                    format!("Partial HTTP(S); {observer_dropped} events lost across all tabs")
                };
                network_view::draw_network(
                    canvas,
                    fonts,
                    scale,
                    panel::scaled(bounds, scale),
                    &layout,
                    NetworkView {
                        rows: &rows,
                        discarded: loss
                            .evicted_rows
                            .saturating_add(loss.dropped_events)
                            .saturating_add(loss.unmatched_events),
                        scroll: self.network_scroll,
                        clear_hovered: layout.clear_rect().is_some_and(|clear| {
                            window_pointer(frame).is_some_and(|(x, y)| clear.contains(x, y))
                        }),
                        coverage_note: Some(&coverage),
                    },
                );
            }
        }
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
            // Engine snapshots drain in order and expose aggregate source loss
            // separately; model sequence numbers only order the received records.
            self.console.record(
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
                    source: record.source.as_deref(),
                    phase,
                    capture_truncated: record.truncated,
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
        // The observer supplies elapsed durations rather than timestamps.
        // Use one origin for relative offsets: the model only compares two
        // offsets within the same request. Never display this origin as a time.
        let mut at = self.network_clock.checked_add(event.elapsed);
        let phase = match event.phase {
            Phase::Started { method, url } => {
                let Ok(url) = url::Url::parse(&url) else {
                    return false;
                };
                NetworkPhase::Started {
                    method: DisplayMethod::from_method(method.as_deref().unwrap_or_default()),
                    url: DisplayUrl::from_url(&url),
                }
            }
            Phase::NavigationOpened {
                status,
                response_time,
            } => {
                at = self.network_clock.checked_add(response_time);
                NetworkPhase::ResponseHeaders {
                    status: Some(status),
                }
            }
            Phase::NavigationFinished => NetworkPhase::Terminal(TerminalOutcome::Completed),
            Phase::FetchResponse { status } => NetworkPhase::ResponseHeaders { status },
            Phase::Failed => NetworkPhase::Terminal(TerminalOutcome::Failed),
            Phase::Cancelled => NetworkPhase::Terminal(TerminalOutcome::Cancelled),
            Phase::ResourceLoaded { url } => {
                let Ok(url) = url::Url::parse(&url) else {
                    return false;
                };
                // A single successful buffered GET observation admits one
                // row with the observed final URL, not a separate start row.
                self.network.observe(NetworkEvent {
                    key,
                    at: Some(self.network_clock),
                    phase: NetworkPhase::Started {
                        method: DisplayMethod::from_method("GET"),
                        url: DisplayUrl::from_url(&url),
                    },
                });
                NetworkPhase::Terminal(TerminalOutcome::Completed)
            }
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
            ConsoleOrigin::ConsoleApi => None,
            ConsoleOrigin::BrowserReported => Some(Failure::BrowserReported),
        },
        source: row.source.as_deref(),
        phase: row.phase.as_deref(),
        capture_truncated: row.capture_truncated,
        repeat: 1, // Each call is a separate record; never infer grouping or caught errors.
    }
}

fn network_row(row: &network::NetworkRow) -> network_view::NetworkRow<'_> {
    use network_view::NetworkOutcome as ViewOutcome;
    network_view::NetworkRow {
        method: Some(row.method.as_str()),
        display_url: Some(row.url.as_str()),
        category: Some(match row.key.kind {
            RequestKind::Navigation => "Navigation",
            RequestKind::Resource => "Resource",
            RequestKind::FetchOrXhr => "Fetch/XHR",
        }),
        http_status: row.status(),
        outcome: match row.outcome() {
            NetworkOutcome::Started => Some(ViewOutcome::Pending),
            NetworkOutcome::Terminal(TerminalOutcome::Failed) => Some(ViewOutcome::Failed),
            NetworkOutcome::Terminal(TerminalOutcome::Cancelled) => Some(ViewOutcome::Cancelled),
            NetworkOutcome::HeadersReceived
            | NetworkOutcome::Terminal(TerminalOutcome::Completed) => None,
        },
        elapsed: row.elapsed(),
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
        devtools.input(dock(7, Some((300., 20.))), DockInput::Click);
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
        let event = |navigation, elapsed_ms, phase| observer::Event {
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
            },
        )));
        assert!(devtools.observe_network(event(
            5,
            23,
            observer::Phase::FetchResponse { status: Some(404) },
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
        layout.paint(canvas, fonts, scale, self.section);
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
                if let Some(section) = layout.section_at(x, y) {
                    self.section = section;
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
                repeat: false,
            } if frame.focused && modifiers.is_empty() => match key {
                Key::Named(NamedKey::ArrowLeft) => self.section = Section::Console,
                Key::Named(NamedKey::ArrowRight) => self.section = Section::Network,
                _ => {}
            },
            _ => {}
        }
    }
}
