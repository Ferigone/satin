use super::*;

/// Pixel coverage of the active elliptical corner. Straight edges use the
/// renderer's usual pixel-center convention.
pub(super) fn coverage(r: Rect, radii: &[[f32; 2]; 4], x: f32, y: f32) -> f32 {
    if r.width <= 0. || r.height <= 0. || !r.contains(x, y) {
        return 0.;
    }
    let left = x - r.x;
    let top = y - r.y;
    let right = r.width - left;
    let bottom = r.height - top;
    for ([dx, dy], [rx, ry]) in [[left, top], [right, top], [right, bottom], [left, bottom]]
        .into_iter()
        .zip(*radii)
    {
        if dx >= rx || dy >= ry || rx <= 0. || ry <= 0. {
            continue;
        }
        let ax = (rx - dx) / rx;
        let ay = (ry - dy) / ry;
        let f = ax * ax + ay * ay - 1.;
        // Signed distance to the tangent approximates subpixel edge coverage.
        let gradient = 2. * (ax / rx).hypot(ay / ry);
        return if gradient > 0. {
            (0.5 - f / gradient).clamp(0., 1.)
        } else {
            1.
        };
    }
    1.
}

impl Canvas {
    pub(super) fn rounded_border(
        &mut self,
        rect: Rect,
        radii: &[[f32; 2]; 4],
        widths: [f32; 4],
        colors: &[Color; 4],
    ) {
        if !valid_rect(rect) || colors.iter().all(|c| c.3 == 0) {
            return;
        }
        let [t, r, b, l] = widths;
        let inner = Rect {
            x: rect.x + l,
            y: rect.y + t,
            width: rect.width - l - r,
            height: rect.height - t - b,
        };
        let mut inner_radii = *radii;
        for (corner, sides) in inner_radii.iter_mut().zip([[l, t], [r, t], [r, b], [l, b]]) {
            for i in 0..2 {
                corner[i] = (corner[i] - sides[i]).max(0.);
            }
        }
        // Outside these four strips the inner and outer box cover every
        // pixel equally. Avoid charging a large, empty outline interior.
        let top = t.max(radii[0][1]).max(radii[1][1]).min(rect.height);
        let bottom = b.max(radii[2][1]).max(radii[3][1]).min(rect.height - top);
        let left = l.min(rect.width);
        let right = r.min(rect.width - left);
        let middle = rect.height - top - bottom;
        for strip in [
            Rect {
                height: top,
                ..rect
            },
            Rect {
                y: rect.y + top,
                height: middle,
                width: left,
                ..rect
            },
            Rect {
                x: rect.x + rect.width - right,
                y: rect.y + top,
                height: middle,
                width: right,
            },
            Rect {
                y: rect.y + rect.height - bottom,
                height: bottom,
                ..rect
            },
        ] {
            self.raster_rect(strip, |x, y| {
                // Half-open edges avoid double blending at fractional strip joins.
                if x >= strip.x + strip.width || y >= strip.y + strip.height {
                    return None;
                }
                let opacity = coverage(rect, radii, x, y) - coverage(inner, &inner_radii, x, y);
                if opacity <= 0. {
                    return None;
                }
                let edges = [
                    y - rect.y,
                    rect.x + rect.width - x,
                    rect.y + rect.height - y,
                    x - rect.x,
                ];
                let mut side = 0;
                let mut best = f32::INFINITY;
                for (i, w) in widths.iter().enumerate() {
                    if *w > 0. && edges[i] / w < best {
                        best = edges[i] / w;
                        side = i;
                    }
                }
                let Color(r, g, b, a) = colors[side];
                Some(Color(r, g, b, (f32::from(a) * opacity).round() as u8))
            });
        }
    }
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
