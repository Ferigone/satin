//! Native Console body. The caller owns the snapshot, scroll offset and Clear action.

use rust_browser_engine::{Fonts, Rect, css::Color, paint::Canvas};
use std::{borrow::Cow, ops::Range};

const TOOLBAR_HEIGHT: f32 = 34.;
const STATUS_HEIGHT: f32 = 22.;
const ROW_HEIGHT: f32 = 42.;
const PAD: f32 = 8.;
const CLEAR_WIDTH: f32 = 64.;
const SCROLL_GUTTER: f32 = 10.;
const BG: Color = Color(29, 28, 30, 255);
const PANEL: Color = Color(42, 41, 43, 255);
const LINE: Color = Color(55, 54, 55, 255);
const TEXT: Color = Color(242, 241, 241, 255);
const MUTED: Color = Color(184, 179, 181, 255);
const WARN: Color = Color(255, 210, 126, 255);
const ERROR: Color = Color(255, 159, 154, 255);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Severity {
    Debug,
    Log,
    Info,
    Warn,
    Error,
}

impl Severity {
    fn style(self) -> (&'static str, Color) {
        match self {
            Self::Debug => ("Debug", MUTED),
            Self::Log => ("Log", TEXT),
            Self::Info => ("Info", Color(169, 213, 255, 255)),
            Self::Warn => ("Warning", WARN),
            Self::Error => ("Error", ERROR),
        }
    }
}

/// Explicit provenance supplied by capture. An ordinary `console.error` has no failure marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Caught,
    BrowserReported,
}

impl Failure {
    fn label(self) -> &'static str {
        match self {
            Self::Caught => "Caught",
            Self::BrowserReported => "Browser-reported",
        }
    }
}

pub(super) struct ConsoleRow<'a> {
    pub(super) severity: Severity,
    pub(super) message: &'a str,
    pub(super) failure: Option<Failure>,
    pub(super) source: Option<&'a str>,
    pub(super) phase: Option<&'a str>,
    pub(super) capture_truncated: bool,
    /// Total occurrences for a merged row; 0 or 1 does not draw a count.
    pub(super) repeat: usize,
}

/// Geometry in logical window pixels, including the input regions.
pub(super) struct ConsoleLayout {
    bounds: Rect,
    pub(super) clear: Option<Rect>,
    pub(super) list: Rect,
    pub(super) scroll: f32,
    max_scroll: f32,
    row_count: usize,
    status: Option<(Rect, u64)>,
}

impl ConsoleLayout {
    /// Rows run oldest to newest. Pass `f32::INFINITY` to position at the latest row.
    pub(super) fn new(bounds: Rect, row_count: usize, scroll: f32, discarded: u64) -> Self {
        let bounds = if drawable(bounds) {
            bounds
        } else {
            Rect::default()
        };
        let toolbar_height = bounds.height.min(TOOLBAR_HEIGHT);
        let mut header_height = toolbar_height;
        let mut clear = (bounds.width >= CLEAR_WIDTH + 2. * PAD && bounds.height >= TOOLBAR_HEIGHT)
            .then(|| {
                rect(
                    bounds.x + bounds.width - PAD - CLEAR_WIDTH,
                    bounds.y + 5.,
                    CLEAR_WIDTH,
                    TOOLBAR_HEIGHT - 10.,
                )
            });
        let status = (discarded > 0).then(|| {
            // A narrow toolbar gives the count its own line when there's room.
            let (y, width, height) = if let Some(button) = clear {
                if bounds.width >= 320. {
                    (bounds.y, button.x - bounds.x, toolbar_height)
                } else if bounds.height >= TOOLBAR_HEIGHT + STATUS_HEIGHT + 18. {
                    header_height += STATUS_HEIGHT;
                    (bounds.y + toolbar_height, bounds.width, STATUS_HEIGHT)
                } else {
                    clear = None;
                    (bounds.y, bounds.width, toolbar_height)
                }
            } else {
                (bounds.y, bounds.width, toolbar_height)
            };
            (
                rect(bounds.x + PAD, y, (width - 2. * PAD).max(0.), height),
                discarded,
            )
        });
        let list = rect(
            bounds.x,
            bounds.y + header_height,
            bounds.width,
            bounds.height - header_height,
        );
        let max_scroll = if drawable(list) {
            (row_count as f32 * ROW_HEIGHT - list.height).max(0.)
        } else {
            0.
        };
        Self {
            bounds,
            clear,
            list,
            scroll: if scroll.is_nan() {
                0.
            } else {
                scroll.clamp(0., max_scroll)
            },
            max_scroll,
            row_count,
            status,
        }
    }

    /// Apply a logical-pixel delta after the caller has hit-tested `list`.
    pub(super) fn scroll_by(&self, delta: f32) -> f32 {
        if delta.is_finite() {
            (self.scroll + delta).clamp(0., self.max_scroll)
        } else {
            self.scroll
        }
    }

