//! Shared native list and field styling for Satin's library views.
use crate::app::{
    Canvas, Color, Fonts, Key, ModifiersState, NamedKey, Rect,
    address_bar::{AddressBar, Movement},
    elide, rounded_chrome_rect, scaled_rect,
};

pub(crate) const PAGE: Color = Color(25, 25, 28, 255);
pub(crate) const CARD: Color = Color(41, 40, 44, 255);
pub(crate) const TEXT: Color = Color(240, 237, 240, 255);
pub(crate) const MUTED: Color = Color(184, 175, 181, 255);
pub(crate) const LINE: Color = Color(62, 58, 66, 255);
pub(crate) const HOVER: Color = Color(51, 48, 56, 255);
pub(crate) const ACCENT: Color = Color(196, 163, 244, 255);
pub(crate) const SOFT: Color = Color(68, 61, 80, 255);
pub(crate) const ROW: f32 = 52.;

pub(crate) fn bounds(x: f32, y: f32, w: f32, h: f32) -> Rect {
    scaled_rect(1., x, y, w.max(0.), h.max(0.))
}

pub(crate) struct Layout {
    pub body: Rect,
    pub filter: Rect,
    pub close: Rect,
    pub list: Rect,
    pub heading: f32,
    pub heading_size: f32,
    pub capacity: usize,
}

impl Layout {
    pub fn new(body: Rect, extra_header: f32) -> Self {
        let width = body.width.min(1110.);
        let gutter = (width * 0.05).clamp(12., 55.).min(width * 0.1);
        let x = body.x + (body.width - width) * 0.5 + gutter;
        let right = body.x + (body.width + width) * 0.5 - gutter;
        let heading_size = (width * 0.04).clamp(27., 45.);
        let heading = body.y + if body.height < 400. { 25. } else { 44. } + heading_size;
        let filter_width = (width * 0.32)
            .clamp(118., 240.)
            .min((right - x - 42.).max(0.));
        let list = bounds(x, heading + 50. + extra_header, right - x, 0.);
        let available = body.y + body.height - list.y - 40.;
        Self {
            body,
            filter: bounds(right - 42. - filter_width, heading - 17., filter_width, 34.),
            close: bounds(right - 34., heading - 17., 34., 34.),
            list,
            heading,
            heading_size,
            capacity: ((available / ROW).floor() as usize).max(1),
        }
    }

    pub fn row(&self, offset: usize) -> Rect {
        bounds(
            self.list.x + 1.,
            self.list.y + 1. + offset as f32 * ROW,
            self.list.width - 2.,
            ROW,
        )
    }

    pub fn card(&self, visible: usize) -> Rect {
        bounds(
            self.list.x,
            self.list.y,
            self.list.width,
            ROW * visible.max(1) as f32 + 2.,
        )
    }

