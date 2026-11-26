//! Quiet Cinder's single-row capsule tab strip. All rectangles here are logical
//! pixels relative to the tab strip's top-left, before window scale or placement.
//!
//! Paint the uncut `TabSlot::face` under the `TabStrip::list` clip; use
//! `TabSlot::visible_face` for select and `CloseButton::hit` for close (close
//! takes priority). Row margins and inter-tab gaps are not tab targets. The
//! window's existing resize and native titlebar-drag rules still apply here.
//! Keep one scroll offset in the caller, clamp it with `scroll_to` when laying
//! out a frame, and call `reveal` on activation/tab mutations, not on every
//! draw: otherwise a wheel or drag scroll away from the active tab is undone.
//! Only use tab hits from the last successfully presented, current layout.

use rust_browser_engine::layout::Rect;

pub(super) const TAB_ROW_HEIGHT: f32 = 44.;
const FACE_HEIGHT: f32 = 31.;
pub(super) const TAB_RADIUS: f32 = FACE_HEIGHT / 2.;
pub(super) const TAB_FONT_SIZE: f32 = 13.;
pub(super) const CONTROL_RADIUS: f32 = 6.;
pub(super) const NARROW_WIDTH: f32 = 660.;

const ROW_BORDER: f32 = 1.;
const FACE_Y: f32 = (TAB_ROW_HEIGHT - ROW_BORDER - FACE_HEIGHT) / 2.;
const TAB_GAP: f32 = 3.;
const CONTROL_GAP: f32 = 4.;
const CONTROL_SIZE: f32 = 31.;
// CSS .tab-list max-width: calc(100% - 76px); this leaves another six
// pixels after the two 31px controls and their 4px gaps on overflow.
const CONTROL_RESERVE: f32 = 76.;
const CLOSE_WIDTH: f32 = 21.;
const CLOSE_HEIGHT: f32 = 23.;
const END_INSET: f32 = 8.; // 7px padding + 1px border
const TITLE_GAP: f32 = 8.;
const FAVICON_INSET: f32 = 12.; // 11px padding + 1px border
const FAVICON_SIZE: f32 = 19.;

/// Match the regular and <= 660px titlebar layouts.
pub(super) fn caption_button_width(width: f32) -> f32 {
    if width <= NARROW_WIDTH { 35. } else { 42. }
}

pub(super) struct TabStrip {
    pub(super) list: Rect,
    pub(super) new_tab: Option<StripButton>,
    pub(super) tab_list: Option<StripButton>,
    count: usize,
    tab_width: f32,
    tabs_width: f32,
    scroll: f32,
}

/// The controls after the list do not scroll. `bounds` is also the hit target.
pub(super) struct StripButton {
    pub(super) bounds: Rect,
    pub(super) glyph: Rect,
}

pub(super) struct TabSlot {
    pub(super) index: usize,
    /// Unclipped geometry, also the source of tab centers for drag reorder.
    pub(super) face: Rect,
    pub(super) visible_face: Rect,
    pub(super) favicon: Rect,
    pub(super) title: Rect,
    pub(super) close: Option<CloseButton>,
}

pub(super) struct CloseButton {
    pub(super) hit: Rect,
    pub(super) glyph: Rect,
    /// Paint the hover circle under the same list clip as the tab.
    pub(super) hover_circle: Rect,
}

impl TabStrip {
    /// `caption_width` is the total width of the native caption controls,
    /// or zero without them. Left/right padding is part of the reference; a
    /// wider native caption reservation can only make the list shorter.
    pub(super) fn new(width: f32, caption_width: f32, count: usize) -> Self {
        let (left, right, caption_clearance): (f32, f32, f32) = if width <= NARROW_WIDTH {
            (48., 109., 4.)
        } else {
            (107., 134., 8.)
        };
        // Quiet/Capsule has no active width bonus and no compact-variant squeeze.
        let tab_width = (width * 15. / 100.).clamp(106., 216.);
        let tabs_width = count as f32 * tab_width + count.saturating_sub(1) as f32 * TAB_GAP;
        let content_width = (width - left - right.max(caption_width + caption_clearance)).max(0.);
        let list_width = tabs_width.min((content_width - CONTROL_RESERVE).max(0.));
        let control_x = left + list_width + CONTROL_GAP;
        let new_tab =
            (content_width >= CONTROL_GAP + CONTROL_SIZE).then(|| strip_button(control_x));
        let tab_list = (content_width >= 2. * (CONTROL_GAP + CONTROL_SIZE))
            .then(|| strip_button(control_x + CONTROL_SIZE + CONTROL_GAP));
        Self {
            list: rect(left, FACE_Y, list_width, FACE_HEIGHT),
            new_tab,
            tab_list,
            count,
            tab_width,
            tabs_width,
            scroll: 0.,
        }
    }

