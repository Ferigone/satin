use super::{
    App, CHROME_HOVER, CHROME_LINE, CHROME_MUTED, CHROME_PANEL, CHROME_TEXT, Canvas, CursorIcon,
    Key, NamedKey, Rect, SessionHistory, TOP, rounded_chrome_rect, scaled_rect,
};
use url::{Host, Url};

const NEARBY: i32 = 3;
const HEADER: f32 = 51.;
const ROW: f32 = 30.;
const MARGIN: f32 = 12.;

#[derive(PartialEq, Eq)]
struct Entry {
    delta: i32,
    url: Url,
}

pub(super) struct HistoryMenu {
    tab: u64,
    entries: Vec<Entry>,
    selected: Option<usize>,
    hovered: Option<usize>,
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
        }
    }

    fn move_focus(&mut self, previous: bool) {
        let Some(mut index) = self.selected else {
            return;
        };
        let count = self.entries.len();
        let step = if previous { count - 1 } else { 1 };
        loop {
            index = (index + step) % count;
            if self.entries[index].delta != 0 {
                self.selected = Some(index);
                break;
            }
        }
    }

    fn target(&self, index: usize, tab: u64, history: &SessionHistory) -> Option<i32> {
        let delta = self.entries.get(index)?.delta;
        // A script can update the stack while the popup is open. Don't jump
        // to a different URL than the one at this position when it was shown.
        (delta != 0 && self.tab == tab && self.entries == Self::entries(history)).then_some(delta)
    }

    fn bounds(&self, width: f32) -> Rect {
        let panel = (width - 2. * MARGIN).clamp(0., 350.);
        Rect {
            x: width - MARGIN - panel,
            y: TOP + 4.,
            width: panel,
            height: HEADER + ROW * self.entries.len() as f32 + 6.,
        }
    }

    fn row(bounds: Rect, index: usize) -> Rect {
        Rect {
            x: bounds.x + 6.,
            y: bounds.y + HEADER + ROW * index as f32,
            width: (bounds.width - 12.).max(0.),
            height: ROW,
        }
    }

    fn hit(&self, bounds: Rect, x: f32, y: f32) -> Option<usize> {
        self.entries.iter().enumerate().find_map(|(index, entry)| {
            (entry.delta != 0 && Self::row(bounds, index).contains(x, y)).then_some(index)
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
        self.select_popup = None;
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
        let Some(menu) = &mut self.history_menu else {
            return false;
        };
        match key {
            Key::Named(NamedKey::Escape) => self.dismiss_history_menu(),
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::Tab) => {
                let previous = *key == Key::Named(NamedKey::ArrowUp)
                    || (*key == Key::Named(NamedKey::Tab) && self.modifiers.shift_key());
                menu.move_focus(previous);
                self.redraw();
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(index) = menu.selected {
                    self.select_history_entry(index);
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
        if let Some(window) = &self.window {
            let bounds = menu.bounds(window.inner_size().width as f32 / self.scale);
            if let Some(index) = menu.hit(bounds, x, y) {
                self.select_history_entry(index);
            } else if !bounds.contains(x, y) {
                self.dismiss_history_menu();
            }
        }
        true
    }

    pub(super) fn update_history_pointer(&mut self) -> bool {
        let Some(menu) = &mut self.history_menu else {
            return false;
        };
        if let Some(window) = &self.window {
            let bounds = menu.bounds(window.inner_size().width as f32 / self.scale);
            let hovered = menu.hit(bounds, self.mouse.0, self.mouse.1);
            window.set_cursor(if hovered.is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
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
        let scale = self.scale;
        let panel = menu.bounds(width);
        let rect = |x, y, w, h| scaled_rect(scale, x, y, w, h);
        let panel_rect = rect(panel.x, panel.y, panel.width, panel.height);
        rounded_chrome_rect(canvas, panel_rect, 10. * scale, CHROME_LINE);
        rounded_chrome_rect(
            canvas,
            rect(
                panel.x + 1.,
                panel.y + 1.,
                panel.width - 2.,
                panel.height - 2.,
            ),
            9. * scale,
            CHROME_PANEL,
        );
        canvas.clip(panel_rect);
        let mut text = |canvas: &mut Canvas, label: &str, x, y, size, color, bold| {
            canvas.label(
                label,
                x * scale,
                y * scale,
                size * scale,
                color,
                bold,
                &mut self.fonts,
            );
        };
        let x = panel.x + 14.;
        text(
            canvas,
            "Tab history",
            x,
            panel.y + 22.,
            15.,
            CHROME_TEXT,
            true,
        );
        let subtitle = if menu.selected.is_none() {
            "No earlier or later entries in this tab."
        } else {
            "This session only · Up to 3 in each direction"
        };
        text(canvas, subtitle, x, panel.y + 41., 12., CHROME_MUTED, false);
        for (index, entry) in menu.entries.iter().enumerate() {
            let row = HistoryMenu::row(panel, index);
            if menu.selected == Some(index) || menu.hovered == Some(index) {
                rounded_chrome_rect(
                    canvas,
                    rect(row.x, row.y, row.width, row.height),
                    6. * scale,
                    CHROME_HOVER,
                );
            }
            if menu.selected == Some(index) {
                canvas.fill_rect(rect(row.x + 2., row.y + 7., 2., 16.), CHROME_TEXT);
            }
            let (marker, color) = match entry.delta {
                0 => ("Current".into(), CHROME_TEXT),
                n if n < 0 => (format!("{} back", -n), CHROME_MUTED),
                n => (format!("{n} forward"), CHROME_MUTED),
            };
            text(
                canvas,
                &marker,
                x,
                row.y + 20.,
                12.,
                color,
                entry.delta == 0,
            );
            text(
                canvas,
                site_label(&entry.url),
                x + 80.,
                row.y + 20.,
                14.,
                CHROME_TEXT,
                entry.delta == 0,
            );
        }
        canvas.clip(Rect {
            x: 0.,
            y: 0.,
            width: canvas.width as f32,
            height: canvas.height as f32,
        });
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
        let bounds = menu.bounds(640.);
        assert_eq!(
            menu.hit(bounds, bounds.x + 15., bounds.y + HEADER + 1.),
            Some(0)
        );
        assert_eq!(
            menu.hit(bounds, bounds.x + 15., bounds.y + HEADER + ROW + 1.),
            None
        );
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
}
