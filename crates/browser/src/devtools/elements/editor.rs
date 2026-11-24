//! Transient author-style editor. Only an explicit Apply or Remove queues a mutation.
use super::{ACCENT, ACTIVE, BG, CHROME, DIM, INK, Ink, panel, rect};
use crate::app::address_bar::{AddressBar, Movement};
use rust_browser_engine::{Rect, dom::NodeId};
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub(super) enum Action {
    None,
    Apply,
    Remove,
    Cancel,
}

pub(super) struct Editor {
    pub node: NodeId,
    name: AddressBar,
    value: AddressBar,
    focus: usize,
    important: bool,
}

struct Controls {
    fields: [Rect; 2],
    buttons: [Rect; 4],
}

impl Controls {
    fn new(p: Rect) -> Self {
        let pad = 4_f32.min(p.width / 12.);
        let height = (p.height / 3.).clamp(0., 26.);
        let width = (p.width - 2. * pad).max(0.);
        let fields =
            std::array::from_fn(|i| rect(p.x + pad, p.y + i as f32 * height, width, height));
        let buttons = std::array::from_fn(|i| {
            rect(
                p.x + pad + width / 4. * i as f32,
                p.y + 2. * height,
                width / 4.,
                height,
            )
        });
        Self { fields, buttons }
    }
}

impl Editor {
    pub fn new(node: NodeId, name: &str, value: &str, important: bool) -> Self {
        let focus = usize::from(!name.is_empty());
        let mut prop = AddressBar::default();
        prop.set_text(name.into());
        let mut val = AddressBar::default();
        val.set_text(value.into());
        val.select_all();
        Self {
            node,
            name: prop,
            value: val,
            focus,
            important,
        }
    }

    pub fn data(&self, remove: bool) -> (NodeId, String, String, bool) {
        (
            self.node,
            self.name.text().trim().to_owned(),
            if remove {
                String::new()
            } else {
                self.value.text().trim().to_owned()
            },
            self.important,
        )
    }

    pub fn clipboard_selection(&mut self, cut: bool) -> Option<String> {
        let input = if self.focus == 0 {
            &mut self.name
        } else {
            &mut self.value
        };
        let text = input.selected_text()?.to_owned();
        if cut {
            input.delete_selection();
        }
        Some(text)
    }

    pub fn key(&mut self, key: &Key, mods: ModifiersState) -> Action {
        if mods.alt_key() && matches!(key,Key::Character(c) if c.eq_ignore_ascii_case("i")) {
            self.important = !self.important;
            return Action::None;
        }
        if mods.control_key() && *key == Key::Named(NamedKey::Delete) {
            return Action::Remove;
        }
        match key {
            Key::Named(NamedKey::Escape) => return Action::Cancel,
            Key::Named(NamedKey::Tab) => {
                self.focus = 1 - self.focus;
                return Action::None;
            }
            Key::Named(NamedKey::Enter) => {
                if self.focus == 0 && !mods.control_key() {
                    self.focus = 1;
                    return Action::None;
                }
                return Action::Apply;
            }
            _ => {}
        }
        let selected = if self.focus == 0 {
            &mut self.name
        } else {
            &mut self.value
        };
        let limit = if self.focus == 0 { 256 } else { 16_384 };
        match key {
            Key::Character(s) if mods.control_key() && s.eq_ignore_ascii_case("a") => {
                selected.select_all()
            }
            Key::Character(s) if !mods.control_key() && !mods.super_key() && !mods.alt_key() => {
                let current = selected.text().len() - selected.selection().len();
                if current + s.len() <= limit {
                    selected.insert(s);
                }
            }
            Key::Named(NamedKey::Backspace) => selected.delete(true),
            Key::Named(NamedKey::Delete) => selected.delete(false),
            Key::Named(NamedKey::ArrowLeft) => {
                selected.move_caret(Movement::Left, mods.shift_key())
            }
            Key::Named(NamedKey::ArrowRight) => {
                selected.move_caret(Movement::Right, mods.shift_key())
            }
            Key::Named(NamedKey::Home) => selected.move_caret(Movement::Start, mods.shift_key()),
            Key::Named(NamedKey::End) => selected.move_caret(Movement::End, mods.shift_key()),
            _ => {}
        }
        Action::None
    }

    pub fn click(&mut self, pane: Rect, x: f32, y: f32) -> Action {
        let controls = Controls::new(pane);
        for (i, field) in controls.fields.iter().enumerate() {
            if panel::contains(*field, x, y) {
                self.focus = i;
                return Action::None;
            }
        }
        match controls
            .buttons
            .iter()
            .position(|button| panel::contains(*button, x, y))
        {
            Some(0) => {
                self.important = !self.important;
                Action::None
            }
            Some(1) => Action::Apply,
            Some(2) => Action::Remove,
            Some(3) => Action::Cancel,
            _ => Action::None,
        }
    }

    pub fn draw(&self, ink: &mut Ink<'_>, pane: Rect) {
        let controls = Controls::new(pane);
        for (i, (label, input)) in [("Property", &self.name), ("Value", &self.value)]
            .iter()
            .enumerate()
        {
            let field = controls.fields[i];
            let left = 48_f32.min(field.width * 0.25);
            let label_area = rect(field.x, field.y, left, field.height);
            ink.text(label_area, 2., label, DIM, false);
            let area = rect(
                field.x + left,
                field.y,
                field.width - left,
                (field.height - 1.).max(0.),
            );
            ink.fill(area, if self.focus == i { ACTIVE } else { BG });
            // Keep the insertion point in view in long declarations on a narrow dock.
            let before = &input.text()[..input.caret()];
            let keep = ((area.width - 14.) / 8.).max(1.) as usize;
            let start = before
                .char_indices()
                .rev()
                .nth(keep.saturating_sub(1))
                .map_or(0, |(i, _)| i);
            let remaining = &input.text()[start..];
            let end = remaining
                .char_indices()
                .nth(keep + 2)
                .map_or(input.text().len(), |(i, _)| start + i);
            let sel = input.selection();
            if self.focus == i && sel.start < end && sel.end > start {
                let x_for = |n: usize| {
                    ink.fonts
                        .shape(
                            &input.text()[start..n],
                            12. * ink.scale,
                            false,
                            false,
                            false,
                        )
                        .advance
                        / ink.scale
                };
                let x1 = (4. + x_for(sel.start.max(start))).min(area.width);
                let x2 = (4. + x_for(sel.end.min(end))).min(area.width);
                ink.fill(
                    rect(area.x + x1, area.y + 2., x2 - x1, area.height - 4.),
                    rust_browser_engine::css::Color(93, 73, 123, 255),
                );
            }
            let mut value = super::preview(remaining, keep + 2);
            if self.focus == i && input.selection().is_empty() {
                let cut = input.caret() - start;
                if value.is_char_boundary(cut) {
                    value.insert(cut, '│');
                }
            }
            ink.text(
                area,
                4.,
                &value,
                if self.focus == i { INK } else { DIM },
                false,
            );
        }
        for (i, button) in controls.buttons.iter().copied().enumerate() {
            let label = [
                if self.important {
                    "✓ !important"
                } else {
                    "!important"
                },
                "Apply",
                "Remove",
                "Cancel",
            ][i];
            let active = i == 1 || i == 0 && self.important;
            ink.fill(button, if active { ACTIVE } else { CHROME });
            ink.text(button, 3., label, if active { ACCENT } else { DIM }, active);
        }
    }
}
