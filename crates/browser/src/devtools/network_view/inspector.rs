//! Filtered request selection and its scrollable detail pane.
use super::*;
use crate::devtools::{network_row, window_pointer};
#[derive(Default)]
pub(in crate::devtools) struct Inspector {
    filter: Filter,
    sort: Option<(usize, bool)>,
    selected: Option<RequestKey>,
    query: String,
    searching: bool,
    scroll: f32,
    detail_scroll: f32,
}

fn panes(bounds: Rect, selected: bool) -> (Option<NetworkLayout>, Option<Rect>) {
    if !selected {
        return (Some(NetworkLayout::new(bounds)), None);
    }
    if bounds.width < 780. {
        return (None, Some(bounds));
    }
    let width = (bounds.width * 0.42).clamp(325., 560.);
    let list = Rect {
        width: bounds.width - width,
        ..bounds
    };
    (
        Some(NetworkLayout::new(list)),
        Some(Rect {
            x: list.x + list.width,
            width,
            ..bounds
        }),
    )
}

fn detail_toolbar(bounds: Rect) -> Rect {
    Rect {
        height: bounds.height.min(TOOLBAR_H),
        ..bounds
    }
}
fn detail_close(bounds: Rect) -> Rect {
    let toolbar = detail_toolbar(bounds);
    Rect {
        x: toolbar.x + (toolbar.width - 60.).max(0.),
        width: toolbar.width.min(60.),
        ..toolbar
    }
}

fn detail_entries(row: &Record) -> Vec<(String, bool)> {
    let mut lines = Vec::new();
    let mut text = |entry: &str, title: bool| lines.push((entry.to_owned(), title));
    text("Request URL", true);
    text(row.url.as_str(), false);
    if let Some(final_url) = &row.details.final_url
        && final_url != row.url.as_str()
    {
        text("Response URL", true);
        text(final_url, false);
    }
    text("General", true);
    text(&format!("Method   {}", row.method.as_str()), false);
    text(&format!("Type   {}", row.category()), false);
    let status = row
        .status()
        .map_or_else(|| UNOBSERVED.into(), |v| v.to_string());
    text(&format!("HTTP status   {status}"), false);
    let state = match row.outcome() {
        Lifecycle::Started => "Pending",
        Lifecycle::HeadersReceived => "Response observed",
        Lifecycle::Terminal(TerminalOutcome::Completed) => "Finished",
        Lifecycle::Terminal(TerminalOutcome::Failed) => "Failed",
        Lifecycle::Terminal(TerminalOutcome::Cancelled) => "Cancelled",
    };
    text(&format!("Lifecycle   {state}"), false);
    if let Some(kind) = row.details.response_type {
        text(&format!("Response type   {kind}"), false);
    }
    text("Response metadata", true);
    text(
        &format!(
            "Content-Type   {}",
            row.details.mime.as_deref().unwrap_or(UNOBSERVED)
        ),
        false,
    );
    let bytes = |n: Option<u64>| n.map_or_else(|| UNOBSERVED.into(), |n| format!("{n} B"));
    text(
        &format!("Content-Length   {}", bytes(row.details.declared_bytes)),
        false,
    );
    text(
        &format!("Decoded body   {}", bytes(row.details.body_bytes)),
        false,
    );
    text("Loader timings", true);
    text(
        &format!("To response headers   {}", elapsed(row.headers_elapsed())),
        false,
    );
    text(
        &format!("To final outcome   {}", elapsed(row.end_elapsed())),
        false,
    );
    if let Some(after) = row
        .end_elapsed()
        .zip(row.headers_elapsed())
        .and_then(|(end, head)| end.checked_sub(head))
    {
        text(&format!("After headers   {}", elapsed(Some(after))), false);
    }
    lines
}

fn detail_lines(row: &Record, area: Rect) -> Vec<(String, bool)> {
    let width = ((area.width - 2. * PAD) / FONT_SIZE).floor().max(1.) as usize;
    let mut lines = Vec::new();
    for (entry, title) in detail_entries(row) {
        let chars: Vec<_> = entry.chars().collect();
        for part in chars.chunks(width) {
            lines.push((part.iter().collect(), title));
        }
    }
    lines
}

