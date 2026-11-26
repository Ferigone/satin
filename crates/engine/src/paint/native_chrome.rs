//! Device-space primitives for native chrome, not the document paint pipeline.

use super::*;
use std::sync::OnceLock;

/// An outer rounded shadow. All lengths are in device pixels.
#[derive(Clone, Copy, Debug)]
pub struct RoundedShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    /// Nonnegative CSS-like blur radius; values above 256 px are capped.
    pub blur_radius: f32,
    /// Expansion before blurring; negative values shrink the shadow source.
    pub spread: f32,
    pub color: Color,
}

impl Canvas {
    /// Fill a device-space rectangle with three CSS-style linear-gradient stops.
    ///
    /// Angles go clockwise from up (0°). Stop positions are fractions of the
    /// gradient line (0.0 = start, 1.0 = end), may extend outside that range,
    /// and are raised to the preceding position when decreasing, as in CSS.
    /// Colors interpolate in premultiplied sRGB. The current clip is honored.
    pub fn fill_linear_gradient(&mut self, rect: Rect, angle_deg: f32, stops: [(f32, Color); 3]) {
        self.fill_rounded_linear_gradient(rect, [0.; 4], angle_deg, stops);
    }

    /// Like [`Self::fill_linear_gradient`], with antialiased circular corners.
    ///
    /// Radii are in device pixels, clockwise from top-left. Each is clamped
    /// between zero and half the shorter side. Rounding does not move the stops.
    pub fn fill_rounded_linear_gradient(
        &mut self,
        rect: Rect,
        radii: [f32; 4],
        angle_deg: f32,
        stops: [(f32, Color); 3],
    ) {
        let (Some(shape), Some(gradient)) = (
            RoundedBox::new(rect, radii),
            LinearGradient::new(rect, angle_deg, stops),
        ) else {
            return;
        };
        self.paint_chrome(shape.edges.grow(0.5), None, |x, y, pixel| {
            let coverage = shape.coverage(x, y);
            if coverage != 0. {
                *pixel = gradient.at(x, y).over(*pixel, coverage);
            }
        });
    }

    /// Fill an antialiased rounded rectangle in device space. Radii are
    /// clockwise from top-left and clamped as in [`Self::fill_rounded_linear_gradient`].
    pub fn fill_rounded_rect(&mut self, rect: Rect, radii: [f32; 4], color: Color) {
        let Some(shape) = RoundedBox::new(rect, radii) else {
            return;
        };
        self.paint_chrome_color(shape.edges.grow(0.5), None, color, |x, y| {
            shape.coverage(x, y)
        });
    }

    /// Paint a single antialiased ring without filling its center. `offset`
    /// is the gap from `rect` to the ring's inner edge. Use `0.` for a flush
    /// outer ring or `-width` for an inside border. All lengths are in device
    /// pixels; radii are ordered as in [`Self::fill_rounded_linear_gradient`].
    pub fn outline_rounded_rect(
        &mut self,
        rect: Rect,
        radii: [f32; 4],
        offset: f32,
        width: f32,
        color: Color,
    ) {
        let Some(shape) = RoundedBox::new(rect, radii) else {
            return;
        };
        if !offset.is_finite() || !width.is_finite() || width <= 0. {
            return;
        }
        let offset = f64::from(offset);
        let Some(outer) = shape.grow(offset + f64::from(width)) else {
            return;
        };
        let inner = shape.grow(offset);
        self.paint_chrome_color(
            outer.edges.grow(0.5),
            inner.map(RoundedBox::opaque_interior),
            color,
            |x, y| (outer.coverage(x, y) - inner.map_or(0., |b| b.coverage(x, y))).max(0.),
        );
    }

