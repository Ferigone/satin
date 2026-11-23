use super::{CHROME_HOVER, CHROME_MUTED, Canvas, Rect, rounded_chrome_rect};

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
    let snap = |edge: f32| (edge * scale).round();
    let left = snap(bounds.x);
    let top = snap(bounds.y);
    let width = snap(bounds.x + bounds.width) - left;
    let height = snap(bounds.y + bounds.height) - top;
    let outer = Rect {
        x: left,
        y: top,
        width,
        height,
    };
    let radius = 7. * scale;
    if selected {
        // Enter still acts on the selected row when another row is hovered.
        let stroke = (2. * scale).round();
        rounded_chrome_rect(canvas, outer, radius, CHROME_MUTED);
        rounded_chrome_rect(
            canvas,
            Rect {
                x: left + stroke,
                y: top + stroke,
                width: width - 2. * stroke,
                height: height - 2. * stroke,
            },
            radius - stroke,
            CHROME_HOVER,
        );
    } else {
        rounded_chrome_rect(canvas, outer, radius, CHROME_HOVER);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{CHROME_PANEL, Color};

    fn pixel(canvas: &Canvas, x: usize, y: usize) -> u32 {
        canvas.pixels[y * canvas.width as usize + x]
    }

    fn rgb(color: Color) -> u32 {
        (u32::from(color.0) << 16) | (u32::from(color.1) << 8) | u32::from(color.2)
    }

    #[test]
    fn selection_keeps_a_distinct_ring_beside_hover_at_fractional_dpi() {
        for (scale, pixel_width, stroke) in
            [(1., 640, 2), (1.25, 800, 3), (1.5, 961, 3), (2., 1280, 4)]
        {
            for selected in 0..2 {
                let mut canvas = Canvas::new(pixel_width, (400. * scale) as u32, CHROME_PANEL);
                let row = |index| Rect {
                    x: pixel_width as f32 / scale - 206.,
                    y: 132. + index as f32 * 35.,
                    width: 188.,
                    height: 35.,
                };
                for index in 0..3 {
                    draw_row_highlight(
                        &mut canvas,
                        row(index),
                        scale,
                        index == 1 - selected,
                        index == selected,
                    );
                }

                let bounds = row(selected);
                let at = |logical: f32| (logical * scale).round() as usize;
                let top = at(bounds.y);
                let bottom = at(bounds.y + bounds.height);
                let left = at(bounds.x);
                let right = at(bounds.x + bounds.width);
                let x = (left + right) / 2;
                let y = (top + bottom) / 2;
                let focus = rgb(CHROME_MUTED);
                let hover = rgb(CHROME_HOVER);
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
                assert_eq!(pixel(&canvas, x, at(row(1 - selected).y + 17.)), hover);
                assert_eq!(pixel(&canvas, x, at(row(2).y + 17.)), rgb(CHROME_PANEL));
            }
        }
    }
}
