use rust_browser_engine::{Fonts, Rect, css::Color, paint::Canvas};
use std::{borrow::Cow, ops::Range, time::Duration};

const TOOLBAR_H: f32 = 30.;
const HEADER_H: f32 = 24.;
const ROW_H: f32 = 24.;
const PAD: f32 = 7.;
const CLEAR_W: f32 = 60.;
const CLEAR_H: f32 = 22.;
const GUTTER: f32 = 9.;
const COMFORTABLE_TABLE_W: f32 = 526.;
const FONT_SIZE: f32 = 12.;
const MAX_LABEL_CHARS: usize = 256;
const UNOBSERVED: &str = "—";

const PANEL: Color = Color(36, 35, 37, 255);
const HEADER: Color = Color(42, 41, 43, 255);
const STRIPE: Color = Color(40, 39, 41, 255);
const LINE: Color = Color(57, 56, 59, 255);
const TEXT: Color = Color(242, 241, 241, 255);
const MUTED: Color = Color(179, 175, 178, 255);
const ALERT: Color = Color(255, 170, 162, 255);
const HOVER: Color = Color(71, 69, 74, 255);

/// Explicit lifecycle information supplied by the observer, not inferred by the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NetworkOutcome {
    Pending,
    Failed,
    Cancelled,
}

pub(super) struct NetworkRow<'a> {
    pub(super) method: Option<&'a str>,
    /// Already redacted by the model. Never pass an unredacted URL to this view.
    pub(super) display_url: Option<&'a str>,
    pub(super) category: Option<&'a str>,
    /// An observed HTTP status, typically available at headers. Not body completion.
    pub(super) http_status: Option<u16>,
    pub(super) outcome: Option<NetworkOutcome>,
    /// Measured time to the observed event, not an estimate of download duration.
    pub(super) elapsed: Option<Duration>,
}

pub(super) struct NetworkView<'a> {
    /// Bounded snapshot in display order; this view never retains request data.
    pub(super) rows: &'a [NetworkRow<'a>],
    /// Snapshot's aggregate count of records discarded by capture or retention.
    pub(super) discarded: u64,
    /// Offset from the first retained row in logical pixels.
    pub(super) scroll: f32,
    pub(super) clear_hovered: bool,
    /// Optional truthful description of the sources the adapter observes.
    pub(super) coverage_note: Option<&'a str>,
}

struct Column {
    title: &'static str,
    x: f32,
    width: f32,
}

impl Column {
    fn cell(&self, band: Rect) -> Rect {
        Rect {
            x: self.x,
            width: self.width,
            ..band
        }
    }
}

/// Paint and pointer routing share these logical-pixel bounds. The dock owns input state.
pub(super) struct NetworkLayout {
    bounds: Rect,
    toolbar: Rect,
    summary: Rect,
    header: Rect,
    list: Rect,
    clear: Option<Rect>,
    columns: [Column; 5],
}

impl NetworkLayout {
    pub(super) fn new(bounds: Rect) -> Self {
        let toolbar = Rect {
            height: bounds.height.clamp(0., TOOLBAR_H),
            ..bounds
        };
        let below_toolbar = (bounds.height - toolbar.height).max(0.);
        let header = Rect {
            y: toolbar.y + toolbar.height,
            height: below_toolbar.min(HEADER_H),
            ..bounds
        };
        let list = Rect {
            y: header.y + header.height,
            width: (bounds.width - GUTTER).max(0.),
            height: below_toolbar - header.height,
            ..bounds
        };
        let clear = (toolbar.width >= CLEAR_W + PAD && toolbar.height >= CLEAR_H).then_some(Rect {
            x: toolbar.x + toolbar.width - PAD - CLEAR_W,
            y: toolbar.y + (toolbar.height - CLEAR_H) / 2.,
            width: CLEAR_W,
            height: CLEAR_H,
        });
        let summary = Rect {
            width: clear.map_or(toolbar.width, |r| (r.x - toolbar.x - PAD).max(0.)),
            ..toolbar
        };

        // On wide panels URL gets all extra room; on narrow panels every column
        // gives up space so the status and time never move off-screen.
        let compact = (list.width / COMFORTABLE_TABLE_W).min(1.);
        let [method, category, status, time] = [68., 92., 116., 90.].map(|w| w * compact);
        let url = (list.width - (method + category + status + time)).max(0.);
        let mut x = bounds.x;
        let columns = [
            ("Method", method),
            ("URL", url),
            ("Type", category),
            ("Status", status),
            ("Time", time),
        ]
        .map(|(title, width)| {
            let column = Column { title, x, width };
            x += width;
            column
        });

        Self {
            bounds,
            toolbar,
            summary,
            header,
            list,
            clear,
            columns,
        }
    }

