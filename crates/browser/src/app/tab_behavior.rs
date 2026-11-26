//! Native tab operations deliberately keep no closed browsing contexts.
//! Reopening and duplicating load one eligible address as a fresh GET in the
//! existing profile. They don't copy a request, history, document or storage.
//! Pins and closed-tab pin flags live only in this process. The v1 session
//! still saves the live URL order; a restart restores those URLs unpinned.

use std::{collections::VecDeque, ops::Range, time::Instant};

use rust_browser_engine::layout::Rect;
use url::Url;

pub(super) type TabId = u64;
const MAX_CLOSED_TABS: usize = 20;
const MAX_ADDRESS_BYTES: usize = 4096;

pub(super) struct FreshTab {
    address: String,
    pub(super) pinned: bool,
}

impl FreshTab {
    pub(super) fn capture(address: &str, get_navigation: bool) -> Option<Self> {
        if !get_navigation || address.len() > MAX_ADDRESS_BYTES {
            return None;
        }
        let url = Url::parse(address).ok()?;
        // Skip embedded credentials and opaque parameters, not just the
        // password field. Stripping these could silently open a different page.
        if url.as_str().len() > MAX_ADDRESS_BYTES
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        let allowed = match url.scheme() {
            "http" | "https" => url.has_host(),
            "about" => matches!(
                url.as_str(),
                "about:home" | "about:version" | "about:shortcuts" | "about:start-shortcuts"
            ),
            // Local file paths, data payloads and unknown internal pages
            // don't go into the undo buffer.
            _ => false,
        };
        allowed.then(|| Self {
            address: url.into(),
            pinned: false,
        })
    }

    pub(super) fn into_address(self) -> String {
        self.address
    }
}

#[derive(Default)]
pub(super) struct ClosedTabs(VecDeque<FreshTab>);

impl ClosedTabs {
    pub(super) fn remember(&mut self, tab: Option<FreshTab>) {
        if let Some(tab) = tab {
            if self.0.len() == MAX_CLOSED_TABS {
                self.0.pop_front();
            }
            self.0.push_back(tab);
        }
    }

