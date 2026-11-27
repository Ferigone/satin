//! Quiet Cinder overflow menu, using the actual toolbar hit as its anchor.
//!
//! App integration: delegate the `Menu::Main` drawing branch to `draw_main_menu`;
//! use `first_item()` on open and `step_item()` for the existing Arrow/Tab routing.
//! In the guarded `Action::MenuItem` arm call `activate_main_menu_item`. Leave
//! `MENU_ITEMS` and the bookmarks return path intact: their IDs are still used.
//! Escape/Enter/Space and outside-click handling stay owned by App.
//! In cursor routing, `MenuItem(BACKGROUND_ITEM)` should use the default cursor,
//! while still occluding the page or dock. A popup wheel event can be consumed
//! using the same menu-hit guard; the available rows all fit the minimum window.
//!
//! Only wired commands are shown. The reference's private window, reopen closed
//! tab, global history, downloads, extensions, open-tabs page, workspaces,
//! find-in-page, split view and quiet mode still need real browser routes.
//! Native per-tab history is retained as such; fixed Cinder has no appearance row.

use super::{
    Action, ActiveEventLoop, App, Canvas, Color, Command, Fonts, MENU_ITEMS, Rect, TAB_H, elide,
    icons::Glyph, paint_centered_glyph, rounded_chrome_rect, scaled_rect, shortcuts,
};

fn draw_glyph(canvas: &mut Canvas, glyph: Glyph, x: f32, y: f32, scale: f32, color: Color) {
    paint_centered_glyph(
        canvas,
        scaled_rect(1., x, y, ICON_SIZE, ICON_SIZE),
        ICON_SIZE,
        scale,
        glyph,
        color,
    );
}

const CARD: Color = Color(41, 40, 44, 255);
const LINE: Color = Color(62, 58, 66, 255);
const HOVER: Color = Color(51, 48, 56, 255);
const TEXT: Color = Color(240, 237, 240, 255);
const MUTED: Color = Color(184, 175, 181, 255);
const ICON: Color = Color(198, 187, 195, 255);
const FOCUS: Color = Color(196, 163, 244, 255);
const WIDTH: f32 = 352.;
const VIEWPORT_GAP: f32 = 8.;
const ANCHOR_GAP: f32 = 5.;
const PADDING: f32 = 7.; // 1 px border plus 6 px internal padding.
const ROW_HEIGHT: f32 = 35.;
const MIN_ROW_HEIGHT: f32 = 22.;
const DIVIDER_HEIGHT: f32 = 10.;
const ZOOM_BUTTON: f32 = 31.;
const ZOOM_HEIGHT: f32 = ZOOM_BUTTON + 2. * 7.;
const ROW_INSET: f32 = 8.;
const ICON_SIZE: f32 = 17.;
const FONT_SIZE: f32 = 13.;

// The panel hit is pushed before its children. It swallows padding/divider
// clicks (also over the dock) without invoking a command.
pub(super) const BACKGROUND_ITEM: usize = usize::MAX;

const EXTRA_COMMANDS: &[Command] = &[
    Command::ZoomOut,
    Command::ResetZoom,
    Command::ZoomIn,
    Command::ToggleDevtools,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Browser(Action),
    Shortcut(Command),
}

impl Target {
    fn index(self) -> usize {
        match self {
            Self::Browser(action) => MENU_ITEMS
                .iter()
                .position(|(candidate, _)| *candidate == action),
            Self::Shortcut(command) => EXTRA_COMMANDS
                .iter()
                .position(|candidate| *candidate == command)
                .map(|i| MENU_ITEMS.len() + i),
        }
        .expect("each menu target has an App dispatch ID")
    }

    fn at(index: usize) -> Option<Self> {
        if let Some((action, _)) = MENU_ITEMS.get(index) {
            return Some(Self::Browser(*action));
        }
        EXTRA_COMMANDS
            .get(index.checked_sub(MENU_ITEMS.len())?)
            .copied()
            .map(Self::Shortcut)
    }
}