    pub(super) fn clear_rect(&self) -> Option<Rect> {
        self.clear
    }

    /// Use this for wheel routing; the adjacent scrollbar is a passive indicator.
    pub(super) fn list_rect(&self) -> Rect {
        Rect {
            width: self.bounds.width,
            ..self.list
        }
    }

    fn max_scroll(&self, row_count: usize) -> f32 {
        if self.list.height > 0. && self.list.width > 0. {
            (row_count as f32 * ROW_H - self.list.height).max(0.)
        } else {
            0.
        }
    }

    pub(super) fn clamp_scroll(&self, row_count: usize, scroll: f32) -> f32 {
        if scroll.is_nan() {
            0.
        } else {
            scroll.clamp(0., self.max_scroll(row_count))
        }
    }

    /// `delta` is in logical pixels; positive scrolls toward later rows.
    pub(super) fn scroll_by(&self, row_count: usize, current: f32, delta: f32) -> f32 {
        let current = self.clamp_scroll(row_count, current);
        if delta.is_nan() {
            current
        } else {
            self.clamp_scroll(row_count, current + delta)
        }
    }

    fn visible_rows(&self, row_count: usize, scroll: f32) -> Range<usize> {
        if self.list.height <= 0. || self.list.width <= 0. {
            return 0..0;
        }
        let first = (scroll / ROW_H).floor() as usize;
        let end = ((scroll + self.list.height) / ROW_H).ceil() as usize;
        first.min(row_count)..end.min(row_count)
    }

    fn thumb(&self, row_count: usize, scroll: f32) -> Option<Rect> {
        let max = self.max_scroll(row_count);
        if max <= 0. {
            return None;
        }
        let height = (self.list.height * (self.list.height / (row_count as f32 * ROW_H)))
            .max(12.)
            .min(self.list.height);
        Some(Rect {
            x: self.list.x + self.list.width + 3.,
            y: self.list.y + (self.list.height - height) * (scroll / max),
            width: 3.,
            height,
        })
    }
}

fn status(row: &NetworkRow<'_>) -> (Cow<'static, str>, Color) {
    let issue = match row.outcome {
        Some(NetworkOutcome::Failed) => Some(("Failed", ALERT)),
        Some(NetworkOutcome::Cancelled) => Some(("Cancelled", MUTED)),
        _ => None,
    };
    // XHR's zero sentinel is not an observed HTTP status.
    let code = row.http_status.filter(|value| (100..1000).contains(value));
    match (code, issue) {
        (Some(code), Some((label, color))) => (format!("{code} · {label}").into(), color),
        (Some(code), None) => (
            code.to_string().into(),
            if (400..600).contains(&code) {
                ALERT
            } else {
                TEXT
            },
        ),
        (None, Some((label, color))) => (label.into(), color),
        (None, None) if row.outcome == Some(NetworkOutcome::Pending) => ("Pending".into(), MUTED),
        (None, None) => (UNOBSERVED.into(), MUTED),
    }
}

fn elapsed(time: Option<Duration>) -> Cow<'static, str> {
    let Some(time) = time else {
        return UNOBSERVED.into();
    };
    if time.is_zero() {
        "0 ms".into()
    } else if time < Duration::from_micros(100) {
        "<0.1 ms".into()
    } else if time < Duration::from_secs(1) {
        format!("{:.1} ms", time.as_secs_f64() * 1000.).into()
    } else {
        format!("{:.2} s", time.as_secs_f64()).into()
    }
}

fn summary(discarded: u64, coverage_note: Option<&str>) -> String {
    let coverage = coverage_note
        .filter(|note| !note.is_empty())
        .unwrap_or("Partial capture");
    if discarded == 0 {
        format!("{coverage} · {UNOBSERVED} = not observed")
    } else {
        format!("{discarded} discarded · {coverage} · {UNOBSERVED} = not observed")
    }
}

fn empty_label(discarded: u64) -> &'static str {
    if discarded == 0 {
        "No requests observed."
    } else {
        "No request rows retained."
    }
}

