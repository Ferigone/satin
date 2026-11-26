use super::bookmark_menu::{
    library::{ACCENT, CARD, Field, HOVER, LINE, Layout, MUTED, PAGE, Page, Paint, TEXT, bounds},
    library_body, library_window,
};
use super::{App, Canvas, CursorIcon, Key, NamedKey, SessionHistory};
use url::{Host, Url};

const NEARBY: i32 = 3;

#[derive(PartialEq, Eq)]
struct Entry {
    delta: i32,
    url: Url,
}

impl Entry {
    fn position(&self) -> String {
        match self.delta {
            0 => "Current page".into(),
            n if n < 0 => format!("{} back", -n),
            n => format!("{n} forward"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Entry(usize),
    Filter,
    Close,
    Previous,
    Next,
}

pub(super) struct HistoryMenu {
    tab: u64,
    entries: Vec<Entry>,
    selected: Option<usize>,
    hovered: Option<Control>,
    filter: Field,
    filter_focused: bool,
    page_anchor: Option<usize>,
}

impl HistoryMenu {
    fn entries(history: &SessionHistory) -> Vec<Entry> {
        (-NEARBY..=NEARBY)
            .filter_map(|delta| history.entry_url(delta).map(|url| Entry { delta, url }))
            .collect()
    }

    fn new(tab: u64, history: &SessionHistory) -> Self {
        let entries = Self::entries(history);
        let selected = [-1, 1]
            .into_iter()
            .find_map(|delta| entries.iter().position(|entry| entry.delta == delta));
        Self {
            tab,
            entries,
            selected,
            hovered: None,
            filter: Field::default(),
            filter_focused: false,
            page_anchor: None,
        }
    }

    fn filtered(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                self.filter
                    .matches(&[site_label(&entry.url), &entry.position()])
                    .then_some(index)
            })
            .collect()
    }

    fn move_focus(&mut self, previous: bool) {
        let rows: Vec<_> = self
            .filtered()
            .into_iter()
            .filter(|&i| self.entries[i].delta != 0)
            .collect();
        if !rows.is_empty() {
            let count = rows.len();
            let step = if previous { count - 1 } else { 1 };
            let index = self
                .selected
                .and_then(|selected| rows.iter().position(|&i| i == selected));
            self.selected = Some(
                rows[index.map_or(if previous { count - 1 } else { 0 }, |i| (i + step) % count)],
            );
        }
        self.page_anchor = None;
    }

    fn target(&self, index: usize, tab: u64, history: &SessionHistory) -> Option<i32> {
        let delta = self.entries.get(index)?.delta;
        // A script can update the stack while the popup is open. Don't jump
        // to a different URL than the one at this position when it was shown.
        (delta != 0 && self.tab == tab && self.entries == Self::entries(history)).then_some(delta)
    }

    fn page(&self, matches: &[usize], layout: &Layout) -> Page {
        let anchor = self
            .page_anchor
            .or_else(|| {
                self.selected
                    .and_then(|selected| matches.iter().position(|&i| i == selected))
            })
            .unwrap_or(0);
        Page::for_selection(anchor, matches.len(), layout.capacity)
    }

    fn page_by(&mut self, layout: &Layout, previous: bool) {
        let matches = self.filtered();
        let page = self.page(&matches, layout);
        let target = if previous {
            page.first.saturating_sub(layout.capacity)
        } else {
            page.first + layout.capacity
        };
        if target < matches.len() {
            self.page_anchor = Some(target);
            self.selected = matches
                .iter()
                .skip(target)
                .take(layout.capacity)
                .copied()
                .find(|&i| self.entries[i].delta != 0);
            self.filter_focused = false;
        }
    }

    fn hit(&self, layout: &Layout, x: f32, y: f32) -> Option<Control> {
        if !layout.body.contains(x, y) {
            return None;
        }
        if layout.close.contains(x, y) {
            return Some(Control::Close);
        }
        if layout.filter.contains(x, y) {
            return Some(Control::Filter);
        }
        let matches = self.filtered();
        let page = self.page(&matches, layout);
        let (previous, next) = layout.pager(page.limit);
        if page.number > 0 && previous.contains(x, y) {
            return Some(Control::Previous);
        }
        if page.number + 1 < page.count && next.contains(x, y) {
            return Some(Control::Next);
        }
        matches
            .iter()
            .skip(page.first)
            .take(page.limit)
            .enumerate()
            .find_map(|(offset, &index)| {
                (self.entries[index].delta != 0 && layout.row(offset).contains(x, y))
                    .then_some(Control::Entry(index))
            })
    }
}

// This isn't the address bar. Display site names, never paths, fragments,
// queries, ports or user info, even for entries that can still be traversed.
fn site_label(url: &Url) -> &str {
    match url.scheme() {
        "about" => "Internal page",
        "file" => "Local file",
        "http" | "https" if !url.username().is_empty() || url.password().is_some() => {
            "Private web page"
        }
        "http" | "https" => match url.host() {
            Some(Host::Domain(name))
                if name.trim_end_matches('.').contains('.')
                    && ![
                        ".localhost",
                        ".local",
                        ".internal",
                        ".lan",
                        ".home.arpa",
                        ".onion",
                    ]
                    .iter()
                    .any(|suffix| name.trim_end_matches('.').ends_with(suffix)) =>
            {
                name
            }
            _ => "Local or private site",
        },
        _ => "Private page",
    }
}

impl App {
    pub(super) fn open_history_menu(&mut self) {
        let tab = &self.tabs[self.active];
        self.history_menu = Some(HistoryMenu::new(tab.id, &tab.history));
        self.chrome_menu = None;
        self.select_popup = None;
        self.set_dock_focus(false);
        self.chrome_hover = None;
        self.hover = None;
        self.redraw();
    }

