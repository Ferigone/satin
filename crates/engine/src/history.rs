//! The session history belongs to the browsing context. A document's realm
//! shares this handle with the window, while entries store portable data.
pub(crate) mod state_data;

use crate::layout::ScrollOffset;
use state_data::StateData;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
use url::Url;

const ENTRY_LIMIT: usize = 100;
const STATE_BUDGET: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NavigationMode {
    Push,
    Replace,
    Reload,
}

pub enum ScriptNavigation {
    Navigate { url: Url, mode: NavigationMode },
    Traverse(i32),
    Reload,
}

#[derive(Clone, Copy)]
pub enum HistoryScroll {
    Keep,
    Fragment,
    Restore(ScrollOffset),
}

pub fn fragment_offset(address: &str, layout: &crate::Layout) -> Option<f32> {
    let url = Url::parse(address).ok()?;
    let fragment = url.fragment()?;
    let decoded: Vec<_> = crate::data_url::percent_decode(fragment.as_bytes()).collect();
    let name = String::from_utf8_lossy(&decoded);
    layout
        .anchors
        .get(name.as_ref())
        .copied()
        .or_else(|| (name.is_empty() || name.eq_ignore_ascii_case("top")).then_some(0.))
}

pub struct HistoryTransition {
    pub url: Url,
    pub same_document: bool,
    pub scroll: HistoryScroll,
    pub(crate) previous: Url,
    pub(crate) changed_entry: bool,
}

struct Entry {
    url: Url,
    document: u64,
    state: Option<Rc<StateData>>,
    scroll: Option<ScrollOffset>,
    auto_scroll: bool,
}

struct Entries {
    items: VecDeque<Entry>,
    index: usize,
    next_document: u64,
}

impl Entries {
    fn current(&self) -> &Entry {
        &self.items[self.index]
    }

    fn current_mut(&mut self) -> &mut Entry {
        &mut self.items[self.index]
    }

    fn insert(&mut self, entry: Entry, replace: bool) {
        if replace {
            *self.current_mut() = entry;
        } else {
            self.items.truncate(self.index + 1);
            self.items.push_back(entry);
            if self.items.len() > ENTRY_LIMIT {
                self.items.pop_front();
            }
            self.index = self.items.len() - 1;
        }
    }
}

#[derive(Clone)]
pub struct SessionHistory(Rc<RefCell<Entries>>);

impl SessionHistory {
    pub fn new(url: Url) -> Self {
        Self(Rc::new(RefCell::new(Entries {
            items: VecDeque::from([Entry {
                url,
                document: 0,
                state: None,
                scroll: None,
                auto_scroll: true,
            }]),
            index: 0,
            next_document: 1,
        })))
    }

    pub fn url(&self) -> Url {
        self.0.borrow().current().url.clone()
    }

    pub fn can_traverse(&self, delta: i32) -> bool {
        let entries = self.0.borrow();
        entries
            .index
            .checked_add_signed(delta as isize)
            .is_some_and(|index| index < entries.items.len())
    }

    pub(crate) fn length(&self) -> usize {
        self.0.borrow().items.len()
    }

    pub(crate) fn state(&self) -> Option<Rc<StateData>> {
        self.0.borrow().current().state.clone()
    }

    pub(crate) fn auto_scroll(&self) -> bool {
        self.0.borrow().current().auto_scroll
    }

    pub(crate) fn set_auto_scroll(&self, automatic: bool) {
        self.0.borrow_mut().current_mut().auto_scroll = automatic;
    }

    pub fn commit_address(&self, url: Url) {
        let mut entries = self.0.borrow_mut();
        if !can_rewrite(&entries.current().url, &url) {
            // A network redirect can change origins while revisiting a state
            // entry. Its new realm must never inherit that state's data or
            // share the old document's same-document traversal group.
            let document = entries.next_document;
            entries.next_document += 1;
            let entry = entries.current_mut();
            entry.document = document;
            entry.state = None;
            entry.scroll = None;
            entry.auto_scroll = true;
        }
        entries.current_mut().url = url;
    }