fn intersection(rect: Rect, clip: Rect) -> Rect {
    let x = rect.x.max(clip.x);
    let y = rect.y.max(clip.y);
    Rect {
        x,
        y,
        width: ((rect.x + rect.width).min(clip.x + clip.width) - x).max(0.),
        height: ((rect.y + rect.height).min(clip.y + clip.height) - y).max(0.),
    }
}

/// Paint into the dock body. Layout uses logical pixels; `canvas_clip` is the
/// caller's current device-pixel clip, intersected during paint and restored after it.
pub(super) fn draw_network(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    scale: f32,
    canvas_clip: Rect,
    layout: &NetworkLayout,
    view: NetworkView<'_>,
) {
    if !scale.is_finite() || scale <= 0. {
        return;
    }
    let mut painter = Painter {
        canvas,
        fonts,
        scale,
        canvas_clip,
        clip: canvas_clip,
    };
    let panel_clip = intersection(painter.device(layout.bounds), canvas_clip);
    if panel_clip.width <= 0. || panel_clip.height <= 0. {
        return;
    }

    painter.set_clip(layout.bounds);
    painter.fill(layout.bounds, PANEL);
    painter.fill(layout.toolbar, HEADER);
    if let Some(clear) = layout.clear_rect() {
        painter.fill(clear, if view.clear_hovered { HOVER } else { LINE });
        painter.label(clear, "Clear", TEXT, true);
    }
    painter.label(
        layout.summary,
        &summary(view.discarded, view.coverage_note),
        if view.discarded > 0 { ALERT } else { MUTED },
        false,
    );

    painter.fill(layout.header, HEADER);
    painter.fill(
        Rect {
            height: layout.header.height.min(1.),
            ..layout.header
        },
        LINE,
    );
    for column in &layout.columns {
        painter.label(column.cell(layout.header), column.title, MUTED, true);
    }

    painter.set_clip(layout.list);
    if view.rows.is_empty() {
        painter.label(
            Rect {
                height: ROW_H,
                ..layout.list
            },
            empty_label(view.discarded),
            MUTED,
            false,
        );
    }

    let scroll = layout.clamp_scroll(view.rows.len(), view.scroll);
    for index in layout.visible_rows(view.rows.len(), scroll) {
        let row = &view.rows[index];
        let band = Rect {
            y: layout.list.y + (index as f32 * ROW_H - scroll),
            height: ROW_H,
            ..layout.list
        };
        if index % 2 == 1 {
            painter.fill(band, STRIPE);
        }
        let (status, tint) = status(row);
        let elapsed = elapsed(row.elapsed);
        let time_tint = if row.elapsed.is_some() { TEXT } else { MUTED };
        let labels = [
            optional_label(row.method),
            optional_label(row.display_url),
            optional_label(row.category),
            (status.as_ref(), tint),
            (elapsed.as_ref(), time_tint),
        ];
        for (column, (label, color)) in layout.columns.iter().zip(labels) {
            painter.label(column.cell(band), label, color, false);
        }
    }
    painter.set_clip(layout.list_rect());
    if let Some(thumb) = layout.thumb(view.rows.len(), scroll) {
        painter.fill(thumb, MUTED);
    }
    painter.canvas.clip(canvas_clip);
}

fn optional_label(value: Option<&str>) -> (&str, Color) {
    value
        .filter(|text| !text.is_empty())
        .map_or((UNOBSERVED, MUTED), |text| (text, TEXT))
}

struct Painter<'a> {
    canvas: &'a mut Canvas,
    fonts: &'a mut Fonts,
    scale: f32,
    canvas_clip: Rect,
    clip: Rect,
}

impl Painter<'_> {
    fn device(&self, rect: Rect) -> Rect {
        Rect {
            x: rect.x * self.scale,
            y: rect.y * self.scale,
            width: rect.width * self.scale,
            height: rect.height * self.scale,
        }
    }

    fn set_clip(&mut self, clip: Rect) {
        self.clip = intersection(self.device(clip), self.canvas_clip);
        self.canvas.clip(self.clip);
    }

    fn fill(&mut self, rect: Rect, color: Color) {
        self.canvas.fill_rect(self.device(rect), color);
    }

    fn label(&mut self, rect: Rect, text: &str, color: Color, bold: bool) {
        let width = (rect.width - PAD * 2.) * self.scale;
        let size = FONT_SIZE * self.scale;
        let clip = intersection(self.device(rect), self.clip);
        if width <= 0. || clip.width <= 0. || clip.height <= 0. {
            return;
        }
        let label = fit_label(text, width, |s| {
            self.fonts.shape(s, size, bold, false, false).advance
        });
        if label.is_empty() {
            return;
        }
        let x = (rect.x + PAD) * self.scale;
        let baseline = (rect.y + (rect.height + FONT_SIZE) / 2. - 2.) * self.scale;
        self.canvas.clip(clip);
        self.canvas
            .label(&label, x, baseline, size, color, bold, self.fonts);
        self.canvas.clip(self.clip);
    }
}