    /// Paint a single outer shadow, masked out by the original rounded box.
    ///
    /// The compact approximation has a Gaussian-like falloff at straight
    /// edges (sigma = blur_radius / 2), tapered to zero at 3 sigma. Rounded
    /// corners are estimated from distance, not a CSS blur convolution.
    /// Zero blur draws a hard antialiased shadow. Paint before the box fill.
    pub fn shadow_rounded_rect(&mut self, rect: Rect, radii: [f32; 4], shadow: RoundedShadow) {
        let Some(mask) = RoundedBox::new(rect, radii) else {
            return;
        };
        if ![
            shadow.offset_x,
            shadow.offset_y,
            shadow.blur_radius,
            shadow.spread,
        ]
        .into_iter()
        .all(f32::is_finite)
            || shadow.blur_radius < 0.
            || shadow.color.3 == 0
        {
            return;
        }
        if shadow.blur_radius == 0.
            && shadow.offset_x == 0.
            && shadow.offset_y == 0.
            && shadow.spread <= 0.
        {
            return;
        }
        let Some(mut source) = mask.grow(f64::from(shadow.spread)) else {
            return;
        };
        source.edges = source
            .edges
            .translate(f64::from(shadow.offset_x), f64::from(shadow.offset_y));
        let sigma = f64::from(shadow.blur_radius.min(256.)) * 0.5;
        let hole = Some(mask.opaque_interior());
        if sigma == 0. {
            self.paint_chrome_color(source.edges.grow(0.5), hole, shadow.color, |x, y| {
                // Subtract overlapping edge coverage. In particular an
                // unshifted, unspread hard shadow must not leave a halo.
                (source.coverage(x, y) - mask.coverage(x, y)).max(0.)
            });
            return;
        }
        let cdf = GaussianCdf::new(sigma);
        self.paint_chrome_color(source.edges.grow(3. * sigma), hole, shadow.color, |x, y| {
            let uncovered = 1. - mask.coverage(x, y);
            if uncovered == 0. {
                return 0.;
            }
            let b = source.edges;
            let square = cdf.span(x - b.left, x - b.right) * cdf.span(y - b.top, y - b.bottom);
            let rounded = source
                .corner_distance(x, y)
                .map_or(square, |distance| square.min(cdf.at(-distance)));
            uncovered * rounded
        });
    }

    fn paint_chrome_color(
        &mut self,
        edges: Edges,
        hole: Option<Edges>,
        color: Color,
        coverage: impl Fn(f64, f64) -> f64,
    ) {
        if color.3 == 0 {
            return;
        }
        let ink = Premul::from_color(color);
        self.paint_chrome(edges, hole, |x, y, pixel| {
            let coverage = coverage(x, y);
            if coverage > 0. {
                *pixel = ink.over(*pixel, coverage);
            }
        });
    }