    pub fn pager(&self, visible: usize) -> (Rect, Rect) {
        let card = self.card(visible);
        let y = card.y + card.height + 5.;
        let w = 76_f32.min(card.width * 0.24);
        (
            bounds(card.x, y, w, 28.),
            bounds(card.x + card.width - w, y, w, 28.),
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Page {
    pub first: usize,
    pub limit: usize,
    pub number: usize,
    pub count: usize,
}

impl Page {
    pub fn for_selection(selection: usize, total: usize, capacity: usize) -> Self {
        let count = total.div_ceil(capacity);
        let number = (selection / capacity).min(count.saturating_sub(1));
        Self {
            first: number * capacity,
            limit: total.saturating_sub(number * capacity).min(capacity),
            number,
            count,
        }
    }
}

#[derive(Default)]
pub(crate) struct Field(AddressBar);

impl Field {
    pub fn selecting(text: String) -> Self {
        let mut field = Self::default();
        field.0.set_text(text);
        field.0.select_all();
        field
    }

    pub fn text(&self) -> &str {
        self.0.text()
    }

    pub fn matches(&self, labels: &[&str]) -> bool {
        let query = self.text().trim().to_lowercase();
        query.is_empty()
            || labels
                .iter()
                .any(|label| label.to_lowercase().contains(&query))
    }

    pub fn key(
        &mut self,
        key: &Key,
        modifiers: ModifiersState,
        clipboard: &mut Option<arboard::Clipboard>,
        limit: usize,
    ) -> bool {
        let ctrl = modifiers.control_key() && !modifiers.alt_key();
        let extend = modifiers.shift_key();
        match key {
            Key::Named(NamedKey::Backspace) => self.0.delete(true),
            Key::Named(NamedKey::Delete) => self.0.delete(false),
            Key::Named(NamedKey::ArrowLeft) => self.0.move_caret(Movement::Left, extend),
            Key::Named(NamedKey::ArrowRight) => self.0.move_caret(Movement::Right, extend),
            Key::Named(NamedKey::Home) => self.0.move_caret(Movement::Start, extend),
            Key::Named(NamedKey::End) => self.0.move_caret(Movement::End, extend),
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => self.0.select_all(),
            Key::Character(c)
                if ctrl && (c.eq_ignore_ascii_case("c") || c.eq_ignore_ascii_case("x")) =>
            {
                if let Some(text) = self.0.selected_text()
                    && let Some(clipboard) = clipboard
                    && clipboard.set_text(text.to_owned()).is_ok()
                    && c.eq_ignore_ascii_case("x")
                {
                    self.0.delete_selection();
                }
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => {
                if let Some(text) = clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
                    self.insert(&text, limit);
                }
            }
            Key::Character(c)
                if !ctrl
                    && !modifiers.super_key()
                    && (!modifiers.alt_key() || modifiers.control_key()) =>
            {
                self.insert(c, limit)
            }
            Key::Named(NamedKey::Space) if !ctrl && !modifiers.alt_key() => self.insert(" ", limit),
            _ => return false,
        }
        true
    }

    fn insert(&mut self, text: &str, limit: usize) {
        let old_length = self.0.text().chars().count();
        let replacing = self
            .0
            .selected_text()
            .map_or(0, |text| text.chars().count());
        let text: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(limit.saturating_sub(old_length - replacing))
            .collect();
        self.0.insert_text(&text);
    }
}

pub(crate) struct Paint<'a> {
    pub canvas: &'a mut Canvas,
    pub fonts: &'a mut Fonts,
    pub scale: f32,
}

impl Paint<'_> {
    fn physical(&self, r: Rect) -> Rect {
        scaled_rect(self.scale, r.x, r.y, r.width, r.height)
    }

    pub fn clip(&mut self, r: Rect) {
        self.canvas.clip(self.physical(r));
    }

    pub fn reset_clip(&mut self) {
        self.canvas.clip(bounds(
            0.,
            0.,
            self.canvas.width as f32,
            self.canvas.height as f32,
        ));
    }

    pub fn fill(&mut self, r: Rect, color: Color) {
        self.canvas.fill_rect(self.physical(r), color);
    }

    pub fn rounded(&mut self, r: Rect, radius: f32, color: Color) {
        if r.width * self.scale >= 1. && r.height * self.scale >= 1. {
            rounded_chrome_rect(self.canvas, self.physical(r), radius * self.scale, color);
        }
    }

    pub fn border(&mut self, r: Rect, radius: f32, color: Color, border: Color) {
        self.rounded(r, radius, border);
        self.rounded(
            bounds(r.x + 1., r.y + 1., r.width - 2., r.height - 2.),
            radius - 1.,
            color,
        );
    }

    pub fn text(&mut self, s: &str, x: f32, y: f32, w: f32, size: f32, color: Color, bold: bool) {
        if w <= 0. {
            return;
        }
        let u = self.scale;
        let s = elide(self.fonts, s, w * u, size * u);
        self.canvas
            .label(&s, x * u, y * u, size * u, color, bold, self.fonts);
    }

    fn advance(&self, s: &str, size: f32) -> f32 {
        self.fonts
            .shape(s, size * self.scale, false, false, false)
            .advance
            / self.scale
    }

    pub fn center(&mut self, label: &str, r: Rect, size: f32, color: Color) {
        let width = self.advance(label, size);
        let x = ((r.width - width) * 0.5).max(3.);
        self.text(
            label,
            r.x + x,
            r.y + (r.height + size * 0.72) * 0.5,
            r.width - x,
            size,
            color,
            false,
        );
    }

    pub fn heading(&mut self, layout: &Layout, title: &str, subtitle: &str) {
        let x = layout.list.x;
        let w = (layout.filter.x - x - 12.).max(0.);
        self.text(title, x, layout.heading, w, layout.heading_size, TEXT, true);
        self.text(subtitle, x, layout.heading + 20., w, 13., MUTED, false);
    }

    pub fn button(&mut self, label: &str, r: Rect, hovered: bool, focused: bool, primary: bool) {
        let fill = if primary {
            ACCENT
        } else if hovered {
            HOVER
        } else {
            CARD
        };
        self.border(r, 6., fill, if primary || focused { ACCENT } else { LINE });
        self.center(label, r, 13., if primary { PAGE } else { TEXT });
    }

    pub fn field(
        &mut self,
        r: Rect,
        field: &Field,
        placeholder: &str,
        focus: bool,
        filter: bool,
        clip: Rect,
    ) {
        self.border(
            r,
            7.,
            if filter { CARD } else { PAGE },
            if focus { ACCENT } else { LINE },
        );
        let x = r.x + if filter { 29. } else { 10. };
        let width = (r.x + r.width - x - 7.).max(0.);
        let y = r.y + 22.;
        if filter {
            let c = bounds(r.x + 9., r.y + 10., 10., 10.);
            self.rounded(c, 5., MUTED);
            self.rounded(bounds(c.x + 1.3, c.y + 1.3, 7.4, 7.4), 4., CARD);
            self.canvas.stroke_line(
                (c.x + 8.) * self.scale,
                (c.y + 8.) * self.scale,
                (c.x + 13.) * self.scale,
                (c.y + 13.) * self.scale,
                1.3 * self.scale,
                MUTED,
            );
        }
        if focus {
            let text = field.text();
            let prefix = |end| self.advance(&text[..end], 13.);
            let caret = prefix(field.0.caret());
            let selection = field.0.selection();
            let (start, end) = (prefix(selection.start), prefix(selection.end));
            let left = x - (caret - (width - 2.).max(0.)).max(0.);
            self.clip(bounds(x, r.y + 2., width, r.height - 4.));
            if !selection.is_empty() {
                self.fill(bounds(left + start, r.y + 8., end - start, 18.), SOFT);
            }
            if text.is_empty() {
                self.text(placeholder, x, y, width, 13., MUTED, false);
            } else {
                self.canvas.label(
                    text,
                    left * self.scale,
                    y * self.scale,
                    13. * self.scale,
                    TEXT,
                    false,
                    self.fonts,
                );
            }
            self.fill(bounds(left + caret, r.y + 8., 1., 18.), ACCENT);
            self.clip(clip);
        } else {
            let (text, color) = if field.text().is_empty() {
                (placeholder, MUTED)
            } else {
                (field.text(), TEXT)
            };
            self.text(text, x, y, width, 13., color, false);
        }
    }

    pub fn badge(&mut self, label: &str, x: f32, y: f32) {
        let badge = bounds(x, y, 19., 19.);
        self.rounded(badge, 5., SOFT);
        self.center(label, badge, 11., ACCENT);
    }

    pub fn page_count(&mut self, page: Page, list: Rect, y: f32) {
        self.center(
            &format!("{} / {}", page.number + 1, page.count),
            bounds(list.x, y, list.width, 28.),
            11.,
            MUTED,
        );
    }
}