#[derive(Clone, Copy)]
enum Row {
    Item(Target, Glyph, &'static str, Option<Command>),
    Divider,
    Zoom,
}

impl Row {
    fn height(self) -> f32 {
        match self {
            Self::Item(..) => ROW_HEIGHT,
            Self::Divider => DIVIDER_HEIGHT,
            Self::Zoom => ZOOM_HEIGHT,
        }
    }

    fn targets(self) -> impl Iterator<Item = Target> + Clone {
        match self {
            Self::Item(target, ..) => [Some(target), None, None],
            Self::Divider => [None; 3],
            Self::Zoom => [
                Some(Target::Shortcut(Command::ZoomOut)),
                Some(Target::Shortcut(Command::ResetZoom)),
                Some(Target::Shortcut(Command::ZoomIn)),
            ],
        }
        .into_iter()
        .flatten()
    }
}

const ROWS: &[Row] = &[
    Row::Item(
        Target::Browser(Action::NewTab),
        Glyph::Plus,
        "Nowa karta",
        Some(Command::NewTab),
    ),
    Row::Item(
        Target::Browser(Action::Home),
        Glyph::House,
        "Strona startowa",
        None,
    ),
    Row::Item(
        Target::Browser(Action::StartShortcuts),
        Glyph::BookMarked,
        "Skróty startowe",
        None,
    ),
    Row::Divider,
    Row::Item(
        Target::Browser(Action::VisitHistory),
        Glyph::History,
        "Historia",
        Some(Command::VisitHistory),
    ),
    Row::Item(
        Target::Browser(Action::TabHistory),
        Glyph::History,
        "Historia tej karty",
        None,
    ),
    Row::Item(
        Target::Browser(Action::Bookmarks),
        Glyph::BookMarked,
        "Zakładki",
        None,
    ),
    Row::Item(
        Target::Browser(Action::Downloads),
        Glyph::Download,
        "Pobrane pliki",
        Some(Command::Downloads),
    ),
    Row::Divider,
    Row::Zoom,
    Row::Item(
        Target::Shortcut(Command::ToggleDevtools),
        Glyph::Code,
        "Narzędzia deweloperskie",
        Some(Command::ToggleDevtools),
    ),
    Row::Divider,
    Row::Item(
        Target::Browser(Action::QuietMode),
        Glyph::Focus,
        "Tryb skupienia",
        None,
    ),
    Row::Item(
        Target::Browser(Action::Settings),
        Glyph::Settings,
        "Ustawienia",
        None,
    ),
    Row::Item(
        Target::Browser(Action::Version),
        Glyph::Info,
        "O Satin",
        None,
    ),
    #[cfg(all(windows, target_arch = "x86_64"))]
    Row::Item(
        Target::Browser(Action::CheckUpdates),
        Glyph::Download,
        "Sprawdź aktualizacje",
        None,
    ),
    Row::Item(
        Target::Browser(Action::ShortcutHelp),
        Glyph::Keyboard,
        "Skróty klawiaturowe",
        None,
    ),
];

fn focus_order() -> impl Iterator<Item = usize> + Clone {
    ROWS.iter()
        .copied()
        .flat_map(Row::targets)
        .map(Target::index)
}

pub(super) fn first_item() -> usize {
    focus_order().next().expect("overflow menu is not empty")
}

pub(super) fn step_item(selected: usize, previous: bool) -> usize {
    let mut order = focus_order();
    let Some(position) = order.clone().position(|index| index == selected) else {
        return first_item();
    };
    let len = order.clone().count();
    let step = if previous { len - 1 } else { 1 };
    order
        .nth((position + step) % len)
        .expect("wrapped overflow focus index")
}

struct Layout {
    panel: Rect,
    density: f32,
}

impl Layout {
    fn new(window_width: f32, window_height: f32, anchor: Rect) -> Option<Self> {
        let width = WIDTH.min(window_width - 18.);
        let rows_height = ROWS.iter().map(|row| row.height()).sum::<f32>();
        // Compress only when the full panel cannot fit below native titlebar
        // hits. Its first row must never compete with caption or tab targets.
        let floor = TAB_H;
        let bottom = window_height - VIEWPORT_GAP;
        let height = (rows_height + 2. * PADDING).min(bottom - floor);
        let density = (height - 2. * PADDING) / rows_height;
        if width < 190. || ROW_HEIGHT * density < MIN_ROW_HEIGHT {
            return None;
        }
        let right = anchor.x + anchor.width;
        let mut top = anchor.y + anchor.height + ANCHOR_GAP;
        if top + height > bottom && anchor.y - 6. - height >= floor {
            top = anchor.y - height - 6.;
        }
        Some(Self {
            panel: Rect {
                x: (right - width).clamp(VIEWPORT_GAP, window_width - width - VIEWPORT_GAP),
                y: top.clamp(floor, (bottom - height).max(floor)),
                width,
                height,
            },
            density,
        })
    }

