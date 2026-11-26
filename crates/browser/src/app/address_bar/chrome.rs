//! Quiet Cinder omnibox chrome. Layout uses window-logical coordinates, not
//! document zoom. Popup rows are supplied by the caller in action order; this
//! module does not collect, persist, or query any address data.
#![allow(dead_code, reason = "The app owns toolbar and suggestion routing")]

use super::super::{elide, rounded_chrome_rect};
use rust_browser_engine::{Fonts, Rect, css::Color, paint::Canvas};

pub(in crate::app) const FIELD_HEIGHT: f32 = 34.;
const FIELD_RADIUS: f32 = 7.;
const FOCUS_HALO: f32 = 2.;
const BORDER: f32 = 1.;
const PAGE: Color = Color(25, 25, 28, 255);
const LINE: Color = Color(62, 58, 66, 255);
const ACCENT: Color = Color(196, 163, 244, 255);
const ACCENT_SOFT: Color = Color(68, 61, 80, 255);
const CARD: Color = Color(41, 40, 44, 255);
const HOVER: Color = Color(51, 48, 56, 255);
const TEXT: Color = Color(240, 237, 240, 255);
const MUTED: Color = Color(184, 175, 181, 255);

const WINDOW_MARGIN: f32 = 8.;
const POPUP_WIDTH_MARGIN: f32 = 18.;
const POPUP_HEIGHT_MARGIN: f32 = 20.;
const BELOW_GAP: f32 = 5.;
const ABOVE_GAP: f32 = 6.;
const POPUP_RADIUS: f32 = 10.;
const POPUP_INSET: f32 = BORDER + 6.;
const ROW_RADIUS: f32 = 6.;
const ROW_HEIGHT: f32 = 35.;
const CAPTIONED_HEIGHT: f32 = 44.;
const ROW_PAD_X: f32 = 8.;
const ICON_SIZE: f32 = 17.;
const ICON_GAP: f32 = 10.;
const LABEL_SIZE: f32 = 13.;
const CAPTION_SIZE: f32 = 11.;

/// Paint only the address field surface. Keep the existing editor, text clip,
/// selection, caret, and pointer hit rectangle in the toolbar.
pub(in crate::app) fn paint_address_field(
    canvas: &mut Canvas,
    bounds: Rect,
    scale: f32,
    focused: bool,
) {
    if focused {
        rounded(
            canvas,
            inset(bounds, -FOCUS_HALO),
            FIELD_RADIUS + FOCUS_HALO,
            scale,
            ACCENT_SOFT,
        );
    }
    paint_panel(
        canvas,
        bounds,
        FIELD_RADIUS,
        scale,
        if focused { ACCENT } else { LINE },
        PAGE,
    );
}

/// Text already approved for display by the caller. Use localized labels and
/// sanitized captions; this type does not carry actions or navigation targets.
pub(in crate::app) struct SuggestionRow<'a> {
    pub label: &'a str,
    pub caption: Option<&'a str>,
}

impl SuggestionRow<'_> {
    fn caption(&self) -> Option<&str> {
        self.caption.filter(|caption| !caption.is_empty())
    }

    fn height(&self) -> f32 {
        if self.caption().is_some() {
            CAPTIONED_HEIGHT
        } else {
            ROW_HEIGHT
        }
    }
}

pub(in crate::app) struct SuggestionPopup<'a> {
    pub bounds: Rect,
    rows: &'a [SuggestionRow<'a>],
    starts: Vec<f32>,
    clip: Rect,
    scroll: f32,
    pub max_scroll: f32,
}

