//! Live, read-only Web Storage inspection for the active top-level document.
use super::panel;
use rust_browser_engine::{
    Fonts, LoadedPage, Rect,
    css::Color,
    paint::Canvas,
    web_storage::{StorageInspector, TabStorage, storage_origin},
};
use winit::keyboard::{Key, ModifiersState, NamedKey};

const TAB_HEIGHT: f32 = 29.;
const HEADER: f32 = 25.;
const ROW: f32 = 21.;
const LEFT: f32 = 7.;
const TEXT: Color = Color(237, 236, 240, 255);
const MUTED: Color = Color(181, 179, 189, 255);
const BG: Color = Color(29, 28, 31, 255);
const PANEL: Color = Color(41, 40, 44, 255);
const SELECTED: Color = Color(52, 63, 76, 255);
const ACCENT: Color = Color(151, 201, 245, 255);

#[derive(Default)]
pub(super) struct Application {
    origin: Option<String>,
    page_present: bool,
    session: bool,
    initialized: bool,
    store: Option<StorageInspector>,
    failed: bool,
    top: usize,
    selected: Option<usize>,
    detail_top: usize,
}

#[derive(Clone, Copy)]
struct Layout {
    bounds: Rect,
    local: Rect,
    session: Rect,
    list: Rect,
    detail: Option<Rect>,
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x,
        y,
        width: width.max(0.),
        height: height.max(0.),
    }
}

impl Layout {
    fn new(b: Rect, selection: bool) -> Self {
        let tab_w = (b.width / 2.).min(103.);
        let tabs = TAB_HEIGHT.min(b.height);
        let split = if !selection {
            b.width
        } else if b.width < 460. {
            0.
        } else {
            (b.width * 0.48).round()
        };
        let content = (b.height - tabs).max(0.);
        Self {
            bounds: b,
            local: rect(b.x, b.y, tab_w, tabs),
            session: rect(b.x + tab_w, b.y, tab_w, tabs),
            list: rect(b.x, b.y + tabs, split, content),
            detail: selection.then_some(rect(b.x + split, b.y + tabs, b.width - split, content)),
        }
    }

    fn rows(&self) -> usize {
        ((self.list.height - HEADER) / ROW).floor().max(1.) as usize
    }
    fn list_index(&self, x: f32, y: f32, top: usize) -> Option<usize> {
        (self.list.width > 0. && self.list.contains(x, y) && y >= self.list.y + HEADER)
            .then(|| top + ((y - self.list.y - HEADER) / ROW) as usize)
    }
}

impl Application {
    pub(super) fn sync(&mut self, page: Option<&LoadedPage>, tab: &TabStorage) {
        let url = page.map(|page| url::Url::parse(&page.url).unwrap_or_else(|_| page.base.clone()));
        let origin = url.as_ref().and_then(storage_origin);
        if self.origin != origin {
            self.selected = None;
            self.top = 0;
            self.detail_top = 0;
            self.initialized = false;
        }
        self.page_present = page.is_some();
        self.origin = origin;
        if !self.initialized {
            self.store = None;
            self.failed = false;
            if let Some(url) = &url {
                match tab.inspect_storage(url, self.session) {
                    Ok(area) => self.store = area,
                    Err(_) => self.failed = true,
                }
            }
            self.initialized = true;
        }
        let count = self.count();
        if self.selected.is_some_and(|i| i >= count) {
            self.selected = None;
            self.detail_top = 0;
        }
        self.top = self.top.min(count.saturating_sub(1));
    }

    fn count(&self) -> usize {
        self.store.as_ref().map_or(0, |store| store.usage().0)
    }
    fn switch(&mut self, session: bool) {
        if self.session == session {
            return;
        }
        self.session = session;
        self.store = None;
        self.initialized = false;
        self.top = 0;
        self.detail_top = 0;
        self.selected = None;
    }
    fn select(&mut self, index: usize, b: Rect) {
        let n = self.count();
        if n == 0 {
            return;
        }
        let i = index.min(n - 1);
        self.selected = Some(i);
        self.detail_top = 0;
        let rows = Layout::new(b, true).rows();
        self.top = self.top.min(i).max((i + 1).saturating_sub(rows));
    }
    pub(super) fn click(&mut self, b: Rect, x: f32, y: f32) {
        let l = Layout::new(b, self.selected.is_some());
        if l.local.contains(x, y) {
            self.switch(false);
        } else if l.session.contains(x, y) {
            self.switch(true);
        } else if let Some(detail) = l.detail
            && detail.contains(x, y)
            && y < detail.y + HEADER
        {
            self.selected = None;
            self.detail_top = 0;
        } else if let Some(i) = l.list_index(x, y, self.top)
            && i < self.count()
        {
            self.select(i, b);
        }
    }

