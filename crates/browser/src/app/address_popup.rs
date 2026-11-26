//! Wiring for the local, explicitly activated omnibox popup.

use super::{
    Action, App, Canvas,
    address_bar::chrome::SuggestionPopup,
    address_bar::suggestions::{Direction, Source},
    chrome_box,
    icons::Glyph,
    paint_centered_glyph, quiet_cinder, reserve_downloads_button, scaled_rect,
};

impl App {
    pub(super) fn dismiss_address_suggestions(&mut self) -> bool {
        self.suggestion_scroll = 0.;
        self.hits.retain(|(_, action)| !action.address_suggestion());
        let changed = self.address_suggestions.dismiss();
        if changed {
            self.redraw();
        }
        changed
    }

    pub(super) fn refresh_address_suggestions(&mut self) {
        self.dismiss_address_suggestions();
        if !self.address_focus || self.preedit {
            return;
        }
        let bookmarks = self
            .bookmarks
            .as_ref()
            .ok()
            .into_iter()
            .flat_map(|store| store.entries())
            .map(|entry| (entry.title.as_str(), entry.url.as_str()));
        let history = &self.tabs[self.active].history;
        let history = (1..=32).flat_map(|distance| {
            [-distance, distance]
                .into_iter()
                .filter_map(move |delta| history.entry_url(delta))
        });
        self.address_suggestions
            .refresh(self.address.text(), bookmarks, history);
        self.redraw();
    }

    pub(super) fn suggestions_visible(&self) -> bool {
        self.address_focus
            && !self.preedit
            && !self.devtools_focused
            && self.chrome_menu.is_none()
            && self.history_menu.is_none()
            && self.settings_panel.is_none()
            && self.downloads_panel.is_none()
            && !self.address_suggestions.rows().is_empty()
    }

    fn suggestion_popup_layout<'a>(
        width: f32,
        height: f32,
        rows: &'a [super::address_bar::chrome::SuggestionRow<'a>],
        scroll: f32,
    ) -> Option<SuggestionPopup<'a>> {
        let mut toolbar = quiet_cinder::Toolbar::new(width);
        reserve_downloads_button(&mut toolbar);
        SuggestionPopup::new(
            chrome_box(toolbar.address),
            scaled_rect(1., 0., 0., width, height),
            rows,
            scroll,
        )
    }

    pub(super) fn step_address_suggestions(&mut self, direction: Direction) -> bool {
        if !self.suggestions_visible() || !self.address_suggestions.step(direction) {
            return false;
        }
        if let Some(window) = &self.window {
            let size = window.inner_size();
            let rows: Vec<_> = self
                .address_suggestions
                .rows()
                .iter()
                .map(|row| row.display())
                .collect();
            let selected = self.address_suggestions.selected();
            if let Some(scroll) = Self::suggestion_popup_layout(
                size.width as f32 / self.scale,
                size.height as f32 / self.scale,
                &rows,
                self.suggestion_scroll,
            )
            .and_then(|popup| popup.reveal(selected?))
            {
                if scroll != self.suggestion_scroll {
                    self.hits.retain(|(_, action)| !action.address_suggestion());
                    self.suggestion_scroll = scroll;
                }
            }
        }
        self.redraw();
        true
    }

    pub(super) fn draw_address_suggestions(
        &mut self,
        canvas: &mut Canvas,
        width: f32,
        height: f32,
    ) {
        if !self.suggestions_visible() {
            return;
        }
        let suggestions = self.address_suggestions.rows();
        let rows: Vec<_> = suggestions.iter().map(|row| row.display()).collect();
        let Some(popup) =
            Self::suggestion_popup_layout(width, height, &rows, self.suggestion_scroll)
        else {
            return;
        };
        let hovered = self
            .pointer_inside
            .then(|| popup.hit(self.mouse.0, self.mouse.1))
            .flatten();
        let u = self.scale;
        popup.paint(
            canvas,
            &mut self.fonts,
            u,
            self.address_suggestions.selected(),
            hovered,
            |canvas, index, bounds, color| {
                let glyph = match suggestions[index].source {
                    Source::Bookmark => Glyph::Star,
                    Source::TabHistory => Glyph::History,
                };
                paint_centered_glyph(canvas, bounds, 17., u, glyph, color);
            },
        );
        self.hits.push((popup.bounds, Action::SuggestionBackground));
        self.hits.extend(
            popup
                .visible_targets()
                .map(|(index, bounds)| (bounds, Action::Suggestion(index))),
        );
    }
}