    fn visible_rows(&self) -> Range<usize> {
        if !drawable(self.list) {
            return 0..0;
        }
        let start = (self.scroll / ROW_HEIGHT).floor() as usize;
        let end = ((self.scroll + self.list.height) / ROW_HEIGHT).ceil() as usize;
        start.min(self.row_count)..end.min(self.row_count)
    }

    fn scrollbar(&self) -> Option<(Rect, Rect)> {
        if self.max_scroll == 0. || self.list.width <= SCROLL_GUTTER {
            return None;
        }
        let track = rect(
            self.list.x + self.list.width - SCROLL_GUTTER,
            self.list.y,
            SCROLL_GUTTER,
            self.list.height,
        );
        let thumb_height = (track.height * (track.height / (self.max_scroll + track.height)))
            .max(14.)
            .min(track.height);
        let y = track.y + (track.height - thumb_height) * (self.scroll / self.max_scroll);
        Some((track, rect(track.x + 3., y, 4., thumb_height)))
    }

    /// Draw the same row snapshot used for layout. `clip` is the caller's current
    /// device-pixel Canvas clip; it is intersected with this view and restored.
    pub(super) fn draw(
        &self,
        canvas: &mut Canvas,
        fonts: &mut Fonts,
        rows: &[ConsoleRow<'_>],
        scale: f32,
        clip: Rect,
    ) {
        if !scale.is_finite() || scale <= 0. || !drawable(self.bounds) || !drawable(clip) {
            return;
        }
        let panel_clip = intersection(device_rect(self.bounds, scale), clip);
        if !drawable(panel_clip) {
            return;
        }
        let mut paint = Painter {
            canvas,
            fonts,
            scale,
            clip: panel_clip,
        };
        paint.fill(self.bounds, BG);
        paint.fill(
            rect(
                self.bounds.x,
                self.bounds.y,
                self.bounds.width,
                self.list.y - self.bounds.y,
            ),
            PANEL,
        );
        if let Some(clear) = self.clear {
            paint.fill(clear, LINE);
            paint.label(
                rect(clear.x + 14., clear.y, clear.width - 14., clear.height),
                "Clear",
                TEXT,
                true,
            );
        }
        if let Some((area, count)) = self.status {
            paint.label(area, &format!("{count} discarded"), WARN, true);
        }

        let scrollbar = self.scrollbar();
        let row_width = self.list.width - scrollbar.map_or(0., |(track, _)| track.width);
        let row_area = rect(self.list.x, self.list.y, row_width, self.list.height);
        paint.clip = intersection(panel_clip, device_rect(row_area, scale));
        if self.row_count == 0 {
            paint.label(
                line(row_area, PAD, 0., 24.),
                "No console messages.",
                MUTED,
                false,
            );
        } else {
            let visible = self.visible_rows();
            for (index, entry) in rows
                .iter()
                .enumerate()
                .take(visible.end)
                .skip(visible.start)
            {
                let y = self.list.y + (index as f32 * ROW_HEIGHT - self.scroll);
                let row = rect(row_area.x, y, row_width, ROW_HEIGHT);
                if !index.is_multiple_of(2) {
                    paint.fill(row, Color(34, 33, 35, 255));
                }
                paint.fill(rect(row.x, y + ROW_HEIGHT - 1., row.width, 1.), LINE);
                paint.row(row, entry);
            }
        }
        if let Some((track, thumb)) = scrollbar {
            paint.clip = panel_clip;
            paint.fill(track, PANEL);
            paint.fill(thumb, MUTED);
        }
        paint.canvas.clip(clip);
    }
}

struct Painter<'a> {
    canvas: &'a mut Canvas,
    fonts: &'a mut Fonts,
    scale: f32,
    clip: Rect,
}

impl Painter<'_> {
    fn fill(&mut self, logical: Rect, color: Color) {
        if drawable(self.clip) {
            self.canvas.clip(self.clip);
            self.canvas
                .fill_rect(device_rect(logical, self.scale), color);
        }
    }

    fn label(&mut self, area: Rect, text: &str, color: Color, bold: bool) {
        let clip = intersection(device_rect(area, self.scale), self.clip);
        if drawable(clip) {
            self.canvas.clip(clip);
            self.canvas.label(
                text,
                area.x * self.scale,
                (area.y + area.height * 0.5 + 4.) * self.scale,
                13. * self.scale,
                color,
                bold,
                self.fonts,
            );
        }
    }

    fn row(&mut self, row: Rect, entry: &ConsoleRow<'_>) {
        let (severity, color) = entry.severity.style();
        let badge_width = 64.;
        self.label(
            rect(
                row.x + PAD,
                row.y + 2.,
                (row.width - PAD).clamp(0., badge_width),
                18.,
            ),
            severity,
            color,
            true,
        );
        self.label(
            line(row, PAD + badge_width, 2., 18.),
            &inert_line(entry.message, 512),
            TEXT,
            false,
        );

        let mut inset = PAD;
        if let Some(failure) = entry.failure {
            let width = match failure {
                Failure::Caught => 54.,
                Failure::BrowserReported => 116.,
            };
            self.label(
                rect(
                    row.x + inset,
                    row.y + 21.,
                    (row.width - inset).clamp(0., width),
                    18.,
                ),
                failure.label(),
                color,
                true,
            );
            inset += width;
        }
        if let Some(details) = details(entry) {
            self.label(line(row, inset, 21., 18.), &details, MUTED, false);
        }
    }
}

