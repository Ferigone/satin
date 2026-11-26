//! Installed Windows updater chrome. Web content cannot open or activate it.
use super::{
    Action, ActiveEventLoop, App, CHROME_HOVER, CHROME_LINE, CHROME_MUTED, CHROME_PANEL,
    CHROME_TEXT, Canvas, Color, Key, ModifiersState, NamedKey, Rect, TOP, chrome_icon, elide,
    rounded_chrome_rect, scaled_rect,
};
use crate::updater::{UpdateAction, Updates, View};

const ACCENT: Color = Color(176, 203, 255, 255);
const BUTTON_X: f32 = 211.;
const TOOLBAR_ROOM: f32 = 50.;
const PANEL_WIDTH: f32 = 350.;

fn panel_rect(width: f32) -> Rect {
    let width_available = (width - 24.).clamp(1., PANEL_WIDTH);
    Rect {
        x: (width - BUTTON_X + 46. - width_available).max(12.),
        y: TOP + 4.,
        width: width_available,
        height: 120.,
    }
}

impl App {
    pub(super) fn update_view(&self) -> Option<View> {
        self.updater.as_ref()?.view(self.update_panel)
    }

    pub(super) fn update_applying(&self) -> bool {
        self.updater.as_ref().is_some_and(Updates::applying)
    }

    pub(super) fn dismiss_update_panel(&mut self) {
        if std::mem::take(&mut self.update_panel) {
            self.redraw();
        }
    }

    pub(super) fn toggle_update_panel(&mut self) {
        if self.update_panel {
            self.dismiss_update_panel();
        } else if self.update_view().is_some() {
            self.dismiss_history_menu();
            self.chrome_menu = None;
            self.select_popup = None;
            self.address_focus = false;
            self.preedit = false;
            self.set_dock_focus(false);
            self.hover = None;
            self.update_panel = true;
            self.redraw();
        }
    }

    pub(super) fn handle_update_click(
        &mut self,
        action: Option<Action>,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        if let Some(
            action @ (Action::UpdatePanel | Action::UpdatePrimary(_) | Action::UpdateSurface),
        ) = action
        {
            self.action(action, event_loop);
            return true;
        }
        if self.update_panel {
            self.dismiss_update_panel();
            // Don't click a page or dock control under a dismissed chrome panel.
            return action.is_none();
        }
        false
    }

    pub(super) fn update_key(&mut self, key: &Key, repeat: bool) -> bool {
        if self.settings_panel.is_some() {
            return false;
        }
        let toggle = self.modifiers == ModifiersState::ALT
            && matches!(key, Key::Character(text) if text.eq_ignore_ascii_case("u"));
        if toggle && self.update_view().is_some() {
            if !repeat {
                self.toggle_update_panel();
            }
            return true;
        }
        if !self.update_panel {
            return false;
        }
        match key {
            Key::Named(NamedKey::Escape) => self.dismiss_update_panel(),
            Key::Named(NamedKey::Enter | NamedKey::Space) if self.modifiers.is_empty() => {
                if !repeat
                    && self.tab_hits_current
                    && let Some(action) = self.hits.iter().rev().find_map(|(_, hit)| match hit {
                        Action::UpdatePrimary(action) => Some(*action),
                        _ => None,
                    })
                {
                    // Use what was painted (not a newly arrived worker state)
                    // so an extra keypress cannot activate an unpainted Restart.
                    self.update_command(action);
                }
            }
            _ => {}
        }
        true
    }

    pub(super) fn update_command(&mut self, action: UpdateAction) {
        if !self.update_panel
            || !self
                .updater
                .as_ref()
                .is_some_and(|updates| updates.can_start(action))
        {
            return;
        }
        // A fast double click or stale painted hit may still refer to the old action.
        // The updater separately checks the exact current action before accepting it.
        if action == UpdateAction::Restart
            && let Err(error) = self
                .save_session()
                .and_then(|_| self.profile.save_storage())
        {
            if let Some(updates) = &mut self.updater {
                updates.report_failure(UpdateAction::Restart, &error);
            }
        } else if let Some(updates) = &mut self.updater {
            updates.start_job(action);
        }
        self.redraw();
    }

    pub(super) fn poll_updates(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let Some(updates) = &mut self.updater else {
            return false;
        };
        let (changed, restart) = updates.poll();
        if let Some(restart) = restart {
            self.update_restart = Some(restart);
            self.finish_close(event_loop);
            return true;
        }
        if changed {
            self.redraw();
        }
        false
    }

