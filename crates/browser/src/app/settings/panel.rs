use super::{
    super::{Canvas, Color, Fonts, Rect, TOP, rounded_chrome_rect, scaled_rect},
    Store,
};
use rust_browser_engine::fonts::{FontFamily, FontRequest, FontSet, FontStyle, GenericFamily, Run};
use std::sync::{Arc, OnceLock};

// The fixed Amethyst palette from Quiet Cinder. This panel is painted in
// browser chrome, independently of the current document and its zoom.
const PAGE: Color = Color(25, 25, 28, 255);
const CARD: Color = Color(41, 40, 44, 255);
const TEXT: Color = Color(240, 237, 240, 255);
const MUTED: Color = Color(184, 175, 181, 255);
const LINE: Color = Color(62, 58, 66, 255);
const HOVER: Color = Color(51, 48, 56, 255);
const ACCENT: Color = Color(196, 163, 244, 255);
const ACCENT_SOFT: Color = Color(68, 61, 80, 255);
const SCRIM: Color = Color(16, 21, 26, 101);

const NAV_HEIGHT: f32 = 33.;
const ROW_HEIGHT: f32 = 61.;
const CARD_HEAD: f32 = 38.;
const CARD_PAD: f32 = 16.;
const BUTTON: f32 = 34.;
const ACTION_WIDTH: f32 = 82.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Section {
    Startup,
    About,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Self::Startup => "Przy uruchamianiu",
            Self::About => "Informacje o Satin",
        }
    }

    fn tab_order(self, unreadable: bool) -> &'static [Control] {
        use Control::{Done, RestoreTabs, Shortcuts, Version};
        // Keep this order the same as the visual order: header, categories, card.
        const NAV: [Control; 3] = [
            Done,
            Control::Section(Section::Startup),
            Control::Section(Section::About),
        ];
        const STARTUP: [Control; 4] = [NAV[0], NAV[1], NAV[2], RestoreTabs];
        const ABOUT: [Control; 5] = [NAV[0], NAV[1], NAV[2], Version, Shortcuts];
        match self {
            Self::Startup if unreadable => &NAV,
            Self::Startup => &STARTUP,
            Self::About => &ABOUT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Control {
    Done,
    Section(Section),
    RestoreTabs,
    Version,
    Shortcuts,
}

struct PanelLayout {
    surface: Rect,
    title_size: f32,
    title_baseline: f32,
    subtitle_baseline: f32,
    done: Rect,
    nav: [Rect; 2],
    card: Rect,
}

impl PanelLayout {
    fn new(width: f32, height: f32, section: Section) -> Self {
        let width = width.max(0.);
        let surface_y = TOP.min(height);
        let surface = rect(0., surface_y, width, height - surface_y);
        let narrow = width <= 660.;
        let short = surface.height < 320.;
        let margin = if narrow {
            12.
        } else {
            (width * 0.05).clamp(17., 64.)
        };
        let content = width.min(1110.);
        let x = (width - content) * 0.5 + margin;
        let w = (content - 2. * margin).max(0.);
        let y = surface_y
            + if short {
                8.
            } else if narrow {
                25.
            } else {
                44.
            };
        let title_size = (width * 0.04).clamp(27., 45.);
        let title_baseline = y + title_size;
        let subtitle_baseline = title_baseline + if short { 19. } else { 22. };
        let nav_y = subtitle_baseline + if short { 12. } else { 23. };
        let (nav, card_x, card_y) = if narrow {
            let nav_width = ((w - 4.) * 0.5).clamp(0., 148.);
            (
                [
                    rect(x, nav_y, nav_width, NAV_HEIGHT),
                    rect(x + nav_width + 4., nav_y, nav_width, NAV_HEIGHT),
                ],
                x,
                nav_y + NAV_HEIGHT + if short { 8. } else { 12. },
            )
        } else {
            let (nav_width, gap) = if width <= 880. {
                (145., 12.)
            } else {
                (188., 23.)
            };
            (
                [
                    rect(x, nav_y, nav_width, NAV_HEIGHT),
                    rect(x, nav_y + NAV_HEIGHT + 4., nav_width, NAV_HEIGHT),
                ],
                x + nav_width + gap,
                nav_y,
            )
        };
        let rows = if section == Section::Startup { 1. } else { 2. };
        let card = rect(
            card_x,
            card_y,
            (x + w - card_x).max(0.),
            CARD_HEAD + rows * ROW_HEIGHT + 8.,
        );
        Self {
            surface,
            title_size,
            title_baseline,
            subtitle_baseline,
            done: rect(x + w - ACTION_WIDTH, y, ACTION_WIDTH, BUTTON),
            nav,
            card,
        }
    }

    fn row_top(&self, index: usize) -> f32 {
        self.card.y + CARD_HEAD + index as f32 * ROW_HEIGHT
    }

    fn row_control(&self, index: usize, width: f32) -> Rect {
        rect(
            self.card.x + self.card.width - CARD_PAD - width,
            self.row_top(index) + (ROW_HEIGHT - BUTTON) * 0.5,
            width,
            BUTTON,
        )
    }

    fn bounds(&self, control: Control) -> Rect {
        match control {
            Control::Done => self.done,
            Control::Section(Section::Startup) => self.nav[0],
            Control::Section(Section::About) => self.nav[1],
            Control::RestoreTabs => self.row_control(0, 44.),
            Control::Version => self.row_control(0, ACTION_WIDTH),
            Control::Shortcuts => self.row_control(1, ACTION_WIDTH),
        }
    }
}

pub(in crate::app) struct Panel {
    section: Section,
    focus: Control,
    focus_visible: bool,
    hovered: Option<Control>,
    pressed: Option<Control>,
    write_failed: bool,
}

impl Panel {
    pub(super) fn new(unreadable: bool) -> Self {
        Self {
            section: Section::Startup,
            focus: if unreadable {
                Control::Section(Section::Startup)
            } else {
                Control::RestoreTabs
            },
            focus_visible: true,
            hovered: None,
            pressed: None,
            write_failed: false,
        }
    }

    pub(super) fn select(&mut self, section: Section) {
        self.section = section;
        self.focus = Control::Section(section);
        self.pressed = None;
    }

    pub(super) fn focused_control(&mut self) -> Control {
        self.focus_visible = true;
        self.focus
    }

    pub(super) fn step_focus(&mut self, backward: bool, unreadable: bool) {
        let order = self.section.tab_order(unreadable);
        let index = order
            .iter()
            .position(|&control| control == self.focus)
            .unwrap_or(0);
        let step = if backward { order.len() - 1 } else { 1 };
        self.focus = order[(index + step) % order.len()];
        self.focus_visible = true;
    }

    pub(super) fn set_hover(&mut self, control: Option<Control>) -> bool {
        std::mem::replace(&mut self.hovered, control) != control
    }

    pub(super) fn press(&mut self, control: Option<Control>) -> bool {
        let changed = self.pressed != control;
        self.pressed = control;
        if let Some(control) = control {
            self.focus = control;
            self.focus_visible = false;
        }
        changed
    }

    pub(super) fn release(&mut self) -> Option<Control> {
        self.pressed.take()
    }

    pub(super) fn cancel_pointer(&mut self) -> bool {
        self.hovered.take().is_some() | self.pressed.take().is_some()
    }

    pub(super) fn set_write_failed(&mut self, failed: bool) {
        self.write_failed = failed;
    }

    pub(super) fn control_at(
        &self,
        width: f32,
        height: f32,
        (x, y): (f32, f32),
        unreadable: bool,
    ) -> Option<Control> {
        let layout = PanelLayout::new(width, height, self.section);
        if !layout.surface.contains(x, y) {
            return None;
        }
        self.section
            .tab_order(unreadable)
            .iter()
            .copied()
            .find(|&control| layout.bounds(control).contains(x, y))
    }

    pub(in crate::app) fn draw(
        &self,
        canvas: &mut Canvas,
        fonts: &mut Fonts,
        scale: f32,
        settings: &Store,
    ) {
        let width = canvas.width as f32 / scale;
        let height = canvas.height as f32 / scale;
        let layout = PanelLayout::new(width, height, self.section);
        let mut p = Painter {
            canvas,
            fonts,
            scale,
        };
        // The existing settings overlay stays modal. Keep window chrome visible
        // but dimmed rather than showing a selected tab that does not exist.
        p.fill(rect(0., 0., width, layout.surface.y), SCRIM);
        p.fill(layout.surface, PAGE);
        let title_x = layout.nav[0].x;
        p.label(
            "Ustawienia",
            title_x,
            layout.title_baseline,
            layout.title_size,
            600,
            TEXT,
        );
        p.label(
            "Ustawienia przeglądarki Satin",
            title_x,
            layout.subtitle_baseline,
            13.,
            400,
            MUTED,
        );
        self.draw_action(&mut p, &layout, Control::Done, "Gotowe");
        for section in [Section::Startup, Section::About] {
            let control = Control::Section(section);
            let bounds = layout.bounds(control);
            self.draw_focus(&mut p, control, bounds, 6., PAGE);
            let color = if self.section == section || self.hovered == Some(control) {
                p.round(
                    bounds,
                    6.,
                    if self.is_pressed(control) {
                        HOVER
                    } else {
                        ACCENT_SOFT
                    },
                );
                ACCENT
            } else {
                TEXT
            };
            p.section_icon(section, bounds.x + 9., bounds.y + 8., color);
            p.label(
                section.label(),
                bounds.x + 34.,
                bounds.y + 21.,
                13.,
                540,
                color,
            );
        }

        p.bordered(layout.card, 10., CARD, LINE);
        let x = layout.card.x + CARD_PAD;
        p.label(self.section.label(), x, layout.card.y + 25., 15., 670, TEXT);
        match self.section {
            Section::Startup => {
                self.draw_row(
                    &mut p,
                    &layout,
                    0,
                    "Przywróć otwarte karty",
                    &[
                        "Otwiera zapisane karty przy następnym uruchomieniu.",
                        "Gdy wyłączone, Satin otwiera stronę startową.",
                    ],
                );
                self.draw_switch(&mut p, &layout, settings);
                let warning = if settings.unreadable {
                    Some("Nie można odczytać ustawień. Użyto wartości domyślnej; plik zachowano.")
                } else if self.write_failed {
                    Some("Nie można zapisać ustawień. Nie wprowadzono zmian.")
                } else {
                    None
                };
                if let Some(warning) = warning {
                    p.label(
                        warning,
                        x,
                        layout.card.y + layout.card.height + 16.,
                        11.,
                        400,
                        ACCENT,
                    );
                }
            }
            Section::About => {
                self.draw_row(
                    &mut p,
                    &layout,
                    0,
                    "Satin",
                    &["Sprawdź wersję i możliwości przeglądarki."],
                );
                self.draw_action(&mut p, &layout, Control::Version, "Informacje");
                self.draw_row(
                    &mut p,
                    &layout,
                    1,
                    "Skróty klawiaturowe",
                    &["Zobacz skróty dostępne w Satin."],
                );
                self.draw_action(&mut p, &layout, Control::Shortcuts, "Pokaż");
            }
        }
    }

    fn is_pressed(&self, control: Control) -> bool {
        self.pressed == Some(control) && self.hovered == Some(control)
    }

    fn draw_focus(
        &self,
        p: &mut Painter<'_>,
        control: Control,
        bounds: Rect,
        radius: f32,
        backdrop: Color,
    ) {
        if self.focus_visible && self.focus == control {
            p.round(inset(bounds, -4.), radius + 4., ACCENT);
            p.round(inset(bounds, -2.), radius + 2., backdrop);
        }
    }

    fn draw_action(
        &self,
        p: &mut Painter<'_>,
        layout: &PanelLayout,
        control: Control,
        label: &str,
    ) {
        let bounds = layout.bounds(control);
        let backdrop = if control == Control::Done { PAGE } else { CARD };
        self.draw_focus(p, control, bounds, 6., backdrop);
        let fill = if self.is_pressed(control) {
            ACCENT_SOFT
        } else if self.hovered == Some(control) {
            HOVER
        } else {
            CARD
        };
        p.bordered(bounds, 6., fill, LINE);
        p.action_label(label, bounds);
    }

    fn draw_row(
        &self,
        p: &mut Painter<'_>,
        layout: &PanelLayout,
        index: usize,
        label: &str,
        caption: &[&str],
    ) {
        let x = layout.card.x + CARD_PAD;
        let y = layout.row_top(index);
        p.fill(
            rect(x, y, (layout.card.width - 2. * CARD_PAD).max(0.), 1.),
            LINE,
        );
        let baseline = y + if caption.len() == 1 { 26. } else { 20. };
        p.label(label, x, baseline, 13., 620, TEXT);
        for (i, line) in caption.iter().enumerate() {
            p.label(line, x, baseline + 17. + i as f32 * 15., 11., 400, MUTED);
        }
    }

    fn draw_switch(&self, p: &mut Painter<'_>, layout: &PanelLayout, settings: &Store) {
        let control = Control::RestoreTabs;
        let bounds = layout.bounds(control);
        let track = rect(bounds.x + 4., bounds.y + 7., 36., 20.);
        let enabled = !settings.unreadable;
        let on = settings.current.startup == super::Startup::RestoreTabs;
        if enabled {
            let backdrop = if self.hovered == Some(control) {
                let fill = if self.is_pressed(control) {
                    ACCENT_SOFT
                } else {
                    HOVER
                };
                p.round(bounds, 17., fill);
                fill
            } else {
                CARD
            };
            self.draw_focus(p, control, track, 10., backdrop);
        }
        let fill = match (enabled, on) {
            (true, true) => ACCENT,
            (true, false) => MUTED,
            (false, _) => ACCENT_SOFT,
        };
        p.bordered(track, 10., fill, LINE);
        let thumb_x = track.x + if on { 20. } else { 4. };
        let thumb = if enabled { Color::WHITE } else { MUTED };
        p.round(rect(thumb_x, track.y + 4., 12., 12.), 6., thumb);
    }
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    scaled_rect(1., x, y, width, height)
}

fn inset(bounds: Rect, n: f32) -> Rect {
    rect(
        bounds.x + n,
        bounds.y + n,
        bounds.width - 2. * n,
        bounds.height - 2. * n,
    )
}

struct Painter<'a> {
    canvas: &'a mut Canvas,
    fonts: &'a mut Fonts,
    scale: f32,
}

