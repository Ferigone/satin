//! Navigation glyphs on a 20-DIP grid. Sample the union of all strokes before
//! compositing, so joins do not darken or thicken when partially covered.

const SIZE: f32 = 20.;
const HALF_STROKE: f32 = 0.75;
const OFFSETS: [f32; 4] = [0.125, 0.375, 0.625, 0.875];
const SAMPLES: u16 = (OFFSETS.len() * OFFSETS.len()) as u16;

const RELOAD_X: f32 = 6.25;
const RELOAD_Y: f32 = 2.55;
const RELOAD_RADIUS: f32 = 6.75;
const RELOAD_TIP: (f32, f32) = (10. + RELOAD_X, 10. - RELOAD_Y);
const RELOAD_TAIL: (f32, f32) = (10. + RELOAD_X, 10. + RELOAD_Y);

#[derive(Clone, Copy, Debug)]
pub(super) enum Glyph {
    Back,
    Forward,
    Reload,
    Search,
}

struct Segment {
    start: (f32, f32),
    delta: (f32, f32),
    inverse_length_sq: f32,
}

impl Segment {
    fn new(start: (f32, f32), end: (f32, f32)) -> Self {
        let delta = (end.0 - start.0, end.1 - start.1);
        Self {
            start,
            delta,
            inverse_length_sq: length_sq(delta.0, delta.1).recip(),
        }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        let (x, y) = (x - self.start.0, y - self.start.1);
        let (dx, dy) = self.delta;
        let t = ((x * dx + y * dy) * self.inverse_length_sq).clamp(0., 1.);
        length_sq(x - t * dx, y - t * dy) <= HALF_STROKE * HALF_STROKE
    }
}

fn length_sq(x: f32, y: f32) -> f32 {
    x * x + y * y
}

fn in_ring(distance_sq: f32, radius: f32) -> bool {
    let inner = radius - HALF_STROKE;
    let outer = radius + HALF_STROKE;
    distance_sq >= inner * inner && distance_sq <= outer * outer
}

enum Shape {
    Arrow {
        shaft: Segment,
        upper: Segment,
        lower: Segment,
        flip: bool,
    },
    Reload {
        up: Segment,
        left: Segment,
    },
    Search {
        handle: Segment,
    },
}

impl Shape {
    fn contains(&self, x: f32, y: f32) -> bool {
        match self {
            Self::Arrow {
                shaft,
                upper,
                lower,
                flip,
            } => {
                let x = if *flip { SIZE - x } else { x };
                shaft.contains(x, y) || upper.contains(x, y) || lower.contains(x, y)
            }
            Self::Reload { up, left } => {
                let (dx, dy) = (x - 10., y - 10.);
                // Omit the wedge to the right; the head and tail cover both round caps.
                let outside_gap = dx * RELOAD_Y <= dy.abs() * RELOAD_X;
                (outside_gap && in_ring(length_sq(dx, dy), RELOAD_RADIUS))
                    || length_sq(x - RELOAD_TAIL.0, y - RELOAD_TAIL.1) <= HALF_STROKE * HALF_STROKE
                    || up.contains(x, y)
                    || left.contains(x, y)
            }
            Self::Search { handle } => {
                in_ring(length_sq(x - 8.75, y - 8.75), 5.65) || handle.contains(x, y)
            }
        }
    }
}

impl Glyph {
    fn shape(self) -> Shape {
        match self {
            Self::Back | Self::Forward => {
                let tip = (3.25, 10.);
                Shape::Arrow {
                    shaft: Segment::new(tip, (16.75, 10.)),
                    upper: Segment::new(tip, (8.75, 4.5)),
                    lower: Segment::new(tip, (8.75, 15.5)),
                    flip: matches!(self, Self::Forward),
                }
            }
            Self::Reload => Shape::Reload {
                up: Segment::new(RELOAD_TIP, (RELOAD_TIP.0, 3.15)),
                left: Segment::new(RELOAD_TIP, (11.95, RELOAD_TIP.1)),
            },
            Self::Search => Shape::Search {
                handle: Segment::new((12.75, 12.75), (17., 17.)),
            },
        }
    }