    fn rows(&self) -> impl Iterator<Item = (Row, Rect)> + '_ {
        ROWS.iter().copied().scan(self.panel.y + PADDING, |y, row| {
            let bounds = Rect {
                x: self.panel.x + PADDING,
                y: *y,
                width: self.panel.width - 2. * PADDING,
                height: row.height() * self.density,
            };
            *y += bounds.height;
            Some((row, bounds))
        })
    }
}

impl App {
    pub(super) fn step_main_menu(&self, selected: usize, previous: bool) -> usize {
        let next = step_item(selected, previous);
        if !self.settings.chrome().quiet_enabled()
            && Target::at(next) == Some(Target::Browser(Action::QuietMode))
        {
            step_item(next, previous)
        } else {
            next
        }
    }

    pub(super) fn draw_main_menu(&mut self, canvas: &mut Canvas, selected: usize) {
        let Some(anchor) = self
            .hits
            .iter()
            .rev()
            .find_map(|(bounds, action)| (*action == Action::Menu).then_some(*bounds))
        else {
            return;
        };
        let scale = self.scale;
        let Some(layout) = Layout::new(
            canvas.width as f32 / scale,
            canvas.height as f32 / scale,
            anchor,
        ) else {
            return;
        };
        draw_panel(canvas, layout.panel, scale);
        self.hits
            .push((layout.panel, Action::MenuItem(BACKGROUND_ITEM)));
        for (row, bounds) in layout.rows() {
            match row {
                Row::Item(target, glyph, label, hint) => {
                    let index = target.index();
                    let quiet = target == Target::Browser(Action::QuietMode);
                    let enabled = !quiet || self.settings.chrome().quiet_enabled();
                    let label = if quiet {
                        self.settings.chrome().quiet_menu_label()
                    } else {
                        label
                    };
                    let hovered = enabled && self.pointer_in(bounds);
                    let focused = enabled && index == selected;
                    draw_row_highlight(canvas, bounds, scale, hovered, focused);
                    let text_right = hint.map_or(bounds.x + bounds.width - ROW_INSET, |command| {
                        let hint = shortcuts::label(command);
                        // The reference advertises the primary F12 binding.
                        let primary = hint.split_once(" / ").map_or(hint, |(first, _)| first);
                        let bg = if hovered || focused { HOVER } else { CARD };
                        draw_hint(canvas, &mut self.fonts, bounds, scale, primary, bg) - 10.
                    });
                    draw_row_label(
                        canvas,
                        &mut self.fonts,
                        scale,
                        bounds,
                        glyph,
                        label,
                        (10., text_right),
                    );
                    if enabled {
                        self.hits.push((bounds, Action::MenuItem(index)));
                    }
                }
                Row::Divider => {
                    let line = Rect {
                        x: bounds.x + 3.,
                        y: bounds.y + (bounds.height - 1.) / 2.,
                        width: bounds.width - 6.,
                        height: 1.,
                    };
                    canvas.fill_rect(snap_rect(line, scale), LINE);
                }
                Row::Zoom => self.draw_zoom_row(canvas, row, bounds, selected),
            }
        }
    }