    pub(super) fn reopen(&mut self) -> Option<FreshTab> {
        self.0.pop_back()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TabMove {
    Left,
    Right,
}

pub(super) fn move_tab<T>(
    tabs: &mut [T],
    id: TabId,
    direction: TabMove,
    pinned: usize,
    tab_id: impl Fn(&T) -> TabId,
) -> Option<usize> {
    let from = tabs.iter().position(|tab| tab_id(tab) == id)?;
    let to = match direction {
        TabMove::Left => from.checked_sub(1)?,
        TabMove::Right => from + 1,
    };
    if !group(from, pinned, tabs.len()).contains(&to) {
        return None;
    }
    tabs.swap(from, to);
    Some(to)
}

fn group(source: usize, pinned: usize, count: usize) -> Range<usize> {
    if source < pinned {
        0..pinned
    } else {
        pinned..count
    }
}

/// Destination at the pinned/unpinned boundary, measured after removing the source.
pub(super) fn pin_destination(source: usize, pinned: usize) -> usize {
    if source < pinned { pinned - 1 } else { pinned }
}

pub(super) fn place_tab<T>(tabs: &mut [T], from: usize, to: usize) {
    if from < to {
        tabs[from..=to].rotate_left(1);
    } else if from > to {
        tabs[to..=from].rotate_right(1);
    }
}

pub(super) struct TabDrag {
    pub(super) id: TabId,
    pub(super) initial_scroll: f32,
    pub(super) last_scroll: Instant,
    pub(super) destination: Option<usize>,
    pub(super) started: bool,
    start: (f32, f32),
    grab_x: f32,
}

impl TabDrag {
    pub(super) fn new(id: TabId, start: (f32, f32), face: Rect, scroll: f32) -> Self {
        Self {
            id,
            initial_scroll: scroll,
            last_scroll: Instant::now(),
            destination: None,
            started: false,
            start,
            grab_x: start.0 - face.x,
        }
    }

    pub(super) fn sample(&mut self, cursor: (f32, f32)) -> bool {
        self.started |= (cursor.0 - self.start.0).hypot(cursor.1 - self.start.1) >= 6.;
        self.started
    }

    pub(super) fn left(&self, x: f32) -> f32 {
        x - self.grab_x
    }

    pub(super) fn in_band(cursor: (f32, f32), list: Rect) -> bool {
        cursor.1 >= list.y - 42. && cursor.1 <= list.y + list.height + 42.
    }

    pub(super) fn edge_step(cursor: (f32, f32), list: Rect) -> f32 {
        if !Self::in_band(cursor, list) || list.width <= 0. {
            return 0.;
        }
        let edge = 26_f32.min(list.width / 2.);
        let left = list.x + edge - cursor.0;
        let right = cursor.0 - (list.x + list.width - edge);
        if left > 0. {
            -(left * 0.45).clamp(2., 14.)
        } else if right > 0. {
            (right * 0.45).clamp(2., 14.)
        } else {
            0.
        }
    }

    pub(super) fn target(
        &self,
        source: usize,
        pinned: usize,
        count: usize,
        cursor: (f32, f32),
        list: Rect,
        face: impl Fn(usize) -> Rect,
    ) -> Option<usize> {
        if !Self::in_band(cursor, list) || list.width <= 0. {
            return None;
        }
        let center = |index| {
            let rect = face(index);
            rect.x + rect.width / 2.
        };
        let dragged_center = self.left(cursor.0) + face(source).width / 2.;
        let peers = group(source, pinned, count);
        if dragged_center < center(source) {
            (peers.start..source)
                .find(|&i| dragged_center < center(i))
                .or(Some(source))
        } else {
            (source + 1..peers.end)
                .rev()
                .find(|&i| dragged_center > center(i))
                .or(Some(source))
        }
    }
}

/// Paint the reordered peers as a preview without mutating the live tab model.
pub(super) fn preview_at(slot: usize, from: usize, to: usize) -> Option<usize> {
    if slot == to {
        None
    } else if (from..to).contains(&slot) {
        Some(slot + 1)
    } else if (to + 1..=from).contains(&slot) {
        Some(slot - 1)
    } else {
        Some(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(url: &str) -> Option<FreshTab> {
        FreshTab::capture(url, true)
    }

    #[test]
    fn closed_tabs_reopen_in_lifo_order_skipping_ineligible_closes() {
        let mut closed = ClosedTabs::default();
        closed.remember(capture("about:home"));
        closed.remember(capture("about:shortcuts"));
        closed.remember(capture("file:///C:/private.html"));
        assert_eq!(closed.reopen().unwrap().into_address(), "about:shortcuts");
        assert_eq!(closed.reopen().unwrap().into_address(), "about:home");
        assert!(closed.reopen().is_none());
    }

    #[test]
    fn closed_tab_buffer_evicts_the_oldest_address() {
        let mut closed = ClosedTabs::default();
        for i in 0..(MAX_CLOSED_TABS + 3) {
            closed.remember(capture(&format!("https://example.test/{i}")));
        }
        for i in (3..(MAX_CLOSED_TABS + 3)).rev() {
            assert_eq!(
                closed.reopen().unwrap().into_address(),
                format!("https://example.test/{i}")
            );
        }
        assert!(closed.reopen().is_none());
    }

    #[test]
    fn a_fresh_address_never_replays_non_get_or_copies_unsupported_urls() {
        assert!(FreshTab::capture("https://example.test/form", false).is_none());
        assert!(FreshTab::capture("about:home", false).is_none());
        for unfit in [
            "https://alice@example.test/",
            "https://:secret@example.test/",
            "https://example.test/?token=opaque",
            "https://example.test/#access_token=opaque",
            "file:///C:/private.html",
            "data:text/html,sensitive-payload",
            "blob:https://example.test/123",
            "about:unknown",
            "invalid",
        ] {
            assert!(capture(unfit).is_none(), "unexpectedly accepted {unfit}");
        }
        let oversized = format!("https://example.test/{}", "a".repeat(MAX_ADDRESS_BYTES));
        assert!(capture(&oversized).is_none());
        let expands_after_parsing = format!("https://example.test/{}", "é".repeat(1000));
        assert!(capture(&expands_after_parsing).is_none());
        assert_eq!(
            capture("https://example.test/article")
                .unwrap()
                .into_address(),
            "https://example.test/article"
        );
    }

    #[test]
    fn reorder_uses_current_ids_and_does_not_wrap_or_replace_tab_state() {
        let mut tabs = [(18, "a"), (42, "b"), (31, "c")];
        assert_eq!(
            move_tab(&mut tabs, 42, TabMove::Left, 0, |tab| tab.0),
            Some(0)
        );
        assert_eq!(tabs, [(42, "b"), (18, "a"), (31, "c")]);
        assert_eq!(move_tab(&mut tabs, 42, TabMove::Left, 0, |tab| tab.0), None);
        assert_eq!(
            move_tab(&mut tabs, 31, TabMove::Right, 0, |tab| tab.0),
            None
        );
        assert_eq!(move_tab(&mut tabs, 99, TabMove::Left, 0, |tab| tab.0), None);
        assert_eq!(
            move_tab(&mut tabs, 42, TabMove::Right, 0, |tab| tab.0),
            Some(1)
        );
        assert_eq!(tabs, [(18, "a"), (42, "b"), (31, "c")]);
    }

    #[test]
    fn pinned_prefix_sets_both_keyboard_and_drag_boundaries() {
        let mut tabs = [7, 8, 9, 10];
        assert_eq!(move_tab(&mut tabs, 8, TabMove::Right, 2, |id| *id), None);
        assert_eq!(move_tab(&mut tabs, 9, TabMove::Left, 2, |id| *id), None);
        let to = pin_destination(0, 2);
        place_tab(&mut tabs, 0, to);
        assert_eq!(tabs, [8, 7, 9, 10]);
        let to = pin_destination(3, 1);
        place_tab(&mut tabs, 3, to);
        assert_eq!(tabs, [8, 10, 7, 9]);

        let strip = super::super::tab_strip::TabStrip::new(640., 105., 4, 2);
        let from = strip.face(1);
        let drag = TabDrag::new(10, (from.x + 7., 20.), from, 0.);
        let target = |x, y| drag.target(1, 2, 4, (x, y), strip.list, |i| strip.face(i));
        assert_eq!(target(650., 20.), Some(1));
        assert_eq!(target(55., 20.), Some(1)); // exact peer center doesn't cross it
        assert_eq!(target(54., 20.), Some(0));
        assert_eq!(target(55., 89.), None);
    }

    #[test]
    fn drag_preview_crosses_centers_without_reordering_the_live_tabs() {
        let strip = super::super::tab_strip::TabStrip::new(640., 105., 5, 1);
        let source = 1;
        let x = strip.face(source).x + 10.;
        let mut drag = TabDrag::new(7, (x, 20.), strip.face(source), 0.);
        assert!(!drag.sample((x + 5., 20.)));
        assert!(drag.sample((x + 6., 20.)));
        assert!(drag.sample((x, 20.)));
        let face = strip.face(3);
        let target = |x| drag.target(source, 1, 5, (x, 20.), strip.list, |i| strip.face(i));
        assert_eq!(target(face.x + 10.), Some(2));
        assert_eq!(target(face.x + 11.), Some(3));
        let mut tabs = [6, 7, 8, 9, 10];
        let preview: Vec<_> = (0..5)
            .map(|slot| preview_at(slot, 1, 3).map(|i| tabs[i]))
            .collect();
        assert_eq!(preview, vec![Some(6), Some(8), Some(9), None, Some(10)]);
        assert_eq!(tabs, [6, 7, 8, 9, 10]);
        place_tab(&mut tabs, 1, 3);
        assert_eq!(tabs, [6, 8, 9, 7, 10]);
        assert!(TabDrag::edge_step((strip.list.x, 20.), strip.list) < 0.);
        assert_eq!(TabDrag::edge_step((strip.list.x, 89.), strip.list), 0.);
    }
}
