pub(super) mod library;

use super::{
    Action, App, Canvas, Color, Key, NamedKey, Rect,
    bookmarks::{Bookmark, MAX_TITLE_CHARS, page_url},
};
use library::{
    ACCENT, CARD, Field, HOVER, LINE, Layout, MUTED, PAGE, Page, Paint, SOFT, TEXT, bounds,
};
use url::Url;

const BACKGROUND: usize = 0;
const CURRENT: usize = 1;
const FILTER: usize = 2;
const CLOSE: usize = 3;
const PREVIOUS: usize = 4;
const NEXT: usize = 5;
const EDIT_NAME: usize = 6;
const CANCEL: usize = 7;
const SAVE: usize = 8;
const ENTRY: usize = 9;
const HEADER_EXTRA: f32 = 28.;

#[derive(Clone, Copy)]
enum RowCommand {
    Open = 0,
    Edit = 1,
    Remove = 2,
}

impl RowCommand {
    fn id(self, index: usize) -> usize {
        ENTRY + index * 3 + self as usize
    }

    fn decode(id: usize) -> (usize, Self) {
        let row = id - ENTRY;
        let command = match row % 3 {
            0 => Self::Open,
            1 => Self::Edit,
            _ => Self::Remove,
        };
        (row / 3, command)
    }
}

pub(super) enum Menu {
    Main(usize),
    Bookmarks(BookmarkPanel),
    Tabs(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Current,
    Row,
    Filter,
    Close,
}

struct Rename {
    url: Url,
    title: Field,
    focus: usize,
    error: Option<String>,
}

pub(super) struct BookmarkPanel {
    selected: usize, // Position in the filtered list, not in the store.
    focus: Focus,
    filter: Field,
    rename: Option<Rename>,
    notice: Option<String>,
}

impl BookmarkPanel {
    fn matching(&self, entries: &[Bookmark]) -> Vec<usize> {
        entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                self.filter
                    .matches(&[&entry.title, entry.url.as_str()])
                    .then_some(index)
            })
            .collect()
    }

    fn selected_row(&self, matches: &[usize]) -> Option<usize> {
        matches
            .get(self.selected.min(matches.len().saturating_sub(1)))
            .copied()
    }
}

pub(super) fn library_body(app: &App, width: f32, height: f32) -> Rect {
    let top = app.window_regions(app.active).page.y;
    bounds(
        0.,
        top,
        width,
        height - top - app.tabs[app.active].footer_height(),
    )
}

pub(super) fn library_window(app: &App) -> Rect {
    let (width, height) = app.window.as_ref().map_or((1280., 800.), |window| {
        let size = window.inner_size();
        (
            size.width as f32 / app.scale,
            size.height as f32 / app.scale,
        )
    });
    library_body(app, width, height)
}

impl App {
    pub(super) fn show_bookmarks(&mut self) {
        self.select_popup = None;
        self.dismiss_history_menu();
        self.set_dock_focus(false);
        let focus = if self.bookmark_current().is_some() {
            Focus::Current
        } else if self.bookmark_entries().is_empty() {
            Focus::Filter
        } else {
            Focus::Row
        };
        self.chrome_menu = Some(Menu::Bookmarks(BookmarkPanel {
            selected: 0,
            focus,
            filter: Field::default(),
            rename: None,
            notice: None,
        }));
        self.refresh_bookmark_rows();
    }