    fn draw_zoom_row(&mut self, canvas: &mut Canvas, row: Row, bounds: Rect, selected: usize) {
        let scale = self.scale;
        let mid = bounds.y + bounds.height / 2.;
        // In a compact menu the zoom hitboxes must remain within their row,
        // independent of the unchanged preferred horizontal button width.
        let button_height = ZOOM_BUTTON.min(bounds.height - 2.);
        // The reference's - / current percent / + cluster uses a 5 px gap.
        let percent = format!("{:.0}%", self.tabs[self.active].zoom * 100.);
        let label_width = self
            .fonts
            .shape(&percent, FONT_SIZE * scale, false, false, false)
            .advance;
        let reset_width = (label_width / scale).ceil() + 16.;
        let width = ZOOM_BUTTON * 2. + reset_width + 10.;
        let x = bounds.x + bounds.width - ROW_INSET - width;
        if self.pointer_in(bounds) {
            draw_row_highlight(canvas, bounds, scale, true, false);
        }
        draw_row_label(
            canvas,
            &mut self.fonts,
            scale,
            bounds,
            Glyph::ZoomIn,
            "Powiększenie",
            (5., x - 5.),
        );
        let buttons = [
            (x, ZOOM_BUTTON, Some(Glyph::Minus)),
            (x + ZOOM_BUTTON + 5., reset_width, None),
            (x + width - ZOOM_BUTTON, ZOOM_BUTTON, Some(Glyph::Plus)),
        ];
        for (target, (x, width, glyph)) in row.targets().zip(buttons) {
            let index = target.index();
            let bounds = Rect {
                x,
                y: mid - button_height / 2.,
                width,
                height: button_height,
            };
            let hovered = self.pointer_in(bounds);
            draw_row_highlight(canvas, bounds, scale, hovered, index == selected);
            if let Some(glyph) = glyph {
                let color = if hovered || index == selected {
                    TEXT
                } else {
                    ICON
                };
                draw_glyph(
                    canvas,
                    glyph,
                    x + (width - ICON_SIZE) / 2.,
                    mid - ICON_SIZE / 2.,
                    scale,
                    color,
                );
            } else {
                canvas.label(
                    &percent,
                    (x + width / 2.) * scale - label_width / 2.,
                    (mid + 4.5) * scale,
                    FONT_SIZE * scale,
                    TEXT,
                    false,
                    &mut self.fonts,
                );
            }
            self.hits.push((bounds, Action::MenuItem(index)));
        }
    }

    pub(super) fn activate_main_menu_item(&mut self, index: usize, event_loop: &ActiveEventLoop) {
        match Target::at(index) {
            Some(Target::Browser(action)) => self.action(action, event_loop),
            Some(Target::Shortcut(command)) => self.run_shortcut(command, event_loop),
            None => {} // Padding/dividers or a stale, invalid hit.
        }
    }
}

fn snap_rect(bounds: Rect, scale: f32) -> Rect {
    let snap = |edge: f32| (edge * scale).round();
    let x = snap(bounds.x);
    let y = snap(bounds.y);
    Rect {
        x,
        y,
        width: snap(bounds.x + bounds.width) - x,
        height: snap(bounds.y + bounds.height) - y,
    }
}

fn inset(bounds: Rect, amount: f32) -> Rect {
    Rect {
        x: bounds.x + amount,
        y: bounds.y + amount,
        width: bounds.width - 2. * amount,
        height: bounds.height - 2. * amount,
    }
}

fn draw_panel(canvas: &mut Canvas, bounds: Rect, scale: f32) {
    let bounds = snap_rect(bounds, scale);
    let radii = [10. * scale; 4];
    canvas.shadow_rounded_rect(
        bounds,
        radii,
        rust_browser_engine::paint::RoundedShadow {
            offset_x: 0.,
            offset_y: 16. * scale,
            blur_radius: 48. * scale,
            spread: 0.,
            color: Color(0, 0, 0, 136),
        },
    );
    canvas.fill_rounded_rect(bounds, radii, CARD);
    canvas.outline_rounded_rect(bounds, radii, -scale, scale, LINE);
}

