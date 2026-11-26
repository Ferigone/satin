//! Deferred tab clicks and a transactional reorder gesture. Dragging moves a
//! preview, not App's tab vector; only release commits the permutation.

use super::*;

pub(super) enum TabPress {
    Select(TabDrag),
    Close(TabId),
}

impl App {
    pub(super) fn start_tab_press(&mut self, action: Action) -> bool {
        let press = match action {
            Action::Select(id) => {
                let Some(index) = self.tab_index(id) else {
                    return false;
                };
                let strip = self.tab_strip(self.window_regions(self.active).page.width);
                TabPress::Select(TabDrag::new(
                    id,
                    self.mouse,
                    strip.face(index),
                    strip.scroll(),
                ))
            }
            Action::Close(id) => TabPress::Close(id),
            _ => return false,
        };
        self.select_popup = None;
        self.caption_pressed = None;
        self.tab_press = Some(press);
        self.redraw();
        true
    }

    pub(super) fn cancel_tab_press(&mut self) {
        if let Some(TabPress::Select(drag)) = self.tab_press.take()
            && drag.started
        {
            self.tab_scroll = drag.initial_scroll;
            self.tab_hits_current = false;
            if let Some(window) = &self.window {
                window.set_cursor(CursorIcon::Default);
            }
            self.redraw();
        }
    }

    fn drag_overlay_open(&self) -> bool {
        self.settings_panel.is_some()
            || self.history_menu.is_some()
            || self.chrome_menu.is_some()
            || self.downloads_panel.is_some()
            || self.site_info.is_some()
            || self.update_panel_open()
            || self.select_popup.is_some()
            || self.resizing_dock
    }

    pub(super) fn track_tab_press(&mut self) -> bool {
        if self.drag_overlay_open() {
            self.cancel_tab_press();
            return false;
        }
        if !matches!(self.tab_press, Some(TabPress::Select(_))) {
            return false;
        }
        let Some(TabPress::Select(mut drag)) = self.tab_press.take() else {
            unreachable!()
        };
        if !drag.sample(self.mouse) {
            self.tab_press = Some(TabPress::Select(drag));
            return false;
        }
        let Some(source) = self.tab_index(drag.id) else {
            return false;
        };
        let strip = self.tab_strip(self.window_regions(self.active).page.width);
        drag.destination = drag.target(
            source,
            self.pinned_tabs(),
            self.tabs.len(),
            self.mouse,
            strip.list,
            |index| strip.face(index),
        );
        self.tab_press = Some(TabPress::Select(drag));
        self.chrome_hover = None;
        self.hover = None;
        if let Some(window) = &self.window {
            window.set_cursor(CursorIcon::Grabbing);
        }
        self.redraw();
        true
    }

    pub(super) fn tick_tab_drag(&mut self, now: Instant) -> Option<Instant> {
        let Some(TabPress::Select(drag)) = &self.tab_press else {
            return None;
        };
        if !drag.started || !self.pointer_inside || self.drag_overlay_open() {
            return None;
        }
        let strip = self.tab_strip(self.window_regions(self.active).page.width);
        let next = (strip.scroll() + TabDrag::edge_step(self.mouse, strip.list))
            .clamp(0., strip.max_scroll());
        if next == strip.scroll() {
            return None;
        }
        let interval = Duration::from_millis(16);
        let due = drag.last_scroll + interval;
        if now < due {
            return Some(due);
        }
        self.tab_scroll = next;
        self.tab_hits_current = false;
        if let Some(TabPress::Select(drag)) = &mut self.tab_press {
            drag.last_scroll = now;
        }
        self.track_tab_press();
        Some(now + interval)
    }

    pub(super) fn finish_tab_press(&mut self, event_loop: &ActiveEventLoop) -> bool {
        if self.tab_press.is_none() {
            return false;
        }
        if !self.pointer_inside || self.drag_overlay_open() {
            self.cancel_tab_press();
            return true;
        }
        if matches!(self.tab_press, Some(TabPress::Select(_))) {
            self.track_tab_press();
        }
        let action = match self.tab_press.take() {
            Some(TabPress::Select(drag)) if drag.started => {
                self.tab_hits_current = false;
                if let Some(window) = &self.window {
                    window.set_cursor(CursorIcon::Default);
                }
                if let Some(from) = self.tab_index(drag.id)
                    && let Some(to) = drag.destination
                {
                    if from != to {
                        let active = self.tabs[self.active].id;
                        tab_behavior::place_tab(&mut self.tabs, from, to);
                        self.tabs_reordered(active);
                    }
                    // After a successful pointer drop, keep the dragged tab
                    // in view even if it wasn't the active document.
                    let width = self.window_regions(self.active).page.width;
                    self.tab_scroll = self.tab_strip(width).reveal(to).scroll();
                } else {
                    self.tab_scroll = drag.initial_scroll;
                }
                self.redraw();
                return true;
            }
            Some(TabPress::Select(drag)) => Action::Select(drag.id),
            Some(TabPress::Close(id)) => Action::Close(id),
            None => return true,
        };
        if self.frame_resize_at_pointer().is_none()
            && let Some(action) = matching_tab_release(
                action,
                self.chrome_action_at_pointer(),
                self.tab_hits_current,
            )
        {
            self.action(action, event_loop);
        }
        true
    }

    pub(super) fn right_tab_or_frame_press(&mut self) {
        if !self.pointer_inside
            || self.history_menu.is_some()
            || self.downloads_panel.is_some()
            || self.update_panel_open()
            || self.settings_panel.is_some()
            || !self.tab_hits_current
        {
            return;
        }
        let hit = self.chrome_action_at_pointer();
        let context_open = matches!(self.chrome_menu, Some(Menu::TabContext(_)));
        if let Some(Action::Select(id) | Action::Close(id)) = hit
            && (self.chrome_menu.is_none() || context_open)
            && self.select_popup.is_none()
            && self.frame_resize_at_pointer().is_none()
        {
            self.open_tab_context(id);
        } else {
            if context_open {
                self.dismiss_tab_context();
            }
            if WINDOW_FRAME
                && self.mouse.1 < TAB_H
                && hit.is_none()
                && let Some(window) = &self.window
            {
                window.show_window_menu(LogicalPosition::new(self.mouse.0, self.mouse.1));
            }
        }
    }
}

fn matching_tab_release(pressed: Action, hit: Option<Action>, presented: bool) -> Option<Action> {
    (presented && hit == Some(pressed)).then_some(pressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_click_cannot_release_as_a_close_or_against_a_stale_frame() {
        let press = Action::Select(17);
        assert!(matching_tab_release(press, Some(Action::Close(17)), true).is_none());
        assert!(matching_tab_release(press, Some(Action::Select(18)), true).is_none());
        assert!(matching_tab_release(press, Some(press), false).is_none());
        assert!(matching_tab_release(press, Some(press), true) == Some(press));
    }
}