    pub(super) fn max_scroll(&self) -> f32 {
        (self.tabs_width - self.list.width).max(0.)
    }

    pub(super) fn scroll(&self) -> f32 {
        self.scroll
    }

    pub(super) fn scroll_to(mut self, offset: f32) -> Self {
        self.scroll = offset.max(0.).min(self.max_scroll());
        self
    }

    /// Match scrollIntoView(inline: "nearest") without moving the controls,
    /// including the case where a single tab is wider than the list.
    pub(super) fn reveal(mut self, index: usize) -> Self {
        let list_width = self.list.width;
        if index < self.count && list_width > 0. {
            let start = index as f32 * self.stride();
            let end = start + self.tab_width;
            let view_start = self.scroll;
            let view_end = view_start + list_width;
            if !(start < view_start && end > view_end) {
                let fits = self.tab_width <= list_width;
                let align_start = if fits {
                    start < view_start
                } else {
                    end > view_end
                };
                let align_end = if fits {
                    end > view_end
                } else {
                    start < view_start
                };
                if align_start {
                    self = self.scroll_to(start);
                } else if align_end {
                    self = self.scroll_to(end - list_width);
                }
            }
        }
        self
    }

    pub(super) fn visible_tabs(&self) -> impl Iterator<Item = TabSlot> + '_ {
        let first = (self.scroll / self.stride()).floor() as usize;
        let end = ((self.scroll + self.list.width) / self.stride()).ceil() as usize;
        (first..end.min(self.count)).filter_map(|index| self.tab(index))
    }

    pub(super) fn tab(&self, index: usize) -> Option<TabSlot> {
        if index >= self.count {
            return None;
        }
        let x = self.list.x + (index as f32 * self.stride() - self.scroll);
        let face = rect(x, FACE_Y, self.tab_width, FACE_HEIGHT);
        let visible_face = clip_to_list(face, self.list)?;
        let close_bounds = centered_y(
            right(face) - END_INSET - CLOSE_WIDTH,
            CLOSE_WIDTH,
            CLOSE_HEIGHT,
        );
        let close_glyph = centered_square(close_bounds, 10.);
        // A clipped sliver of a close button with no clear X must not be an
        // invisible close hit. Base this on geometry, never hover state.
        let close = if close_glyph.x >= self.list.x && right(close_glyph) <= right(self.list) {
            clip_to_list(close_bounds, self.list).map(|hit| CloseButton {
                hit,
                glyph: close_glyph,
                hover_circle: centered_square(close_bounds, 16.),
            })
        } else {
            None
        };
        let favicon = centered_y(x + FAVICON_INSET, FAVICON_SIZE, FAVICON_SIZE);
        let title_x = right(favicon) + TITLE_GAP;
        let title = rect(
            title_x,
            FACE_Y,
            close_bounds.x - TITLE_GAP - title_x,
            FACE_HEIGHT,
        );
        Some(TabSlot {
            index,
            face,
            visible_face,
            favicon,
            title,
            close,
        })
    }

    fn stride(&self) -> f32 {
        self.tab_width + TAB_GAP
    }
}

fn strip_button(x: f32) -> StripButton {
    let bounds = centered_y(x, CONTROL_SIZE, CONTROL_SIZE);
    StripButton {
        bounds,
        glyph: centered_square(bounds, 17.),
    }
}

fn clip_to_list(bounds: Rect, list: Rect) -> Option<Rect> {
    let x = bounds.x.max(list.x);
    let end = right(bounds).min(right(list));
    (end > x).then(|| rect(x, bounds.y, end - x, bounds.height))
}

fn centered_y(x: f32, width: f32, height: f32) -> Rect {
    rect(x, FACE_Y + (FACE_HEIGHT - height) / 2., width, height)
}

fn centered_square(bounds: Rect, size: f32) -> Rect {
    rect(
        bounds.x + (bounds.width - size) / 2.,
        bounds.y + (bounds.height - size) / 2.,
        size,
        size,
    )
}

fn right(bounds: Rect) -> f32 {
    bounds.x + bounds.width
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests;