    fn detail_lines(&self, area: Rect) -> Vec<String> {
        let Some(entry) = self
            .selected
            .and_then(|i| self.store.as_ref()?.entry(i, true))
        else {
            return vec![];
        };
        let cols = ((area.width - 2. * LEFT) / 8.).floor().max(1.) as usize;
        let mut lines = vec![format!("Key  ·  {} UTF-16 units", entry.key_units)];
        wrap(&entry.key, cols, &mut lines);
        lines.push(String::new());
        lines.push(format!("Value  ·  {} UTF-16 units", entry.value_units));
        wrap(&entry.value, cols, &mut lines);
        if entry.key_units > 512 || entry.value_units > 4096 {
            lines.push("Preview truncated".into());
        }
        lines
    }
    fn shift_detail(&mut self, area: Rect, delta: isize) {
        let visible = ((area.height - HEADER) / ROW).floor().max(1.) as usize;
        let limit = self.detail_lines(area).len().saturating_sub(visible);
        self.detail_top = self.detail_top.saturating_add_signed(delta).min(limit);
    }
    pub(super) fn scroll(&mut self, b: Rect, x: f32, y: f32, delta: f32) {
        let l = Layout::new(b, self.selected.is_some());
        let steps = (delta / ROW).round();
        let steps = if steps == 0. { delta.signum() } else { steps } as isize;
        if let Some(d) = l.detail
            && d.contains(x, y)
        {
            self.shift_detail(d, steps);
        } else if l.list.contains(x, y) {
            self.top = self
                .top
                .saturating_add_signed(steps)
                .min(self.count().saturating_sub(l.rows()));
        }
    }
    pub(super) fn key(&mut self, b: Rect, key: &Key, mods: ModifiersState) -> bool {
        if mods.control_key() || mods.alt_key() || mods.super_key() {
            return false;
        }
        let l = Layout::new(b, self.selected.is_some());
        let i = self.selected.unwrap_or(self.top);
        match key {
            Key::Named(NamedKey::Tab) => self.switch(!self.session),
            Key::Named(NamedKey::Escape | NamedKey::ArrowLeft) => self.selected = None,
            Key::Named(NamedKey::ArrowDown) => {
                self.select(if self.selected.is_some() { i + 1 } else { i }, b)
            }
            Key::Named(NamedKey::ArrowUp) => self.select(i.saturating_sub(1), b),
            Key::Named(NamedKey::Home) => self.select(0, b),
            Key::Named(NamedKey::End) => self.select(self.count().saturating_sub(1), b),
            Key::Named(NamedKey::PageDown | NamedKey::PageUp) => {
                let down = matches!(key, Key::Named(NamedKey::PageDown));
                if let Some(d) = l.detail {
                    self.shift_detail(d, l.rows() as isize * if down { 1 } else { -1 });
                } else {
                    self.select(
                        if down {
                            i + l.rows()
                        } else {
                            i.saturating_sub(l.rows())
                        },
                        b,
                    );
                }
            }
            Key::Character(s) if s.eq_ignore_ascii_case("r") => self.initialized = false,
            _ => return false,
        }
        true
    }

