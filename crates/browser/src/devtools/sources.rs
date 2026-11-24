use super::panel;
use rust_browser_engine::{
    Fonts, LoadedPage, Rect,
    css::Color,
    paint::Canvas,
    script::{DocumentSession, InspectedSource, ScriptRuntime},
};
use std::sync::Arc;
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Default)]
pub(super) struct Sources {
    files: Vec<InspectedSource>,
    omitted: usize,
    selected: usize,
    list_offset: usize,
    top: usize,
    column: usize,
    lines: Vec<usize>,
    indexed: Option<Arc<str>>,
    query: String,
    searching: bool,
    found: Option<usize>,
    selected_line: Option<usize>,
    goto: Option<String>,
}

fn rows(height: f32) -> usize {
    (height.max(0.) / 18.).floor() as usize
}

impl Sources {
    pub(super) fn sync(
        &mut self,
        page: Option<&LoadedPage>,
        runtime: Option<&ScriptRuntime>,
        session: Option<&DocumentSession>,
    ) {
        let old = self.files.get(self.selected).map(|file| file.text.clone());
        self.files.clear();
        self.omitted = 0;
        if let Some(runtime) = runtime {
            runtime.with_inspected_scripts(|items, omitted| {
                self.files.extend_from_slice(items);
                self.omitted += omitted;
            });
        }
        if let Some(session) = session {
            let (items, omitted) = session.inspected_modules();
            self.files.extend_from_slice(items);
            self.omitted += omitted;
        }
        if let Some(page) = page {
            for (i, sheet) in page.stylesheets.iter().take(80).enumerate() {
                self.files.push(InspectedSource {
                    node: None,
                    label: format!("Stylesheet {}", i + 1),
                    text: sheet.clone(),
                    original_bytes: sheet.len(),
                });
            }
            self.omitted += page.stylesheets.len().saturating_sub(80);
        }
        self.selected = old
            .as_ref()
            .and_then(|old| self.files.iter().position(|f| Arc::ptr_eq(old, &f.text)))
            .unwrap_or(0);
        self.list_offset = self.list_offset.min(self.files.len().saturating_sub(1));
        self.index();
    }

    pub(super) fn reveal(&mut self, node: usize, line: Option<usize>) -> bool {
        let Some(i) = self
            .files
            .iter()
            .position(|f| f.node.is_some_and(|id| id.0 == node))
        else {
            return false;
        };
        self.selected = i;
        self.list_offset = i;
        self.index();
        self.searching = false;
        self.query.clear();
        self.found = None;
        self.top = line
            .unwrap_or(1)
            .saturating_sub(1)
            .min(self.lines.len().saturating_sub(1));
        self.selected_line = Some(self.top);
        self.column = 0;
        true
    }
    pub(super) fn copy_line(&self) -> Option<String> {
        let i = self.selected_line.unwrap_or(self.top);
        self.lines.get(i)?;
        Some(self.line(i).to_owned())
    }

    fn index(&mut self) {
        let selected = self.files.get(self.selected).map(|f| f.text.clone());
        if let (Some(a), Some(b)) = (&self.indexed, &selected)
            && Arc::ptr_eq(a, b)
        {
            return;
        }
        self.indexed = selected;
        self.top = 0;
        self.column = 0;
        self.found = None;
        self.selected_line = None;
        self.goto = None;
        self.lines.clear();
        if let Some(s) = &self.indexed {
            self.lines.push(0);
            self.lines.extend(
                s.as_bytes()
                    .iter()
                    .take(128 * 1024)
                    .enumerate()
                    .filter_map(|(i, b)| (*b == b'\n').then_some(i + 1)),
            );
        }
    }

    fn line(&self, i: usize) -> &str {
        let Some(text) = &self.indexed else { return "" };
        let start = self.lines[i];
        let last = text.floor_char_boundary(text.len().min(128 * 1024));
        text[start..self.lines.get(i + 1).copied().unwrap_or(last)].trim_end_matches(['\r', '\n'])
    }

    fn find(&mut self, next: bool, reverse: bool) {
        if self.query.is_empty() || self.lines.is_empty() {
            self.found = None;
            return;
        }
        let n = self.lines.len();
        let start = if next {
            self.found.unwrap_or(self.top)
        } else {
            0
        };
        let step = usize::from(next);
        self.found = (0..n)
            .map(|i| {
                if reverse {
                    (start + n - (i + step) % n) % n
                } else {
                    (start + i + step) % n
                }
            })
            .find(|&i| self.line(i).contains(&self.query));
        if let Some(i) = self.found {
            self.top = i;
            self.selected_line = Some(i);
            let line = self.line(i);
            let offset = line[..line.find(&self.query).unwrap()].chars().count();
            self.column = if offset > 48 {
                offset.saturating_sub(12)
            } else {
                0
            };
        }
    }