impl<'a> SuggestionPopup<'a> {
    /// Anchor to the complete address field, not its input text. Prefer below,
    /// flip if there is more room above, and clip rather than cover the editor
    /// on short windows. Pass an empty slice to suppress the popup.
    pub(in crate::app) fn new(
        field: Rect,
        window: Rect,
        rows: &'a [SuggestionRow<'a>],
        scroll: f32,
    ) -> Option<Self> {
        if rows.is_empty() || !valid(field) || !valid(window) {
            return None;
        }
        intersect(field, window)?;

        let width = field.width.min(window.width - POPUP_WIDTH_MARGIN);
        if width <= 2. * POPUP_INSET {
            return None;
        }
        let x = field.x.clamp(
            window.x + WINDOW_MARGIN,
            right(window) - WINDOW_MARGIN - width,
        );
        let min_y = window.y + WINDOW_MARGIN;
        let max_y = bottom(window) - WINDOW_MARGIN;
        let below = (bottom(field) + BELOW_GAP).max(min_y);
        let above = (field.y - ABOVE_GAP).min(max_y);
        let below_space = max_y - below;
        let above_space = above - min_y;

        let mut content_height = 0.;
        let mut starts = Vec::with_capacity(rows.len() + 1);
        starts.push(0.);
        for row in rows {
            content_height += row.height();
            starts.push(content_height);
        }
        let wanted_height =
            (content_height + 2. * POPUP_INSET).min(window.height - POPUP_HEIGHT_MARGIN);
        let flipped = below_space < wanted_height && above_space > below_space;
        let height = wanted_height.min(if flipped { above_space } else { below_space });
        if height <= 2. * POPUP_INSET {
            return None;
        }
        let bounds = Rect {
            x,
            y: if flipped { above - height } else { below },
            width,
            height,
        };
        let clip = inset(bounds, POPUP_INSET);
        let max_scroll = (content_height - clip.height).max(0.);
        Some(Self {
            bounds,
            rows,
            starts,
            clip,
            scroll: scroll.max(0.).min(max_scroll),
            max_scroll,
        })
    }

    /// Returns the original row index. The border, padding, and portions of
    /// rows outside the scrolling viewport never dispatch an action.
    pub(in crate::app) fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if !self.clip.contains(x, y) {
            return None;
        }
        let index = self.index_at(y - self.clip.y + self.scroll);
        (index < self.rows.len()).then_some(index)
    }

    pub(in crate::app) fn visible_targets(&self) -> impl Iterator<Item = (usize, Rect)> + '_ {
        (self.index_at(self.scroll)..self.rows.len()).map_while(|index| {
            intersect(self.row_bounds(index), self.clip).map(|bounds| (index, bounds))
        })
    }

    /// Bring the selected row into view, aligning its top if it cannot fit.
    pub(in crate::app) fn reveal(&self, index: usize) -> Option<f32> {
        self.rows.get(index)?;
        let top = self.starts[index];
        let end = self.starts[index + 1];
        Some(
            if top < self.scroll || end - top > self.clip.height {
                top
            } else {
                self.scroll.max(end - self.clip.height)
            }
            .min(self.max_scroll),
        )
    }

    /// Paint over the composited window at the full-window canvas clip. The
    /// icon callback receives the original row index and a logical 17px box;
    /// it must respect the row viewport clip. Restores the full-window clip.
    pub(in crate::app) fn paint(
        &self,
        canvas: &mut Canvas,
        fonts: &mut Fonts,
        scale: f32,
        selected: Option<usize>,
        hovered: Option<usize>,
        mut icon: impl FnMut(&mut Canvas, usize, Rect, Color),
    ) {
        if !scale.is_finite() || scale <= 0. {
            return;
        }
        paint_panel(canvas, self.bounds, POPUP_RADIUS, scale, LINE, CARD);
        let clip = snapped(self.clip, scale);
        for index in self.index_at(self.scroll)..self.rows.len() {
            let row = self.row_bounds(index);
            let Some(visible) = intersect(row, self.clip) else {
                break;
            };
            canvas.clip(clip);
            if Some(index) == selected || Some(index) == hovered {
                rounded(canvas, row, ROW_RADIUS, scale, HOVER);
            }
            let glyph = Rect {
                x: row.x + ROW_PAD_X,
                y: row.y + (row.height - ICON_SIZE) / 2.,
                width: ICON_SIZE,
                height: ICON_SIZE,
            };
            icon(canvas, index, glyph, MUTED);

            let caption = self.rows[index].caption();
            let x = right(glyph) + ICON_GAP;
            let width = right(row) - ROW_PAD_X - x;
            if width > 0. {
                canvas.clip(snapped(
                    Rect {
                        x,
                        width,
                        ..visible
                    },
                    scale,
                ));
                let mut label = |text: &str, baseline: f32, size: f32, color| {
                    let text = elide(fonts, text, width * scale, size * scale);
                    canvas.label(
                        &text,
                        x * scale,
                        (row.y + baseline) * scale,
                        size * scale,
                        color,
                        false,
                        fonts,
                    );
                };
                label(
                    self.rows[index].label,
                    if caption.is_some() { 19. } else { 22. },
                    LABEL_SIZE,
                    TEXT,
                );
                if let Some(caption) = caption {
                    label(caption, 34., CAPTION_SIZE, MUTED);
                }
            }
        }
        canvas.clip(Rect {
            width: canvas.width as f32,
            height: canvas.height as f32,
            ..Rect::default()
        });
    }

    fn index_at(&self, offset: f32) -> usize {
        self.starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1)
    }

    fn row_bounds(&self, index: usize) -> Rect {
        Rect {
            y: self.clip.y + self.starts[index] - self.scroll,
            height: self.starts[index + 1] - self.starts[index],
            ..self.clip
        }
    }
}

