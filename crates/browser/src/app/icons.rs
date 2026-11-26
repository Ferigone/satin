//! Native rasterization of the Quiet · Cinder icon artwork. All geometry stays
//! on the source 24-unit viewBox until sampling at the requested device size.

// Explicit paths also support the standalone rustc harness in CHROME_GLYPHS.md.
#[path = "icons/paths.rs"]
mod paths;

pub(super) use paths::Glyph;

const VIEWBOX: usize = 24;
const HALF_STROKE: f32 = 1.65 / 2.;
const OFFSETS: [f32; 4] = [0.125, 0.375, 0.625, 0.875];
const SAMPLES: u32 = (OFFSETS.len() * OFFSETS.len()) as u32;

#[derive(Clone, Copy)]
struct Point(f32, f32);

impl Point {
    fn dot(self, other: Self) -> f32 {
        self.0 * other.0 + self.1 * other.1
    }

    fn cross(self, other: Self) -> f32 {
        self.0 * other.1 - self.1 * other.0
    }

    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0, self.1 - other.1)
    }

    fn midpoint(self, other: Self) -> Self {
        Self((self.0 + other.0) * 0.5, (self.1 + other.1) * 0.5)
    }
}

#[derive(Clone, Copy)]
struct Bounds {
    min: Point,
    max: Point,
}

impl Bounds {
    fn union(self, other: Self) -> Self {
        Self {
            min: Point(self.min.0.min(other.min.0), self.min.1.min(other.min.1)),
            max: Point(self.max.0.max(other.max.0), self.max.1.max(other.max.1)),
        }
    }

    fn outset(self, distance: f32) -> Self {
        Self {
            min: Point(self.min.0 - distance, self.min.1 - distance),
            max: Point(self.max.0 + distance, self.max.1 + distance),
        }
    }

    fn at(point: Point) -> Self {
        Self {
            min: point,
            max: point,
        }
    }
}

#[derive(Clone, Copy)]
struct Segment {
    from: Point,
    to: Point,
    delta: Point,
    inverse_length_sq: f32,
}

impl Segment {
    fn new(from: Point, to: Point) -> Self {
        let delta = to.sub(from);
        let length_sq = delta.dot(delta);
        Self {
            from,
            to,
            delta,
            inverse_length_sq: if length_sq > 0. {
                length_sq.recip()
            } else {
                0.
            },
        }
    }

    fn distance_sq(self, point: Point) -> f32 {
        let p = point.sub(self.from);
        let t = (p.dot(self.delta) * self.inverse_length_sq).clamp(0., 1.);
        let d = Point(p.0 - t * self.delta.0, p.1 - t * self.delta.1);
        d.dot(d)
    }
}

#[derive(Clone, Copy)]
enum Stroke {
    Segment(Segment),
    Circle {
        center: Point,
        radius: f32,
    },
    Arc {
        center: Point,
        radius: f32,
        // Radial vectors in SVG's positive sweep direction.
        start: Point,
        end: Point,
        large: bool,
    },
}

fn in_ring(distance_sq: f32, radius: f32) -> bool {
    let inner = (radius - HALF_STROKE).max(0.);
    let outer = radius + HALF_STROKE;
    (inner * inner..=outer * outer).contains(&distance_sq)
}

impl Stroke {
    fn bounds(self) -> Bounds {
        match self {
            Self::Segment(line) => Bounds::at(line.from).union(Bounds::at(line.to)),
            Self::Circle { center, radius } | Self::Arc { center, radius, .. } => {
                Bounds::at(center).outset(radius)
            }
        }
        .outset(HALF_STROKE)
    }

    fn contains(self, point: Point) -> bool {
        match self {
            Self::Segment(line) => line.distance_sq(point) <= HALF_STROKE * HALF_STROKE,
            Self::Circle { center, radius } => {
                let p = point.sub(center);
                in_ring(p.dot(p), radius)
            }
            Self::Arc {
                center,
                radius,
                start,
                end,
                large,
            } => {
                let p = point.sub(center);
                let a = start.cross(p) >= 0.;
                let b = p.cross(end) >= 0.;
                (if large { a || b } else { a && b }) && in_ring(p.dot(p), radius)
            }
        }
    }
}

// A typed, absolute-coordinate transcription of the SVG subset in the reference.
enum Command {
    M(f32, f32),
    L(f32, f32),
    A(f32, bool, bool, f32, f32),
    C(f32, f32, f32, f32, f32, f32),
    Z,
}

#[derive(Clone, Copy)]
enum Paint {
    Foreground(u32), // SVG opacity as an integer percent (100 or 49).
    AccentFill,      // Only painted when the toolbar bookmark star is active.
}

