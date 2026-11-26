//! Persisted chrome policy and shared paint/hit geometry for the focus control.
use super::super::{
    CHROME_ACCENT, CHROME_ACCENT_SOFT, CHROME_HOVER, CHROME_ICON, CHROME_TEXT, Canvas, Color, Rect,
    chrome_box,
    quiet_cinder::{Box2, CONTENT_TOP, TOOLBAR_END, Toolbar},
    rounded_chrome_rect, scaled_rect,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) struct ChromeState {
    bookmarks_bar: bool,
    quiet_mode: bool,
    can_update: bool,
}

impl ChromeState {
    pub(super) fn new(bookmarks_bar: bool, quiet_mode: bool, can_update: bool) -> Self {
        Self {
            bookmarks_bar,
            quiet_mode,
            can_update,
        }
    }

    /// The saved preference is independent of the effective row visibility.
    pub(in crate::app) fn bookmarks_bar_visible(self) -> bool {
        self.bookmarks_bar && !self.quiet_mode
    }

    pub(in crate::app) fn content_top(self) -> f32 {
        if self.bookmarks_bar_visible() {
            CONTENT_TOP
        } else {
            TOOLBAR_END
        }
    }

    pub(in crate::app) fn quiet_menu_label(self) -> &'static str {
        if self.quiet_mode {
            "Wyłącz tryb skupienia"
        } else {
            "Włącz tryb skupienia"
        }
    }

    pub(in crate::app) fn quiet_enabled(self) -> bool {
        self.can_update
    }

    pub(in crate::app) fn toolbar(self, width: f32) -> ToolbarLayout {
        let mut toolbar = Toolbar::new(width);
        let gap = (toolbar.menu.x - toolbar.address.right()).max(0.);
        let mut right = toolbar.menu;
        let mut reserve = || {
            let space = toolbar.menu.w + gap;
            toolbar.address.w = (toolbar.address.w - space).max(0.);
            toolbar.edit.w = (toolbar.edit.w - space).max(0.);
            toolbar.bookmark.x -= space;
            right.x -= space;
            right
        };
        // Menu stays put. The reference places focus to the left of any
        // secondary controls. Downloads is the only such native control here.
        let downloads = (!self.quiet_mode).then(&mut reserve);
        let quiet = QuietButton {
            bounds: chrome_box(reserve()),
            pressed: self.quiet_mode,
            enabled: self.quiet_enabled(),
            label: "Tryb skupienia",
        };
        ToolbarLayout {
            toolbar,
            quiet,
            downloads,
        }
    }
}

pub(in crate::app) struct ToolbarLayout {
    pub toolbar: Toolbar,
    pub quiet: QuietButton,
    pub downloads: Option<Box2>,
}

impl ToolbarLayout {
    /// The downloads flyout still needs an anchor when opened by Ctrl+J while
    /// the dedicated button is hidden. This is an anchor, never a hit target.
    pub(in crate::app) fn downloads_anchor(&self) -> Box2 {
        self.downloads.unwrap_or(self.toolbar.menu)
    }
}

pub(in crate::app) struct QuietButton {
    pub bounds: Rect,
    pub pressed: bool,
    pub enabled: bool,
    pub label: &'static str,
}

impl QuietButton {
    pub(in crate::app) fn hit_rect(&self) -> Option<Rect> {
        self.enabled.then_some(self.bounds)
    }

    pub(in crate::app) fn paint(&self, canvas: &mut Canvas, scale: f32, hovered: bool) {
        let fill = if self.pressed {
            Some(CHROME_ACCENT_SOFT)
        } else if self.enabled && hovered {
            Some(CHROME_HOVER)
        } else {
            None
        };
        if let Some(fill) = fill {
            let Rect {
                x,
                y,
                width,
                height,
            } = self.bounds;
            rounded_chrome_rect(
                canvas,
                scaled_rect(scale, x, y, width, height),
                6. * scale,
                fill,
            );
        }
        let color = if self.pressed {
            CHROME_ACCENT
        } else if hovered {
            CHROME_TEXT
        } else {
            CHROME_ICON
        };
        let color = if self.enabled {
            color
        } else {
            Color(color.0, color.1, color.2, 84)
        };
        paint_quiet_glyph(canvas, self.bounds, scale, color);
    }
}

