//! Native downloads flyout. App feeds it real transfer events and owns their
//! effects; this module never opens a file, starts a request, or writes to disk.

mod row;

use super::{elide, rounded_chrome_rect, scaled_rect};
use row::{Action as RowAction, Event, Failure, Row, SavedFileActions};
use rust_browser_engine::{Fonts, Rect, css::Color, paint::Canvas};
use std::{borrow::Cow, collections::VecDeque};

const WIDTH: f32 = 352.;
const MARGIN: f32 = 8.;
const INSET: f32 = 7.;
const HEADER: f32 = 45.;
const ROW: f32 = 52.;
const EMPTY: f32 = 26.;
const DIVIDER: f32 = 11.;
const FOOTER: f32 = 35.;
const FRAME: f32 = HEADER + DIVIDER + FOOTER + INSET;
const RECENT: usize = 4;
const RETAINED: usize = 256;

pub(super) enum Start {
    Transfer,
    Cancelled,
    DestinationError,
}

fn caption(entry: &Row) -> String {
    let view = entry.view();
    if let Some(bytes) = view.saved_bytes {
        format!("{} · Saved", byte_count(bytes))
    } else if let Some(bytes) = view
        .progress
        .map(|progress| progress.written)
        .filter(|&n| n > 0)
    {
        format!("{} · {}", byte_count(bytes), view.status)
    } else if let Some(detail) = view.detail {
        format!("{} · {detail}", view.status)
    } else {
        view.status.into()
    }
}

fn byte_count(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut amount = bytes as f64 / 1024.;
    for unit in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        if amount < 1024. || unit == "EiB" {
            return format!("{amount:.1} {unit}");
        }
        amount /= 1024.;
    }
    unreachable!()
}

/// Window-session data only. Names are basenames, not source URLs or paths.
#[derive(Default)]
pub(super) struct Downloads {
    entries: VecDeque<Row>,
}

impl Downloads {
    /// Use the same unique ID passed to the download worker. An offer rejected
    /// by the destination dialog can also be recorded directly as a terminal
    /// event using that offer's suggested basename.
    pub(super) fn record(&mut self, id: u64, basename: &str, state: Start) {
        if self.entries.iter().any(|entry| entry.id() == id) {
            return;
        }
        let name = basename
            .chars()
            .filter(|ch| {
                !ch.is_control() && !matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
            .take(255)
            .collect::<String>();
        let name = if name.is_empty() {
            "Download".into()
        } else {
            name
        };
        let mut row = Row::new(id, name, None);
        match state {
            Start::Transfer => {}
            Start::Cancelled => row.apply(id, Event::Canceled),
            Start::DestinationError => row.apply(id, Event::Failed(Failure::Save)),
        }
        self.entries.push_front(row);
        self.trim();
    }

    fn trim(&mut self) {
        while self.entries.len() > RETAINED {
            let Some(index) = self.entries.iter().rposition(|entry| !entry.is_active()) else {
                break;
            };
            self.entries.remove(index);
        }
    }

    pub(super) fn has_active(&self) -> bool {
        self.entries.iter().any(Row::is_active)
    }

    pub(super) fn progress(&mut self, id: u64, bytes: u64) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id() == id) {
            entry.apply(id, Event::BytesWritten(bytes));
        }
    }

    /// Only call after matching the ID against a live transfer in App; the
    /// caller still signals the existing worker's cancellation token itself.
    pub(super) fn request_cancel(&mut self, id: u64) -> bool {
        matches!(
            self.entries.iter_mut().find(|entry| entry.id() == id)
                .and_then(|entry| entry.activate(RowAction::Cancel)),
            Some(row::Intent { id: target, action: RowAction::Cancel }) if target == id
        )
    }

    pub(super) fn finish(&mut self, id: u64, result: &Result<u64, String>) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id() == id) {
            // A cancellation request can lose the race with publishing a
            // saved file. Only the worker's result determines the outcome.
            let event = match result {
                Ok(bytes) => Event::Saved(*bytes, SavedFileActions::None),
                Err(error) if error == "Download cancelled" => Event::Canceled,
                Err(_) => Event::Failed(Failure::Save),
            };
            entry.apply(id, event);
            self.trim();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Dismiss,
    Cancel(u64),
    ShowAll,
    ShowRecent,
}

#[derive(Default)]
pub(super) struct Panel {
    all: bool,
    first: usize,
    focused: Option<Action>,
}