impl Inspector {
    fn matches(&self, row: &Record) -> bool {
        if !self.filter.matches(row) {
            return false;
        }
        if self.query.is_empty() {
            return true;
        }
        let needle = self.query.as_bytes();
        let contains = |value: &str| {
            value
                .as_bytes()
                .windows(needle.len())
                .any(|window| window.eq_ignore_ascii_case(needle))
        };
        contains(row.url.as_str())
            || contains(row.method.as_str())
            || contains(row.category())
            || row.status().is_some_and(|code| contains(&code.to_string()))
            || row.details.mime.as_deref().is_some_and(contains)
    }

    fn rows<'a>(&self, log: &'a NetworkLog) -> Vec<&'a Record> {
        let mut rows: Vec<_> = log.rows().filter(|r| self.matches(r)).collect();
        if let Some((column, descending)) = self.sort {
            rows.sort_by(|left, right| {
                let order = match column {
                    0 => left.method.as_str().cmp(right.method.as_str()),
                    1 => left.url.as_str().cmp(right.url.as_str()),
                    2 => left.category().cmp(right.category()),
                    3 => left.status().cmp(&right.status()),
                    _ => left.elapsed().cmp(&right.elapsed()),
                };
                if descending { order.reverse() } else { order }
            });
        }
        rows
    }

    pub(in crate::devtools) fn copy(&self, log: &NetworkLog) -> Option<String> {
        let row = self.selected_row(log)?;
        let mut summary = String::new();
        for (entry, title) in detail_entries(row) {
            if !summary.is_empty() {
                summary.push('\n');
                if title {
                    summary.push('\n');
                }
            }
            summary.push_str(&entry);
        }
        Some(summary)
    }

    fn selected_row<'a>(&self, log: &'a NetworkLog) -> Option<&'a Record> {
        log.rows()
            .find(|row| self.selected == Some(row.key) && self.matches(row))
    }

    pub(in crate::devtools) fn click(
        &mut self,
        log: &mut NetworkLog,
        bounds: Rect,
        x: f32,
        y: f32,
    ) {
        let (list, detail) = panes(bounds, self.selected_row(log).is_some());
        if let Some(detail) = detail
            && detail_close(detail).contains(x, y)
        {
            self.selected = None;
            self.detail_scroll = 0.;
            return;
        }
        let Some(layout) = list else {
            return;
        };
        if layout.clear_rect().is_some_and(|rect| rect.contains(x, y)) {
            log.clear();
            self.selected = None;
            self.scroll = 0.;
            self.detail_scroll = 0.;
            return;
        }
        let (buttons, rest) = filter_controls(&layout, self.filter);
        let search = search_rect(rest);
        self.searching = search.width > 0. && search.contains(x, y);
        if self.searching {
            return;
        }
        for &(filter, rect) in &buttons {
            if rect.contains(x, y) {
                self.filter = if buttons.len() == 1 {
                    Filter::ALL[(Filter::ALL.iter().position(|&f| f == filter).unwrap() + 1)
                        % Filter::ALL.len()]
                } else {
                    filter
                };
                self.scroll = 0.;
                self.selected = None;
                self.detail_scroll = 0.;
                return;
            }
        }
        if layout.header.contains(x, y) {
            if let Some(index) = layout
                .columns
                .iter()
                .position(|column| column.cell(layout.header).contains(x, y))
            {
                self.sort = match self.sort {
                    Some((i, true)) if i == index => None,
                    Some((i, false)) if i == index => Some((index, true)),
                    _ => Some((index, false)),
                };
                self.scroll = 0.;
            }
            return;
        }
        if layout.list.contains(x, y) {
            let rows = self.rows(log);
            let offset = layout.clamp_scroll(rows.len(), self.scroll);
            let index = ((y - layout.list.y + offset) / ROW_H).floor() as usize;
            if let Some(row) = rows.get(index) {
                self.selected = Some(row.key);
                self.detail_scroll = 0.;
            }
        }
    }

    pub(in crate::devtools) fn scroll(
        &mut self,
        log: &NetworkLog,
        bounds: Rect,
        x: f32,
        y: f32,
        delta: f32,
    ) {
        let selected = self.selected_row(log);
        let (list, details) = panes(bounds, selected.is_some());
        if let (Some(row), Some(bounds)) = (selected, details)
            && bounds.contains(x, y)
        {
            let content = bounds.height - detail_toolbar(bounds).height;
            let max = (detail_lines(row, bounds).len() as f32 * DETAIL_LINE - content).max(0.);
            self.detail_scroll = (self.detail_scroll.min(max) + delta).clamp(0., max);
        } else if let Some(layout) = list
            && layout.list_rect().contains(x, y)
        {
            self.scroll = layout.scroll_by(
                log.rows().filter(|r| self.matches(r)).count(),
                self.scroll,
                delta,
            );
        }
    }

    pub(in crate::devtools) fn key(&mut self, log: &NetworkLog, key: &Key, bounds: Rect) -> bool {
        if self.searching {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.query.clear();
                    self.searching = false;
                }
                Key::Named(NamedKey::Enter) => self.searching = false,
                Key::Named(NamedKey::Backspace) => {
                    self.query.pop();
                }
                Key::Character(value) => {
                    for ch in value.chars().filter(|c| !c.is_control()) {
                        if self.query.len() + ch.len_utf8() > 128 {
                            break;
                        }
                        self.query.push(ch);
                    }
                }
                _ => return false,
            }
            self.selected = None;
            self.scroll = 0.;
            return true;
        }
        if matches!(key, Key::Character(ch) if ch == "/") {
            self.searching = true;
            self.selected = None;
            return true;
        }
        let rows = self.rows(log);
        let current = rows.iter().position(|r| Some(r.key) == self.selected);
        let index = match key {
            Key::Named(NamedKey::Escape) if current.is_some() => {
                self.selected = None;
                return true;
            }
            Key::Named(NamedKey::ArrowDown) => current
                .map_or(0, |n| n + 1)
                .min(rows.len().saturating_sub(1)),
            Key::Named(NamedKey::ArrowUp) => current.unwrap_or(0).saturating_sub(1),
            Key::Named(NamedKey::Enter) if current.is_none() => 0,
            Key::Named(NamedKey::PageDown | NamedKey::PageUp) if current.is_some() => {
                let (_, Some(detail)) = panes(bounds, true) else {
                    return false;
                };
                let delta = (detail.height - TOOLBAR_H).max(DETAIL_LINE)
                    * if *key == Key::Named(NamedKey::PageUp) {
                        -1.
                    } else {
                        1.
                    };
                self.scroll(log, bounds, detail.x + 1., detail.y + 1., delta);
                return true;
            }
            _ => return false,
        };
        let Some(row) = rows.get(index) else {
            return false;
        };
        self.selected = Some(row.key);
        self.detail_scroll = 0.;
        if let (Some(layout), _) = panes(bounds, true) {
            let top = index as f32 * ROW_H;
            self.scroll = self
                .scroll
                .clamp((top + ROW_H - layout.list.height).max(0.).min(top), top);
        }
        true
    }

    pub(in crate::devtools) fn draw(
        &self,
        log: &NetworkLog,
        canvas: &mut Canvas,
        fonts: &mut Fonts,
        frame: DockFrame,
        bounds: Rect,
        observer_dropped: usize,
    ) {
        let selected = self.selected_row(log);
        let (list, detail) = panes(bounds, selected.is_some());
        let clip = panel::scaled(bounds, frame.scale);
        if let Some(layout) = list {
            let rows = self.rows(log);
            let index = rows.iter().position(|r| Some(r.key) == self.selected);
            let first = log.rows().filter_map(|r| r.started_at).min();
            let last = log
                .rows()
                .filter_map(|r| r.started_at?.checked_add(r.elapsed().unwrap_or_default()))
                .max();
            let timeline = first.zip(last).map(|(start, end)| {
                (
                    start,
                    end.saturating_duration_since(start)
                        .max(Duration::from_millis(1)),
                )
            });
            let display: Vec<_> = rows.iter().map(|row| network_row(row)).collect();
            let loss = log.loss();
            let coverage = if observer_dropped == 0 {
                format!("{} / {} requests · HTTP(S)", rows.len(), log.rows().len())
            } else {
                format!(
                    "{} / {} requests · {observer_dropped} events lost across tabs",
                    rows.len(),
                    log.rows().len()
                )
            };
            draw_network(
                canvas,
                fonts,
                frame.scale,
                clip,
                &layout,
                NetworkView {
                    rows: &display,
                    filter: self.filter,
                    sort: self.sort,
                    query: &self.query,
                    searching: self.searching,
                    selected: index,
                    timeline,
                    scroll: self.scroll,
                    discarded: loss
                        .evicted_rows
                        .saturating_add(loss.dropped_events)
                        .saturating_add(loss.unmatched_events),
                    clear_hovered: layout.clear_rect().is_some_and(|r| {
                        window_pointer(frame).is_some_and(|(x, y)| r.contains(x, y))
                    }),
                    coverage_note: Some(&coverage),
                },
            );
        }
        if let (Some(bounds), Some(row)) = (detail, selected) {
            let mut p = Painter {
                canvas,
                fonts,
                scale: frame.scale,
                canvas_clip: clip,
                clip,
            };
            p.set_clip(bounds);
            p.fill(bounds, PANEL);
            let bar = detail_toolbar(bounds);
            p.fill(bar, HEADER);
            let close = detail_close(bounds);
            p.label(
                Rect {
                    width: (bar.width - close.width).max(0.),
                    ..bar
                },
                "Request details",
                TEXT,
                true,
            );
            if bar.width >= 285. {
                p.label(
                    Rect {
                        x: close.x - 118.,
                        width: 118.,
                        ..bar
                    },
                    "Ctrl+C  Copy",
                    MUTED,
                    false,
                );
            }
            p.label(close, "Close", ACCENT, true);
            let content = Rect {
                y: bar.y + bar.height,
                height: bounds.height - bar.height,
                ..bounds
            };
            p.set_clip(content);
            let lines = detail_lines(row, bounds);
            let max = (lines.len() as f32 * DETAIL_LINE - content.height).max(0.);
            let scroll = self.detail_scroll.min(max);
            for (i, (line, title)) in lines.iter().enumerate() {
                let band = Rect {
                    y: content.y + i as f32 * DETAIL_LINE - scroll,
                    height: DETAIL_LINE,
                    ..content
                };
                if band.y + band.height <= content.y || band.y >= content.y + content.height {
                    continue;
                }
                if *title {
                    p.fill(band, HEADER);
                }
                p.label(band, line, if *title { ACCENT } else { TEXT }, *title);
            }
            if max > 0. && content.height > 0. {
                let thumb = (content.height * content.height / (content.height + max))
                    .clamp(8_f32.min(content.height), content.height);
                p.fill(
                    Rect {
                        x: content.x + content.width - 5.,
                        y: content.y + (content.height - thumb) * scroll / max,
                        width: 3.,
                        height: thumb,
                    },
                    MUTED,
                );
            }
            p.set_clip(bounds);
            p.fill(
                Rect {
                    width: 1.,
                    ..bounds
                },
                LINE,
            );
            p.canvas.clip(clip);
        }
    }
}