    fn divider(bounds: Rect) -> f32 {
        (bounds.width * 0.30).min(252.)
    }
    pub(super) fn click(&mut self, bounds: Rect, x: f32, y: f32) {
        if y < bounds.y + 30. {
            self.searching = true;
            self.goto = None;
            return;
        }
        if x < bounds.x + Self::divider(bounds) {
            let i = self.list_offset + ((y - bounds.y - 30.) / 18.) as usize;
            if i < self.files.len() {
                self.selected = i;
                self.searching = false;
                self.index();
            }
        } else {
            let line = self.top + ((y - bounds.y - 30.) / 18.) as usize;
            if line < self.lines.len() {
                self.selected_line = Some(line);
                self.searching = false;
                self.goto = None;
            }
        }
    }
    pub(super) fn scroll(&mut self, bounds: Rect, x: f32, delta: f32) {
        let lines = (delta / 18.).round() as isize;
        if x < bounds.x + Self::divider(bounds) {
            self.list_offset = self.list_offset.saturating_add_signed(lines).min(
                self.files
                    .len()
                    .saturating_sub(rows(bounds.height - 30.).max(1)),
            );
        } else {
            self.top = self
                .top
                .saturating_add_signed(lines)
                .min(self.lines.len().saturating_sub(1));
        }
    }
    pub(super) fn key(&mut self, bounds: Rect, key: &Key, mods: ModifiersState) -> bool {
        if mods == ModifiersState::CONTROL
            && matches!(key,Key::Character(c) if c.eq_ignore_ascii_case("g"))
        {
            self.goto = Some(String::new());
            self.searching = false;
            return true;
        }
        if mods.control_key() && matches!(key,Key::Character(c) if c.eq_ignore_ascii_case("f")) {
            self.goto = None;
            self.searching = true;
            self.query.clear();
            self.found = None;
            return true;
        }
        if mods.control_key() || mods.alt_key() || mods.super_key() {
            return false;
        }
        if let Some(input) = &mut self.goto {
            match key {
                Key::Named(NamedKey::Escape) => self.goto = None,
                Key::Named(NamedKey::Backspace) => {
                    input.pop();
                }
                Key::Character(c) => input.extend(
                    c.chars()
                        .filter(char::is_ascii_digit)
                        .take(8usize.saturating_sub(input.len())),
                ),
                Key::Named(NamedKey::Enter) => {
                    if let Ok(line) = input.parse::<usize>()
                        && (1..=self.lines.len()).contains(&line)
                    {
                        self.top = line - 1;
                        self.column = 0;
                        self.selected_line = Some(line - 1);
                        self.goto = None;
                    }
                }
                _ => return false,
            }
            return true;
        }
        if self.searching {
            match key {
                Key::Named(NamedKey::Escape) => self.searching = false,
                Key::Named(NamedKey::Enter) => self.find(true, mods.shift_key()),
                Key::Named(NamedKey::Backspace) => {
                    self.query.pop();
                    self.find(false, false);
                }
                Key::Named(NamedKey::Space) => {
                    if self.query.len() < 160 {
                        self.query.push(' ');
                        self.find(false, false);
                    }
                }
                Key::Character(c) => {
                    self.query.extend(
                        c.chars()
                            .filter(|c| !c.is_control())
                            .take(160usize.saturating_sub(self.query.chars().count())),
                    );
                    self.find(false, false);
                }
                _ => return false,
            }
        } else {
            let last = self.lines.len().saturating_sub(1);
            let page = rows(bounds.height - 30.).max(1);
            match key {
                Key::Named(NamedKey::ArrowDown) => self.top = (self.top + 1).min(last),
                Key::Named(NamedKey::ArrowUp) => self.top = self.top.saturating_sub(1),
                Key::Named(NamedKey::PageDown) => self.top = (self.top + page).min(last),
                Key::Named(NamedKey::PageUp) => self.top = self.top.saturating_sub(page),
                Key::Named(NamedKey::ArrowRight) => {
                    self.column = (self.column + 12).min(128 * 1024)
                }
                Key::Named(NamedKey::ArrowLeft) => self.column = self.column.saturating_sub(12),
                Key::Named(NamedKey::Home) => {
                    self.top = 0;
                    self.column = 0;
                }
                Key::Named(NamedKey::End) => self.top = last,
                Key::Named(NamedKey::Tab) if !self.files.is_empty() => {
                    let n = self.files.len();
                    self.selected = (self.selected + if mods.shift_key() { n - 1 } else { 1 }) % n;
                    self.index();
                    self.list_offset = self
                        .list_offset
                        .min(self.selected)
                        .max((self.selected + 1).saturating_sub(page));
                }
                _ => return false,
            }
        }
        true
    }
    pub(super) fn draw(&self, canvas: &mut Canvas, fonts: &mut Fonts, b: Rect, s: f32) {
        let area = |x, y, w, h| Rect {
            x: b.x + x,
            y: b.y + y,
            width: w,
            height: h,
        };
        let rect = |r| panel::scaled(r, s);
        let div = Self::divider(b);
        let blue = Color(153, 202, 250, 255);
        let white = Color(230, 230, 235, 255);
        let muted = Color(177, 176, 186, 255);
        canvas.fill_rect(rect(b), Color(28, 28, 31, 255));
        canvas.fill_rect(
            rect(area(0., 0., b.width, b.height.min(30.))),
            Color(41, 40, 45, 255),
        );
        let title = if let Some(input) = &self.goto {
            format!("Go to line (1–{}): {}", self.lines.len(), input)
        } else if self.searching {
            format!(
                "Find in file: {}   {}",
                self.query,
                if self.query.is_empty() {
                    ""
                } else if self.found.is_some() {
                    "Enter: next"
                } else {
                    "No match"
                }
            )
        } else if let Some(f) = self.files.get(self.selected) {
            format!(
                "{}  ·  {} bytes{}   |   Ctrl+F  Find   ·   Ctrl+G  Line",
                f.label,
                f.original_bytes,
                if f.original_bytes > f.text.len().min(128 * 1024) {
                    " · preview truncated"
                } else {
                    ""
                }
            )
        } else {
            "No script or stylesheet captured for this document".into()
        };
        canvas.label(
            &title,
            (b.x + 8.) * s,
            (b.y + 20.) * s,
            12. * s,
            white,
            false,
            fonts,
        );
        if b.height < 31. {
            return;
        }
        let left = area(0., 30., div, b.height - 30.);
        let right = area(div, 30., b.width - div, b.height - 30.);
        canvas.clip(rect(left));
        canvas.fill_rect(rect(left), Color(34, 34, 38, 255));
        for (row, f) in self
            .files
            .iter()
            .skip(self.list_offset)
            .take(rows(left.height) + 1)
            .enumerate()
        {
            let y = 30. + row as f32 * 18.;
            if self.list_offset + row == self.selected {
                canvas.fill_rect(rect(area(0., y, div, 18.)), Color(57, 63, 78, 255));
            }
            canvas.label(
                &f.label,
                (b.x + 6.) * s,
                (b.y + y + 14.) * s,
                12. * s,
                white,
                false,
                fonts,
            );
        }
        if self.omitted > 0 && rows(left.height) > self.files.len().saturating_sub(self.list_offset)
        {
            let y = left.y + (self.files.len() - self.list_offset) as f32 * 18. + 14.;
            canvas.label(
                &format!("{} additional sources omitted", self.omitted),
                (b.x + 6.) * s,
                y * s,
                11. * s,
                muted,
                false,
                fonts,
            );
        }
        canvas.clip(rect(right));
        for i in self.top..(self.top + rows(right.height) + 1).min(self.lines.len()) {
            let y = right.y + (i - self.top) as f32 * 18.;
            if self.selected_line == Some(i) {
                canvas.fill_rect(
                    rect(Rect {
                        y,
                        height: 18.,
                        ..right
                    }),
                    Color(42, 51, 65, 255),
                );
            }
            if self.searching && self.found == Some(i) {
                canvas.fill_rect(
                    rect(Rect {
                        y,
                        height: 18.,
                        ..right
                    }),
                    Color(72, 61, 40, 255),
                );
            }
            let code: String = self
                .line(i)
                .chars()
                .skip(self.column)
                .take(((right.width / 4.).ceil() as usize).min(600))
                .map(|c| if c == '\t' { ' ' } else { c })
                .collect();
            canvas.label(
                &(i + 1).to_string(),
                (right.x + 5.) * s,
                (y + 14.) * s,
                11. * s,
                blue,
                false,
                fonts,
            );
            let start = (right.x + 48.) * s;
            let baseline = (y + 14.) * s;
            let run = fonts.shape(&code, 12. * s, false, false, true);
            if self.searching
                && self.found == Some(i)
                && !self.query.is_empty()
                && let Some(at) = code.find(&self.query)
            {
                let before = fonts.shape(&code[..at], 12. * s, false, false, true);
                let match_run = fonts.shape(&self.query, 12. * s, false, false, true);
                canvas.fill_rect(
                    Rect {
                        x: start + before.advance,
                        y: y * s,
                        width: match_run.advance,
                        height: 18. * s,
                    },
                    Color(111, 83, 38, 255),
                );
            }
            let color =
                if code.trim_start().starts_with("//") || code.trim_start().starts_with("/*") {
                    Color(155, 191, 161, 255)
                } else {
                    white
                };
            canvas.text(start, baseline, &run, color, fonts);
        }
        canvas.clip(rect(b));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_wraps_to_the_previous_line_and_preserves_indentation() {
        let mut viewer = Sources::default();
        viewer.files.push(InspectedSource {
            node: None,
            label: "example.js".into(),
            text: "let value=1;\n// between\nvalue++;".into(),
            original_bytes: 32,
        });
        viewer.index();
        viewer.query = "value".into();
        viewer.find(false, false);
        assert_eq!((viewer.found, viewer.column), (Some(0), 0));
        viewer.find(true, true);
        assert_eq!(viewer.found, Some(2));
        viewer.find(true, false);
        assert_eq!(viewer.found, Some(0));
    }
}