pub(in crate::app) fn paint_quiet_glyph(
    canvas: &mut Canvas,
    bounds: Rect,
    scale: f32,
    color: Color,
) {
    // Four open corners and a small ring, matching the reference's focus mark.
    let x = (bounds.x + bounds.width * 0.5) * scale;
    let y = (bounds.y + bounds.height * 0.5) * scale;
    let u = 17. * scale / 24.;
    let mut line = |a: (f32, f32), b: (f32, f32)| {
        canvas.stroke_line(
            x + a.0 * u,
            y + a.1 * u,
            x + b.0 * u,
            y + b.1 * u,
            1.65 * u,
            color,
        );
    };
    for (dx, dy) in [(-1., -1.), (1., -1.), (-1., 1.), (1., 1.)] {
        let corner = (8. * dx, 8. * dy);
        line((3. * dx, corner.1), corner);
        line(corner, (corner.0, 3. * dy));
    }
    let ring = [
        (3., 0.),
        (2.12, 2.12),
        (0., 3.),
        (-2.12, 2.12),
        (-3., 0.),
        (-2.12, -2.12),
        (0., -3.),
        (2.12, -2.12),
        (3., 0.),
    ];
    for pair in ring.windows(2) {
        line(pair[0], pair[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::NAVIGATION_BAR;

    #[test]
    fn content_top_tracks_effective_favorites_row_in_all_saved_combinations() {
        for bar in [false, true] {
            for quiet in [false, true] {
                let state = ChromeState::new(bar, quiet, true);
                assert_eq!(state.bookmarks_bar_visible(), bar && !quiet);
                assert_eq!(
                    state.content_top(),
                    if bar && !quiet {
                        CONTENT_TOP
                    } else {
                        TOOLBAR_END
                    }
                );
            }
        }
    }

    #[test]
    fn the_toolbar_reserves_non_overlapping_visible_bounds_and_a_quiet_anchor() {
        for width in [390., 410., 411., 660., 661., 880., 881., 1280.] {
            let normal = ChromeState::new(true, false, true).toolbar(width);
            let quiet = ChromeState::new(true, true, true).toolbar(width);
            for layout in [&normal, &quiet] {
                let bar = &layout.toolbar;
                let focus = layout.quiet.hit_rect().unwrap();
                assert!(bar.edit.w > 0.);
                assert!(bar.edit.right() < bar.bookmark.x);
                assert!(bar.bookmark.right() < bar.address.right());
                assert!(bar.address.right() < focus.x);
                assert!(focus.x + focus.width < bar.menu.x);
            }
            let downloads = normal.downloads.unwrap();
            assert!(normal.quiet.bounds.x + normal.quiet.bounds.width < downloads.x);
            assert!(downloads.right() < normal.toolbar.menu.x);
            assert_eq!(normal.downloads_anchor(), downloads);
            assert!(quiet.downloads.is_none());
            assert_eq!(quiet.downloads_anchor(), quiet.toolbar.menu);
            assert_eq!(normal.toolbar.menu, quiet.toolbar.menu);
            assert!(quiet.toolbar.address.w > normal.toolbar.address.w);
        }
    }

    #[test]
    fn focus_button_paint_and_hits_follow_pressed_hover_and_read_only_state() {
        let sample = |button: QuietButton, hovered| {
            let mut canvas = Canvas::new(585, 150, NAVIGATION_BAR);
            button.paint(&mut canvas, 1.5, hovered);
            // Sample the button background, clear of the glyph and rounded corners.
            let x = ((button.bounds.x + 6.) * 1.5) as usize;
            let y = ((button.bounds.y + button.bounds.height * 0.5) * 1.5) as usize;
            canvas.pixels[y * canvas.width as usize + x]
        };
        let rgb = |color| Canvas::new(1, 1, color).pixels[0];
        let normal = ChromeState::new(true, false, true);
        assert_eq!(normal.quiet_menu_label(), "Włącz tryb skupienia");
        assert_eq!(normal.toolbar(390.).quiet.label, "Tryb skupienia");
        assert_eq!(
            sample(normal.toolbar(390.).quiet, false),
            rgb(NAVIGATION_BAR)
        );
        assert_eq!(sample(normal.toolbar(390.).quiet, true), rgb(CHROME_HOVER));
        let quiet = ChromeState::new(true, true, true);
        assert_eq!(quiet.quiet_menu_label(), "Wyłącz tryb skupienia");
        assert_eq!(
            sample(quiet.toolbar(390.).quiet, false),
            rgb(CHROME_ACCENT_SOFT)
        );
        let read_only = ChromeState::new(true, true, false).toolbar(390.).quiet;
        assert!(read_only.pressed);
        assert!(read_only.hit_rect().is_none());
        assert_eq!(sample(read_only, true), rgb(CHROME_ACCENT_SOFT));
    }
}