#[cfg(test)]
mod inspector_tests {
    use super::*;
    use crate::devtools::network::{
        DisplayMethod, DisplayUrl, NetworkEvent, NetworkPhase, NetworkScope,
    };
    use url::Url;

    fn populated() -> NetworkLog {
        let scope = NetworkScope {
            tab: 1,
            navigation: 5,
        };
        let mut log = NetworkLog::new(scope);
        let now = Instant::now();
        for (id, kind, path, status) in [
            (1, RequestKind::Navigation, "page", 200),
            (2, RequestKind::FetchOrXhr, "missing", 404),
        ] {
            let key = RequestKey { scope, kind, id };
            let url = Url::parse(&format!(
                "https://private:password@test.example/{path}?do-not-show=1#secret"
            ))
            .unwrap();
            log.observe(NetworkEvent {
                key,
                at: Some(now),
                phase: NetworkPhase::Started {
                    method: DisplayMethod::from_method("GET"),
                    url: DisplayUrl::from_url(&url),
                    resource_type: None,
                },
            });
            log.observe(NetworkEvent {
                key,
                at: Some(now + Duration::from_millis(10)),
                phase: NetworkPhase::ResponseHeaders {
                    status: Some(status),
                    details: Default::default(),
                },
            });
        }
        log
    }