fn right(rect: Rect) -> f32 {
    rect.x + rect.width
}

fn bottom(rect: Rect) -> f32 {
    rect.y + rect.height
}

fn valid(rect: Rect) -> bool {
    rect.width > 0.
        && rect.height > 0.
        && [rect.x, rect.y, right(rect), bottom(rect)]
            .iter()
            .all(|edge| edge.is_finite())
}

fn inset(rect: Rect, amount: f32) -> Rect {
    Rect {
        x: rect.x + amount,
        y: rect.y + amount,
        width: rect.width - 2. * amount,
        height: rect.height - 2. * amount,
    }
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let width = right(a).min(right(b)) - x;
    let height = bottom(a).min(bottom(b)) - y;
    (width > 0. && height > 0.).then_some(Rect {
        x,
        y,
        width,
        height,
    })
}

fn paint_panel(
    canvas: &mut Canvas,
    bounds: Rect,
    radius: f32,
    scale: f32,
    line: Color,
    fill: Color,
) {
    rounded(canvas, bounds, radius, scale, line);
    rounded(canvas, inset(bounds, BORDER), radius - BORDER, scale, fill);
}

fn rounded(canvas: &mut Canvas, bounds: Rect, radius: f32, scale: f32, color: Color) {
    if valid(bounds) && scale.is_finite() && scale > 0. {
        rounded_chrome_rect(canvas, snapped(bounds, scale), radius * scale, color);
    }
}