    pub(super) fn bookmark_entries(&self) -> &[Bookmark] {
        self.bookmarks
            .as_ref()
            .map_or(&[][..], |store| store.entries())
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

    pub(super) fn bookmark_current(&self) -> Option<(Url, bool)> {
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
        let saved = self
            .bookmark_current()
            .and_then(|(url, saved)| saved.then_some(url));
        let panel = self.bookmark_panel();
        panel.notice = Some(result.map_or_else(|message| message, str::to_owned));
        if let Some(url) = saved
            && let Some(Menu::Bookmarks(panel)) = &self.chrome_menu
            && let Some(position) = panel
                .matching(self.bookmark_entries())
                .iter()
                .position(|&index| self.bookmark_entries()[index].url == url)
        {
            let panel = self.bookmark_panel();
            panel.selected = position;
            panel.focus = Focus::Row;
        }
        self.refresh_bookmark_rows();
    }

    fn remove_bookmark(&mut self, url: &Url) {
        if let Ok(store) = &mut self.bookmarks {
            let result = store.remove(url);
            self.bookmark_panel().notice = Some(match result {
                Ok(true) => "Bookmark removed.".into(),
                Ok(false) => "Bookmark no longer exists.".into(),
                Err(error) => error,
            });
            self.refresh_bookmark_rows();
        }
    }

    fn rename_bookmark(&mut self) {
        let Some(Menu::Bookmarks(panel)) = &mut self.chrome_menu else {
            return;
        };
        let Some(rename) = &mut panel.rename else {
            return;
        };
        let result = self
            .bookmarks
            .as_mut()
            .map_err(|_| "Bookmarks are unavailable.".to_owned())
            .and_then(|store| store.rename(&rename.url, rename.title.text()));
        match result {
            Ok(()) => {
                panel.rename = None;
                panel.notice = Some("Bookmark renamed.".into());
            }
            Err(error) => {
                rename.error = Some(error);
                rename.focus = EDIT_NAME;
            }
        }
        self.refresh_bookmark_rows();
    }

    pub(super) fn pick_bookmark_row(&mut self, index: usize) {
        let Some(Menu::Bookmarks(panel)) = &self.chrome_menu else {
            return;
        };
        if panel.rename.is_some() {
            match index {
                SAVE => self.rename_bookmark(),
                CANCEL => {
                    self.bookmark_panel().rename = None;
                    self.refresh_bookmark_rows();
                }
                EDIT_NAME => {
                    if let Some(rename) = &mut self.bookmark_panel().rename {
                        rename.focus = EDIT_NAME;
                    }
                    self.redraw();
                }
                _ => {}
            }
            return;
        }
        match index {
            BACKGROUND => {}
            CLOSE => {
                self.chrome_menu = None;
                self.refresh_bookmark_rows();
            }
            FILTER => {
                self.bookmark_panel().focus = Focus::Filter;
                self.redraw();
            }
            PREVIOUS | NEXT => {
                let matches = panel.matching(self.bookmark_entries());
                let capacity = Layout::new(library_window(self), HEADER_EXTRA).capacity;
                let page = Page::for_selection(panel.selected, matches.len(), capacity);
                let target = if index == PREVIOUS {
                    page.first.saturating_sub(capacity)
                } else {
                    page.first + capacity
                };
                if target < matches.len() {
                    let panel = self.bookmark_panel();
                    panel.selected = target;
                    panel.focus = Focus::Row;
                    self.refresh_bookmark_rows();
                }
            }
            CURRENT => match self.bookmark_current() {
                Some((url, true)) => self.remove_bookmark(&url),
                Some((_, false)) => self.save_current_bookmark(),
                None => {}
            },
            index if index >= ENTRY => {
                let (index, command) = RowCommand::decode(index);
                let Some(entry) = self.bookmark_entries().get(index) else {
                    return;
                };
                match command {
                    RowCommand::Open => {
                        let url = entry.url.as_str().to_owned();
                        self.chrome_menu = None;
                        self.navigate(url, true);
                        self.refresh_bookmark_rows();
                    }
                    RowCommand::Edit => {
                        let rename = Rename {
                            url: entry.url.clone(),
                            title: Field::selecting(entry.title.clone()),
                            focus: EDIT_NAME,
                            error: None,
                        };
                        let selected = panel
                            .matching(self.bookmark_entries())
                            .iter()
                            .position(|&i| i == index);
                        let panel = self.bookmark_panel();
                        if let Some(selected) = selected {
                            panel.selected = selected;
                        }
                        panel.focus = Focus::Row;
                        panel.rename = Some(rename);
                        self.refresh_bookmark_rows();
                    }
                    RowCommand::Remove => {
                        let url = entry.url.clone();
                        self.remove_bookmark(&url);
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn bookmark_panel_key(&mut self, key: &Key) -> bool {
        let Some(Menu::Bookmarks(panel)) = &mut self.chrome_menu else {
            return false;
        };
        if let Some(rename) = &mut panel.rename {
            match key {
                Key::Named(NamedKey::Escape) => self.pick_bookmark_row(CANCEL),
                Key::Named(NamedKey::Tab) => {
                    let step = if self.modifiers.shift_key() { 2 } else { 1 };
                    rename.focus = EDIT_NAME + (rename.focus - EDIT_NAME + step) % 3;
                    self.redraw();
                }
                Key::Named(NamedKey::Enter) => {
                    let action = if rename.focus == CANCEL { CANCEL } else { SAVE };
                    self.pick_bookmark_row(action);
                }
                Key::Named(NamedKey::Space) if rename.focus != EDIT_NAME => {
                    let action = rename.focus;
                    self.pick_bookmark_row(action);
                }
                _ if rename.focus == EDIT_NAME => {
                    if rename
                        .title
                        .key(key, self.modifiers, &mut self.clipboard, MAX_TITLE_CHARS)
                    {
                        rename.error = None;
                        self.redraw();
                    }
                }
                _ => {}
            }
            return true;
        }

        let ctrl = self.modifiers.control_key() && !self.modifiers.alt_key();
        if ctrl && matches!(key, Key::Character(c) if c.eq_ignore_ascii_case("f")) {
            panel.focus = Focus::Filter;
            self.redraw();
            return true;
        }
        if panel.focus == Focus::Filter
            && panel
                .filter
                .key(key, self.modifiers, &mut self.clipboard, 120)
        {
            panel.selected = 0;
            self.refresh_bookmark_rows();
            return true;
        }

        let Some(Menu::Bookmarks(panel)) = &self.chrome_menu else {
            unreachable!()
        };
        let matches = panel.matching(self.bookmark_entries());
        let can_bookmark = self.bookmark_current().is_some();
        let has_rows = !matches.is_empty();
        let selected = panel.selected.min(matches.len().saturating_sub(1));
        match key {
            Key::Named(NamedKey::Escape) => self.pick_bookmark_row(CLOSE),
            Key::Named(NamedKey::ArrowLeft) => {
                let index = super::MENU_ITEMS
                    .iter()
                    .position(|(action, _)| *action == Action::Bookmarks)
                    .unwrap_or_default();
                self.chrome_menu = Some(Menu::Main(index));
                self.refresh_bookmark_rows();
            }
            Key::Named(NamedKey::Tab) if !ctrl => {
                let order = [
                    Some(Focus::Filter),
                    can_bookmark.then_some(Focus::Current),
                    has_rows.then_some(Focus::Row),
                    Some(Focus::Close),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
                let from = order
                    .iter()
                    .position(|&focus| focus == panel.focus)
                    .unwrap_or(0);
                let step = if self.modifiers.shift_key() {
                    order.len() - 1
                } else {
                    1
                };
                self.bookmark_panel().focus = order[(from + step) % order.len()];
                self.redraw();
            }
            Key::Named(
                NamedKey::ArrowDown | NamedKey::ArrowUp | NamedKey::Home | NamedKey::End,
            ) if has_rows => {
                let previous = *key == Key::Named(NamedKey::ArrowUp);
                let panel = self.bookmark_panel();
                let (focus, target) = match key {
                    Key::Named(NamedKey::Home) => (Focus::Row, 0),
                    Key::Named(NamedKey::End) => (Focus::Row, matches.len() - 1),
                    _ if panel.focus != Focus::Row => {
                        (Focus::Row, if previous { matches.len() - 1 } else { 0 })
                    }
                    _ if previous && selected == 0 && can_bookmark => (Focus::Current, 0),
                    _ if !previous && selected + 1 == matches.len() && can_bookmark => {
                        (Focus::Current, selected)
                    }
                    _ => {
                        let step = if previous { matches.len() - 1 } else { 1 };
                        (Focus::Row, (selected + step) % matches.len())
                    }
                };
                panel.focus = focus;
                panel.selected = target;
                self.refresh_bookmark_rows();
            }
            Key::Named(NamedKey::PageUp | NamedKey::PageDown) => {
                self.pick_bookmark_row(if *key == Key::Named(NamedKey::PageUp) {
                    PREVIOUS
                } else {
                    NEXT
                });
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let index = match panel.focus {
                    Focus::Current => Some(CURRENT),
                    Focus::Filter | Focus::Row => {
                        panel.selected_row(&matches).map(|i| RowCommand::Open.id(i))
                    }
                    Focus::Close => Some(CLOSE),
                };
                if let Some(index) = index {
                    self.pick_bookmark_row(index);
                }
            }
            Key::Named(NamedKey::F2 | NamedKey::Delete) if panel.focus == Focus::Row => {
                let command = if *key == Key::Named(NamedKey::F2) {
                    RowCommand::Edit
                } else {
                    RowCommand::Remove
                };
                if let Some(index) = panel.selected_row(&matches) {
                    self.pick_bookmark_row(command.id(index));
                }
            }
            _ => return !self.modifiers.control_key() && !self.modifiers.alt_key(),
        }
        true
    }

    pub(super) fn draw_bookmarks(&mut self, canvas: &mut Canvas, width: f32) {
        let Some(Menu::Bookmarks(panel)) = &self.chrome_menu else {
            return;
        };
        let current = self.bookmark_current();
        let body = library_body(self, width, canvas.height as f32 / self.scale);
        let layout = Layout::new(body, HEADER_EXTRA);
        // Borrow fields separately so the live store is read directly while painting.
        let entries = self
            .bookmarks
            .as_ref()
            .map_or(&[][..], |store| store.entries());
        let matches = panel.matching(entries);
        let page = Page::for_selection(panel.selected, matches.len(), layout.capacity);
        let hovered = self.chrome_hover;
        let mut hit = |rect, id| self.hits.push((rect, Action::BookmarkItem(id)));
        let hot = |id| hovered == Some(Action::BookmarkItem(id));
        let mut paint = Paint {
            canvas,
            fonts: &mut self.fonts,
            scale: self.scale,
        };
        paint.clip(body);
        paint.fill(body, PAGE);
        paint.heading(&layout, "Bookmarks", "Saved pages");
        paint.field(
            layout.filter,
            &panel.filter,
            "Search bookmarks",
            panel.focus == Focus::Filter,
            true,
            body,
        );
        paint.button(
            "×",
            layout.close,
            hot(CLOSE),
            panel.focus == Focus::Close,
            false,
        );
        hit(body, BACKGROUND);
        hit(layout.filter, FILTER);
        hit(layout.close, CLOSE);

        let y = layout.list.y - 36.;
        let notice = panel
            .notice
            .as_deref()
            .unwrap_or(if self.bookmarks.is_err() {
                "Bookmarks could not be read. The file was left unchanged."
            } else if entries.is_empty() {
                ""
            } else {
                "F2 renames · Delete removes"
            });
        let mut notice_width = layout.list.width;
        if let Some((_, saved)) = current {
            let w = 158_f32.min(layout.list.width * 0.46);
            let button = bounds(layout.list.x + layout.list.width - w, y, w, 29.);
            paint.button(
                if saved {
                    "Remove current"
                } else {
                    "Bookmark this page"
                },
                button,
                hot(CURRENT),
                panel.focus == Focus::Current,
                false,
            );
            hit(button, CURRENT);
            notice_width -= w + 8.;
        }
        paint.text(
            notice,
            layout.list.x,
            y + 20.,
            notice_width,
            12.,
            MUTED,
            false,
        );
        paint.border(layout.card(page.limit), 10., CARD, LINE);
        if page.limit == 0 {
            let empty = if self.bookmarks.is_err() {
                "Saved pages are unavailable."
            } else if entries.is_empty() {
                "No saved pages. Use Ctrl+D on a web page to save it."
            } else {
                "No matching bookmarks."
            };
            paint.text(
                empty,
                layout.list.x + 13.,
                layout.list.y + 32.,
                layout.list.width - 26.,
                13.,
                MUTED,
                false,
            );
        }
        for (offset, &index) in matches.iter().skip(page.first).take(page.limit).enumerate() {
            let entry = &entries[index];
            let row = layout.row(offset);
            let open = RowCommand::Open.id(index);
            let edit = RowCommand::Edit.id(index);
            let remove = RowCommand::Remove.id(index);
            let remove_rect = bounds(row.x + row.width - 40., row.y + 10., 31., 31.);
            let edit_rect = bounds(remove_rect.x - 36., remove_rect.y, 31., 31.);
            let main = bounds(row.x + 5., row.y + 5., edit_rect.x - row.x - 10., 42.);
            let focused = panel.focus == Focus::Row && panel.selected_row(&matches) == Some(index);
            if focused {
                paint.rounded(main, 6., HOVER);
                paint.fill(bounds(main.x + 2., main.y + 8., 2., 26.), ACCENT);
            }
            if offset + 1 < page.limit {
                paint.fill(bounds(row.x, row.y + row.height - 1., row.width, 1.), LINE);
            }
            let url = entry.url.as_str();
            let title = if entry.title.is_empty() {
                entry.url.host_str().unwrap_or(url)
            } else {
                &entry.title
            };
            let badge = entry
                .url
                .host_str()
                .and_then(|host| host.trim_start_matches("www.").chars().next())
                .map_or_else(
                    || "•".to_owned(),
                    |initial| initial.to_uppercase().collect(),
                );
            paint.badge(&badge, row.x + 12., row.y + 16.);
            let x = row.x + 41.;
            let text_width = edit_rect.x - x - 7.;
            paint.text(
                title,
                x,
                row.y + 22.,
                text_width,
                13.,
                if hot(open) { ACCENT } else { TEXT },
                false,
            );
            paint.text(url, x, row.y + 39., text_width, 11., MUTED, false);
            for (rect, command, glyph) in [(edit_rect, edit, "✎"), (remove_rect, remove, "×")] {
                if hot(command) {
                    paint.rounded(rect, 6., HOVER);
                }
                paint.center(glyph, rect, 17., if hot(command) { TEXT } else { MUTED });
                hit(rect, command);
            }
            hit(main, open);
        }
        if page.count > 1 {
            let (previous, next) = layout.pager(page.limit);
            paint.page_count(page, layout.list, previous.y);
            if page.number > 0 {
                paint.button("‹ Prev", previous, hot(PREVIOUS), false, false);
                hit(previous, PREVIOUS);
            }
            if page.number + 1 < page.count {
                paint.button("Next ›", next, hot(NEXT), false, false);
                hit(next, NEXT);
            }
        }
        if let Some(rename) = &panel.rename {
            hit(body, BACKGROUND);
            draw_rename(&mut paint, body, rename, hot, hit);
        }
        paint.reset_clip();
    }
}

fn draw_rename(
    paint: &mut Paint<'_>,
    body: Rect,
    rename: &Rename,
    hot: impl Fn(usize) -> bool,
    mut hit: impl FnMut(Rect, usize),
) {
    paint.fill(body, Color(16, 21, 26, 140));
    let width = (body.width - 24.).clamp(0., 465.);
    let height = 178.;
    let x = body.x + (body.width - width) / 2.;
    let y = body.y + ((body.height - height) / 2.).max(0.);
    let dialog = bounds(x, y, width, height);
    paint.border(dialog, 13., CARD, LINE);
    paint.text(
        "Edit bookmark",
        x + 22.,
        y + 35.,
        width - 44.,
        19.,
        TEXT,
        true,
    );
    paint.text("Name", x + 22., y + 62., width - 44., 13., TEXT, false);
    let field = bounds(x + 22., y + 72., width - 44., 34.);
    paint.field(
        field,
        &rename.title,
        "",
        rename.focus == EDIT_NAME,
        false,
        body,
    );
    let save = bounds(x + width - 92., y + 125., 70., 34.);
    let cancel = bounds(save.x - 82., save.y, 75., 34.);
    paint.button("Cancel", cancel, hot(CANCEL), rename.focus == CANCEL, false);
    let save_focus = rename.focus == SAVE;
    paint.button("Save", save, hot(SAVE), save_focus, true);
    if save_focus {
        paint.fill(bounds(save.x + 16., save.y + 27., 38., 1.), SOFT);
    }
    if let Some(error) = &rename.error {
        paint.text(error, x + 22., y + 121., width - 44., 11., ACCENT, false);
    }
    hit(field, EDIT_NAME);
    hit(cancel, CANCEL);
    hit(save, SAVE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_commands_point_to_the_underlying_store_entry() {
        let entries = [
            Bookmark {
                url: Url::parse("https://example.org").unwrap(),
                title: "A bookmark".into(),
            },
            Bookmark {
                url: Url::parse("https://example.com/notes").unwrap(),
                title: "Łódź Guide".into(),
            },
        ];
        let mut panel = BookmarkPanel {
            selected: 0,
            focus: Focus::Row,
            filter: Field::selecting("  ŁÓDŹ ".into()),
            rename: None,
            notice: None,
        };
        let matches = panel.matching(&entries);
        assert_eq!(matches, [1]);
        let id = RowCommand::Edit.id(panel.selected_row(&matches).unwrap());
        assert!(matches!(RowCommand::decode(id), (1, RowCommand::Edit)));

        panel.filter = Field::selecting("EXAMPLE.ORG".into());
        assert_eq!(panel.matching(&entries), [0]);
    }
}