    #[test]
    fn search_selection_and_stale_scope_do_not_expose_private_url_fields() {
        let mut log = populated();
        let bounds = Rect {
            x: 0.,
            y: 60.,
            width: 900.,
            height: 180.,
        };
        let layout = NetworkLayout::new(bounds);
        let mut ui = Inspector::default();
        ui.click(&mut log, bounds, 10., layout.list.y + ROW_H + 3.);
        assert_eq!(ui.selected_row(&log).unwrap().key.id, 2);
        let narrow = Rect {
            width: 360.,
            ..bounds
        };
        assert!(panes(narrow, true).0.is_none());
        assert!(panes(bounds, true).0.is_some());
        let copied = ui.copy(&log).expect("selected request");
        assert!(
            copied.contains("https://test.example/missing") && copied.contains("HTTP status   404")
        );
        assert!(copied.contains("To response headers   10.0 ms"));
        assert!(!copied.contains("private") && !copied.contains("do-not-show"));
        let labels = detail_lines(ui.selected_row(&log).unwrap(), narrow);
        assert!(labels.iter().any(|(s, _)| s.contains("404")));
        assert!(
            !labels
                .iter()
                .any(|(s, _)| s.contains("private") || s.contains("do-not-show"))
        );
        assert!(ui.key(&log, &Key::Named(NamedKey::Escape), bounds));
        assert!(ui.key(&log, &Key::Character("/".into()), bounds));
        assert!(ui.key(&log, &Key::Character("MiSsInG".into()), bounds));
        assert!(ui.key(&log, &Key::Named(NamedKey::Enter), bounds));
        assert_eq!(log.rows().filter(|r| ui.matches(r)).count(), 1);
        assert!(ui.key(&log, &Key::Named(NamedKey::ArrowDown), bounds));
        assert_eq!(ui.selected_row(&log).unwrap().key.id, 2);
        log.clear();
        assert!(ui.selected_row(&log).is_none());
        assert!(ui.copy(&log).is_none());
        assert!(!ui.key(&log, &Key::Named(NamedKey::ArrowDown), bounds));
    }