impl Panel {
    /// `window` and the toolbar `anchor` are in logical window pixels.
    pub(super) fn layout<'a>(
        &self,
        downloads: &'a Downloads,
        window: Rect,
        anchor: Rect,
    ) -> Option<Layout<'a>> {
        let width = (window.width - 18.).min(WIDTH);
        let height_limit = window.height - 20.;
        if width < 70. || height_limit < FRAME + EMPTY {
            return None;
        }
        let count = downloads.entries.len();
        let wanted = if self.all { count } else { count.min(RECENT) };
        let visible = wanted.min(((height_limit - FRAME) / ROW).floor() as usize);
        let max_first = wanted.saturating_sub(visible);
        let first = self.first.min(max_first);
        let height = FRAME
            + if count == 0 {
                EMPTY
            } else {
                visible as f32 * ROW
            };
        let right = window.x + window.width;
        let bottom = window.y + window.height;
        let x = (anchor.x + anchor.width - width).clamp(window.x + MARGIN, right - width - MARGIN);
        let below = anchor.y + anchor.height + 5.;
        let above = anchor.y - height - 6.;
        let y = if below + height > bottom - MARGIN && above >= window.y + MARGIN {
            above
        } else {
            below.clamp(window.y + MARGIN, bottom - height - MARGIN)
        };
        Some(Layout {
            bounds: rect(x, y, width, height),
            downloads,
            first,
            visible,
            max_first,
            footer: if self.all {
                Action::ShowRecent
            } else {
                Action::ShowAll
            },
        })
    }

    /// A native all-files view is available without navigating to a placeholder
    /// page. Shells with a dedicated Downloads page can route ShowAll there.
    pub(super) fn show_all(&mut self, all: bool) {
        self.all = all;
        self.first = 0;
        self.focused = None;
    }

    /// Scroll by whole rows; no partially drawn row has a clickable action.
    pub(super) fn scroll_rows(&mut self, layout: &Layout<'_>, rows: isize) {
        let next = layout
            .first
            .saturating_add_signed(rows)
            .min(layout.max_first);
        if next != layout.first {
            self.first = next;
            self.focused = None;
        }
    }

    pub(super) fn move_focus(&mut self, layout: &Layout<'_>, previous: bool) {
        let actions = layout
            .controls()
            .map(|(_, action)| action)
            .collect::<Vec<_>>();
        let current = self
            .focused(layout)
            .and_then(|action| actions.iter().position(|a| *a == action));
        let next = match (previous, current) {
            (true, Some(index)) => (index + actions.len() - 1) % actions.len(),
            (false, Some(index)) => (index + 1) % actions.len(),
            (true, None) => actions.len() - 1,
            (false, None) => 0,
        };
        self.focused = Some(actions[next]);
    }

    pub(super) fn focused(&self, layout: &Layout<'_>) -> Option<Action> {
        self.focused.map(|focused| {
            if layout.controls().any(|(_, action)| action == focused) {
                focused
            } else {
                Action::Dismiss
            }
        })
    }
}

pub(super) struct Layout<'a> {
    pub(super) bounds: Rect,
    downloads: &'a Downloads,
    first: usize,
    visible: usize,
    max_first: usize,
    footer: Action,
}

impl Layout<'_> {
    fn rows(&self) -> impl Iterator<Item = (Rect, &Row)> + '_ {
        self.downloads
            .entries
            .range(self.first..self.first + self.visible)
            .enumerate()
            .map(|(index, entry)| {
                let r = self.bounds;
                (
                    rect(
                        r.x + INSET,
                        r.y + HEADER + index as f32 * ROW,
                        r.width - 2. * INSET,
                        ROW,
                    ),
                    entry,
                )
            })
    }

    fn close_button(&self) -> Rect {
        rect(
            self.bounds.x + self.bounds.width - 40.,
            self.bounds.y + 14.,
            26.,
            26.,
        )
    }

    fn footer(&self) -> Rect {
        let r = self.bounds;
        rect(
            r.x + INSET,
            r.y + r.height - FOOTER - INSET,
            r.width - 2. * INSET,
            FOOTER,
        )
    }

    fn cancel_button(row: Rect) -> Rect {
        rect(row.x + row.width - 41., row.y + (ROW - 31.) / 2., 31., 31.)
    }

    /// App can capture these painted targets for pointer dispatch. If another
    /// download inserts a row before the next frame, the target is still the
    /// ID shown when the user clicked, not the new item at that position.
    pub(super) fn controls(&self) -> impl Iterator<Item = (Rect, Action)> + '_ {
        std::iter::once((self.close_button(), Action::Dismiss))
            .chain(self.rows().filter_map(|(row, entry)| {
                entry
                    .view()
                    .actions
                    .contains(&RowAction::Cancel)
                    .then(|| (Self::cancel_button(row), Action::Cancel(entry.id())))
            }))
            .chain(std::iter::once((self.footer(), self.footer)))
    }

    pub(super) fn hit(&self, x: f32, y: f32) -> Option<Action> {
        self.controls()
            .find_map(|(bounds, action)| bounds.contains(x, y).then_some(action))
    }
}