impl Painter<'_> {
    fn physical(&self, r: Rect) -> Rect {
        scaled_rect(self.scale, r.x, r.y, r.width, r.height)
    }

    fn fill(&mut self, r: Rect, color: Color) {
        self.canvas.fill_rect(self.physical(r), color);
    }

    fn round(&mut self, r: Rect, radius: f32, color: Color) {
        if r.width > 0. && r.height > 0. {
            let physical = self.physical(r);
            rounded_chrome_rect(self.canvas, physical, radius * self.scale, color);
        }
    }

    fn bordered(&mut self, r: Rect, radius: f32, color: Color, border: Color) {
        self.round(r, radius, border);
        self.round(inset(r, 1.), radius - 1., color);
    }

    fn shape(&self, text: &str, size: f32, weight: u16) -> Run {
        static FAMILIES: OnceLock<Arc<[FontFamily]>> = OnceLock::new();
        let families = FAMILIES.get_or_init(|| {
            Arc::from([
                FontFamily::Named("Segoe UI Variable Text".into()),
                FontFamily::Named("Segoe UI".into()),
                FontFamily::Generic(GenericFamily::SystemUi),
                FontFamily::Generic(GenericFamily::SansSerif),
            ])
        });
        // Do not use document fonts; a site must not affect chrome.
        let request = FontRequest {
            families: Arc::clone(families),
            weight,
            style: FontStyle::Normal,
        };
        self.fonts
            .shape_text(text, size * self.scale, &request, &FontSet::default())
    }

    fn label(&mut self, text: &str, x: f32, baseline: f32, size: f32, weight: u16, color: Color) {
        let run = self.shape(text, size, weight);
        self.canvas.text(
            x * self.scale,
            baseline * self.scale,
            &run,
            color,
            self.fonts,
        );
    }

    fn action_label(&mut self, text: &str, r: Rect) {
        let run = self.shape(text, 13., 600);
        let x = (r.x + r.width * 0.5) * self.scale - run.advance * 0.5;
        let y = (r.y + r.height * 0.5 + 4.5) * self.scale;
        self.canvas.text(x, y, &run, TEXT, self.fonts);
    }

    fn section_icon(&mut self, section: Section, x: f32, y: f32, color: Color) {
        let mut stroke = |a: f32, b: f32, c: f32, d: f32| {
            self.canvas.stroke_line(
                (x + a) * self.scale,
                (y + b) * self.scale,
                (x + c) * self.scale,
                (y + d) * self.scale,
                1.2 * self.scale,
                color,
            );
        };
        match section {
            Section::Startup => {
                stroke(5., 3., 13., 8.5);
                stroke(13., 8.5, 5., 14.);
                stroke(5., 14., 5., 3.);
            }
            Section::About => {
                let points = [
                    (8.5, 1.5),
                    (13.5, 3.5),
                    (15.5, 8.5),
                    (13.5, 13.5),
                    (8.5, 15.5),
                    (3.5, 13.5),
                    (1.5, 8.5),
                    (3.5, 3.5),
                    (8.5, 1.5),
                ];
                for pair in points.windows(2) {
                    stroke(pair[0].0, pair[0].1, pair[1].0, pair[1].1);
                }
                stroke(8.5, 8., 8.5, 12.);
                stroke(8., 5., 9., 5.);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_wraps_and_only_visits_available_controls() {
        let mut panel = Panel::new(false);
        assert_eq!(panel.focused_control(), Control::RestoreTabs);
        panel.step_focus(false, false);
        assert_eq!(panel.focused_control(), Control::Done);
        panel.step_focus(true, false);
        assert_eq!(panel.focused_control(), Control::RestoreTabs);

        let mut panel = Panel::new(true);
        panel.step_focus(false, true);
        assert_eq!(panel.focused_control(), Control::Section(Section::About));
        panel.select(Section::About);
        panel.step_focus(false, true);
        assert_eq!(panel.focused_control(), Control::Version);
        panel.step_focus(false, true);
        assert_eq!(panel.focused_control(), Control::Shortcuts);
        panel.step_focus(false, true);
        assert_eq!(panel.focused_control(), Control::Done);
    }

    #[test]
    fn hit_targets_follow_the_current_category_and_fit_the_minimum_window() {
        let mut panel = Panel::new(false);
        for (w, h) in [(640., 400.), (750., 480.), (1440., 900.)] {
            panel.select(Section::Startup);
            let layout = PanelLayout::new(w, h, Section::Startup);
            let center = |r: Rect| (r.x + r.width * 0.5, r.y + r.height * 0.5);
            let switch = center(layout.bounds(Control::RestoreTabs));
            assert_eq!(
                panel.control_at(w, h, switch, false),
                Some(Control::RestoreTabs)
            );
            assert_eq!(panel.control_at(w, h, switch, true), None);
            assert_eq!(
                panel.control_at(w, h, (layout.card.x + CARD_PAD, switch.1), false),
                None
            );

            panel.select(Section::About);
            let layout = PanelLayout::new(w, h, Section::About);
            assert!(layout.card.y + layout.card.height <= h);
            for &control in Section::About.tab_order(false) {
                let bounds = layout.bounds(control);
                assert_eq!(panel.control_at(w, h, center(bounds), false), Some(control));
            }
        }
    }

    #[test]
    fn cancelling_pointer_clears_hover_and_prevents_a_stale_release() {
        let mut panel = Panel::new(false);
        assert!(panel.set_hover(Some(Control::RestoreTabs)));
        assert!(panel.press(Some(Control::RestoreTabs)));
        assert!(panel.is_pressed(Control::RestoreTabs));
        assert!(panel.cancel_pointer());
        assert!(panel.release().is_none());
        assert!(!panel.is_pressed(Control::RestoreTabs));
        assert!(!panel.cancel_pointer());
    }
}