fn draw_hint(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    row: Rect,
    scale: f32,
    hint: &str,
    background: Color,
) -> f32 {
    let size = 10. * scale;
    let width = (fonts.shape(hint, size, false, false, false).advance / scale).ceil() + 10.;
    let mid = row.y + row.height / 2.;
    let bounds = Rect {
        x: row.x + row.width - ROW_INSET - width,
        y: mid - 9.,
        width,
        height: 18.,
    };
    rounded_chrome_rect(canvas, snap_rect(bounds, scale), 4. * scale, LINE);
    rounded_chrome_rect(
        canvas,
        snap_rect(inset(bounds, 1.), scale),
        3. * scale,
        background,
    );
    canvas.label(
        hint,
        (bounds.x + 5.) * scale,
        (mid + 3.5) * scale,
        size,
        MUTED,
        false,
        fonts,
    );
    bounds.x
}

fn draw_row_label(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    scale: f32,
    row: Rect,
    glyph: Glyph,
    label: &str,
    (gap, right): (f32, f32),
) {
    let left = row.x + ROW_INSET;
    let mid = row.y + row.height / 2.;
    draw_glyph(canvas, glyph, left, mid - ICON_SIZE / 2., scale, MUTED);
    let x = left + ICON_SIZE + gap;
    let size = FONT_SIZE * scale;
    let available = (right - x) * scale;
    if available >= fonts.shape("…", size, false, false, false).advance {
        let label = elide(fonts, label, available, size);
        canvas.label(
            &label,
            x * scale,
            (mid + 4.5) * scale,
            size,
            TEXT,
            false,
            fonts,
        );
    }
}