enum Element {
    Path(&'static [Command]),
    Circle(f32, f32, f32),
    Ellipse(f32, f32, f32, f32),
    Rect(f32, f32, f32, f32, f32),
    Polygon(&'static [Point], Paint),
}

struct Shape {
    // Index by viewBox row so a sample only tests strokes that can reach it.
    rows: [Vec<(Bounds, Stroke)>; VIEWBOX],
    fills: Vec<(&'static [Point], Paint)>,
    bounds: Bounds,
}

impl Shape {
    fn new(elements: &[Element]) -> Self {
        let mut shape = Self {
            rows: std::array::from_fn(|_| Vec::new()),
            fills: Vec::new(),
            bounds: Bounds {
                min: Point(f32::INFINITY, f32::INFINITY),
                max: Point(f32::NEG_INFINITY, f32::NEG_INFINITY),
            },
        };
        for element in elements {
            match *element {
                Element::Path(commands) => shape.path(commands),
                Element::Circle(x, y, radius) => shape.push(Stroke::Circle {
                    center: Point(x, y),
                    radius,
                }),
                Element::Ellipse(x, y, rx, ry) => {
                    // The source ellipse has radii 4 and 9. At 128 segments its
                    // chord error is below 0.003 viewBox units.
                    let mut from = Point(x + rx, y);
                    for step in 1..=128 {
                        let (sin, cos) = (std::f32::consts::TAU * step as f32 / 128.).sin_cos();
                        let to = Point(x + rx * cos, y + ry * sin);
                        shape.line(from, to);
                        from = to;
                    }
                }
                Element::Rect(x, y, width, height, r) => {
                    let (right, bottom) = (x + width, y + height);
                    let mut from = Point(x + r, y);
                    for (line, arc) in [
                        (Point(right - r, y), Point(right, y + r)),
                        (Point(right, bottom - r), Point(right - r, bottom)),
                        (Point(x + r, bottom), Point(x, bottom - r)),
                        (Point(x, y + r), Point(x + r, y)),
                    ] {
                        shape.line(from, line);
                        shape.arc(line, arc, r, false, true);
                        from = arc;
                    }
                }
                Element::Polygon(points, paint) => {
                    shape.fills.push((points, paint));
                    for (&from, &to) in points.iter().zip(points.iter().cycle().skip(1)) {
                        shape.bounds = shape.bounds.union(Bounds::at(from));
                        if matches!(paint, Paint::AccentFill) {
                            shape.line(from, to);
                        }
                    }
                }
            }
        }
        shape
    }

    fn push(&mut self, stroke: Stroke) {
        let bounds = stroke.bounds();
        self.bounds = self.bounds.union(bounds);
        let first = (bounds.min.1.floor() as usize).min(VIEWBOX);
        let last = (bounds.max.1.floor() as usize + 1).min(VIEWBOX);
        for row in &mut self.rows[first..last] {
            row.push((bounds, stroke));
        }
    }

    fn line(&mut self, from: Point, to: Point) {
        self.push(Stroke::Segment(Segment::new(from, to)));
    }

    fn path(&mut self, commands: &[Command]) {
        let mut pen = Point(0., 0.);
        let mut start = pen;
        for command in commands {
            match *command {
                Command::M(x, y) => {
                    start = Point(x, y);
                    pen = start;
                }
                Command::L(x, y) => {
                    self.line(pen, Point(x, y));
                    pen = Point(x, y);
                }
                Command::A(radius, large, sweep, x, y) => {
                    self.arc(pen, Point(x, y), radius, large, sweep);
                    pen = Point(x, y);
                }
                Command::C(x1, y1, x2, y2, x, y) => {
                    self.cubic(pen, Point(x1, y1), Point(x2, y2), Point(x, y));
                    pen = Point(x, y);
                }
                Command::Z => {
                    self.line(pen, start);
                    pen = start;
                }
            }
        }
    }

    fn arc(&mut self, from: Point, to: Point, radius: f32, large: bool, sweep: bool) {
        // SVG endpoint-to-center conversion for an unrotated circular arc.
        // Enlarge a radius that cannot span the chord, as SVG does.
        let half = from.midpoint(to).sub(from);
        let length_sq = half.dot(half);
        if length_sq == 0. {
            return;
        }
        let radius = radius.max(length_sq.sqrt());
        let sign = if large == sweep { -1. } else { 1. };
        let offset = (radius * radius / length_sq - 1.).max(0.).sqrt() * sign;
        let middle = from.midpoint(to);
        let center = Point(middle.0 - half.1 * offset, middle.1 + half.0 * offset);
        let (first, last) = if sweep { (from, to) } else { (to, from) };
        self.push(Stroke::Arc {
            center,
            radius,
            start: first.sub(center),
            end: last.sub(center),
            large,
        });
        // Disks at arc ends give the same round caps/joins as line capsules.
        self.line(from, from);
        self.line(to, to);
    }

    fn cubic(&mut self, from: Point, a: Point, b: Point, to: Point) {
        // A cubic stays in the convex hull of its control points. Subdivide
        // until that hull is within 0.003 viewBox units of the chord.
        let line = Segment::new(from, to);
        if line.distance_sq(a).max(line.distance_sq(b)) <= 0.003 * 0.003 {
            self.push(Stroke::Segment(line));
        } else {
            let left = from.midpoint(a);
            let right = b.midpoint(to);
            let middle = a.midpoint(b);
            let l2 = left.midpoint(middle);
            let r2 = middle.midpoint(right);
            let split = l2.midpoint(r2);
            self.cubic(from, left, l2, split);
            self.cubic(split, r2, right, to);
        }
    }

    fn sample(&self, p: Point, accent_fill: bool) -> (u32, u32) {
        if !(0. ..VIEWBOX as f32).contains(&p.1) || !(0. ..VIEWBOX as f32).contains(&p.0) {
            return (0, 0);
        }
        if self.rows[p.1 as usize].iter().any(|&(bounds, stroke)| {
            (bounds.min.0..=bounds.max.0).contains(&p.0) && stroke.contains(p)
        }) {
            return (100, 0);
        }
        for &(polygon, paint) in self.fills.iter().rev() {
            if matches!(paint, Paint::AccentFill) && !accent_fill {
                continue;
            }
            if point_in_polygon(p, polygon) {
                return match paint {
                    Paint::Foreground(opacity) => (opacity, 0),
                    Paint::AccentFill => (0, 100),
                };
            }
        }
        (0, 0)
    }

    fn rasterize(
        &self,
        origin: Point,
        size: f32,
        width: u32,
        height: u32,
        accent_fill: bool,
        mut pixel: impl FnMut(u32, u32, u8, u8),
    ) {
        if !size.is_finite() || size <= 0. || !origin.0.is_finite() || !origin.1.is_finite() {
            return;
        }
        let scale = size / VIEWBOX as f32;
        let left = (origin.0 + self.bounds.min.0.max(0.) * scale).floor() as u32;
        let top = (origin.1 + self.bounds.min.1.max(0.) * scale).floor() as u32;
        let right =
            ((origin.0 + self.bounds.max.0.min(VIEWBOX as f32) * scale).ceil() as u32).min(width);
        let bottom =
            ((origin.1 + self.bounds.max.1.min(VIEWBOX as f32) * scale).ceil() as u32).min(height);
        let units_per_pixel = scale.recip();
        let alpha = |pct| ((pct * 255 + SAMPLES * 50) / (SAMPLES * 100)) as u8;
        for row in top..bottom {
            for column in left..right {
                let mut foreground = 0;
                let mut fill = 0;
                for sy in OFFSETS {
                    for sx in OFFSETS {
                        let (fg, bg) = self.sample(
                            Point(
                                (column as f32 - origin.0 + sx) * units_per_pixel,
                                (row as f32 - origin.1 + sy) * units_per_pixel,
                            ),
                            accent_fill,
                        );
                        foreground += fg;
                        fill += bg;
                    }
                }
                if foreground != 0 || fill != 0 {
                    let foreground = alpha(foreground);
                    pixel(column, row, foreground, alpha(fill).min(255 - foreground));
                }
            }
        }
    }
}

fn point_in_polygon(p: Point, polygon: &[Point]) -> bool {
    // The source mark and star are simple polygons, so parity and SVG's
    // default nonzero fill rule agree.
    polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .filter(|&(&a, &b)| {
            (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) * (b.0 - a.0) / (b.1 - a.1)
        })
        .count()
        % 2
        != 0
}

impl Glyph {
    // Keep the existing native chrome call sites source-compatible.
    #[allow(non_upper_case_globals)]
    pub(super) const Back: Self = Self::ArrowLeft;
    #[allow(non_upper_case_globals)]
    pub(super) const Forward: Self = Self::ArrowRight;
    #[allow(non_upper_case_globals)]
    pub(super) const Reload: Self = Self::RotateCw;
    #[allow(dead_code, non_upper_case_globals)]
    pub(super) const Minimize: Self = Self::Minus;
    #[allow(dead_code, non_upper_case_globals)]
    pub(super) const Restore: Self = Self::Copy;

    /// Visit covered device pixels at the given physical origin and DPI scale
    /// using the existing 20-DIP chrome tile.
    pub(super) fn rasterize(
        self,
        x: f32,
        y: f32,
        scale: f32,
        width: u32,
        height: u32,
        pixel: impl FnMut(u32, u32, u8),
    ) {
        self.rasterize_sized(x, y, 20. * scale, width, height, pixel);
    }

    /// Visit each covered device pixel once. All coordinates and `size` are in
    /// physical pixels. Coverage includes the Satin mark's path opacity.
    pub(super) fn rasterize_sized(
        self,
        x: f32,
        y: f32,
        size: f32,
        width: u32,
        height: u32,
        mut pixel: impl FnMut(u32, u32, u8),
    ) {
        self.shape()
            .rasterize(Point(x, y), size, width, height, false, |x, y, fg, _| {
                pixel(x, y, fg);
            });
    }

    /// The active bookmark star: stroke coverage uses the accent foreground
    /// and the disjoint interior coverage uses the accent-soft fill. Combine
    /// those color contributions against the background, not as nested alpha.
    #[allow(dead_code)] // For the toolbar's active bookmark state.
    pub(super) fn rasterize_bookmarked_star(
        x: f32,
        y: f32,
        size: f32,
        width: u32,
        height: u32,
        pixel: impl FnMut(u32, u32, u8, u8),
    ) {
        Self::Star
            .shape()
            .rasterize(Point(x, y), size, width, height, true, pixel);
    }
}

#[cfg(test)]
#[path = "icons/tests.rs"]
mod tests;
