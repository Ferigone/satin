use super::{
    super::{Canvas, Color, Fonts, Rect, rounded_chrome_rect, scaled_rect},
    Access, Startup, Store,
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
    Appearance,
    Startup,
    About,
}

impl Section {
    const ALL: [Self; 3] = [Self::Appearance, Self::Startup, Self::About];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Wygląd",
            Self::Startup => "Przy uruchamianiu",
            Self::About => "Informacje o Satin",
        }
    }

    fn tab_order(self, can_update: bool) -> &'static [Control] {
        use Control::{BookmarksBar, Done, RestoreTabs, Shortcuts, Version};
        // Keep this order the same as the visual order: header, categories, card.
        const NAV: [Control; 4] = [
            Done,
            Control::Section(Section::Appearance),
            Control::Section(Section::Startup),
            Control::Section(Section::About),
        ];
        const APPEARANCE: [Control; 5] = [NAV[0], NAV[1], NAV[2], NAV[3], BookmarksBar];
        const STARTUP: [Control; 5] = [NAV[0], NAV[1], NAV[2], NAV[3], RestoreTabs];
        const ABOUT: [Control; 6] = [NAV[0], NAV[1], NAV[2], NAV[3], Version, Shortcuts];
        match self {
            Self::Appearance | Self::Startup if !can_update => &NAV,
            Self::Appearance => &APPEARANCE,
            Self::Startup => &STARTUP,
            Self::About => &ABOUT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Control {
    Done,
    Section(Section),
    BookmarksBar,
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
    nav: [Rect; 3],
    labels_only: bool,
    card: Rect,
}

impl PanelLayout {
    fn new(width: f32, height: f32, content_top: f32, section: Section) -> Self {
        let width = width.max(0.);
        let surface_y = content_top.min(height);
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
            let nav_width = ((w - 8.) / 3.).clamp(0., 148.);
            (
                std::array::from_fn(|i| {
                    rect(
                        x + i as f32 * (nav_width + 4.),
                        nav_y,
                        nav_width,
                        NAV_HEIGHT,
                    )
                }),
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
                std::array::from_fn(|i| {
                    rect(
                        x,
                        nav_y + i as f32 * (NAV_HEIGHT + 4.),
                        nav_width,
                        NAV_HEIGHT,
                    )
                }),
                x + nav_width + gap,
                nav_y,
            )
        };
        let rows = if section == Section::About { 2. } else { 1. };
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
            labels_only: width <= 470.,
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
            Control::Section(Section::Appearance) => self.nav[0],
            Control::Section(Section::Startup) => self.nav[1],
            Control::Section(Section::About) => self.nav[2],
            Control::BookmarksBar | Control::RestoreTabs => self.row_control(0, 44.),
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
    pub(super) fn new(can_update: bool) -> Self {
        Self {
            section: Section::Appearance,
            focus: if can_update {
                Control::BookmarksBar
            } else {
                Control::Section(Section::Appearance)
            },
            focus_visible: true,
            hovered: None,
            pressed: None,
            write_failed: false,
        }
    }

    pub(super) fn select(&mut self, section: Section) {
        self.section = section;
        self.focus_category();
        self.pressed = None;
    }

    pub(super) fn focus_category(&mut self) {
        self.focus = Control::Section(self.section);
    }

    pub(super) fn focused_control(&mut self) -> Control {
        self.focus_visible = true;
        self.focus
    }

    pub(super) fn step_focus(&mut self, backward: bool, can_update: bool) {
        let order = self.section.tab_order(can_update);
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
        content_top: f32,
        (x, y): (f32, f32),
        can_update: bool,
    ) -> Option<Control> {
        let layout = PanelLayout::new(width, height, content_top, self.section);
        if !layout.surface.contains(x, y) {
            return None;
        }
        self.section
            .tab_order(can_update)
            .iter()
            .copied()
            .find(|&control| layout.bounds(control).contains(x, y))
    }

    // Match the live document origin for settings paint and pointer routing.
    pub(in crate::app) fn draw_at(
        &self,
        canvas: &mut Canvas,
        fonts: &mut Fonts,
        scale: f32,
        settings: &Store,
        content_top: f32,
    ) {
        let width = canvas.width as f32 / scale;
        let height = canvas.height as f32 / scale;
        let layout = PanelLayout::new(width, height, content_top, self.section);
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
        for section in Section::ALL {
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
            if layout.labels_only {
                p.centered_label(section.label(), bounds, 12., 540, color);
            } else {
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
        }

        p.bordered(layout.card, 10., CARD, LINE);
        let x = layout.card.x + CARD_PAD;
        p.label(self.section.label(), x, layout.card.y + 25., 15., 670, TEXT);
        match self.section {
            Section::Appearance => {
                let caption = "Wyświetl zapisane strony pod paskiem nawigacji.";
                let lines = [caption, "Tryb skupienia tymczasowo ukrywa ten pasek."];
                self.draw_row(
                    &mut p,
                    &layout,
                    0,
                    "Pasek zakładek",
                    if settings.current.quiet_mode {
                        &lines
                    } else {
                        &lines[..1]
                    },
                );
                self.draw_switch(
                    &mut p,
                    &layout,
                    Control::BookmarksBar,
                    settings.current.bookmarks_bar,
                    settings.can_update(),
                );
            }
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
                self.draw_switch(
                    &mut p,
                    &layout,
                    Control::RestoreTabs,
                    settings.current.startup == Startup::RestoreTabs,
                    settings.can_update(),
                );
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
        if self.section != Section::About {
            let warning: &[&str] = match settings.access {
                Access::Unreadable => &[
                    "Nie można odczytać ustawień.",
                    "Użyto wartości domyślnych; plik zachowano.",
                ],
                Access::ReadOnly => &["Plik ustawień jest tylko do odczytu."],
                Access::ReadWrite if self.write_failed => {
                    &["Nie można zapisać ustawień. Nie wprowadzono zmian."]
                }
                Access::ReadWrite => &[],
            };
            for (i, line) in warning.iter().enumerate() {
                let y = layout.card.y + layout.card.height + 16. + 15. * i as f32;
                p.label(line, x, y, 11., 400, ACCENT);
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
        p.centered_label(label, bounds, 13., 600, TEXT);
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

    fn draw_switch(
        &self,
        p: &mut Painter<'_>,
        layout: &PanelLayout,
        control: Control,
        on: bool,
        enabled: bool,
    ) {
        let bounds = layout.bounds(control);
        let track = rect(bounds.x + 4., bounds.y + 7., 36., 20.);
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

    fn centered_label(&mut self, text: &str, r: Rect, size: f32, weight: u16, color: Color) {
        let run = self.shape(text, size, weight);
        let x = (r.x + r.width * 0.5) * self.scale - run.advance * 0.5;
        let y = (r.y + r.height * 0.5 + 4.5) * self.scale;
        self.canvas.text(x, y, &run, color, self.fonts);
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
            Section::Appearance => {
                let points = [
                    (3., 5.),
                    (6., 2.),
                    (11., 2.),
                    (15., 5.),
                    (15., 8.),
                    (13., 10.),
                    (10., 10.),
                    (9., 11.),
                    (9., 14.),
                    (7., 15.),
                    (4., 14.),
                    (2., 11.),
                    (2., 7.),
                    (3., 5.),
                ];
                for pair in points.windows(2) {
                    stroke(pair[0].0, pair[0].1, pair[1].0, pair[1].1);
                }
                for (x, y) in [(5., 8.), (8., 5.), (12., 6.)] {
                    stroke(x, y, x + 0.65, y);
                }
            }
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
        let mut panel = Panel::new(true);
        assert_eq!(panel.focused_control(), Control::BookmarksBar);
        panel.step_focus(false, true);
        assert_eq!(panel.focused_control(), Control::Done);
        panel.step_focus(true, true);
        assert_eq!(panel.focused_control(), Control::BookmarksBar);
        panel.select(Section::Startup);
        panel.step_focus(true, true);
        assert_eq!(
            panel.focused_control(),
            Control::Section(Section::Appearance)
        );

        let mut panel = Panel::new(false);
        panel.step_focus(false, false);
        assert_eq!(panel.focused_control(), Control::Section(Section::Startup));
        panel.select(Section::Startup);
        panel.step_focus(false, false);
        assert_eq!(panel.focused_control(), Control::Section(Section::About));
        panel.step_focus(false, false);
        assert_eq!(panel.focused_control(), Control::Done);
        panel.select(Section::Appearance);
        panel.step_focus(true, false);
        assert_eq!(panel.focused_control(), Control::Done);
        panel.step_focus(true, false);
        assert_eq!(panel.focused_control(), Control::Section(Section::About));
        panel.select(Section::About);
        panel.step_focus(false, false);
        assert_eq!(panel.focused_control(), Control::Version);
        panel.step_focus(false, false);
        assert_eq!(panel.focused_control(), Control::Shortcuts);
        panel.step_focus(false, false);
        assert_eq!(panel.focused_control(), Control::Done);
    }

    #[test]
    fn hit_targets_follow_the_current_category_and_fit_the_minimum_window() {
        let mut panel = Panel::new(true);
        for top in [
            super::super::chrome::ChromeState::new(true, true, true).content_top(),
            crate::app::quiet_cinder::CONTENT_TOP,
        ] {
            for (w, h) in [(390., 400.), (640., 400.), (750., 480.), (1440., 900.)] {
                let center = |r: Rect| (r.x + r.width * 0.5, r.y + r.height * 0.5);
                for (section, control) in [
                    (Section::Appearance, Control::BookmarksBar),
                    (Section::Startup, Control::RestoreTabs),
                ] {
                    panel.select(section);
                    let layout = PanelLayout::new(w, h, top, section);
                    let switch = center(layout.bounds(control));
                    assert_eq!(panel.control_at(w, h, top, switch, true), Some(control));
                    assert_eq!(panel.control_at(w, h, top, switch, false), None);
                    assert_eq!(
                        panel.control_at(w, h, top, (layout.card.x + CARD_PAD, switch.1), true),
                        None
                    );
                }
                panel.select(Section::About);
                let layout = PanelLayout::new(w, h, top, Section::About);
                assert!(layout.card.y + layout.card.height <= h);
                for &control in Section::About.tab_order(false) {
                    let bounds = layout.bounds(control);
                    assert_eq!(
                        panel.control_at(w, h, top, center(bounds), false),
                        Some(control)
                    );
                }
            }
        }
    }

    #[test]
    fn bookmarks_switch_paints_saved_state_even_when_quiet_overrides_row() {
        use super::super::Preferences;
        let mut fonts = Fonts::new().unwrap();
        let state = |bookmarks_bar| Preferences {
            bookmarks_bar,
            quiet_mode: true,
            ..Preferences::default()
        };
        let render = |fonts: &mut Fonts, current, access, scale: f32| {
            let mut canvas =
                Canvas::new((390. * scale) as u32, (400. * scale) as u32, Color::WHITE);
            let settings = Store {
                path: Default::default(),
                current,
                access,
            };
            let top = settings.chrome().content_top();
            let bounds = PanelLayout::new(390., 400., top, Section::Appearance)
                .bounds(Control::BookmarksBar);
            Panel::new(settings.can_update()).draw_at(&mut canvas, fonts, scale, &settings, top);
            let pixel = |x: f32, y: f32| {
                let x = ((bounds.x + x) * scale).floor() as usize;
                let y = ((bounds.y + y) * scale).floor() as usize;
                canvas.pixels[y * canvas.width as usize + x]
            };
            // Left thumb, track between both thumb positions, right thumb and focus outline.
            [
                pixel(14., 17.),
                pixel(22., 17.),
                pixel(30., 17.),
                pixel(22., 4.),
            ]
        };
        let rgb = |color| Canvas::new(1, 1, color).pixels[0];
        for scale in [1., 1.5] {
            let on = render(&mut fonts, state(true), Access::ReadWrite, scale);
            let off = render(&mut fonts, state(false), Access::ReadWrite, scale);
            let disabled = render(&mut fonts, state(false), Access::ReadOnly, scale);
            assert_eq!(
                on,
                [rgb(ACCENT), rgb(ACCENT), rgb(Color::WHITE), rgb(ACCENT)]
            );
            assert_eq!(
                off,
                [rgb(Color::WHITE), rgb(MUTED), rgb(MUTED), rgb(ACCENT)]
            );
            assert_eq!(
                disabled,
                [rgb(MUTED), rgb(ACCENT_SOFT), rgb(ACCENT_SOFT), rgb(CARD)]
            );
        }
    }

    #[test]
    fn cancelling_pointer_clears_hover_and_prevents_a_stale_release() {
        let mut panel = Panel::new(true);
        assert!(panel.set_hover(Some(Control::BookmarksBar)));
        assert!(panel.press(Some(Control::BookmarksBar)));
        assert!(panel.is_pressed(Control::BookmarksBar));
        assert!(panel.cancel_pointer());
        assert!(panel.release().is_none());
        assert!(!panel.is_pressed(Control::BookmarksBar));
        assert!(!panel.cancel_pointer());
    }
}