    /// Visit a different document while preserving an existing entry during
    /// reload or the network portion of traversal.
    pub fn visit(&self, url: Url, mode: NavigationMode, scroll: ScrollOffset) {
        let mut entries = self.0.borrow_mut();
        if mode == NavigationMode::Reload {
            entries.current_mut().url = url;
            return;
        }
        entries.current_mut().scroll = Some(scroll);
        let document = entries.next_document;
        entries.next_document += 1;
        entries.insert(
            Entry {
                url,
                document,
                state: None,
                scroll: None,
                auto_scroll: true,
            },
            mode == NavigationMode::Replace,
        );
    }

    pub fn fragment(
        &self,
        url: Url,
        mode: NavigationMode,
        scroll: ScrollOffset,
    ) -> Option<HistoryTransition> {
        if mode == NavigationMode::Reload || url.fragment().is_none() {
            return None;
        }
        let mut entries = self.0.borrow_mut();
        let previous = entries.current().url.clone();
        if previous[..url::Position::AfterQuery] != url[..url::Position::AfterQuery] {
            return None;
        }
        let changed_entry = previous != url;
        if changed_entry {
            entries.current_mut().scroll = Some(scroll);
            let document = entries.current().document;
            let auto_scroll = entries.current().auto_scroll;
            entries.insert(
                Entry {
                    url: url.clone(),
                    document,
                    state: None,
                    scroll: None,
                    auto_scroll,
                },
                mode == NavigationMode::Replace,
            );
        }
        Some(HistoryTransition {
            url,
            same_document: true,
            scroll: HistoryScroll::Fragment,
            previous,
            changed_entry,
        })
    }

    pub fn traverse(&self, delta: i32, scroll: ScrollOffset) -> Option<HistoryTransition> {
        if delta == 0 {
            return None;
        }
        let mut entries = self.0.borrow_mut();
        let index = entries.index.checked_add_signed(delta as isize)?;
        if index >= entries.items.len() {
            return None;
        }
        let previous = entries.current().url.clone();
        let document = entries.current().document;
        entries.current_mut().scroll = Some(scroll);
        entries.index = index;
        let current = entries.current();
        let same_document = document == current.document && can_rewrite(&previous, &current.url);
        let scroll = if current.auto_scroll {
            current
                .scroll
                .map_or(HistoryScroll::Fragment, HistoryScroll::Restore)
        } else {
            HistoryScroll::Keep
        };
        Some(HistoryTransition {
            url: current.url.clone(),
            same_document,
            scroll,
            previous,
            changed_entry: true,
        })
    }

    pub(crate) fn rewrite(
        &self,
        url: Url,
        state: Rc<StateData>,
        replace: bool,
        scroll: ScrollOffset,
    ) -> bool {
        let mut entries = self.0.borrow_mut();
        let retained = entries
            .items
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                if replace {
                    *index != entries.index
                } else {
                    *index <= entries.index
                }
            })
            .filter_map(|(_, entry)| entry.state.as_ref())
            .map(|data| data.bytes())
            .sum::<usize>();
        if retained + state.bytes() > STATE_BUDGET {
            return false;
        }
        let current = entries.current_mut();
        current.scroll = Some(scroll);
        let entry = Entry {
            url,
            document: current.document,
            state: Some(state),
            scroll: Some(scroll),
            auto_scroll: current.auto_scroll,
        };
        entries.insert(entry, replace);
        true
    }
}

pub(crate) fn can_rewrite(old: &Url, new: &Url) -> bool {
    old.scheme() == new.scheme()
        && old.username() == new.username()
        && old.password() == new.password()
        && old.host() == new.host()
        && old.port() == new.port()
        && (matches!(old.scheme(), "http" | "https")
            || old.path() == new.path() && (old.scheme() == "file" || old.query() == new.query()))
}
