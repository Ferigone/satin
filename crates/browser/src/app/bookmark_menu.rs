use super::{
    Action, App, CHROME_HOVER, CHROME_LINE, CHROME_MUTED, CHROME_PANEL, CHROME_TEXT, Canvas, Key,
    NamedKey, Rect, TOP, bookmarks::page_url, elide, rounded_chrome_rect, scaled_rect,
};

const ROWS_PER_PAGE: usize = 4;
const PREVIOUS: usize = usize::MAX;
const NEXT: usize = usize::MAX - 1;
const BACKGROUND: usize = usize::MAX - 2;

pub(super) enum Menu {
    Main(usize),
    Bookmarks(BookmarkPanel),
}

pub(super) struct BookmarkPanel {
    selected: usize, // 0 is the current-page command; saved links start at 1.
    notice: Option<String>,
}

impl BookmarkPanel {
    fn first_link(&self) -> usize {
        self.selected.saturating_sub(1) / ROWS_PER_PAGE * ROWS_PER_PAGE
    }
}

impl App {
    pub(super) fn show_bookmarks(&mut self) {
        self.select_popup = None;
        let selected = self.first_bookmark_row();
        self.chrome_menu = Some(Menu::Bookmarks(BookmarkPanel {
            selected,
            notice: None,
        }));
        self.refresh_bookmark_rows();
    }

    fn refresh_bookmark_rows(&mut self) {
        self.hits
            .retain(|(_, action)| !matches!(action, Action::BookmarkItem(_)));
        self.chrome_hover = None;
        self.redraw();
    }

    fn bookmark_panel(&mut self) -> &mut BookmarkPanel {
        let Some(Menu::Bookmarks(panel)) = &mut self.chrome_menu else {
            unreachable!("bookmark action without a bookmarks panel");
        };
        panel
    }

    fn bookmark_current(&self) -> Option<(url::Url, bool)> {
        let tab = &self.tabs[self.active];
        let url = page_url(&tab.url)?;
        if self.bookmarks.as_ref().ok()?.contains(&url) {
            Some((url, true))
        } else if tab.document_session.is_some() {
            Some((url, false))
        } else {
            None
        }
    }

    fn bookmark_count(&self) -> usize {
        self.bookmarks
            .as_ref()
            .map_or(0, |store| store.entries().len())
    }

    fn first_bookmark_row(&self) -> usize {
        usize::from(self.bookmark_current().is_none() && self.bookmark_count() != 0)
    }

    pub(super) fn save_current_bookmark(&mut self) {
        if !matches!(self.chrome_menu, Some(Menu::Bookmarks(_))) {
            self.show_bookmarks();
        }
        let tab = &self.tabs[self.active];
        let result = match &mut self.bookmarks {
            Err(_) => Err("Bookmarks could not be read; the file was left unchanged.".into()),
            Ok(store) => match page_url(&tab.url) {
                Some(url) if store.contains(&url) => Ok("Already bookmarked."),
                Some(_) if tab.document_session.is_some() => {
                    store.add(&tab.url, &tab.title).map(|_| "Bookmark saved.")
                }
                _ => Err("Open an HTTP(S) page without URL credentials to bookmark it.".into()),
            },
        };
        let selected = self.bookmarks.as_ref().ok().and_then(|store| {
            let url = page_url(&tab.url)?;
            store
                .entries()
                .iter()
                .position(|entry| entry.url == url)
                .map(|index| index + 1)
        });
        let panel = self.bookmark_panel();
        panel.notice = Some(result.map_or_else(|message| message, str::to_owned));
        if let Some(selected) = selected {
            panel.selected = selected;
        }
        self.refresh_bookmark_rows();
    }

    fn remove_bookmark(&mut self, url: &url::Url) {
        if let Ok(store) = &mut self.bookmarks {
            let result = store.remove(url);
            let last = store.entries().len();
            let panel = self.bookmark_panel();
            panel.selected = panel.selected.max(1).min(last);
            panel.notice = Some(match result {
                Ok(true) => "Bookmark removed.".into(),
                Ok(false) => "Bookmark no longer exists.".into(),
                Err(error) => error,
            });
            self.refresh_bookmark_rows();
        }
    }

