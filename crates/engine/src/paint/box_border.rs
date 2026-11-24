use super::*;

impl Canvas {
    pub(super) fn uniform_border(&mut self, rect: Rect, widths: [f32; 4], color: Color) {
        if !valid_rect(rect) || color.3 == 0 {
            return;
        }
        let [top, right, bottom, left] = widths;
        let (x0, y0) = (rect.x, rect.y);
        let (x3, y3) = (x0 + rect.width, y0 + rect.height);
        let x1 = (x0 + left).min(x3);
        let x2 = (x3 - right).max(x1);
        let y1 = (y0 + top).min(y3);
        let y2 = (y3 - bottom).max(y1);

        // Give each corner to one strip. Center sampling (rather than outward
        // rounding) also keeps a fractional join from blending a pixel twice.
        // Rasterize the strips separately so work is charged only for each
        // strip's visible bounds, not for the enclosed box.
        for [x0, y0, x1, y1] in [
            [x0, y0, x3, y1],
            [x0, y1, x1, y2],
            [x2, y1, x3, y2],
            [x0, y2, x3, y3],
        ] {
            self.raster_rect(
                Rect {
                    x: x0,
                    y: y0,
                    width: x1 - x0,
                    height: y1 - y0,
                },
                |x, y| (x < x1 && y < y1).then_some(color),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color(255, 0, 0, 128);
    const PINK: u32 = 0xff7f7f;
    const WHITE: u32 = 0xffffff;

    #[test]
    fn uniform_border_fractional_join_blends_once() {
        let mut canvas = Canvas::new(8, 8, Color::WHITE);
        canvas.uniform_border(
            Rect {
                width: 8.,
                height: 8.,
                ..Rect::default()
            },
            [2.5; 4],
            RED,
        );
        for (y, row) in canvas.pixels.chunks_exact(8).enumerate() {
            for (x, &pixel) in row.iter().enumerate() {
                let inner = (2..5).contains(&x) && (2..5).contains(&y);
                assert_eq!(pixel, if inner { WHITE } else { PINK }, "({x},{y})");
            }
        }
    }

    #[test]
    fn uniform_border_charges_only_clipped_strips() {
        let huge = Rect {
            width: 1_000_000.,
            height: 1_000_000.,
            ..Rect::default()
        };
        let mut canvas = Canvas::new(16, 16, Color::WHITE);
        canvas.clip(Rect {
            x: 1.,
            y: 1.,
            width: 8.,
            height: 8.,
        });
        // Top: 8×2; left below it: 2×6; right and bottom: offscreen.
        canvas.work_left = 28;
        canvas.uniform_border(huge, [3.; 4], RED);
        assert_eq!(canvas.work_left, 0);
        assert!(!canvas.limited);
        assert_eq!(canvas.pixels.iter().filter(|&&p| p == PINK).count(), 28);
        assert_eq!(canvas.pixels.iter().filter(|&&p| p == WHITE).count(), 228);

        canvas.clear(Color::WHITE);
        canvas.work_left = 15; // Too little even for the first visible strip.
        canvas.uniform_border(huge, [3.; 4], RED);
        assert_eq!(canvas.work_left, 0);
        assert!(canvas.limited);
        assert!(canvas.pixels.iter().all(|&p| p == WHITE));

        canvas.clear(Color::WHITE);
        canvas.work_left = 1;
        canvas.uniform_border(
            Rect {
                x: 32.,
                y: 32.,
                ..huge
            },
            [3.; 4],
            RED,
        );
        assert_eq!(canvas.work_left, 1);
        assert!(!canvas.limited);
        assert!(canvas.pixels.iter().all(|&p| p == WHITE));
    }
}