    /// No mask/image allocation. Each candidate pixel is visited at most
    /// once, and clipped work is charged before touching the surface. A
    /// fully masked interior is cut out as up to four disjoint bands.
    fn paint_chrome(
        &mut self,
        edges: Edges,
        hole: Option<Edges>,
        paint: impl Fn(f64, f64, &mut u32),
    ) {
        let Some(clip) = Edges::from_rect(self.clip) else {
            return;
        };
        let stride = self.width as usize;
        let clip = PixelBox::new(clip, stride, self.height as usize);
        let outer = PixelBox::new(edges, stride, self.height as usize).intersect(clip);
        let hole = hole
            .filter(|b| b.valid())
            .map(|b| PixelBox::new(b, stride, self.height as usize));
        let bands = outer.without(hole);
        let work = bands
            .iter()
            .fold(0usize, |sum, b| sum.saturating_add(b.area()));
        if work == 0 || !self.charge_pixels(work) {
            return;
        }
        for b in bands.into_iter().filter(|b| b.area() > 0) {
            for y in b.top..b.bottom {
                let row = &mut self.pixels[y * stride + b.left..y * stride + b.right];
                for (i, pixel) in row.iter_mut().enumerate() {
                    paint((b.left + i) as f64 + 0.5, y as f64 + 0.5, pixel);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct LinearGradient {
    center: (f64, f64),
    delta: (f64, f64),
    stops: [(f64, Premul); 3],
}

impl LinearGradient {
    fn new(rect: Rect, angle_deg: f32, stops: [(f32, Color); 3]) -> Option<Self> {
        let edges = Edges::from_rect(rect)?;
        if !angle_deg.is_finite()
            || stops.iter().any(|&(position, _)| !position.is_finite())
            || stops.iter().all(|&(_, c)| c.3 == 0)
        {
            return None;
        }
        let (dx, dy) = match f64::from(angle_deg).rem_euclid(360.) {
            0. | 360. => (0., -1.),
            90. => (1., 0.),
            180. => (0., 1.),
            270. => (-1., 0.),
            angle => {
                let (sin, cos) = angle.to_radians().sin_cos();
                (sin, -cos)
            }
        };
        // The start/end lines are perpendicular to the axis and pass
        // through the extreme projected corners, not the side midpoints.
        let length = dx.abs() * f64::from(rect.width) + dy.abs() * f64::from(rect.height);
        let center = (
            edges.left + 0.5 * f64::from(rect.width),
            edges.top + 0.5 * f64::from(rect.height),
        );
        let mut previous = f64::NEG_INFINITY;
        let stops = stops.map(|(position, color)| {
            let p = f64::from(position).max(previous);
            previous = p;
            (p, Premul::from_color(color))
        });
        Some(Self {
            center,
            delta: (dx / length, dy / length),
            stops,
        })
    }

    fn at(&self, x: f64, y: f64) -> Premul {
        let t = 0.5 + (x - self.center.0) * self.delta.0 + (y - self.center.1) * self.delta.1;
        let [(a, c0), (b, c1), (c, c2)] = self.stops;
        if t < a {
            c0
        } else if t < b {
            c0.mix(c1, (t - a) / (b - a))
        } else if t < c {
            c1.mix(c2, (t - b) / (c - b))
        } else {
            c2
        }
    }
}

#[derive(Clone, Copy)]
struct Premul([f64; 4]);

impl Premul {
    fn from_color(Color(r, g, b, a): Color) -> Self {
        let a = f64::from(a) / 255.;
        Self([f64::from(r) * a, f64::from(g) * a, f64::from(b) * a, a])
    }

    fn mix(self, next: Self, t: f64) -> Self {
        Self(std::array::from_fn(|i| {
            self.0[i] + (next.0[i] - self.0[i]) * t
        }))
    }

    fn over(self, pixel: u32, coverage: f64) -> u32 {
        let [r, g, b, alpha] = self.0;
        let inv = 1. - alpha * coverage;
        let channel = |src: f64, shift: u32| -> u32 {
            (src * coverage + f64::from((pixel >> shift) & 255) * inv).round() as u32
        };
        channel(r, 16) << 16 | channel(g, 8) << 8 | channel(b, 0)
    }
}

#[derive(Clone, Copy)]
struct Edges {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl Edges {
    fn from_rect(r: Rect) -> Option<Self> {
        let left = f64::from(r.x);
        let top = f64::from(r.y);
        let b = Self {
            left,
            top,
            right: left + f64::from(r.width),
            bottom: top + f64::from(r.height),
        };
        b.valid().then_some(b)
    }

    fn valid(self) -> bool {
        [self.left, self.top, self.right, self.bottom]
            .into_iter()
            .all(f64::is_finite)
            && self.left < self.right
            && self.top < self.bottom
    }

    fn grow(self, amount: f64) -> Self {
        Self {
            left: self.left - amount,
            top: self.top - amount,
            right: self.right + amount,
            bottom: self.bottom + amount,
        }
    }

    fn translate(self, x: f64, y: f64) -> Self {
        Self {
            left: self.left + x,
            top: self.top + y,
            right: self.right + x,
            bottom: self.bottom + y,
        }
    }
}

#[derive(Clone, Copy)]
struct RoundedBox {
    edges: Edges,
    radii: [f64; 4],
}

impl RoundedBox {
    fn new(rect: Rect, radii: [f32; 4]) -> Option<Self> {
        if !radii.into_iter().all(f32::is_finite) {
            return None;
        }
        Some(Self::with_edges(
            Edges::from_rect(rect)?,
            radii.map(f64::from),
        ))
    }

    fn with_edges(edges: Edges, radii: [f64; 4]) -> Self {
        let max_radius = 0.5 * (edges.right - edges.left).min(edges.bottom - edges.top);
        Self {
            edges,
            radii: radii.map(|r| r.clamp(0., max_radius)),
        }
    }

    fn grow(self, amount: f64) -> Option<Self> {
        let edges = self.edges.grow(amount);
        edges.valid().then(|| {
            // Preserve deliberately square corners (e.g. a joined tab's base).
            Self::with_edges(
                edges,
                self.radii.map(|r| if r == 0. { 0. } else { r + amount }),
            )
        })
    }

    fn corner_distance(self, x: f64, y: f64) -> Option<f64> {
        let Edges {
            left,
            top,
            right,
            bottom,
        } = self.edges;
        let l = x - left;
        let r = right - x;
        let t = y - top;
        let b = bottom - y;
        let (dx, dy, radius) = match (l <= r, t <= b) {
            (true, true) => (l, t, self.radii[0]),
            (false, true) => (r, t, self.radii[1]),
            (false, false) => (r, b, self.radii[2]),
            (true, false) => (l, b, self.radii[3]),
        };
        (radius > 0. && dx < radius && dy < radius).then(|| {
            // All lengths originate as f32, so squaring in f64 cannot overflow.
            let x = radius - dx;
            let y = radius - dy;
            (x * x + y * y).sqrt() - radius
        })
    }

    fn coverage(self, x: f64, y: f64) -> f64 {
        let b = self.edges;
        let x_cover = ((x + 0.5).min(b.right) - (x - 0.5).max(b.left)).clamp(0., 1.);
        let y_cover = ((y + 0.5).min(b.bottom) - (y - 0.5).max(b.top)).clamp(0., 1.);
        let square = x_cover * y_cover;
        if square == 0. {
            return 0.;
        }
        self.corner_distance(x, y)
            .map_or(square, |d| square.min((0.5 - d).clamp(0., 1.)))
    }

    fn opaque_interior(self) -> Edges {
        let [tl, tr, br, bl] = self.radii;
        Edges {
            left: self.edges.left + tl.max(bl).max(0.5),
            top: self.edges.top + 0.5,
            right: self.edges.right - tr.max(br).max(0.5),
            bottom: self.edges.bottom - 0.5,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct PixelBox {
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
}

impl PixelBox {
    fn new(edges: Edges, width: usize, height: usize) -> Self {
        // Half-open intervals of pixel centers, including for fractional clips.
        let bound = |edge: f64, limit| (((edge - 0.5).ceil()) as usize).min(limit);
        Self {
            left: bound(edges.left, width),
            top: bound(edges.top, height),
            right: bound(edges.right, width),
            bottom: bound(edges.bottom, height),
        }
    }

    fn intersect(self, b: Self) -> Self {
        Self {
            left: self.left.max(b.left),
            top: self.top.max(b.top),
            right: self.right.min(b.right),
            bottom: self.bottom.min(b.bottom),
        }
    }

    fn area(self) -> usize {
        self.right
            .saturating_sub(self.left)
            .saturating_mul(self.bottom.saturating_sub(self.top))
    }

    fn without(self, hole: Option<Self>) -> [Self; 4] {
        let empty = Self::default();
        let Some(hole) = hole.map(|b| self.intersect(b)).filter(|b| b.area() > 0) else {
            return [self, empty, empty, empty];
        };
        [
            Self {
                bottom: hole.top,
                ..self
            },
            Self {
                top: hole.bottom,
                ..self
            },
            Self {
                top: hole.top,
                bottom: hole.bottom,
                right: hole.left,
                ..self
            },
            Self {
                top: hole.top,
                bottom: hole.bottom,
                left: hole.right,
                ..self
            },
        ]
    }
}

struct GaussianCdf {
    table: &'static [f64; 129],
    steps_per_pixel: f64,
}

impl GaussianCdf {
    fn new(sigma: f64) -> Self {
        static TABLE: OnceLock<[f64; 129]> = OnceLock::new();
        let table = TABLE.get_or_init(|| {
            // Standard normal CDF (Abramowitz and Stegun 26.2.17). Normalize
            // the truncated tails so clipping at +/- 3 sigma is continuous.
            let normal = |z: f64| {
                let t = 1. / (1. + 0.231_641_9 * z.abs());
                let poly = ((((1.330_274_429 * t - 1.821_255_978) * t + 1.781_477_937) * t
                    - 0.356_563_782)
                    * t
                    + 0.319_381_530)
                    * t;
                let tail = 0.398_942_280_4 * (-0.5 * z * z).exp() * poly;
                if z < 0. { tail } else { 1. - tail }
            };
            let tail = normal(-3.);
            std::array::from_fn(|i| (normal(i as f64 * (6. / 128.) - 3.) - tail) / (1. - 2. * tail))
        });
        Self {
            table,
            steps_per_pixel: 128. / (6. * sigma),
        }
    }

    fn at(&self, distance: f64) -> f64 {
        let index = distance * self.steps_per_pixel + 64.;
        if index <= 0. {
            0.
        } else if index >= 128. {
            1.
        } else {
            let i = index as usize;
            self.table[i] + (self.table[i + 1] - self.table[i]) * (index - i as f64)
        }
    }

    fn span(&self, near: f64, far: f64) -> f64 {
        self.at(near) - self.at(far)
    }
}

#[cfg(test)]
mod tests;