    /// Visit covered device pixels at the given physical origin and DPI scale.
    pub(super) fn rasterize(
        self,
        x: f32,
        y: f32,
        scale: f32,
        width: u32,
        height: u32,
        mut pixel: impl FnMut(u32, u32, u8),
    ) {
        let shape = self.shape();
        let right = ((x + SIZE * scale).ceil() as u32).min(width);
        let bottom = ((y + SIZE * scale).ceil() as u32).min(height);
        let dips_per_pixel = scale.recip();
        for row in (y.floor() as u32)..bottom {
            for column in (x.floor() as u32)..right {
                let mut covered = 0;
                for sy in OFFSETS {
                    for sx in OFFSETS {
                        covered += u16::from(shape.contains(
                            (column as f32 - x + sx) * dips_per_pixel,
                            (row as f32 - y + sy) * dips_per_pixel,
                        ));
                    }
                }
                if covered != 0 {
                    pixel(column, row, ((covered * 255 + SAMPLES / 2) / SAMPLES) as u8);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALES: [f32; 4] = [1., 1.25, 1.5, 2.];
    const NAVIGATION: [(Glyph, f32); 4] = [
        (Glyph::Back, 31.),
        (Glyph::Forward, 83.),
        (Glyph::Reload, 137.),
        (Glyph::Search, 208.),
    ];

    fn raster(glyph: Glyph, scale: f32, x: f32, y: f32, side: usize) -> Vec<u8> {
        let mut mask = vec![0; side * side];
        glyph.rasterize(x, y, scale, side as u32, side as u32, |col, row, a| {
            let slot = &mut mask[row as usize * side + col as usize];
            assert_eq!(*slot, 0, "each output pixel must be blended only once");
            *slot = a;
        });
        mask
    }

    #[test]
    fn history_arrows_are_mirrors() {
        for scale in SCALES {
            let side = (SIZE * scale) as usize;
            let back = raster(Glyph::Back, scale, 0., 0., side);
            let forward = raster(Glyph::Forward, scale, 0., 0., side);
            for (a, b) in back.chunks_exact(side).zip(forward.chunks_exact(side)) {
                assert!(a.iter().eq(b.iter().rev()), "at {scale}x DPI");
            }
        }
    }

    #[test]
    fn navigation_glyphs_keep_scale_and_a_connected_core() {
        const SIDE: usize = 44;
        for (glyph, chrome_x) in NAVIGATION {
            let area = |mask: &[u8]| mask.iter().map(|a| f32::from(*a) / 255.).sum::<f32>();
            let nominal = area(&raster(glyph, 1., 0., 0., SIDE));
            for scale in SCALES {
                // Keep the subpixel phase of the live toolbar coordinates.
                let x = (chrome_x * scale).fract();
                let y = (80. * scale).fract();
                let mask = raster(glyph, scale, x, y, SIDE);
                let ratio = area(&mask) / (nominal * scale * scale);
                assert!(
                    (0.94..=1.06).contains(&ratio),
                    "{glyph:?} at {scale}x: {ratio}"
                );

                let mut core = mask.into_iter().map(|a| a >= 128).collect::<Vec<_>>();
                let mut pending = vec![core.iter().position(|&p| p).unwrap()];
                while let Some(index) = pending.pop() {
                    if !std::mem::take(&mut core[index]) {
                        continue;
                    }
                    let (col, row) = (index % SIDE, index / SIDE);
                    // Leave a guard pixel around the 20-DIP tile.
                    assert!(col > 0 && row > 0 && col + 1 < SIDE && row + 1 < SIDE);
                    for next in [index - 1, index + 1, index - SIDE, index + SIDE] {
                        if core[next] {
                            pending.push(next);
                        }
                    }
                }
                assert!(!core.contains(&true), "broken {glyph:?} at {scale}x");
            }
        }
    }

    #[test]
    fn reload_gap_and_search_aperture_stay_open() {
        const SIDE: usize = 44;
        for scale in SCALES {
            let at = |x: f32, y: f32| (y * scale) as usize * SIDE + (x * scale) as usize;
            let reload = raster(Glyph::Reload, scale, 0., 0., SIDE);
            assert!(reload[at(3.25, 10.)] >= 128, "missing arc at {scale}x");
            assert_eq!(reload[at(16.75, 10.)], 0, "closed reload gap at {scale}x");

            let search = raster(Glyph::Search, scale, 0., 0., SIDE);
            assert!(search[at(3.1, 8.75)] >= 128, "missing rim at {scale}x");
            assert_eq!(search[at(8.75, 8.75)], 0, "filled lens at {scale}x");
        }
    }
}
