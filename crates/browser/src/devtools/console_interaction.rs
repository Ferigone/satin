//! Editable fields and command history belong to a single inspected document.
use super::console::{ConsoleOrigin, ConsoleRow, ConsoleSeverity};
use super::console_view::{ConsoleLayout, ConsoleRow as ViewRow};
use super::{ConsoleFilter, ConsoleInput, DevTools, DockFrame, Document, Section, console_row};
use crate::app::address_bar::{AddressBar, Movement};
use rust_browser_engine::{Fonts, Rect, paint::Canvas, script::ScriptRuntime};
use std::collections::VecDeque;
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Default, PartialEq, Eq, Clone, Copy)]
pub(super) enum Field {
    #[default]
    Command,
    Filter,
}

#[derive(Default)]
pub(super) struct ConsoleControls {
    pub(super) command: AddressBar,
    pub(super) filter: AddressBar,
    pub(super) field: Field,
    severity: usize,
    history: VecDeque<String>,
    history_index: Option<usize>,
    draft: String,
    pub(super) pending: Option<String>,
    pub(super) detail: Option<u64>,
    pub(super) detail_scroll: usize,
}

const LEVELS: [(Option<ConsoleSeverity>, &str); 6] = [
    (None, "All levels"),
    (Some(ConsoleSeverity::Error), "Errors"),
    (Some(ConsoleSeverity::Warn), "Warnings"),
    (Some(ConsoleSeverity::Info), "Info"),
    (Some(ConsoleSeverity::Log), "Logs"),
    (Some(ConsoleSeverity::Debug), "Debug"),
];

impl ConsoleControls {
    pub(super) fn cycle_level(&mut self) {
        self.severity = (self.severity + 1) % LEVELS.len();
        self.detail = None;
    }

    pub(super) fn level(&self) -> &str {
        LEVELS[self.severity].1
    }

    pub(super) fn matches(&self, row: &ConsoleRow, query: &str) -> bool {
        if LEVELS[self.severity]
            .0
            .is_some_and(|level| level != row.severity)
        {
            return false;
        }
        query.is_empty()
            || [
                &*row.message,
                row.source.as_deref().unwrap_or(""),
                row.phase.as_deref().unwrap_or(""),
            ]
            .iter()
            .any(|part| part.to_lowercase().contains(query))
    }

    pub(super) fn editor(&mut self) -> &mut AddressBar {
        match self.field {
            Field::Command => &mut self.command,
            Field::Filter => &mut self.filter,
        }
    }

    fn browse_history(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        if self.history_index.is_none() {
            if !older {
                return;
            }
            self.draft = self.command.text().to_owned();
        }
        let current = self.history_index.unwrap_or(self.history.len());
        let next = if older {
            current.saturating_sub(1)
        } else {
            (current + 1).min(self.history.len())
        };
        self.history_index = (next < self.history.len()).then_some(next);
        self.command
            .set_text(self.history.get(next).unwrap_or(&self.draft).to_owned());
    }

    pub(super) fn insert(&mut self, text: &str) -> bool {
        self.history_index = None;
        self.detail = None;
        if self.field == Field::Command {
            self.command.insert_text(text)
        } else {
            self.filter.insert(text)
        }
    }

    pub(super) fn key(&mut self, key: &Key, mods: ModifiersState, repeat: bool) -> bool {
        if mods.alt_key() || mods.super_key() {
            return false;
        }
        if mods.control_key() {
            let Key::Character(ch) = key else {
                return false;
            };
            match ch.to_ascii_lowercase().as_str() {
                "f" => {
                    self.field = Field::Filter;
                    self.detail = None;
                    self.filter.select_all();
                }
                "a" => self.editor().select_all(),
                _ => return false,
            }
            return true;
        }
        let Key::Named(key) = key else { return false };
        let shift = mods.shift_key();
        match key {
            NamedKey::Escape if self.detail.take().is_some() => {}
            NamedKey::Escape if self.field == Field::Filter => {
                self.filter.set_text(String::new());
                self.field = Field::Command;
            }
            NamedKey::Tab if !repeat => {
                self.field = if self.field == Field::Filter {
                    Field::Command
                } else {
                    Field::Filter
                };
                self.detail = None;
            }
            NamedKey::Enter if self.field == Field::Filter => {
                self.field = Field::Command;
            }
            NamedKey::Enter if !repeat && self.pending.is_none() => {
                let source = self.command.text().to_owned();
                if source.trim().is_empty() {
                    return true;
                }
                if self.history.back() != Some(&source) {
                    if self.history.len() == 64 {
                        self.history.pop_front();
                    }
                    self.history.push_back(source.clone());
                }
                self.pending = Some(source);
                self.command.set_text(String::new());
                self.history_index = None;
                self.detail = None;
            }
            NamedKey::ArrowUp | NamedKey::ArrowDown if self.field == Field::Command => {
                self.browse_history(*key == NamedKey::ArrowUp)
            }
            NamedKey::ArrowLeft => self.editor().move_caret(Movement::Left, shift),
            NamedKey::ArrowRight => self.editor().move_caret(Movement::Right, shift),
            NamedKey::Home => self.editor().move_caret(Movement::Start, shift),
            NamedKey::End => self.editor().move_caret(Movement::End, shift),
            NamedKey::Backspace | NamedKey::Delete => {
                self.editor().delete(*key == NamedKey::Backspace);
                self.history_index = None;
            }
            _ => return false,
        }
        true
    }
}