// Snap shared edges so adjacent rows and their clips do not bleed at fractional DPI.
fn snapped(bounds: Rect, scale: f32) -> Rect {
    let snap = |value: f32| (value * scale).round();
    let x = snap(bounds.x);
    let y = snap(bounds.y);
    Rect {
        x,
        y,
        width: snap(right(bounds)) - x,
        height: snap(bottom(bounds)) - y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROWS: [SuggestionRow<'static>; 3] = [
        SuggestionRow {
            label: "Search",
            caption: None,
        },
        SuggestionRow {
            label: "Website",
            caption: Some("example.test"),
        },
        SuggestionRow {
            label: "Local",
            caption: Some(""),
        },
    ];

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn popup_tracks_the_whole_address_and_sizes_captioned_rows() {
        let field = rect(121., 47., 482., FIELD_HEIGHT);
        let popup = SuggestionPopup::new(field, rect(10., 12., 800., 600.), &ROWS, 0.).unwrap();
        assert_eq!(popup.bounds, rect(121., 86., 482., 128.));
        assert_eq!(popup.row_bounds(0), rect(128., 93., 468., 35.));
        assert_eq!(popup.row_bounds(1), rect(128., 128., 468., 44.));
        assert_eq!(popup.row_bounds(2), rect(128., 172., 468., 35.));
        assert_eq!(popup.hit(127., 128.), None);
        assert_eq!(popup.hit(130., 127.99), Some(0));
        assert_eq!(popup.hit(130., 128.), Some(1));
        assert_eq!(popup.hit(130., 207.), None);
    }

    #[test]
    fn popup_flips_above_and_clamps_to_window_margins() {
        let window = rect(10., 20., 160., 280.);
        let field = rect(14., 238., 156., FIELD_HEIGHT);
        let popup = SuggestionPopup::new(field, window, &ROWS, 0.).unwrap();
        assert_eq!(popup.bounds, rect(18., 104., 142., 128.));
        assert_eq!(popup.max_scroll, 0.);
    }

    #[test]
    fn clipped_rows_share_paint_and_hit_order_and_can_be_revealed() {
        let window = rect(0., 0., 400., 153.);
        let field = rect(10., 38., 300., FIELD_HEIGHT);
        let popup = SuggestionPopup::new(field, window, &ROWS, 0.).unwrap();
        assert_eq!(popup.bounds, rect(10., 77., 300., 68.));
        assert_eq!(popup.hit(20., 137.), Some(1));
        assert_eq!(popup.hit(20., 138.), None);
        assert_eq!(popup.reveal(1), Some(25.));
        assert_eq!(popup.reveal(2), Some(60.));
        assert_eq!(popup.reveal(3), None);

        let popup = SuggestionPopup::new(field, window, &ROWS, 900.).unwrap();
        assert_eq!(popup.max_scroll, 60.);
        assert_eq!(popup.hit(20., 84.), Some(1));
        assert_eq!(popup.hit(20., 137.), Some(2));
        assert_eq!(popup.reveal(0), Some(0.));
        let targets: Vec<_> = popup.visible_targets().collect();
        assert_eq!(
            targets,
            [
                (1, rect(17., 84., 286., 19.)),
                (2, rect(17., 103., 286., 35.))
            ]
        );
        for (index, row) in targets {
            assert_eq!(popup.hit(row.x, row.y), Some(index));
        }
    }

    #[test]
    fn empty_or_unplaceable_popup_has_no_action_area() {
        let field = rect(10., 10., 60., FIELD_HEIGHT);
        assert!(SuggestionPopup::new(field, rect(0., 0., 160., 70.), &ROWS, 0.).is_none());
        assert!(SuggestionPopup::new(field, rect(0., 0., 30., 200.), &ROWS, 0.).is_none());
        assert!(SuggestionPopup::new(field, rect(0., 0., 160., 200.), &[], 0.).is_none());
    }

    #[test]
    fn focused_field_has_reference_ring_without_resizing_the_hit_area() {
        for scale in [1., 1.5, 2.] {
            let mut canvas = Canvas::new(200, 100, CARD);
            let field = rect(20., 10., 60., FIELD_HEIGHT);
            let pixel = |canvas: &Canvas, x: usize, y: usize| canvas.pixels[y * 200 + x];
            let packed = |Color(r, g, b, _)| u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b);
            let y = (25. * scale) as usize;
            let edge = (20. * scale) as usize;
            paint_address_field(&mut canvas, field, scale, false);
            assert_eq!(pixel(&canvas, edge, y), packed(LINE));
            assert_eq!(pixel(&canvas, edge - 1, y), packed(CARD));
            paint_address_field(&mut canvas, field, scale, true);
            assert_eq!(pixel(&canvas, edge, y), packed(ACCENT));
            assert_eq!(pixel(&canvas, edge - 1, y), packed(ACCENT_SOFT));
            assert_eq!(
                pixel(&canvas, edge + (2. * scale) as usize, y),
                packed(PAGE)
            );
        }
    }
}