    pub(super) fn pick_bookmark_row(&mut self, index: usize) {
        match index {
            BACKGROUND => {}
            PREVIOUS | NEXT => {
                let count = self.bookmark_count();
                let panel = self.bookmark_panel();
                let first = panel.first_link();
                let target = if index == PREVIOUS {
                    first.saturating_sub(ROWS_PER_PAGE)
                } else {
                    first + ROWS_PER_PAGE
                };
                if target < count {
                    panel.selected = target + 1;
                    self.refresh_bookmark_rows();
                }
            }
            0 => match self.bookmark_current() {
                Some((url, true)) => self.remove_bookmark(&url),
                Some((_, false)) => self.save_current_bookmark(),
                None => {}
            },
            index => {
                if let Some(entry) = self
                    .bookmarks
                    .as_ref()
                    .ok()
                    .and_then(|store| store.entries().get(index - 1))
                {
                    let url = entry.url.as_str().to_owned();
                    self.chrome_menu = None;
                    self.navigate(url, true);
                    self.redraw();
                }
            }
        }
    }

    pub(super) fn bookmark_panel_key(&mut self, key: &Key) -> bool {
        let Some(Menu::Bookmarks(panel)) = &self.chrome_menu else {
            return false;
        };
        let first = self.first_bookmark_row();
        let last = self.bookmark_count();
        let selected = panel.selected.clamp(first, last);
        let next = match key {
            Key::Named(NamedKey::Escape) => {
                self.chrome_menu = None;
                self.redraw();
                return true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                let index = super::MENU_ITEMS
                    .iter()
                    .position(|(action, _)| *action == Action::Bookmarks)
                    .unwrap_or_default();
                self.chrome_menu = Some(Menu::Main(index));
                self.chrome_hover = None;
                self.redraw();
                return true;
            }
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp | NamedKey::Tab) => {
                let previous = *key == Key::Named(NamedKey::ArrowUp)
                    || (*key == Key::Named(NamedKey::Tab) && self.modifiers.shift_key());
                if previous {
                    if selected == first {
                        last
                    } else {
                        selected - 1
                    }
                } else if selected == last {
                    first
                } else {
                    selected + 1
                }
            }
            Key::Named(NamedKey::Home) => first,
            Key::Named(NamedKey::End) => last,
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                self.pick_bookmark_row(selected);
                return true;
            }
            Key::Named(NamedKey::Delete) => {
                if let Some(url) = selected.checked_sub(1).and_then(|index| {
                    self.bookmarks
                        .as_ref()
                        .ok()?
                        .entries()
                        .get(index)
                        .map(|entry| entry.url.clone())
                }) {
                    self.remove_bookmark(&url);
                }
                return true;
            }
            _ => return !self.modifiers.control_key() && !self.modifiers.alt_key(),
        };
        self.bookmark_panel().selected = next;
        self.refresh_bookmark_rows();
        true
    }

    pub(super) fn draw_bookmarks(&mut self, canvas: &mut Canvas, width: f32) {
        let Some(Menu::Bookmarks(panel)) = &self.chrome_menu else {
            return;
        };
        let entries = self
            .bookmarks
            .as_ref()
            .ok()
            .map_or(&[][..], |store| store.entries());
        let current = self.bookmark_current();
        let first = panel.first_link();
        let selected = panel
            .selected
            .clamp(self.first_bookmark_row(), entries.len());
        let visible = entries.len().saturating_sub(first).min(ROWS_PER_PAGE);
        let pages = entries.len().div_ceil(ROWS_PER_PAGE);
        let paged = pages > 1;
        let u = self.scale;
        let w = 356.;
        let x = width - w - 12.;
        let y = TOP + 4.;
        let bottom = y + 86. + visible as f32 * 32.;
        let height = bottom - y + if paged { 30. } else { 5. };
        let rect = |x, y, width, height| scaled_rect(u, x, y, width, height);
        rounded_chrome_rect(canvas, rect(x, y, w, height), 10. * u, CHROME_LINE);
        rounded_chrome_rect(
            canvas,
            rect(x + 1., y + 1., w - 2., height - 2.),
            9. * u,
            CHROME_PANEL,
        );
        self.hits.push((
            Rect {
                x,
                y,
                width: w,
                height,
            },
            Action::BookmarkItem(BACKGROUND),
        ));

        canvas.label(
            "Bookmarks",
            (x + 16.) * u,
            (y + 23.) * u,
            16. * u,
            CHROME_TEXT,
            false,
            &mut self.fonts,
        );
        let notice = panel
            .notice
            .as_deref()
            .unwrap_or(if self.bookmarks.is_err() {
                "Bookmarks could not be read. The file was left unchanged."
            } else if entries.is_empty() {
                "No bookmarks saved in this profile."
            } else {
                "Use ↑ / ↓ to select; Delete removes a bookmark"
            });
        let notice = elide(&self.fonts, notice, (w - 32.) * u, 11. * u);
        canvas.label(
            &notice,
            (x + 16.) * u,
            (y + 42.) * u,
            11. * u,
            CHROME_MUTED,
            false,
            &mut self.fonts,
        );

        let mut row =
            |index, left, top, row_width, label: &str, subtitle: Option<&str>, enabled| {
                let row = Rect {
                    x: left,
                    y: top,
                    width: row_width,
                    height: 32.,
                };
                let action = Action::BookmarkItem(index);
                if enabled {
                    self.hits.push((row, action));
                    if selected == index || self.chrome_hover == Some(action) {
                        rounded_chrome_rect(
                            canvas,
                            rect(left, top, row_width, 32.),
                            6. * u,
                            CHROME_HOVER,
                        );
                    }
                    if selected == index {
                        canvas.fill_rect(rect(left + 3., top + 7., 2., 18.), CHROME_MUTED);
                    }
                }
                let label = elide(&self.fonts, label, (row_width - 16.) * u, 14. * u);
                let baseline = top + if subtitle.is_some() { 14. } else { 21. };
                let color = if enabled { CHROME_TEXT } else { CHROME_MUTED };
                canvas.label(
                    &label,
                    (left + 10.) * u,
                    baseline * u,
                    14. * u,
                    color,
                    false,
                    &mut self.fonts,
                );
                if let Some(subtitle) = subtitle {
                    let url = elide(&self.fonts, subtitle, (row_width - 16.) * u, 11. * u);
                    canvas.label(
                        &url,
                        (left + 10.) * u,
                        (top + 28.) * u,
                        11. * u,
                        CHROME_MUTED,
                        false,
                        &mut self.fonts,
                    );
                }
            };
        row(
            0,
            x + 6.,
            y + 49.,
            w - 12.,
            match &current {
                Some((_, false)) => "Bookmark this page (Ctrl+D)",
                Some((_, true)) => "Remove bookmark",
                None if self.bookmarks.is_err() => "Bookmarking unavailable",
                None => "Open an HTTP(S) page to add a bookmark",
            },
            None,
            current.is_some(),
        );
        for (offset, entry) in entries.iter().skip(first).take(visible).enumerate() {
            let url = entry.url.as_str();
            let (label, subtitle) = if entry.title.is_empty() {
                (url, None)
            } else {
                (entry.title.as_str(), Some(url))
            };
            row(
                first + offset + 1,
                x + 6.,
                y + 83. + offset as f32 * 32.,
                w - 12.,
                label,
                subtitle,
                true,
            );
        }
        if paged {
            let page = first / ROWS_PER_PAGE;
            row(
                PREVIOUS,
                x + 6.,
                bottom - 4.,
                122.,
                "‹ Previous",
                None,
                page > 0,
            );
            row(
                NEXT,
                x + w - 102.,
                bottom - 4.,
                96.,
                "Next ›",
                None,
                page + 1 < pages,
            );
            let label = format!("{} / {pages}", page + 1);
            canvas.label(
                &label,
                (x + 149.) * u,
                (bottom + 17.) * u,
                12. * u,
                CHROME_MUTED,
                false,
                &mut self.fonts,
            );
        }
    }
}