pub(super) fn origin_label(origin: ConsoleOrigin) -> Option<&'static str> {
    match origin {
        ConsoleOrigin::Command => Some("Input"),
        ConsoleOrigin::Evaluation => Some("Result"),
        _ => None,
    }
}

impl Document {
    fn console_rows(&self) -> impl Iterator<Item = &ConsoleRow> {
        let query = self.console_controls.filter.text().to_lowercase();
        self.console
            .rows(ConsoleFilter::ALL)
            .filter(move |r| self.console_controls.matches(r, &query))
    }

    pub(super) fn console_layout(&self, bounds: Rect) -> ConsoleLayout {
        ConsoleLayout::with_prompt(
            bounds,
            self.console_rows().count(),
            self.console_scroll,
            self.console
                .discarded()
                .saturating_add(self.console_source_dropped),
        )
    }

    fn clear_console(&mut self) {
        self.console.clear();
        self.console_source_dropped = 0;
        self.console_scroll = f32::INFINITY;
        self.console_controls.detail = None;
    }

    pub(super) fn console_click(&mut self, bounds: Rect, x: f32, y: f32) {
        let layout = self.console_layout(bounds);
        let hit = |area: Rect| area.contains(x, y);
        if layout.clear.is_some_and(hit) {
            self.clear_console();
        } else if layout.severity_box().is_some_and(hit) {
            self.console_controls.cycle_level();
            self.console_scroll = 0.;
        } else if layout.filter_box().is_some_and(hit) {
            self.console_controls.field = Field::Filter;
            self.console_controls.detail = None;
        } else if hit(ConsoleLayout::prompt(bounds)) {
            if self.console_controls.field != Field::Filter || layout.filter_box().is_some() {
                self.console_controls.field = Field::Command;
            }
            self.console_controls.detail = None;
        } else if hit(layout.list) {
            if self.selected_console_row().is_some() {
                if y < layout.list.y + 23. {
                    self.console_controls.detail = None;
                }
            } else if let Some(index) = layout.row_at(x, y) {
                let sequence = self.console_rows().nth(index).map(|row| row.sequence);
                self.console_controls.detail = sequence;
                self.console_controls.detail_scroll = 0;
            }
        }
    }

    pub(super) fn console_detail_scroll(&mut self, area: Rect, delta: isize) -> bool {
        let Some(selected) = self.selected_console_row() else {
            self.console_controls.detail = None;
            return false;
        };
        let (lines, visible) = super::console_view::detail_rows(area, selected);
        let max = lines.len().saturating_sub(visible);
        self.console_controls.detail_scroll = self
            .console_controls
            .detail_scroll
            .min(max)
            .saturating_add_signed(delta)
            .min(max);
        true
    }

    fn selected_console_row(&self) -> Option<&ConsoleRow> {
        let selection = self.console_controls.detail?;
        self.console
            .rows(ConsoleFilter::ALL)
            .find(|row| row.sequence == selection)
    }

