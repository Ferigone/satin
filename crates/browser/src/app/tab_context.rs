//! The tab flyout is anchored to the right-click point. Its release target is
//! a stable tab ID plus the exact command, never a row or tab index.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ContextCommand {
    Duplicate,
    Pin,
    Close,
    Reopen,
}

const COMMANDS: [ContextCommand; 4] = [
    ContextCommand::Duplicate,
    ContextCommand::Pin,
    ContextCommand::Close,
    ContextCommand::Reopen,
];

#[derive(Clone, Copy)]
pub(super) struct TabContext {
    pub(super) target: TabId,
    anchor: (f32, f32),
    selected: Option<ContextCommand>,
    pressed: Option<ContextCommand>,
}

impl TabContext {
    fn new(target: TabId, anchor: (f32, f32)) -> Self {
        Self {
            target,
            anchor,
            selected: None,
            pressed: None,
        }
    }

    fn bounds(self, width: f32, height: f32) -> Option<Rect> {
        let w = 246_f32.min(width - 18.);
        let h = 165.;
        if w <= 0. {
            return None;
        }
        let x = self.anchor.0.clamp(8., width - w - 8.);
        let caption = chrome_box(quiet_cinder::caption(width, 0));
        // Caption targets are live even before a frame is presented. Never
        // visually cover a native control with an actionable popup row.
        let top = if WINDOW_FRAME && x + w > caption.x {
            8_f32.max(caption.y + caption.height)
        } else {
            8.
        };
        let bottom = height - h - 8.;
        (bottom >= top).then(|| scaled_rect(1., x, self.anchor.1.clamp(top, bottom), w, h))
    }

    fn row(panel: Rect, i: usize) -> Rect {
        scaled_rect(
            1.,
            panel.x + 7.,
            panel.y + 7. + i as f32 * 35. + if i >= 2 { 11. } else { 0. },
            panel.width - 14.,
            35.,
        )
    }

    fn release(&mut self, hit: Option<Action>, presented: bool) -> Option<ContextCommand> {
        let row = self.pressed.take()?;
        (presented && hit == Some(Action::TabContextRow(self.target, row))).then_some(row)
    }
}

impl App {
    pub(super) fn open_tab_context(&mut self, target: TabId) {
        if self.tab_index(target).is_none() {
            return;
        }
        self.wheel = None;
        self.dismiss_address_suggestions();
        self.last_title_press = None;
        self.caption_pressed = None;
        self.chrome_menu = Some(Menu::TabContext(TabContext::new(target, self.mouse)));
        self.tab_hits_current = false;
        self.chrome_hover = None;
        self.hover = None;
        self.redraw();
    }

    pub(super) fn dismiss_tab_context(&mut self) -> bool {
        if !matches!(self.chrome_menu, Some(Menu::TabContext(_))) {
            return false;
        }
        self.chrome_menu = None;
        self.tab_hits_current = false;
        self.chrome_hover = None;
        self.redraw();
        true
    }

    pub(super) fn tab_context_enabled(&self, id: TabId, command: ContextCommand) -> bool {
        let Some(tab) = self.tab_index(id).map(|i| &self.tabs[i]) else {
            return false;
        };
        match command {
            ContextCommand::Duplicate => {
                self.tabs.len() < MAX_TABS && tab.fresh_address().is_some()
            }
            ContextCommand::Pin => true,
            ContextCommand::Close => {
                #[cfg(all(windows, target_arch = "x86_64"))]
                if self.tabs.len() == 1 && self.update_applying() {
                    return false;
                }
                true
            }
            ContextCommand::Reopen => self.tabs.len() < MAX_TABS && !self.closed_tabs.is_empty(),
        }
    }