    pub(super) fn dismiss_history_menu(&mut self) {
        if self.history_menu.take().is_some() {
            if let Some(window) = &self.window {
                window.set_cursor(CursorIcon::Default);
            }
            self.redraw();
        }
    }

    fn select_history_entry(&mut self, index: usize) {
        let tab = &self.tabs[self.active];
        let delta = self
            .history_menu
            .as_ref()
            .and_then(|menu| menu.target(index, tab.id, &tab.history));
        self.dismiss_history_menu();
        if let Some(delta) = delta {
            self.traverse_history(self.active, delta);
        }
    }

    pub(super) fn handle_history_key(&mut self, key: &Key) -> bool {
        if self.history_menu.is_none() {
            return false;
        }
        let layout = Layout::new(library_window(self), 0.);
        let Some(menu) = &mut self.history_menu else {
            return false;
        };
        let ctrl = self.modifiers.control_key() && !self.modifiers.alt_key();
        if ctrl && matches!(key, Key::Character(c) if c.eq_ignore_ascii_case("f")) {
            menu.filter_focused = true;
            self.redraw();
            return true;
        }
        if menu.filter_focused
            && menu
                .filter
                .key(key, self.modifiers, &mut self.clipboard, 120)
        {
            menu.selected = None;
            menu.page_anchor = None;
            menu.hovered = None;
            self.redraw();
            return true;
        }
        match key {
            Key::Named(NamedKey::Escape) => self.dismiss_history_menu(),
            Key::Named(NamedKey::PageUp | NamedKey::PageDown) => {
                menu.page_by(&layout, *key == Key::Named(NamedKey::PageUp));
                self.redraw();
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::Tab) => {
                let previous = *key == Key::Named(NamedKey::ArrowUp)
                    || (*key == Key::Named(NamedKey::Tab) && self.modifiers.shift_key());
                menu.filter_focused = false;
                menu.move_focus(previous);
                self.redraw();
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(index) = menu.selected.filter(|_| !menu.filter_focused) {
                    self.select_history_entry(index);
                } else if menu.filter_focused {
                    menu.filter_focused = false;
                    menu.move_focus(false);
                    self.redraw();
                }
            }
            _ => {}
        }
        true
    }

    pub(super) fn handle_history_click(&mut self) -> bool {
        let Some(menu) = &self.history_menu else {
            return false;
        };
        let (x, y) = self.mouse;
        let layout = Layout::new(library_window(self), 0.);
        match menu.hit(&layout, x, y) {
            Some(Control::Entry(index)) => self.select_history_entry(index),
            Some(Control::Close) => self.dismiss_history_menu(),
            Some(Control::Filter) => {
                if let Some(menu) = &mut self.history_menu {
                    menu.filter_focused = true;
                }
                self.redraw();
            }
            Some(control @ (Control::Previous | Control::Next)) => {
                if let Some(menu) = &mut self.history_menu {
                    menu.page_by(&layout, control == Control::Previous);
                }
                self.redraw();
            }
            None if !layout.body.contains(x, y)
                || x < layout.list.x
                || x >= layout.list.x + layout.list.width =>
            {
                self.dismiss_history_menu();
            }
            None => {}
        }
        true
    }

