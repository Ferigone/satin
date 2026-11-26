//! Installed Windows updater chrome. Web content cannot open or activate it.
use super::{
    Action, ActiveEventLoop, App, CHROME_ACCENT, CHROME_ACCENT_SOFT, CHROME_HOVER, CHROME_LINE,
    CHROME_MUTED, CHROME_PANEL, CHROME_TEXT, Canvas, CursorIcon, Key, ModifiersState,
    NAVIGATION_BAR, NamedKey, Rect, chrome_box, elide, icons::Glyph, paint_centered_glyph,
    physical_rect, resize_cursor, rounded_chrome_rect, scaled_rect,
};
use crate::updater::{UpdateAction, Updates, View};
use std::collections::HashSet;
use winit::{event::ElementState, keyboard::PhysicalKey};

const PANEL_WIDTH: f32 = 350.;

fn panel_rect(width: f32, anchor: Rect) -> Rect {
    let width_available = (width - 24.).clamp(1., PANEL_WIDTH);
    Rect {
        x: (anchor.x + anchor.width - width_available).max(12.),
        y: anchor.y + anchor.height + 8.,
        width: width_available,
        height: 137.,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::quiet_cinder;
    use winit::keyboard::KeyCode;

    #[test]
    fn offer_and_restart_have_distinct_hits_inside_both_panels() {
        for width in [390., 1280.] {
            let bar = quiet_cinder::Toolbar::new(width, true);
            let badge = bar.update.unwrap();
            let panel = panel_rect(width, chrome_box(badge));
            let download = primary_rect(panel, UpdateAction::Download);
            let restart = primary_rect(panel, UpdateAction::Restart);
            assert!(panel.x >= 12. && panel.x + panel.width <= width - 12.);
            assert!(panel.y > badge.y + badge.h);
            assert!(panel.y + panel.height <= 400.);
            assert!(download.x > panel.x);
            assert!(download.x + download.width < restart.x);
            assert!(restart.x + restart.width < panel.x + panel.width);
            assert!(restart.y + restart.height < panel.y + panel.height);
        }
    }

    #[test]
    fn flyout_keys_cannot_repeat_into_ready_or_into_the_dismissed_page() {
        use ElementState::{Pressed, Released};
        for code in [
            KeyCode::Enter,
            KeyCode::Space,
            KeyCode::Escape,
            KeyCode::KeyU,
        ] {
            let mut keys = OwnedKeys::default();
            let key = PhysicalKey::Code(code);
            // Only the fresh press reaches the updater handler.
            assert!(!keys.blocks(key, Pressed, false, false, true));
            assert!(keys.blocks(key, Pressed, true, false, true));
            assert!(keys.blocks(key, Pressed, true, false, false));
            assert!(keys.blocks(key, Released, false, true, false));
            assert!(keys.blocks(key, Pressed, false, true, false));
            assert!(keys.blocks(key, Pressed, true, false, false));
            assert!(keys.blocks(key, Released, false, false, false));
            assert!(!keys.blocks(key, Pressed, false, false, false));
        }
    }

    #[test]
    fn synthetic_presses_cannot_activate_the_panel_and_held_keys_stay_consumed() {
        use ElementState::Pressed;
        let mut keys = OwnedKeys::default();
        let key = PhysicalKey::Code(KeyCode::Space);
        assert!(keys.blocks(key, Pressed, false, true, true));
        assert!(!keys.blocks(key, Pressed, false, false, false));
        keys.capture_held();
        // A key held before the panel opened can't activate its primary or
        // leak after dismissal, even with no intervening repeat in the panel.
        assert!(keys.blocks(key, Pressed, true, false, false));
        assert!(keys.blocks(key, Pressed, true, false, true));
        assert!(keys.blocks(key, Pressed, true, false, false));
        // A fresh real press still works if the release happened unfocused.
        assert!(!keys.blocks(key, Pressed, false, false, false));
    }
}

fn primary_rect(bounds: Rect, action: UpdateAction) -> Rect {
    let width = (bounds.width - 28.).clamp(1., 150.);
    // A stale mouse position from Download must not hit a newly painted Restart.
    let x = if action == UpdateAction::Restart {
        bounds.x + bounds.width - width - 14.
    } else {
        bounds.x + 14.
    };
    scaled_rect(1., x, bounds.y + 71., width, 34.)
}

/// A key consumed by the flyout must not repeat into another surface after
/// dismissal, including when releasing Alt changes the repeated logical key.
#[derive(Default)]
pub(super) struct OwnedKeys {
    held: HashSet<PhysicalKey>,
    captured: HashSet<PhysicalKey>,
}

impl OwnedKeys {
    fn capture_held(&mut self) {
        self.captured.extend(self.held.iter().copied());
    }

    pub(super) fn blocks(
        &mut self,
        physical: PhysicalKey,
        state: ElementState,
        repeat: bool,
        synthetic: bool,
        owns_key: bool,
    ) -> bool {
        if state == ElementState::Released {
            if !synthetic {
                self.held.remove(&physical);
                self.captured.remove(&physical);
            }
            return true;
        }
        if synthetic {
            return owns_key || self.captured.contains(&physical);
        }
        self.held.insert(physical);
        if repeat {
            if owns_key {
                self.captured.insert(physical);
            }
            return self.captured.contains(&physical);
        }
        // A fresh physical press is new intent, even if a release went to
        // another window while this window was unfocused.
        self.captured.remove(&physical);
        if owns_key {
            self.captured.insert(physical);
        }
        false
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
        if self.update_panel {
            self.update_panel = false;
            self.tab_hits_current = false;
            self.chrome_hover = None;
            self.clear_update_primary();
            if let Some(window) = &self.window {
                window.set_ime_allowed(
                    self.settings_panel.is_none() && self.downloads_panel.is_none(),
                );
                window.set_cursor(CursorIcon::Default);
            }
            self.redraw();
        }
    }

    pub(super) fn toggle_update_panel(&mut self) {
        if self.update_panel {
            self.dismiss_update_panel();
        } else if self.settings_panel.is_none() && self.update_view().is_some() {
            self.update_panel = true;
            self.update_keys.capture_held();
            self.update_ime_blocked = true;
            self.tab_hits_current = false;
            self.clear_update_primary();
            self.dismiss_downloads();
            self.dismiss_history_menu();
            self.dismiss_address_suggestions();
            self.chrome_menu = None;
            self.select_popup = None;
            self.address_focus = false;
            self.preedit = false;
            self.set_dock_focus(false);
            self.chrome_hover = None;
            self.hover = None;
            self.caption_pressed = None;
            self.last_title_press = None;
            self.wheel = None;
            self.resizing_dock = false;
            if let Some(window) = &self.window {
                window.set_ime_allowed(false);
            }
            self.redraw();
        }
    }

    pub(super) fn update_press(&mut self) {
        // Stay modal even between presentations. Outside presses only dismiss;
        // they must never also select a suggestion, Downloads, or web content.
        if self.tab_hits_current {
            match self.chrome_action_at_pointer() {
                Some(Action::UpdatePrimary(action)) => self.update_command(action),
                Some(Action::UpdateSurface) => {}
                _ => self.dismiss_update_panel(),
            }
        }
    }

    pub(super) fn update_panel_pointer(&mut self) {
        let action = self.chrome_action_at_pointer();
        if self.chrome_hover != action {
            self.chrome_hover = action;
            self.redraw();
        }
        if let Some(window) = &self.window {
            window.set_cursor(self.frame_resize_at_pointer().map_or_else(
                || match action {
                    Some(Action::UpdatePanel | Action::UpdatePrimary(_)) => CursorIcon::Pointer,
                    _ => CursorIcon::Default,
                },
                resize_cursor,
            ));
        }
    }

    fn update_shortcut(&self, key: &Key) -> bool {
        self.modifiers == ModifiersState::ALT
            && matches!(key, Key::Character(text) if text.eq_ignore_ascii_case("u"))
            && self.settings_panel.is_none()
            && self.update_view().is_some()
    }

    pub(super) fn update_owns_key(&self, key: &Key) -> bool {
        self.update_panel || self.update_shortcut(key)
    }

    pub(super) fn update_key(&mut self, key: &Key, repeat: bool) -> bool {
        if self.update_shortcut(key) {
            if !repeat {
                self.toggle_update_panel();
            }
            return true;
        }
        if !self.update_panel {
            return false;
        }
        match key {
            Key::Named(NamedKey::Escape) if !repeat => self.dismiss_update_panel(),
            Key::Named(NamedKey::Enter | NamedKey::Space)
                if !repeat && self.modifiers.is_empty() =>
            {
                if let Some(action) = self.presented_update_primary() {
                    self.update_command(action);
                }
            }
            _ => {}
        }
        true
    }

    fn clear_update_primary(&mut self) {
        self.hits
            .retain(|(_, action)| !matches!(action, Action::UpdatePrimary(_)));
    }

    fn presented_update_primary(&self) -> Option<UpdateAction> {
        if !self.update_panel || !self.tab_hits_current {
            return None;
        }
        self.hits.iter().rev().find_map(|(_, hit)| match hit {
            Action::UpdatePrimary(action) => Some(*action),
            _ => None,
        })
    }

    pub(super) fn update_command(&mut self, action: UpdateAction) {
        if self.presented_update_primary() != Some(action)
            || !self
                .updater
                .as_ref()
                .is_some_and(|updates| updates.can_start(action))
        {
            return;
        }
        self.clear_update_primary();
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
        let had_offer = self.update_view().is_some();
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
            // A new primary button is not actionable before its own frame.
            self.clear_update_primary();
            if had_offer != self.update_view().is_some() {
                self.tab_hits_current = false;
                self.chrome_hover = None;
            }
            self.redraw();
        }
        false
    }

    pub(super) fn draw_update_button(&mut self, canvas: &mut Canvas, bounds: Rect) {
        let u = self.scale;
        let hovered = self.chrome_hover == Some(Action::UpdatePanel);
        if hovered || self.update_panel {
            rounded_chrome_rect(canvas, physical_rect(u, bounds), 6. * u, CHROME_HOVER);
        }
        paint_centered_glyph(canvas, bounds, 17., u, Glyph::RotateCw, CHROME_ACCENT);
        let x = bounds.x + bounds.width - 9.;
        let y = bounds.y + 4.;
        rounded_chrome_rect(
            canvas,
            scaled_rect(u, x, y, 7., 7.),
            3.5 * u,
            NAVIGATION_BAR,
        );
        rounded_chrome_rect(
            canvas,
            scaled_rect(u, x + 1., y + 1., 5., 5.),
            2.5 * u,
            CHROME_ACCENT,
        );
        self.hits.push((bounds, Action::UpdatePanel));
    }

    pub(super) fn draw_update_panel(&mut self, canvas: &mut Canvas, width: f32) {
        let Some(view) = self.update_view() else {
            return;
        };
        if self.settings_panel.is_some()
            || self.chrome_menu.is_some()
            || self.history_menu.is_some()
            || self.downloads_panel.is_some()
            || self.suggestions_visible()
            || self.select_popup.is_some()
        {
            return;
        }
        let Some(anchor) = self.toolbar_layout(width).update else {
            return;
        };
        let u = self.scale;
        let bounds = panel_rect(width, chrome_box(anchor));
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
                let tooltip = elide(&self.fonts, &tooltip, (tooltip_width - 24.) * u, 13. * u);
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
        let title = elide(&self.fonts, view.title, (width - 32.) * u, 16. * u);
        let detail = elide(&self.fonts, &view.detail, (width - 32.) * u, 13. * u);
        for (text, baseline, size, color) in [
            (title.as_str(), 26., 16., CHROME_TEXT),
            (detail.as_str(), 51., 13., CHROME_MUTED),
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
            canvas.fill_rect(rect(x + 16., y + 69., track * progress, 5.), CHROME_ACCENT);
        }
        if let Some((action, label)) = view.button {
            canvas.label(
                "Enter / Space to select · Esc to close",
                (x + 16.) * u,
                (y + 123.) * u,
                11. * u,
                CHROME_MUTED,
                false,
                &mut self.fonts,
            );
            let button = primary_rect(bounds, action);
            let hit = Action::UpdatePrimary(action);
            rounded_chrome_rect(
                canvas,
                rect(button.x, button.y, button.width, button.height),
                6. * u,
                CHROME_ACCENT,
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
                    CHROME_ACCENT_SOFT
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