    pub(super) fn tab_context_press(&mut self, hit: Option<Action>) -> bool {
        let Some(Menu::TabContext(mut menu)) = self.chrome_menu else {
            return false;
        };
        if hit.is_some_and(Action::caption) {
            self.dismiss_tab_context();
            return false;
        }
        self.cancel_tab_context_press();
        if !self.tab_hits_current {
            return true;
        }
        if let Some(Action::TabContextRow(id, command)) = hit
            && menu.target == id
            && self.tab_context_enabled(id, command)
        {
            menu.pressed = Some(command);
            menu.selected = None;
            self.chrome_menu = Some(Menu::TabContext(menu));
            self.redraw();
        } else if hit != Some(Action::TabContextSurface) {
            self.dismiss_tab_context();
        }
        true
    }

    pub(super) fn tab_context_release(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let Some(Menu::TabContext(mut menu)) = self.chrome_menu else {
            return false;
        };
        let hit = self.chrome_action_at_pointer();
        let command = menu.release(hit, self.tab_hits_current);
        self.chrome_menu = Some(Menu::TabContext(menu));
        if let Some(command) = command {
            self.run_tab_context(menu.target, command, event_loop);
        }
        self.redraw();
        true
    }

    pub(super) fn cancel_tab_context_press(&mut self) {
        if let Some(Menu::TabContext(menu)) = &mut self.chrome_menu {
            menu.pressed = None;
        }
    }

    fn run_tab_context(
        &mut self,
        id: TabId,
        command: ContextCommand,
        event_loop: &ActiveEventLoop,
    ) {
        if !self.tab_context_enabled(id, command) {
            return;
        }
        self.dismiss_tab_context();
        self.action(
            match command {
                ContextCommand::Duplicate => Action::DuplicateTab(id),
                ContextCommand::Pin => Action::TogglePin(id),
                ContextCommand::Close => Action::Close(id),
                ContextCommand::Reopen => Action::ReopenTab,
            },
            event_loop,
        );
    }