fn details(row: &ConsoleRow<'_>) -> Option<String> {
    let mut parts = Vec::new();
    if row.capture_truncated {
        parts.push("Preview truncated".into());
    }
    if row.repeat > 1 {
        parts.push(format!("×{}", row.repeat));
    }
    if let Some(phase) = row.phase.filter(|text| !text.is_empty()) {
        parts.push(format!("Phase: {}", inert_line(phase, 64)));
    }
    if let Some(source) = row.source.filter(|text| !text.is_empty()) {
        parts.push(format!("Source: {}", inert_line(source, 160)));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Literal single-line text only. Bound shaping input and neutralize control
/// characters that can break lines or override text direction inside a field.
fn inert_line(text: &str, limit: usize) -> Cow<'_, str> {
    fn control(ch: char) -> bool {
        ch.is_control()
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{206f}')
    }
    let mut end = text.len();
    let mut has_control = false;
    for (index, (offset, ch)) in text.char_indices().enumerate() {
        if index == limit {
            end = offset;
            break;
        }
        has_control |= control(ch);
    }
    if end == text.len() && !has_control {
        return Cow::Borrowed(text);
    }
    let mut displayed: String = text[..end]
        .chars()
        .map(|ch| if control(ch) { ' ' } else { ch })
        .collect();
    if end < text.len() {
        displayed.push('…');
    }
    Cow::Owned(displayed)
}

fn line(row: Rect, inset: f32, top: f32, height: f32) -> Rect {
    rect(
        row.x + inset,
        row.y + top,
        (row.width - inset - PAD).max(0.),
        height,
    )
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn drawable(rect: Rect) -> bool {
    rect.width > 0.
        && rect.height > 0.
        && [rect.x, rect.y, rect.x + rect.width, rect.y + rect.height]
            .iter()
            .all(|edge| edge.is_finite())
}

fn intersection(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    rect(
        x,
        y,
        ((a.x + a.width).min(b.x + b.width) - x).max(0.),
        ((a.y + a.height).min(b.y + b.height) - y).max(0.),
    )
}

// Canvas fills round outward. Snap shared edges first so adjacent regions
// cannot bleed into each other at a fractional window scale.
fn device_rect(r: Rect, scale: f32) -> Rect {
    let x = (r.x * scale).round();
    let y = (r.y * scale).round();
    rect(
        x,
        y,
        ((r.x + r.width) * scale).round() - x,
        ((r.y + r.height) * scale).round() - y,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_rows_include_partial_edges_and_stop_at_the_end() {
        let bounds = rect(12., 180., 500., TOOLBAR_HEIGHT + 2. * ROW_HEIGHT);
        let aligned = ConsoleLayout::new(bounds, 6, ROW_HEIGHT, 0);
        assert_eq!(aligned.visible_rows(), 1..3);
        let partial = ConsoleLayout::new(bounds, 6, ROW_HEIGHT + 0.5, 0);
        assert_eq!(partial.visible_rows(), 1..4);
        let latest = ConsoleLayout::new(bounds, 6, f32::INFINITY, 0);
        assert_eq!(latest.visible_rows(), 4..6);
        assert_eq!(latest.scroll_by(200.), latest.scroll);
        assert_eq!(latest.scroll_by(-1000.), 0.);
        assert_eq!(latest.scroll_by(f32::NEG_INFINITY), latest.scroll);
        let (track, thumb) = latest.scrollbar().unwrap();
        assert_eq!(thumb.y + thumb.height, track.y + track.height);
    }

    #[test]
    fn a_resize_or_clear_clamps_stale_scroll() {
        let resized = ConsoleLayout::new(rect(0., 0., 600., 300.), 2, 100., 0);
        assert_eq!(resized.scroll, 0.);
        assert_eq!(resized.visible_rows(), 0..2);
        assert!(resized.scrollbar().is_none());
        let cleared = ConsoleLayout::new(resized.bounds, 0, 100., 0);
        assert_eq!(cleared.scroll, 0.);
        assert_eq!(cleared.visible_rows(), 0..0);
    }

    #[test]
    fn clear_never_overlaps_the_list_or_discard_status() {
        for width in [150., 600.] {
            let bounds = rect(13., 117., width, 100.);
            let layout = ConsoleLayout::new(bounds, 10, f32::NAN, u64::MAX);
            let clear = layout.clear.unwrap();
            let (status, count) = layout.status.unwrap();
            assert_eq!(count, u64::MAX);
            assert_eq!(layout.scroll, 0.);
            assert!(bounds.contains(clear.x, clear.y));
            assert!(!drawable(intersection(clear, layout.list)));
            assert!(!drawable(intersection(status, clear)));
            assert!(!drawable(intersection(status, layout.list)));
        }
    }

    #[test]
    fn short_narrow_panels_keep_overflow_without_an_invisible_clear_target() {
        let bounds = rect(10., 20., 140., 45.);
        let no_overflow = ConsoleLayout::new(bounds, 10, 0., 0);
        assert!(no_overflow.clear.is_some());
        let overflow = ConsoleLayout::new(bounds, 10, 0., 4);
        assert!(overflow.clear.is_none());
        assert!(drawable(overflow.status.unwrap().0));
        assert_eq!(overflow.visible_rows(), 0..1);
        let too_short = ConsoleLayout::new(rect(10., 20., 140., 25.), 10, 500., 4);
        assert_eq!(too_short.visible_rows(), 0..0);
        let invalid = ConsoleLayout::new(rect(f32::NAN, 0., 100., 100.), 10, 1., 0);
        assert!(invalid.clear.is_none());
        assert_eq!(invalid.scroll, 0.);
    }

    #[test]
    fn fractional_dpi_shares_the_clip_edge_at_the_start_of_the_list() {
        let layout = ConsoleLayout::new(rect(0.3, 4.2, 400., 125.), 10, 8.5, 0);
        let header = rect(
            layout.bounds.x,
            layout.bounds.y,
            layout.bounds.width,
            TOOLBAR_HEIGHT,
        );
        let header = device_rect(header, 1.25);
        let list = device_rect(layout.list, 1.25);
        assert_eq!(header.y + header.height, list.y);
    }

    #[test]
    fn content_remains_literal_and_single_line() {
        let literal = "<b>log</b> javascript:example()";
        assert!(matches!(inert_line(literal, 512), Cow::Borrowed(_)));
        assert_eq!(
            inert_line("a\nb\tc\u{202e}d\u{2028}e\u{2067}f", 32),
            "a b c d e f"
        );
        assert_eq!(inert_line("ä🍉xyz", 2), "ä🍉…");
    }

    #[test]
    fn unavailable_metadata_is_omitted_and_provenance_is_explicit() {
        let mut row = ConsoleRow {
            severity: Severity::Error,
            message: "example",
            failure: None,
            phase: None,
            source: Some(""),
            capture_truncated: false,
            repeat: 1,
        };
        assert_eq!(details(&row), None);
        row.source = Some("example.js");
        row.phase = Some("script");
        row.repeat = 3;
        assert_eq!(
            details(&row).as_deref(),
            Some("×3 · Phase: script · Source: example.js")
        );
        assert_eq!(Failure::Caught.label(), "Caught");
        assert_eq!(Failure::BrowserReported.label(), "Browser-reported");
    }

    #[test]
    fn engine_truncation_is_labeled_even_when_the_preview_fits() {
        use super::super::{Document, console::ConsoleFilter, console_row};
        use rust_browser_engine::{
            LoadedPage,
            script::{CONSOLE_TEXT_BYTES, ScriptRuntime},
        };

        let page = LoadedPage::from_html("https://console.test/", "<!doctype html>");
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let args = ["'x'"; 64].join(", ");
        runtime
            .eval(&format!(
                "console.error({args}, 'omitted'); console.error({args});"
            ))
            .unwrap();
        let legacy = runtime.diagnostics();
        let snapshot = runtime.take_console_snapshot();
        assert_eq!(snapshot.records.len(), 2);
        assert!(snapshot.records[0].truncated);
        assert!(snapshot.records[0].text.len() < CONSOLE_TEXT_BYTES);
        assert!(!snapshot.records[1].truncated);

        let mut document = Document::new(1, 1);
        assert!(document.ingest_console(snapshot));
        let mut rows = document.console.rows(ConsoleFilter::ALL).map(console_row);
        let truncated = rows.next().unwrap();
        let complete = rows.next().unwrap();
        assert_eq!(truncated.message, complete.message);
        assert_eq!(truncated.failure, None);
        assert_eq!(details(&truncated).as_deref(), Some("Preview truncated"));
        assert_eq!(details(&complete), None);
        assert_eq!(document.console.discarded(), 0);
        assert_eq!(runtime.diagnostics(), legacy);
    }
}