    pub(super) fn draw_update_button(
        &mut self,
        canvas: &mut Canvas,
        width: f32,
        y: f32,
        height: f32,
    ) -> f32 {
        if self.update_view().is_none() {
            return 0.;
        }
        let x = width - BUTTON_X;
        let u = self.scale;
        let hovered = self.chrome_hover == Some(Action::UpdatePanel);
        if hovered || self.update_panel {
            rounded_chrome_rect(
                canvas,
                scaled_rect(u, x, y, 46., height),
                10. * u,
                CHROME_HOVER,
            );
        }
        chrome_icon(
            canvas,
            x + 13.,
            y + 13.,
            u,
            Action::UpdatePanel,
            false,
            ACCENT,
        );
        self.hits.push((
            Rect {
                x,
                y,
                width: 46.,
                height,
            },
            Action::UpdatePanel,
        ));
        TOOLBAR_ROOM
    }

    pub(super) fn draw_update_panel(&mut self, canvas: &mut Canvas, width: f32) {
        let Some(view) = self.update_view() else {
            return;
        };
        if self.settings_panel.is_some()
            || self.chrome_menu.is_some()
            || self.history_menu.is_some()
        {
            return;
        }
        let u = self.scale;
        let bounds = panel_rect(width);
        if !self.update_panel {
            if self.chrome_hover == Some(Action::UpdatePanel) {
                let tooltip = format!("{} · Alt+U", view.title);
                let tooltip_width = ((self
                    .fonts
                    .shape(&tooltip, 13. * u, false, false, false)
                    .advance
                    / u)
                    + 24.)
                    .min(bounds.width);
                let x = bounds.x + bounds.width - tooltip_width;
                rounded_chrome_rect(
                    canvas,
                    scaled_rect(u, x, bounds.y, tooltip_width, 30.),
                    7. * u,
                    CHROME_PANEL,
                );
                canvas.label(
                    &tooltip,
                    (x + 12.) * u,
                    (bounds.y + 20.) * u,
                    13. * u,
                    CHROME_TEXT,
                    false,
                    &mut self.fonts,
                );
            }
            return;
        }
        let Rect {
            x,
            y,
            width,
            height,
        } = bounds;
        let rect = |x, y, w, h| scaled_rect(u, x, y, w, h);
        rounded_chrome_rect(canvas, rect(x, y, width, height), 10. * u, CHROME_LINE);
        rounded_chrome_rect(
            canvas,
            rect(x + 1., y + 1., width - 2., height - 2.),
            9. * u,
            CHROME_PANEL,
        );
        self.hits.push((bounds, Action::UpdateSurface));
        let detail = elide(&self.fonts, &view.detail, (width - 32.) * u, 13. * u);
        for (text, baseline, size, color) in [
            (view.title, 26., 16., CHROME_TEXT),
            (&*detail, 51., 13., CHROME_MUTED),
        ] {
            canvas.label(
                text,
                (x + 16.) * u,
                (y + baseline) * u,
                size * u,
                color,
                false,
                &mut self.fonts,
            );
        }
        if let Some(progress) = view.progress {
            let track = width - 32.;
            canvas.fill_rect(rect(x + 16., y + 69., track, 5.), CHROME_HOVER);
            canvas.fill_rect(rect(x + 16., y + 69., track * progress, 5.), ACCENT);
        }
        if let Some((action, label)) = view.button {
            let button_width = 150.;
            // Separate Download and Restart hit regions, including across frames.
            let button_x = if action == UpdateAction::Restart {
                x + width - button_width - 14.
            } else {
                x + 14.
            };
            let button = Rect {
                x: button_x,
                y: y + 71.,
                width: button_width,
                height: 34.,
            };
            let hit = Action::UpdatePrimary(action);
            rounded_chrome_rect(
                canvas,
                rect(button.x, button.y, button.width, button.height),
                6. * u,
                ACCENT,
            );
            rounded_chrome_rect(
                canvas,
                rect(
                    button.x + 1.,
                    button.y + 1.,
                    button.width - 2.,
                    button.height - 2.,
                ),
                5. * u,
                if self.chrome_hover == Some(hit) {
                    CHROME_HOVER
                } else {
                    CHROME_PANEL
                },
            );
            canvas.label(
                label,
                (button.x + 12.) * u,
                (button.y + 22.) * u,
                13. * u,
                CHROME_TEXT,
                false,
                &mut self.fonts,
            );
            self.hits.push((button, hit));
        }
    }
}