    #[test]
    fn sorting_and_click_target_share_the_stable_request_order() {
        let mut log = populated();
        let bounds = Rect {
            x: 0.,
            y: 0.,
            width: 900.,
            height: 180.,
        };
        let layout = NetworkLayout::new(bounds);
        let x = layout.columns[3].x + 2.;
        let y = layout.header.y + 2.;
        let mut ui = Inspector::default();
        ui.click(&mut log, bounds, x, y);
        assert_eq!(ui.rows(&log)[0].status(), Some(200));
        ui.click(&mut log, bounds, x, y);
        assert_eq!(ui.rows(&log)[0].status(), Some(404));
        ui.click(&mut log, bounds, 3., layout.list.y + 3.);
        assert_eq!(ui.selected_row(&log).unwrap().status(), Some(404));
        ui.key(&log, &Key::Named(NamedKey::Escape), bounds);
        ui.click(&mut log, bounds, x, y);
        assert_eq!(ui.sort, None);
        assert_eq!(ui.rows(&log)[0].status(), Some(200));
    }

    #[test]
    fn filter_controls_use_same_rows_as_hit_testing_and_wheel() {
        let mut log = populated();
        let bounds = Rect {
            x: 0.,
            y: 60.,
            width: 500.,
            height: 180.,
        };
        let layout = NetworkLayout::new(bounds);
        let mut ui = Inspector::default();
        let (buttons, _) = filter_controls(&layout, Filter::All);
        let (_, r) = buttons.iter().find(|(f, _)| *f == Filter::Errors).unwrap();
        ui.click(&mut log, bounds, r.x + 2., r.y + 2.);
        assert_eq!(ui.filter, Filter::Errors);
        ui.click(&mut log, bounds, 10., layout.list.y + 2.);
        let row = ui.selected_row(&log).unwrap();
        assert_eq!(row.key.id, 2);
        assert_eq!(row.headers_elapsed(), Some(Duration::from_millis(10)));
        assert_eq!(row.end_elapsed(), None);
        ui.scroll(&log, bounds, 25., bounds.y + 50., 200.);
        assert!(ui.detail_scroll > 0.);
        let close = detail_close(bounds);
        ui.click(&mut log, bounds, close.x + 1., close.y + 1.);
        assert!(ui.selected.is_none());
    }
}