    pub(super) fn update_history_pointer(&mut self) -> bool {
        if self.history_menu.is_none() {
            return false;
        }
        let layout = Layout::new(library_window(self), 0.);
        let Some(menu) = &mut self.history_menu else {
            return false;
        };
        if let Some(window) = &self.window {
            let hovered = menu.hit(&layout, self.mouse.0, self.mouse.1);
            window.set_cursor(match hovered {
                Some(Control::Filter) => CursorIcon::Text,
                Some(_) => CursorIcon::Pointer,
                None => CursorIcon::Default,
            });
            if menu.hovered != hovered {
                menu.hovered = hovered;
                self.redraw();
            }
        }
        true
    }

    pub(super) fn clear_history_hover(&mut self) {
        if let Some(menu) = &mut self.history_menu {
            menu.hovered = None;
        }
    }

    pub(super) fn draw_history_menu(&mut self, canvas: &mut Canvas, width: f32) {
        let Some(menu) = &self.history_menu else {
            return;
        };
        let body = library_body(self, width, canvas.height as f32 / self.scale);
        let layout = Layout::new(body, 0.);
        let matches = menu.filtered();
        let page = menu.page(&matches, &layout);
        let hot = |control| menu.hovered == Some(control);
        let mut paint = Paint {
            canvas,
            fonts: &mut self.fonts,
            scale: self.scale,
        };
        paint.clip(body);
        paint.fill(body, PAGE);
        paint.heading(
            &layout,
            "History",
            "This tab only · Up to 3 pages in each direction",
        );
        paint.field(
            layout.filter,
            &menu.filter,
            "Filter history",
            menu.filter_focused,
            true,
            body,
        );
        paint.button("×", layout.close, hot(Control::Close), false, false);
        paint.border(layout.card(page.limit), 10., CARD, LINE);
        if page.limit == 0 {
            let text = if menu.entries.is_empty() {
                "No pages in this tab."
            } else {
                "No matching history entries."
            };
            paint.text(
                text,
                layout.list.x + 13.,
                layout.list.y + 32.,
                layout.list.width - 26.,
                13.,
                MUTED,
                false,
            );
        }
        for (offset, &index) in matches.iter().skip(page.first).take(page.limit).enumerate() {
            let entry = &menu.entries[index];
            let row = layout.row(offset);
            let name = site_label(&entry.url);
            let hovered = hot(Control::Entry(index));
            let selected = menu.selected == Some(index) && !menu.filter_focused;
            if selected {
                paint.rounded(
                    bounds(row.x + 5., row.y + 5., row.width - 10., row.height - 10.),
                    6.,
                    HOVER,
                );
                paint.fill(bounds(row.x + 7., row.y + 13., 2., 26.), ACCENT);
            }
            if offset + 1 < page.limit {
                paint.fill(bounds(row.x, row.y + row.height - 1., row.width, 1.), LINE);
            }
            let badge = match entry.delta {
                0 => "•",
                n if n < 0 => "‹",
                _ => "›",
            };
            paint.badge(badge, row.x + 12., row.y + 16.);
            let x = row.x + 41.;
            let text_width = row.width - 53.;
            paint.text(
                name,
                x,
                row.y + 22.,
                text_width,
                13.,
                if hovered { ACCENT } else { TEXT },
                entry.delta == 0,
            );
            paint.text(
                &entry.position(),
                x,
                row.y + 39.,
                text_width,
                11.,
                MUTED,
                false,
            );
        }
        if page.count > 1 {
            let (previous, next) = layout.pager(page.limit);
            paint.page_count(page, layout.list, previous.y);
            if page.number > 0 {
                paint.button("‹ Prev", previous, hot(Control::Previous), false, false);
            }
            if page.number + 1 < page.count {
                paint.button("Next ›", next, hot(Control::Next), false, false);
            }
        }
        paint.reset_clip();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_browser_engine::{history::NavigationMode, layout::ScrollOffset};

    fn url(address: &str) -> Url {
        Url::parse(address).unwrap()
    }

    #[test]
    fn menu_orders_actual_entries_and_uses_relative_traversal() {
        let history = SessionHistory::new(url("https://example.com/start"));
        let scroll = ScrollOffset::default();
        for n in 1..=9 {
            history.visit(
                url(&format!("https://example.com/{n}")),
                NavigationMode::Push,
                scroll,
            );
        }
        history.traverse(-4, scroll).unwrap();
        let menu = HistoryMenu::new(1, &history);
        let paths: Vec<_> = menu
            .entries
            .iter()
            .map(|entry| (entry.delta, entry.url.path()))
            .collect();
        assert_eq!(
            paths,
            [
                (-3, "/2"),
                (-2, "/3"),
                (-1, "/4"),
                (0, "/5"),
                (1, "/6"),
                (2, "/7"),
                (3, "/8")
            ]
        );
        assert_eq!(history.url().path(), "/5");
        assert_eq!(menu.selected, Some(2));
        assert_eq!(menu.target(3, 1, &history), None);
        assert_eq!(menu.target(0, 2, &history), None);
        let back = menu.target(0, 1, &history).unwrap();
        assert_eq!(history.traverse(back, scroll).unwrap().url.path(), "/2");
        assert_eq!(menu.target(0, 1, &history), None);
        let next_menu = HistoryMenu::new(1, &history);
        let forward = next_menu.target(5, 1, &history).unwrap();
        assert_eq!(history.traverse(forward, scroll).unwrap().url.path(), "/5");
        history.visit(
            url("https://example.com/branch"),
            NavigationMode::Push,
            scroll,
        );
        assert!(!history.can_traverse(1));
        assert_eq!(
            HistoryMenu::new(1, &history)
                .entries
                .last()
                .unwrap()
                .url
                .path(),
            "/branch"
        );
    }

    #[test]
    fn focus_skips_current_and_wraps_between_actions() {
        let history = SessionHistory::new(url("about:home"));
        let mut empty = HistoryMenu::new(1, &history);
        empty.move_focus(false);
        assert_eq!(empty.selected, None);
        let scroll = ScrollOffset::default();
        history.visit(url("https://example.com"), NavigationMode::Push, scroll);
        history.visit(url("https://example.org"), NavigationMode::Push, scroll);
        history.traverse(-1, scroll).unwrap();
        let mut menu = HistoryMenu::new(1, &history);
        assert_eq!(menu.selected, Some(0));
        let layout = Layout::new(bounds(0., 0., 640., 600.), 0.);
        let row = layout.row(0);
        assert_eq!(
            menu.hit(&layout, row.x + 15., row.y + 1.),
            Some(Control::Entry(0))
        );
        let current = layout.row(1);
        assert_eq!(menu.hit(&layout, current.x + 15., current.y + 1.), None);
        menu.move_focus(false);
        assert_eq!(menu.selected, Some(2));
        menu.move_focus(false);
        assert_eq!(menu.selected, Some(0));
        menu.move_focus(true);
        assert_eq!(menu.selected, Some(2));
    }

    #[test]
    fn labels_do_not_expose_private_urls_or_url_components() {
        for (address, expected) in [
            ("about:settings?secret=value", "Internal page"),
            ("file:///C:/Users/private/personal.html", "Local file"),
            (
                "https://username:password@example.com/secret",
                "Private web page",
            ),
            ("https://username@example.com/secret", "Private web page"),
            (
                "https://example.com:8443/token?password=secret#private",
                "example.com",
            ),
            ("http://127.0.0.1:8118/private", "Local or private site"),
            ("http://[::1]/private", "Local or private site"),
            ("http://intranet/private", "Local or private site"),
            ("https://wiki.home.arpa./private", "Local or private site"),
            ("data:text/plain,private", "Private page"),
        ] {
            assert_eq!(site_label(&url(address)), expected);
        }
    }

    #[test]
    fn filtering_does_not_test_hidden_url_components() {
        let history = SessionHistory::new(url("https://example.com/from-secret"));
        history.visit(
            url("https://user:password@example.com/current"),
            NavigationMode::Push,
            ScrollOffset::default(),
        );
        let mut menu = HistoryMenu::new(1, &history);
        menu.filter = Field::selecting("secret".into());
        assert!(menu.filtered().is_empty());
        menu.filter = Field::selecting("example.com".into());
        assert_eq!(menu.filtered(), [0]);
        menu.filter = Field::selecting("private web page".into());
        assert_eq!(menu.filtered(), [1]);
    }
}