    pub(super) fn draw(&self, canvas: &mut Canvas, fonts: &mut Fonts, b: Rect, scale: f32) {
        let layout = Layout::new(b, self.selected.is_some());
        let mut p = Painter {
            canvas,
            fonts,
            scale,
        };
        p.fill(b, BG);
        let toolbar = rect(b.x, b.y, b.width, TAB_HEIGHT.min(b.height));
        p.fill(toolbar, PANEL);
        for (tab, label, on) in [
            (layout.local, "Local storage", !self.session),
            (layout.session, "Session storage", self.session),
        ] {
            if on {
                p.fill(tab, SELECTED);
                p.fill(rect(tab.x, tab.y + tab.height - 2., tab.width, 2.), ACCENT);
            }
            p.text(tab, label, TEXT, on);
        }
        let origin_x = layout.session.x + layout.session.width + 3.;
        let usage = self.store.as_ref().map(StorageInspector::usage);
        let origin = self.origin.as_deref().unwrap_or("Opaque origin");
        let title = usage.map_or_else(
            || origin.to_owned(),
            |(n, bytes)| format!("{origin}   ·   {n} entries   ·   {bytes} bytes"),
        );
        p.text(
            rect(origin_x, b.y, b.x + b.width - origin_x, toolbar.height),
            &title,
            MUTED,
            false,
        );
        if layout.list.width > 0. {
            let list = layout.list;
            let count = usage.map_or(0, |pair| pair.0);
            let key_width = list.width * 0.43;
            p.text(rect(list.x, list.y, key_width, HEADER), "Key", MUTED, true);
            p.text(
                rect(list.x + key_width, list.y, list.width - key_width, HEADER),
                "Value",
                MUTED,
                true,
            );
            if count == 0 {
                let message = if !self.page_present {
                    "No document loaded"
                } else if self.origin.is_none() {
                    "Storage is unavailable for this origin"
                } else if self.failed {
                    "Could not load storage. Press R to retry."
                } else {
                    "No stored keys"
                };
                p.text(
                    rect(list.x, list.y + HEADER, list.width, ROW),
                    message,
                    MUTED,
                    false,
                );
            }
            if let Some(store) = &self.store {
                for (line, i) in (self.top..count).take(layout.rows() + 1).enumerate() {
                    let y = list.y + HEADER + line as f32 * ROW;
                    let row = rect(
                        list.x,
                        y,
                        list.width,
                        ROW.min((list.y + list.height - y).max(0.)),
                    );
                    if row.height < 1. {
                        break;
                    }
                    if self.selected == Some(i) {
                        p.fill(row, SELECTED);
                    }
                    if let Some(entry) = store.entry(i, false) {
                        p.text(
                            rect(row.x, y, key_width, row.height),
                            &single_line(&entry.key),
                            ACCENT,
                            false,
                        );
                        p.text(
                            rect(row.x + key_width, y, row.width - key_width, row.height),
                            &single_line(&entry.value),
                            TEXT,
                            false,
                        );
                    }
                }
            }
        }
        if let Some(d) = layout.detail {
            p.fill(d, Color(35, 34, 38, 255));
            p.text(
                rect(d.x, d.y, d.width, HEADER),
                "‹  All entries",
                ACCENT,
                true,
            );
            let n = ((d.height - HEADER) / ROW).floor().max(0.) as usize;
            let lines = self.detail_lines(d);
            let start = self.detail_top.min(lines.len().saturating_sub(n));
            for (i, item) in lines.iter().skip(start).take(n).enumerate() {
                p.text(
                    rect(d.x, d.y + HEADER + i as f32 * ROW, d.width, ROW),
                    item,
                    TEXT,
                    false,
                );
            }
        }
        p.canvas.clip(panel::scaled(layout.bounds, scale));
    }
}

fn single_line(s: &str) -> String {
    s.chars().map(|ch| if ch.is_control() || matches!(ch,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{2028}'..='\u{202e}'|'\u{2066}'..='\u{2069}') {' '}else{ch}).collect()
}

fn wrap(text: &str, columns: usize, lines: &mut Vec<String>) {
    for line in text.split('\n') {
        let mut segment = String::new();
        for (i, ch) in single_line(line).chars().enumerate() {
            if i != 0 && i.is_multiple_of(columns) {
                lines.push(std::mem::take(&mut segment));
            }
            segment.push(ch);
        }
        lines.push(segment);
    }
}

struct Painter<'a> {
    canvas: &'a mut Canvas,
    fonts: &'a mut Fonts,
    scale: f32,
}
impl Painter<'_> {
    fn fill(&mut self, r: Rect, color: Color) {
        if r.width > 0. && r.height > 0. {
            let px = panel::scaled(r, self.scale);
            self.canvas.clip(px);
            self.canvas.fill_rect(px, color);
        }
    }
    fn text(&mut self, r: Rect, text: &str, color: Color, bold: bool) {
        if r.width <= 0. || r.height <= 0. {
            return;
        }
        self.canvas.clip(panel::scaled(r, self.scale));
        self.canvas.label(
            text,
            (r.x + LEFT) * self.scale,
            (r.y + r.height / 2. + 4.) * self.scale,
            12. * self.scale,
            color,
            bold,
            self.fonts,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_browser_engine::script::ScriptRuntime;
    #[test]
    fn storage_controls_and_live_updates_work_at_narrow_width() {
        let mut panel = Application::default();
        let page = LoadedPage::from_html("https://storage.test/a", "<title>t</title>");
        let b = rect(12., 80., 330., 150.);
        let storage = TabStorage::default();
        let mut runtime = ScriptRuntime::with_storage(&page, &storage).unwrap();
        runtime
            .eval("localStorage.x='local';sessionStorage.y='session'")
            .unwrap();
        panel.sync(Some(&page), &storage);
        panel.click(b, b.x + 10., b.y + TAB_HEIGHT + HEADER + 4.);
        assert_eq!(panel.selected, Some(0));
        let narrow = Layout::new(b, true);
        assert_eq!(narrow.list.width, 0.);
        assert!(
            panel
                .detail_lines(narrow.detail.unwrap())
                .contains(&"local".to_owned())
        );
        panel.key(b, &Key::Named(NamedKey::Tab), ModifiersState::empty());
        panel.sync(Some(&page), &storage);
        assert!(panel.session && panel.selected.is_none());
        panel.key(b, &Key::Named(NamedKey::ArrowDown), ModifiersState::empty());
        assert!(
            panel
                .detail_lines(narrow.detail.unwrap())
                .contains(&"session".to_owned())
        );
        runtime.eval("sessionStorage.clear()").unwrap();
        panel.sync(Some(&page), &storage);
        assert!(panel.selected.is_none());
    }
}
