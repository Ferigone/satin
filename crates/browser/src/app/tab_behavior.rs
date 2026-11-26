//! Native tab operations deliberately keep no closed browsing contexts.
//! Reopening and duplicating load one eligible address as a fresh GET in the
//! existing profile. They don't copy a request, history, document or storage.

use std::collections::VecDeque;

use url::Url;

pub(super) type TabId = u64;
const MAX_CLOSED_TABS: usize = 20;
const MAX_ADDRESS_BYTES: usize = 4096;

pub(super) struct FreshTab(String);

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
        allowed.then(|| Self(url.into()))
    }

    pub(super) fn into_address(self) -> String {
        self.0
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
    tab_id: impl Fn(&T) -> TabId,
) -> Option<usize> {
    let from = tabs.iter().position(|tab| tab_id(tab) == id)?;
    let to = match direction {
        TabMove::Left => from.checked_sub(1)?,
        TabMove::Right => (from + 1 < tabs.len()).then_some(from + 1)?,
    };
    tabs.swap(from, to);
    Some(to)
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
        assert_eq!(move_tab(&mut tabs, 42, TabMove::Left, |tab| tab.0), Some(0));
        assert_eq!(tabs, [(42, "b"), (18, "a"), (31, "c")]);
        assert_eq!(move_tab(&mut tabs, 42, TabMove::Left, |tab| tab.0), None);
        assert_eq!(move_tab(&mut tabs, 31, TabMove::Right, |tab| tab.0), None);
        assert_eq!(move_tab(&mut tabs, 99, TabMove::Left, |tab| tab.0), None);
        assert_eq!(
            move_tab(&mut tabs, 42, TabMove::Right, |tab| tab.0),
            Some(1)
        );
        assert_eq!(tabs, [(18, "a"), (42, "b"), (31, "c")]);
    }
}