fn fit_label(text: &str, width: f32, mut measure: impl FnMut(&str) -> f32) -> String {
    // Bound shaping work and keep page-supplied fields on a single inert line.
    let mut chars = text.chars();
    let mut label: String = chars
        .by_ref()
        .take(MAX_LABEL_CHARS)
        .map(|c| match c {
            '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => '�',
            c if c.is_control() => '�',
            c => c,
        })
        .collect();
    if chars.next().is_none() && measure(&label) <= width {
        return label;
    }
    if measure("…") > width {
        return String::new();
    }

    // Find a measured prefix, never cutting a UTF-8 character.
    let cuts: Vec<_> = label
        .char_indices()
        .map(|(index, _)| index)
        .chain([label.len()])
        .collect();
    let mut candidate = String::new();
    let (mut low, mut high) = (0, cuts.len());
    while high - low > 1 {
        let mid = low + (high - low) / 2;
        candidate.clear();
        candidate.push_str(&label[..cuts[mid]]);
        candidate.push('…');
        if measure(&candidate) <= width {
            low = mid;
        } else {
            high = mid;
        }
    }
    label.truncate(cuts[low]);
    label.push('…');
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(width: f32, height: f32) -> NetworkLayout {
        NetworkLayout::new(Rect {
            x: 23.,
            y: 127.,
            width,
            height,
        })
    }

    fn near(left: f32, right: f32) {
        assert!((left - right).abs() < 0.001, "{left} != {right}");
    }

    #[test]
    fn columns_align_with_rows_and_url_takes_extra_width() {
        let narrow = layout(320., 180.);
        let regular = layout(640., 180.);
        let wide = layout(960., 180.);
        for grid in [&narrow, &regular, &wide] {
            let mut x = grid.list.x;
            for column in &grid.columns {
                near(x, column.x);
                assert!(column.width > 0.);
                near(column.cell(grid.header).x, column.cell(grid.list).x);
                x += column.width;
            }
            near(x, grid.list.x + grid.list.width);
        }
        for index in [0, 2, 3, 4] {
            near(regular.columns[index].width, wide.columns[index].width);
        }
        near(wide.columns[1].width - regular.columns[1].width, 320.);
    }

    #[test]
    fn clear_hit_rect_stays_in_toolbar_and_out_of_list() {
        let grid = layout(320., 180.);
        let clear = grid.clear_rect().unwrap();
        assert!(grid.toolbar.contains(clear.x, clear.y));
        assert!(clear.contains(clear.x + 2., clear.y + 2.));
        assert!(!clear.contains(clear.x + clear.width, clear.y));
        assert!(clear.x + clear.width <= grid.toolbar.x + grid.toolbar.width);
        assert!(grid.summary.x + grid.summary.width < clear.x);
        assert!(!grid.list_rect().contains(clear.x, clear.y));
        let list = grid.list_rect();
        assert!(list.contains(list.x + list.width - 1., list.y));
        assert!(layout(30., 180.).clear_rect().is_none());
        assert!(layout(320., 10.).clear_rect().is_none());
    }

    #[test]
    fn scrolling_clamps_and_shrink_does_not_leave_a_stale_offset() {
        let grid = layout(320., TOOLBAR_H + HEADER_H + 2. * ROW_H);
        assert_eq!(grid.clamp_scroll(5, f32::INFINITY), 3. * ROW_H);
        assert_eq!(grid.clamp_scroll(5, f32::NEG_INFINITY), 0.);
        assert_eq!(grid.clamp_scroll(5, f32::NAN), 0.);
        assert_eq!(grid.scroll_by(5, 2. * ROW_H, 99.), 3. * ROW_H);
        assert_eq!(grid.scroll_by(5, ROW_H, f32::NAN), ROW_H);
        assert_eq!(grid.scroll_by(3, 99., -ROW_H), 0.);
        assert_eq!(grid.clamp_scroll(0, 99.), 0.);
        assert_eq!(grid.clamp_scroll(2, 99.), 0.);
    }

    #[test]
    fn visible_rows_include_partial_edges_only_when_visible() {
        let grid = layout(320., TOOLBAR_H + HEADER_H + 2. * ROW_H);
        assert_eq!(grid.visible_rows(5, 0.), 0..2);
        assert_eq!(grid.visible_rows(5, 1.), 0..3);
        assert_eq!(grid.visible_rows(5, ROW_H), 1..3);
        assert_eq!(grid.visible_rows(5, grid.clamp_scroll(5, 99.)), 3..5);
        assert_eq!(grid.visible_rows(1, 0.), 0..1);
        let short = layout(320., TOOLBAR_H + HEADER_H);
        assert_eq!(short.visible_rows(5, 0.), 0..0);
        assert_eq!(short.clamp_scroll(5, 99.), 0.);
    }

    #[test]
    fn scroll_thumb_fits_track_and_reaches_both_ends() {
        let grid = layout(320., 180.);
        assert!(grid.thumb(1, 0.).is_none());
        let top = grid.thumb(100, 0.).unwrap();
        let bottom = grid.thumb(100, grid.clamp_scroll(100, 10_000.)).unwrap();
        near(top.y, grid.list.y);
        near(bottom.y + bottom.height, grid.list.y + grid.list.height);
        assert!(top.x > grid.list.x + grid.list.width);
        assert!(top.x + top.width < grid.bounds.x + grid.bounds.width);
    }

    #[test]
    fn labels_distinguish_unobserved_headers_and_explicit_outcomes() {
        let mut row = NetworkRow {
            method: None,
            display_url: None,
            category: None,
            http_status: None,
            outcome: None,
            elapsed: None,
        };
        assert_eq!(status(&row).0, UNOBSERVED);
        row.http_status = Some(0);
        assert_eq!(status(&row).0, UNOBSERVED);
        row.outcome = Some(NetworkOutcome::Pending);
        assert_eq!(status(&row).0, "Pending");
        row.http_status = Some(404);
        assert_eq!(status(&row).0, "404");
        row.outcome = Some(NetworkOutcome::Failed);
        assert_eq!(status(&row).0, "404 · Failed");
        row.http_status = None;
        row.outcome = Some(NetworkOutcome::Cancelled);
        assert_eq!(status(&row).0, "Cancelled");
        assert_eq!(optional_label(row.method).0, UNOBSERVED);
        assert_eq!(optional_label(row.display_url).0, UNOBSERVED);
        assert_eq!(optional_label(row.category).0, UNOBSERVED);
    }

    #[test]
    fn time_and_overflow_do_not_supply_missing_measurements() {
        assert_eq!(elapsed(None), UNOBSERVED);
        assert_eq!(elapsed(Some(Duration::ZERO)), "0 ms");
        assert_eq!(elapsed(Some(Duration::from_nanos(1))), "<0.1 ms");
        assert_eq!(elapsed(Some(Duration::from_millis(23))), "23.0 ms");
        assert_eq!(elapsed(Some(Duration::from_millis(1210))), "1.21 s");
        assert_eq!(empty_label(0), "No requests observed.");
        assert_eq!(empty_label(8), "No request rows retained.");
        assert_eq!(summary(0, None), "Partial capture · — = not observed");
        assert_eq!(
            summary(8, Some("Navigation only")),
            "8 discarded · Navigation only · — = not observed"
        );
    }

    #[test]
    fn elision_is_measured_bounded_and_never_splits_unicode() {
        let measure = |s: &str| s.chars().count() as f32;
        assert_eq!(fit_label("abçδ", 3., measure), "ab…");
        assert_eq!(fit_label("abçδ", 4., measure), "abçδ");
        assert_eq!(fit_label("abçδ", 0., measure), "");
        assert_eq!(fit_label("a\n\u{202e}b", 4., measure), "a��b");
        let long = "u".repeat(MAX_LABEL_CHARS + 5);
        let label = fit_label(&long, 1_000., measure);
        assert_eq!(label.chars().count(), MAX_LABEL_CHARS + 1);
        assert!(label.ends_with('…'));
    }
}
