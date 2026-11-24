//! Raw JSON documents keep source text and a sparse index of wrapped display
//! rows. No JSON syntax is evaluated and only visible text is shaped.
use crate::{
    css::Color,
    fonts::Fonts,
    layout::{DrawCommand, Layout},
};
use std::sync::{Arc, Mutex};

pub(crate) type SharedSource = Arc<Mutex<RawJson>>;
const ROWS_PER_ANCHOR: usize = 128;
const FONT: f32 = 14.;
const LINE: f32 = 19.;
const MARGIN: f32 = 8.;

#[derive(Debug)]
struct Rows {
    columns: usize,
    row: usize,
    used: usize,
    // The first anchor (row 0, byte 0) is implicit.
    anchors: Vec<u32>,
}
impl Rows {
    fn new(columns: usize) -> Self {
        Self {
            columns,
            row: 0,
            used: 0,
            anchors: vec![],
        }
    }
    fn next(&mut self, offset: usize) {
        self.row += 1;
        self.used = 0;
        if self.row.is_multiple_of(ROWS_PER_ANCHOR) {
            self.anchors.push(offset as u32);
        }
    }
    fn add(&mut self, text: &str, offset: usize) {
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                self.next(offset + i + 1);
            } else {
                if self.used == self.columns {
                    self.next(offset + i);
                }
                self.used += 1;
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct RawJson {
    source: String,
    rows: Rows,
    after_cr: bool,
}
impl RawJson {
    pub(crate) fn shared() -> SharedSource {
        Arc::new(Mutex::new(Self {
            source: String::new(),
            rows: Rows::new(96),
            after_cr: false,
        }))
    }
    pub(crate) fn push(&mut self, mut text: &str) {
        if text.is_empty() {
            return;
        }
        let offset = self.source.len();
        if self.after_cr && text.starts_with('\n') {
            text = &text[1..];
        }
        self.after_cr = text.ends_with('\r');
        // Normalize display line endings, including a CRLF split across events.
        let mut parts = text.split('\r');
        self.source.push_str(parts.next().unwrap());
        for part in parts {
            self.source.push('\n');
            self.source
                .push_str(part.strip_prefix('\n').unwrap_or(part));
        }
        self.rows.add(&self.source[offset..], offset);
    }
    fn reflow(&mut self, columns: usize) {
        if columns != self.rows.columns {
            self.rows = Rows::new(columns);
            self.rows.add(&self.source, 0);
        }
    }
    fn visible_rows(&self, first: usize, count: usize, mut draw: impl FnMut(usize, &str)) {
        if first > self.rows.row {
            return;
        }
        let group = first / ROWS_PER_ANCHOR;
        let mut offset = if group == 0 {
            0
        } else {
            self.rows.anchors[group - 1] as usize
        };
        for row in group * ROWS_PER_ANCHOR..(first.saturating_add(count)).min(self.rows.row + 1) {
            let rest = &self.source[offset..];
            let mut end = rest.len();
            let mut next = end;
            for (n, (i, ch)) in rest.char_indices().enumerate() {
                if ch == '\n' {
                    end = i;
                    next = i + 1;
                    break;
                }
                if n == self.rows.columns {
                    end = i;
                    next = i;
                    break;
                }
            }
            if row >= first {
                draw(row, &rest[..end]);
            }
            offset += next;
        }
    }
    pub(crate) fn paint_rows(&mut self, layout: &mut Layout, fonts: &Fonts) {
        let viewport = layout.viewport;
        let cell = fonts.shape("0", FONT, false, false, true).advance.max(1.);
        self.reflow(
            ((viewport.width - 2. * MARGIN) / cell)
                .floor()
                .clamp(1., 512.) as usize,
        );
        layout.depends_on_viewport_scroll = true;
        layout.height = ((self.rows.row + 1) as f32 * LINE + 2. * MARGIN).max(viewport.height);
        let first = ((viewport.scroll_y - MARGIN).max(0.) / LINE).floor() as usize;
        let count = (viewport.height / LINE).ceil() as usize + 2;
        self.visible_rows(first, count, |row, text| {
            if !text.is_empty() {
                layout.commands.push(DrawCommand::Text {
                    x: MARGIN,
                    baseline: MARGIN + row as f32 * LINE + FONT,
                    run: fonts.shape(text, FONT, false, false, true),
                    color: Color(29, 30, 33, 255),
                    underline: false,
                });
            }
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sparse_index_finds_wrapped_utf8_and_crlf_split_between_chunks() {
        let shared = RawJson::shared();
        let mut doc = shared.lock().unwrap();
        doc.reflow(4);
        doc.push(&"1234\n".repeat(130));
        doc.push("αβγδę\r");
        doc.push("\nlast");
        let mut visible = Vec::new();
        doc.visible_rows(129, 15, |_, line| visible.push(line.to_string()));
        assert_eq!(visible, ["1234", "αβγδ", "ę", "last"]);
        assert_eq!(doc.rows.anchors.len(), 1);
        doc.reflow(2);
        let mut last = Vec::new();
        doc.visible_rows(260, 10, |_, s| last.push(s.to_string()));
        assert_eq!(last, ["αβ", "γδ", "ę", "la", "st"]);
    }
}