    pub(super) fn tab_context_key(
        &mut self,
        key: &Key,
        command: Option<Command>,
        repeat: bool,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        let Some(Menu::TabContext(mut menu)) = self.chrome_menu else {
            return false;
        };
        if command.is_some() {
            // The flyout doesn't redefine the browser's ordinary tab shortcuts.
            self.dismiss_tab_context();
            return false;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                self.dismiss_tab_context();
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::Tab) => {
                let prev = *key == Key::Named(NamedKey::ArrowUp)
                    || (*key == Key::Named(NamedKey::Tab) && self.modifiers.shift_key());
                let enabled: Vec<_> = COMMANDS
                    .into_iter()
                    .filter(|row| self.tab_context_enabled(menu.target, *row))
                    .collect();
                if !enabled.is_empty() {
                    let current = menu
                        .selected
                        .and_then(|row| enabled.iter().position(|r| *r == row));
                    let next = match (prev, current) {
                        (true, Some(i)) => (i + enabled.len() - 1) % enabled.len(),
                        (false, Some(i)) => (i + 1) % enabled.len(),
                        (true, None) => enabled.len() - 1,
                        (false, None) => 0,
                    };
                    menu.selected = Some(enabled[next]);
                    menu.pressed = None;
                    self.chrome_menu = Some(Menu::TabContext(menu));
                    self.redraw();
                }
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if !repeat
                    && self.modifiers.is_empty()
                    && self.tab_hits_current
                    && let Some(row) = menu.selected
                {
                    self.run_tab_context(menu.target, row, event_loop);
                }
            }
            _ => {}
        }
        true
    }

    pub(super) fn draw_tab_context(&mut self, canvas: &mut Canvas, width: f32, menu: TabContext) {
        let Some(index) = self.tab_index(menu.target) else {
            return;
        };
        let u = self.scale;
        let Some(panel) = menu.bounds(width, canvas.height as f32 / u) else {
            return;
        };
        let bounds = physical_rect(u, panel);
        canvas.fill_rounded_rect(bounds, [10. * u; 4], CHROME_PANEL);
        canvas.outline_rounded_rect(bounds, [10. * u; 4], -u, u, CHROME_LINE);
        canvas.fill_rect(
            scaled_rect(u, panel.x + 10., panel.y + 82., panel.width - 20., 1.),
            CHROME_LINE,
        );
        self.hits.push((panel, Action::TabContextSurface));
        let pinned = self.tabs[index].pinned;
        for (i, command) in COMMANDS.into_iter().enumerate() {
            use icons::Glyph;
            let row = TabContext::row(panel, i);
            let enabled = self.tab_context_enabled(menu.target, command);
            let (title, glyph) = match command {
                ContextCommand::Duplicate => ("Duplikuj kartę", Glyph::Copy),
                ContextCommand::Pin if pinned => ("Odepnij kartę", Glyph::PinOff),
                ContextCommand::Pin => ("Przypnij kartę", Glyph::Pin),
                ContextCommand::Close => ("Zamknij kartę", Glyph::X),
                ContextCommand::Reopen => ("Przywróć kartę", Glyph::History),
            };
            let hit = Action::TabContextRow(menu.target, command);
            let hovered = enabled && self.pointer_in(row);
            let focused =
                enabled && (menu.selected == Some(command) || menu.pressed == Some(command));
            chrome_menu::draw_row_highlight(canvas, row, u, hovered, focused);
            let dim = if enabled { 1. } else { 0.45 };
            paint_centered_glyph(
                canvas,
                scaled_rect(1., row.x + 8., row.y, 17., row.height),
                17.,
                u,
                glyph,
                chrome_alpha(CHROME_MUTED, dim),
            );
            let suffix = if command == ContextCommand::Reopen {
                73.
            } else {
                8.
            };
            let text = chrome_elide(
                &self.fonts,
                title,
                (row.width - 35. - suffix).max(0.) * u,
                13. * u,
                400,
            );
            let run = chrome_shape(&self.fonts, &text, 13. * u, 400);
            canvas.text(
                (row.x + 35.) * u,
                (row.y + 22.) * u,
                &run,
                chrome_alpha(CHROME_TEXT, dim),
                &mut self.fonts,
            );
            if command == ContextCommand::Reopen {
                let shortcut = chrome_shape(&self.fonts, "Ctrl+Shift+T", 10. * u, 400);
                canvas.text(
                    (row.x + row.width - 7.) * u - shortcut.advance,
                    (row.y + 21.) * u,
                    &shortcut,
                    chrome_alpha(CHROME_MUTED, dim),
                    &mut self.fonts,
                );
            }
            if enabled {
                self.hits.push((row, hit));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_fits_at_both_window_edges_without_overlapping_rows() {
        let menu = TabContext::new(7, (388., 392.));
        let panel = menu.bounds(390., 400.).unwrap();
        assert_eq!(
            (panel.x + panel.width, panel.y + panel.height),
            (382., 392.)
        );
        for i in 0..4 {
            let row = TabContext::row(panel, i);
            assert!(panel.contains(row.x, row.y));
            assert!(panel.contains(row.x, row.y + row.height - 1.));
            if i > 0 {
                let previous = TabContext::row(panel, i - 1);
                assert!(previous.y + previous.height <= row.y);
            }
        }
        let panel = TabContext::new(7, (0., 0.)).bounds(390., 400.).unwrap();
        assert_eq!((panel.x, panel.y), (8., 8.));
        let panel = TabContext::new(7, (380., 10.)).bounds(390., 400.).unwrap();
        assert_eq!(panel.y, if WINDOW_FRAME { 31. } else { 10. });
    }

    #[test]
    fn release_needs_the_same_live_row_and_is_single_use() {
        let mut menu = TabContext::new(7, (200., 20.));
        let same = Some(Action::TabContextRow(7, ContextCommand::Close));
        let other = Some(Action::TabContextRow(8, ContextCommand::Close));
        assert_eq!(menu.release(same, true), None);
        menu.pressed = Some(ContextCommand::Close);
        assert_eq!(menu.release(other, true), None);
        menu.pressed = Some(ContextCommand::Close);
        assert_eq!(menu.release(same, false), None);
        menu.pressed = Some(ContextCommand::Close);
        assert_eq!(menu.release(same, true), Some(ContextCommand::Close));
        assert_eq!(menu.release(same, true), None);
    }
}