pub(super) fn draw_row_highlight(
    canvas: &mut Canvas,
    bounds: Rect,
    scale: f32,
    hovered: bool,
    selected: bool,
) {
    if !selected && !hovered {
        return;
    }
    // Snap shared edges so adjacent hover and focus highlights do not overlap at fractional DPI.
    let outer = snap_rect(bounds, scale);
    let radius = 6. * scale;
    if selected {
        // Enter still acts on the selected row when another row is hovered.
        let stroke = (2. * scale).round();
        rounded_chrome_rect(canvas, outer, radius, FOCUS);
        rounded_chrome_rect(canvas, inset(outer, stroke), radius - stroke, HOVER);
    } else {
        rounded_chrome_rect(canvas, outer, radius, HOVER);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(canvas: &Canvas, x: usize, y: usize) -> u32 {
        canvas.pixels[y * canvas.width as usize + x]
    }

    fn rgb(color: Color) -> u32 {
        (u32::from(color.0) << 16) | (u32::from(color.1) << 8) | u32::from(color.2)
    }

    #[test]
    fn follows_the_toolbar_anchor_and_keeps_all_rows_inside_the_minimum_window() {
        let anchor = |width| {
            let menu = crate::app::quiet_cinder::Toolbar::new(width, false, false).menu;
            crate::app::chrome_box(menu)
        };
        let normal = Layout::new(1280., 800., anchor(1280.)).unwrap();
        assert_eq!(
            normal.panel,
            Rect {
                x: 917.,
                y: 87.5,
                width: 352.,
                height: if cfg!(all(windows, target_arch = "x86_64")) {
                    544.
                } else {
                    509.
                },
            }
        );
        assert_eq!(normal.density, 1.);
        for width in [390., 640.] {
            let minimum = Layout::new(width, 400., anchor(width)).unwrap();
            assert_eq!(minimum.panel.y, TAB_H);
            assert_eq!(minimum.panel.y + minimum.panel.height, 400. - VIEWPORT_GAP);
            assert!(minimum.panel.x >= VIEWPORT_GAP);
            assert!(minimum.panel.x + minimum.panel.width <= width - VIEWPORT_GAP);
            let mut edge = minimum.panel.y + PADDING;
            for (row, bounds) in minimum.rows() {
                assert_eq!(bounds.y, edge);
                assert_eq!(bounds.x, minimum.panel.x + PADDING);
                assert_eq!(bounds.width, minimum.panel.width - 2. * PADDING);
                assert_eq!(bounds.height, row.height() * minimum.density);
                if matches!(row, Row::Item(..)) {
                    assert!(bounds.height >= MIN_ROW_HEIGHT);
                }
                edge += bounds.height;
            }
            assert!((edge + PADDING - minimum.panel.y - minimum.panel.height).abs() < 0.01);
        }
    }

    #[test]
    fn keyboard_order_matches_display_and_keeps_legacy_dispatch_ids() {
        let order = focus_order().collect::<Vec<_>>();
        assert_eq!(order.len(), MENU_ITEMS.len() + EXTRA_COMMANDS.len());
        assert!(MENU_ITEMS.iter().enumerate().all(|(id, (action, _))| {
            order.contains(&id) && Target::at(id) == Some(Target::Browser(*action))
        }));
        assert!(Target::at(BACKGROUND_ITEM).is_none());
        let first = first_item();
        assert!(Target::at(first) == Some(Target::Browser(Action::NewTab)));
        assert!(
            Target::at(step_item(first, true)) == Some(Target::Browser(Action::ShortcutHelp)),
            "Alt+F, ArrowUp, Enter must still open the shortcut help"
        );
        for (i, &id) in order.iter().enumerate() {
            assert_eq!(
                order.iter().filter(|&&candidate| candidate == id).count(),
                1
            );
            assert_eq!(step_item(id, false), order[(i + 1) % order.len()]);
            assert_eq!(
                step_item(id, true),
                order[(i + order.len() - 1) % order.len()]
            );
        }
        let zoom = order.windows(3).any(|items| {
            items.iter().copied().map(Target::at).eq([
                Some(Target::Shortcut(Command::ZoomOut)),
                Some(Target::Shortcut(Command::ResetZoom)),
                Some(Target::Shortcut(Command::ZoomIn)),
            ])
        });
        assert!(zoom, "zoom controls must be consecutive in visual order");
    }

    #[test]
    fn selection_keeps_a_distinct_ring_beside_hover_at_fractional_dpi() {
        for (scale, pixel_width, stroke) in
            [(1., 640, 2), (1.25, 800, 3), (1.5, 961, 3), (2., 1280, 4)]
        {
            let width = pixel_width as f32 / scale;
            let anchor = Rect {
                x: width - 59.,
                y: 67.,
                width: 46.,
                height: 46.,
            };
            let layout = Layout::new(width, 400., anchor).unwrap();
            let rows = layout
                .rows()
                .filter_map(|(row, bounds)| matches!(row, Row::Item(..)).then_some(bounds))
                .take(3)
                .collect::<Vec<_>>();
            for selected in 0..2 {
                let mut canvas = Canvas::new(pixel_width, (400. * scale) as u32, CARD);
                for (index, &bounds) in rows.iter().enumerate() {
                    draw_row_highlight(
                        &mut canvas,
                        bounds,
                        scale,
                        index == 1 - selected,
                        index == selected,
                    );
                }

                let bounds = rows[selected];
                let at = |logical: f32| (logical * scale).round() as usize;
                let top = at(bounds.y);
                let bottom = at(bounds.y + bounds.height);
                let left = at(bounds.x);
                let right = at(bounds.x + bounds.width);
                let x = (left + right) / 2;
                let y = (top + bottom) / 2;
                let focus = rgb(FOCUS);
                let hover = rgb(HOVER);
                for offset in 0..stroke {
                    assert_eq!(pixel(&canvas, x, top + offset), focus, "scale {scale}");
                    assert_eq!(pixel(&canvas, x, bottom - 1 - offset), focus);
                    assert_eq!(pixel(&canvas, left + offset, y), focus);
                    assert_eq!(pixel(&canvas, right - 1 - offset, y), focus);
                }
                assert_eq!(pixel(&canvas, x, top + stroke), hover);
                assert_eq!(pixel(&canvas, x, bottom - 1 - stroke), hover);
                assert_eq!(pixel(&canvas, left + stroke, y), hover);
                assert_eq!(pixel(&canvas, right - 1 - stroke, y), hover);
                assert_eq!(pixel(&canvas, x, at(rows[1 - selected].y + 17.)), hover);
                assert_eq!(pixel(&canvas, x, at(rows[2].y + 17.)), rgb(CARD));
            }
        }
    }
}