    pub(super) fn draw_console(
        &self,
        canvas: &mut Canvas,
        fonts: &mut Fonts,
        frame: DockFrame,
        bounds: Rect,
    ) {
        let rows: Vec<ViewRow<'_>> = self.console_rows().map(console_row).collect();
        let layout = self.console_layout(bounds);
        layout.draw(
            canvas,
            fonts,
            &rows,
            frame.scale,
            super::panel::scaled(bounds, frame.scale),
        );
        layout.draw_controls(
            canvas,
            fonts,
            bounds,
            frame.scale,
            &self.console_controls,
            self.selected_console_row(),
            frame.focused,
        );
    }

    fn record_evaluation(&mut self, origin: ConsoleOrigin, severity: ConsoleSeverity, value: &str) {
        self.console.record(
            self.console.last_sequence().saturating_add(1),
            ConsoleInput {
                severity,
                origin,
                message: value,
                source: None,
                phase: None,
                capture_truncated: false,
            },
        );
    }
}

impl DevTools {
    fn console_document(&mut self, frame: DockFrame) -> Option<&mut Document> {
        if !frame.focused || self.section != Section::Console {
            return None;
        }
        self.document_mut(frame.tab, frame.document_navigation)
    }

    pub(crate) fn console_key(
        &mut self,
        frame: DockFrame,
        key: &Key,
        modifiers: ModifiersState,
        repeat: bool,
    ) -> bool {
        let Some(doc) = self.console_document(frame) else {
            return false;
        };
        if modifiers == ModifiersState::CONTROL
            && matches!(key, Key::Character(c) if c.eq_ignore_ascii_case("l"))
        {
            doc.clear_console();
            return true;
        }
        if modifiers.is_empty()
            && doc.console_controls.detail.is_some()
            && let Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) = key
            && let Some(layout) = super::panel::Layout::new(frame.bounds)
        {
            let delta = if *key == Key::Named(NamedKey::ArrowUp) {
                -1
            } else {
                1
            };
            if doc.console_detail_scroll(doc.console_layout(layout.body).list, delta) {
                return true;
            }
        }
        let handled = doc.console_controls.key(key, modifiers, repeat);
        if handled && doc.console_controls.field == Field::Filter {
            doc.console_scroll = 0.;
        }
        handled
    }

    pub(crate) fn console_text(&mut self, frame: DockFrame, text: &str) -> bool {
        let Some(doc) = self.console_document(frame) else {
            return false;
        };
        doc.console_controls.insert(text);
        if doc.console_controls.field == Field::Filter {
            doc.console_scroll = 0.;
        }
        true
    }

    pub(crate) fn console_selection(&mut self, frame: DockFrame, cut: bool) -> Option<String> {
        let doc = self.console_document(frame)?;
        if let Some(row) = doc.selected_console_row() {
            return Some(row.message.to_string());
        }
        let editor = doc.console_controls.editor();
        let selected = editor.selected_text()?.to_owned();
        if cut {
            editor.delete_selection();
        }
        Some(selected)
    }

    /// Return the active main-document script and its optional one-based parser line.
    pub(crate) fn console_source_target(&self, frame: DockFrame) -> Option<(usize, Option<usize>)> {
        if self.section != Section::Console || !frame.focused {
            return None;
        }
        let doc = self.documents.get(&frame.tab)?;
        if doc.navigation != frame.document_navigation {
            return None;
        }
        let context = &doc.selected_console_row()?.context;
        Some((context.script_node?, context.line))
    }

    pub(crate) fn console_has_command(&self, frame: DockFrame) -> bool {
        self.documents.get(&frame.tab).is_some_and(|doc| {
            doc.navigation == frame.document_navigation && doc.console_controls.pending.is_some()
        })
    }

    pub(crate) fn run_console_command(
        &mut self,
        frame: DockFrame,
        runtime: Option<&mut ScriptRuntime>,
    ) -> bool {
        let Some(doc) = self.console_document(frame) else {
            return false;
        };
        let Some(command) = doc.console_controls.pending.take() else {
            return false;
        };
        doc.record_evaluation(ConsoleOrigin::Command, ConsoleSeverity::Log, &command);
        let result = if let Some(runtime) = runtime {
            let result =
                runtime.evaluate_console_with_selection(&command, doc.elements.selected_node());
            doc.ingest_console(runtime.take_console_snapshot());
            result
        } else {
            Err("This document has no JavaScript execution context.".to_owned())
        };
        match result {
            Ok(value) => {
                doc.record_evaluation(ConsoleOrigin::Evaluation, ConsoleSeverity::Info, &value)
            }
            Err(error) => {
                doc.record_evaluation(ConsoleOrigin::Evaluation, ConsoleSeverity::Error, &error)
            }
        }
        doc.console_scroll = f32::INFINITY;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browsing_history_restores_draft_and_submit_is_single_shot() {
        let mut c = ConsoleControls::default();
        c.insert("document.title");
        c.key(&Key::Named(NamedKey::Enter), ModifiersState::empty(), false);
        assert_eq!(c.pending.take().as_deref(), Some("document.title"));
        c.insert("saved 🦊");
        c.key(
            &Key::Named(NamedKey::ArrowUp),
            ModifiersState::empty(),
            false,
        );
        assert_eq!(c.command.text(), "document.title");
        c.key(
            &Key::Named(NamedKey::ArrowDown),
            ModifiersState::empty(),
            false,
        );
        assert_eq!(c.command.text(), "saved 🦊");
        c.key(&Key::Named(NamedKey::Enter), ModifiersState::empty(), true);
        assert!(c.pending.is_none());
    }

    #[test]
    fn selection_reaches_console_only_in_the_document_that_was_inspected() {
        let page = rust_browser_engine::LoadedPage::from_html(
            "https://console.test/",
            "<button data-selected='yes'>Press</button>",
        );
        let node = page.document.elements("button").next().unwrap();
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let mut dock = DevTools::default();
        let frame = DockFrame {
            bounds: Rect {
                x: 0.,
                y: 0.,
                width: 640.,
                height: 240.,
            },
            focused: true,
            pointer: None,
            scale: 1.,
            tab: 5,
            document_navigation: 19,
        };
        dock.begin_navigation(5, 19);
        let old = dock.documents.get_mut(&5).unwrap();
        old.elements.sync_page(Some(&page), None);
        old.elements.inspect(node, frame.bounds);
        old.console_controls.pending = Some("$0.getAttribute('data-selected')".into());
        assert!(dock.run_console_command(frame, Some(&mut runtime)));
        assert_eq!(
            dock.documents[&5]
                .console
                .rows(ConsoleFilter::ALL)
                .last()
                .unwrap()
                .message
                .as_ref(),
            "\"yes\""
        );
        dock.begin_navigation(5, 20);
        let new_frame = DockFrame {
            document_navigation: 20,
            ..frame
        };
        let page = rust_browser_engine::LoadedPage::from_html("https://next.test/", "<p>next</p>");
        let mut next_runtime = ScriptRuntime::new(&page).unwrap();
        dock.documents.get_mut(&5).unwrap().console_controls.pending = Some("$0".into());
        assert!(!dock.run_console_command(frame, Some(&mut next_runtime)));
        assert!(dock.run_console_command(new_frame, Some(&mut next_runtime)));
        assert_eq!(
            dock.documents[&5]
                .console
                .rows(ConsoleFilter::ALL)
                .last()
                .unwrap()
                .message
                .as_ref(),
            "undefined"
        );
    }

    #[test]
    fn captured_error_links_only_to_the_selected_live_documents_script() {
        let mut page = rust_browser_engine::LoadedPage::from_html(
            "https://console.test/",
            "<script>\n\nvar = ;</script>",
        );
        let node = page.document.elements("script").next().unwrap();
        let runtime = ScriptRuntime::for_page(&mut page).unwrap().unwrap();
        let mut dock = DevTools::default();
        dock.begin_navigation(11, 7);
        let bounds = Rect {
            x: 0.,
            y: 200.,
            width: 512.,
            height: 200.,
        };
        let frame = DockFrame {
            bounds,
            focused: true,
            pointer: None,
            scale: 1.,
            tab: 11,
            document_navigation: 7,
        };
        dock.sync_console(11, 7, &runtime);
        assert!(dock.console_source_target(frame).is_none());
        let doc = dock.documents.get_mut(&11).unwrap();
        let seq = doc
            .console
            .rows(ConsoleFilter::ALL)
            .next()
            .unwrap()
            .sequence;
        doc.console_controls.detail = Some(seq);
        assert_eq!(dock.console_source_target(frame), Some((node.0, Some(3))));
        assert!(
            dock.console_source_target(DockFrame {
                document_navigation: 8,
                ..frame
            })
            .is_none()
        );
        dock.begin_navigation(11, 8);
        assert!(
            dock.console_source_target(DockFrame {
                document_navigation: 8,
                ..frame
            })
            .is_none()
        );
    }
}