pub(super) struct Colors {
    pub(super) card: Color,
    pub(super) line: Color,
    pub(super) text: Color,
    pub(super) muted: Color,
    pub(super) icon: Color,
    pub(super) hover: Color,
    pub(super) accent: Color,
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn inset(r: Rect, by: f32) -> Rect {
    rect(r.x + by, r.y + by, r.width - 2. * by, r.height - 2. * by)
}

fn physical(r: Rect, scale: f32) -> Rect {
    scaled_rect(scale, r.x, r.y, r.width, r.height)
}

enum Glyph {
    File,
    Close,
    External,
}

impl Glyph {
    fn draw(self, canvas: &mut Canvas, center: (f32, f32), scale: f32, color: Color) {
        // Reference icons use a 24-unit viewBox, painted at 17 DIPs.
        let paths: &[&[(f32, f32)]] = match self {
            Self::File => &[
                &[
                    (14., 2.),
                    (6., 2.),
                    (4., 4.),
                    (4., 20.),
                    (6., 22.),
                    (18., 22.),
                    (20., 20.),
                    (20., 8.),
                    (14., 2.),
                    (14., 8.),
                    (20., 8.),
                ],
                &[(8., 13.), (16., 13.)],
                &[(8., 17.), (16., 17.)],
            ],
            Self::Close => &[&[(6., 6.), (18., 18.)], &[(18., 6.), (6., 18.)]],
            Self::External => &[&[(7., 17.), (17., 7.)], &[(8., 7.), (17., 7.), (17., 16.)]],
        };
        let k = 17. / 24.;
        let x = |v: f32| (center.0 + (v - 12.) * k) * scale;
        let y = |v: f32| (center.1 + (v - 12.) * k) * scale;
        for path in paths {
            for points in path.windows(2) {
                let [(x1, y1), (x2, y2)] = [points[0], points[1]];
                canvas.stroke_line(x(x1), y(y1), x(x2), y(y2), 1.65 * k * scale, color);
            }
        }
    }
}

pub(super) fn draw(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    layout: &Layout<'_>,
    colors: &Colors,
    scale: f32,
    pointer: Option<(f32, f32)>,
    focused: Option<Action>,
) {
    let r = layout.bounds;
    for (spread, down, alpha) in [(16., 9., 14), (9., 6., 24), (3., 3., 42)] {
        let shadow = rect(
            r.x - spread,
            r.y - spread + down,
            r.width + 2. * spread,
            r.height + 2. * spread,
        );
        rounded_chrome_rect(
            canvas,
            physical(shadow, scale),
            (10. + spread) * scale,
            Color(0, 0, 0, alpha),
        );
    }
    rounded_chrome_rect(canvas, physical(r, scale), 10. * scale, colors.line);
    rounded_chrome_rect(
        canvas,
        physical(inset(r, 1.), scale),
        9. * scale,
        colors.card,
    );

    let hovered = pointer.and_then(|(x, y)| layout.hit(x, y));
    for (bounds, action) in layout.controls() {
        if focused == Some(action) {
            rounded_chrome_rect(canvas, physical(bounds, scale), 6. * scale, colors.accent);
            rounded_chrome_rect(
                canvas,
                physical(inset(bounds, 2.), scale),
                4. * scale,
                colors.hover,
            );
        } else if hovered == Some(action) {
            rounded_chrome_rect(canvas, physical(bounds, scale), 6. * scale, colors.hover);
        }
        if matches!(action, Action::Dismiss | Action::Cancel(_)) {
            Glyph::Close.draw(
                canvas,
                (bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.),
                scale,
                colors.icon,
            );
        }
    }

    let close = layout.close_button();
    label(
        canvas,
        fonts,
        "Downloads",
        r.x + 14.,
        close.y + 17.,
        close.x - r.x - 19.,
        13.,
        colors.text,
        true,
        scale,
    );
    if layout.downloads.entries.is_empty() {
        label(
            canvas,
            fonts,
            "No downloads yet.",
            r.x + 15.,
            r.y + HEADER + 13.,
            r.width - 30.,
            12.,
            colors.muted,
            false,
            scale,
        );
    }
    for (row, entry) in layout.rows() {
        Glyph::File.draw(canvas, (row.x + 18.5, row.y + ROW / 2.), scale, colors.text);
        let x = row.x + 37.;
        let view = entry.view();
        let action_width = if view.actions.contains(&RowAction::Cancel) {
            41.
        } else {
            0.
        };
        let width = row.x + row.width - 10. - action_width - x;
        label(
            canvas,
            fonts,
            view.name,
            x,
            row.y + 23.,
            width,
            13.,
            colors.text,
            true,
            scale,
        );
        label(
            canvas,
            fonts,
            &caption(entry),
            x,
            row.y + 37.,
            width,
            11.,
            colors.muted,
            false,
            scale,
        );
        let line = rect(row.x, row.y + ROW - 1., row.width, 1.);
        canvas.fill_rect(physical(line, scale), colors.line);
    }
    if layout.max_first > 0 && layout.visible > 0 {
        let track = layout.visible as f32 * ROW;
        let thumb =
            (track * layout.visible as f32 / (layout.visible + layout.max_first) as f32).max(12.);
        let y = r.y + HEADER + (track - thumb) * layout.first as f32 / layout.max_first as f32;
        rounded_chrome_rect(
            canvas,
            physical(rect(r.x + r.width - 5., y, 2., thumb), scale),
            scale,
            colors.muted,
        );
    }

    let footer = layout.footer();
    let divider = rect(footer.x + 3., footer.y - 6., footer.width - 6., 1.);
    canvas.fill_rect(physical(divider, scale), colors.line);
    Glyph::External.draw(
        canvas,
        (footer.x + 16.5, footer.y + FOOTER / 2.),
        scale,
        colors.muted,
    );
    let text = if layout.footer == Action::ShowAll {
        "Show all files"
    } else {
        "Show recent files"
    };
    label(
        canvas,
        fonts,
        text,
        footer.x + 35.,
        footer.y + 22.,
        footer.width - 43.,
        13.,
        colors.text,
        false,
        scale,
    );
}

#[allow(clippy::too_many_arguments)]
fn label(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    text: &str,
    x: f32,
    baseline: f32,
    width: f32,
    size: f32,
    color: Color,
    bold: bool,
    scale: f32,
) {
    if width < size {
        return;
    }
    let text = if bold {
        fit_bold(fonts, text, width * scale, size * scale)
    } else {
        Cow::Owned(elide(fonts, text, width * scale, size * scale))
    };
    canvas.label(
        &text,
        x * scale,
        baseline * scale,
        size * scale,
        color,
        bold,
        fonts,
    );
}

fn fit_bold<'a>(fonts: &Fonts, name: &'a str, width: f32, size: f32) -> Cow<'a, str> {
    let fits = |candidate: &str| fonts.shape(candidate, size, true, false, false).advance <= width;
    if fits(name) {
        return Cow::Borrowed(name);
    }
    // Retain a short extension when it fits. Never let bold filename text
    // occupy the separate cancel hit target.
    let (stem, tail) = name
        .rsplit_once('.')
        .filter(|(stem, suffix)| !stem.is_empty() && !suffix.is_empty() && suffix.len() <= 16)
        .map(|(stem, suffix)| (stem, format!("….{suffix}")))
        .filter(|(_, tail)| fits(tail))
        .unwrap_or_else(|| (name, "…".to_owned()));
    if !fits(&tail) {
        return Cow::Borrowed("");
    }
    let mut head = stem.chars().take(80).collect::<String>();
    loop {
        let candidate = format!("{head}{tail}");
        if fits(&candidate) || head.pop().is_none() {
            return Cow::Owned(candidate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Rect {
        rect(0., 0., 640., 400.)
    }

    fn toolbar() -> Rect {
        rect(570., 72., 31., 31.)
    }

    fn center(bounds: Rect) -> (f32, f32) {
        (bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.)
    }

    fn hit(layout: &Layout<'_>, bounds: Rect) -> Option<Action> {
        let (x, y) = center(bounds);
        layout.hit(x, y)
    }

    #[test]
    fn empty_panel_is_anchored_and_offers_only_close_and_all_files() {
        let downloads = Downloads::default();
        let panel = Panel::default();
        let layout = panel.layout(&downloads, viewport(), toolbar()).unwrap();
        assert_eq!(layout.bounds.x + layout.bounds.width, 601.);
        assert_eq!(layout.bounds.y, 108.);
        assert_eq!(layout.bounds.width, WIDTH);
        assert_eq!(layout.rows().count(), 0);
        assert_eq!(hit(&layout, layout.close_button()), Some(Action::Dismiss));
        assert_eq!(hit(&layout, layout.footer()), Some(Action::ShowAll));
        assert_eq!(layout.controls().count(), 2);
    }

    #[test]
    fn real_states_are_monotonic_and_success_can_win_cancellation() {
        let mut downloads = Downloads::default();
        downloads.progress(12, 100);
        assert!(!downloads.has_active());
        downloads.record(1, "first.txt", Start::Transfer);
        downloads.record(2, "second.txt", Start::Transfer);
        downloads.progress(1, 2048);
        downloads.progress(1, 256);
        assert_eq!(caption(&downloads.entries[1]), "2.0 KiB · Downloading…");
        assert!(downloads.request_cancel(1));
        downloads.progress(1, 4096);
        assert_eq!(caption(&downloads.entries[1]), "Canceling…");
        downloads.finish(1, &Ok(5000));
        downloads.finish(1, &Err("Download cancelled".into()));
        assert_eq!(downloads.entries[1].view().saved_bytes, Some(5000));
        assert!(downloads.request_cancel(2));
        downloads.finish(2, &Err("Download cancelled".into()));
        assert_eq!(caption(&downloads.entries[0]), "Canceled");
        assert!(!downloads.has_active());
    }

    #[test]
    fn only_running_rows_have_actions_and_focus_uses_a_stable_transfer_id() {
        let mut downloads = Downloads::default();
        downloads.record(1, "notes.txt", Start::Transfer);
        downloads.finish(1, &Ok(100));
        downloads.record(2, "paper.pdf", Start::Transfer);
        downloads.record(3, "archive.zip", Start::DestinationError);
        let mut panel = Panel::default();
        let layout = panel.layout(&downloads, viewport(), toolbar()).unwrap();
        let rows: Vec<_> = layout.rows().collect();
        assert_eq!(hit(&layout, Layout::cancel_button(rows[0].0)), None);
        assert_eq!(
            hit(&layout, Layout::cancel_button(rows[1].0)),
            Some(Action::Cancel(2))
        );
        assert_eq!(hit(&layout, Layout::cancel_button(rows[2].0)), None);
        panel.move_focus(&layout, false);
        panel.move_focus(&layout, false);
        assert_eq!(panel.focused(&layout), Some(Action::Cancel(2)));
        downloads.record(4, "another.txt", Start::Transfer);
        let layout = panel.layout(&downloads, viewport(), toolbar()).unwrap();
        assert_eq!(panel.focused(&layout), Some(Action::Cancel(2)));
        downloads.request_cancel(2);
        let layout = panel.layout(&downloads, viewport(), toolbar()).unwrap();
        assert_eq!(panel.focused(&layout), Some(Action::Dismiss));
    }

    #[test]
    fn recent_is_capped_at_four_and_all_can_reach_older_transfers() {
        let mut downloads = Downloads::default();
        for id in 1..=9 {
            downloads.record(id, "sample.txt", Start::Cancelled);
        }
        let mut panel = Panel::default();
        let layout = panel.layout(&downloads, viewport(), toolbar()).unwrap();
        assert_eq!(
            layout.rows().map(|(_, e)| e.id()).collect::<Vec<_>>(),
            [9, 8, 7, 6]
        );
        assert_eq!(layout.bounds.height, FRAME + 4. * ROW);
        assert!(layout.bounds.y + layout.bounds.height <= viewport().height - MARGIN);
        panel.show_all(true);
        let layout = panel.layout(&downloads, viewport(), toolbar()).unwrap();
        assert_eq!(hit(&layout, layout.footer()), Some(Action::ShowRecent));
        panel.scroll_rows(&layout, isize::MAX);
        let layout = panel.layout(&downloads, viewport(), toolbar()).unwrap();
        assert_eq!(layout.rows().last().unwrap().1.id(), 1);
        panel.show_all(false);
        let layout = panel.layout(&downloads, viewport(), toolbar()).unwrap();
        assert_eq!(layout.rows().next().unwrap().1.id(), 9);
    }

    #[test]
    fn retention_does_not_evict_a_running_transfer() {
        let mut downloads = Downloads::default();
        downloads.record(1, "first.txt", Start::Transfer);
        for id in 2..=RETAINED as u64 + 5 {
            downloads.record(id, "sample.txt", Start::Transfer);
            downloads.finish(id, &Ok(10));
        }
        assert_eq!(downloads.entries.len(), RETAINED);
        assert_eq!(downloads.entries.back().unwrap().id(), 1);
        downloads.finish(1, &Err("I/O error".into()));
        assert_eq!(downloads.entries.back().unwrap().view().status, "Failed");
    }
}
