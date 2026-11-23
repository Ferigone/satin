use super::{Budget, MAX_SVG_COORDINATE};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SvgRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl SvgRect {
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
    pub fn intersect(self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        Self {
            x,
            y,
            width: ((self.x + self.width).min(other.x + other.width) - x).max(0.),
            height: ((self.y + self.height).min(other.y + other.height) - y).max(0.),
        }
    }
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y,
        }
    }
    pub(crate) fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width >= 0.
            && self.height >= 0.
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SvgTransform {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}
impl Default for SvgTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}
impl SvgTransform {
    pub const IDENTITY: Self = Self {
        a: 1.,
        b: 0.,
        c: 0.,
        d: 1.,
        e: 0.,
        f: 0.,
    };
    pub fn translate(x: f64, y: f64) -> Self {
        Self {
            e: x,
            f: y,
            ..Self::IDENTITY
        }
    }
    pub fn scale(x: f64, y: f64) -> Self {
        Self {
            a: x,
            d: y,
            ..Self::IDENTITY
        }
    }
    pub fn multiply(self, r: Self) -> Self {
        Self {
            a: self.a * r.a + self.c * r.b,
            b: self.b * r.a + self.d * r.b,
            c: self.a * r.c + self.c * r.d,
            d: self.b * r.c + self.d * r.d,
            e: self.a * r.e + self.c * r.f + self.e,
            f: self.b * r.e + self.d * r.f + self.f,
        }
    }
    pub fn inverse(self) -> Option<Self> {
        let det = self.a * self.d - self.b * self.c;
        if !det.is_finite() || det.abs() < 1e-15 {
            return None;
        }
        Some(Self {
            a: self.d / det,
            b: -self.b / det,
            c: -self.c / det,
            d: self.a / det,
            e: (self.c * self.f - self.d * self.e) / det,
            f: (self.b * self.e - self.a * self.f) / det,
        })
    }
    pub(crate) fn map(self, p: Point) -> Point {
        Point {
            x: self.a * p.x + self.c * p.y + self.e,
            y: self.b * p.x + self.d * p.y + self.f,
        }
    }
    pub(crate) fn valid(self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|v| v.is_finite() && v.abs() <= MAX_SVG_COORDINATE)
    }
    pub(crate) fn rect(self, r: SvgRect) -> SvgRect {
        bounds(&[
            self.map(Point::new(r.x as f64, r.y as f64)),
            self.map(Point::new((r.x + r.width) as f64, r.y as f64)),
            self.map(Point::new((r.x + r.width) as f64, (r.y + r.height) as f64)),
            self.map(Point::new(r.x as f64, (r.y + r.height) as f64)),
        ])
        .unwrap_or_default()
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Point {
    pub x: f64,
    pub y: f64,
}
impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    pub fn plus(self, p: Self) -> Self {
        Self::new(self.x + p.x, self.y + p.y)
    }
    pub fn minus(self, p: Self) -> Self {
        Self::new(self.x - p.x, self.y - p.y)
    }
    pub fn times(self, s: f64) -> Self {
        Self::new(self.x * s, self.y * s)
    }
    pub fn dot(self, p: Self) -> f64 {
        self.x * p.x + self.y * p.y
    }
    pub fn cross(self, p: Self) -> f64 {
        self.x * p.y - self.y * p.x
    }
    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }
    pub fn valid(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.x.abs() <= 1e12 && self.y.abs() <= 1e12
    }
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Contour {
    pub points: Vec<Point>,
    pub closed: bool,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct FlatPath {
    pub contours: Vec<Contour>,
}
impl FlatPath {
    pub fn bounds(&self) -> Option<SvgRect> {
        let mut out = None;
        for c in &self.contours {
            if let Some(b) = bounds(&c.points) {
                out = Some(out.map_or(b, |r: SvgRect| r.union(b)));
            }
        }
        out
    }
    pub fn transformed(&self, t: SvgTransform) -> Self {
        Self {
            contours: self
                .contours
                .iter()
                .map(|c| Contour {
                    points: c.points.iter().map(|&p| t.map(p)).collect(),
                    closed: c.closed,
                })
                .collect(),
        }
    }
    pub fn polygon(points: Vec<Point>) -> Self {
        Self {
            contours: vec![Contour {
                points,
                closed: true,
            }],
        }
    }
    pub fn rect(r: SvgRect) -> Self {
        Self::polygon(vec![
            Point::new(r.x as f64, r.y as f64),
            Point::new((r.x + r.width) as f64, r.y as f64),
            Point::new((r.x + r.width) as f64, (r.y + r.height) as f64),
            Point::new(r.x as f64, (r.y + r.height) as f64),
        ])
    }
}
pub(crate) fn bounds(points: &[Point]) -> Option<SvgRect> {
    let first = points.first()?;
    let (mut x, mut y, mut right, mut bottom) = (first.x, first.y, first.x, first.y);
    for p in points {
        if !p.valid() {
            return None;
        }
        x = x.min(p.x);
        y = y.min(p.y);
        right = right.max(p.x);
        bottom = bottom.max(p.y);
    }
    Some(SvgRect {
        x: x as f32,
        y: y as f32,
        width: (right - x) as f32,
        height: (bottom - y) as f32,
    })
}

pub(crate) struct Numbers<'a> {
    pub source: &'a str,
    pub pos: usize,
    after: bool,
}
impl<'a> Numbers<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            after: false,
        }
    }
    pub fn whitespace(&mut self) {
        while self
            .source
            .as_bytes()
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }
    pub fn reset_separator(&mut self) {
        self.after = false;
    }
    fn separator(&mut self) -> bool {
        self.whitespace();
        if self.source.as_bytes().get(self.pos) == Some(&b',') {
            if !self.after {
                return false;
            }
            self.pos += 1;
            self.whitespace();
        }
        true
    }
    pub fn number(&mut self, budget: &mut Budget) -> Option<f64> {
        if !self.separator() {
            return None;
        }
        let start = self.pos;
        let bytes = self.source.as_bytes();
        if matches!(bytes.get(self.pos), Some(b'+') | Some(b'-')) {
            self.pos += 1;
        }
        let mut digits = 0;
        while bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
            digits += 1;
            if self.pos - start > 128 {
                return None;
            }
        }
        if bytes.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            while bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
                digits += 1;
                if self.pos - start > 128 {
                    return None;
                }
            }
        }
        if digits == 0 {
            return None;
        }
        if matches!(bytes.get(self.pos), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(bytes.get(self.pos), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            let exponent = self.pos;
            while bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
                if self.pos - start > 128 {
                    return None;
                }
            }
            if exponent == self.pos {
                return None;
            }
        }
        if !budget.work(self.pos - start + 1) {
            return None;
        }
        let value = self.source[start..self.pos].parse::<f64>().ok()?;
        if !value.is_finite() || value.abs() > MAX_SVG_COORDINATE {
            return None;
        }
        self.after = true;
        Some(value)
    }
    pub fn flag(&mut self, budget: &mut Budget) -> Option<bool> {
        if !self.separator() || !budget.work(1) {
            return None;
        }
        let b = *self.source.as_bytes().get(self.pos)?;
        if !matches!(b, b'0' | b'1') {
            return None;
        }
        self.pos += 1;
        self.after = true;
        Some(b == b'1')
    }
    pub fn done(&mut self) -> bool {
        self.whitespace();
        self.pos == self.source.len()
    }
}

pub(crate) fn parse_transform(source: &str, budget: &mut Budget) -> Option<SvgTransform> {
    let mut n = Numbers::new(source);
    let mut result = SvgTransform::IDENTITY;
    while !n.done() {
        if n.source.as_bytes().get(n.pos) == Some(&b',') {
            n.pos += 1;
            n.whitespace();
        }
        let start = n.pos;
        while n
            .source
            .as_bytes()
            .get(n.pos)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            n.pos += 1;
        }
        let name = &source[start..n.pos];
        n.whitespace();
        if source.as_bytes().get(n.pos) != Some(&b'(') {
            return None;
        }
        n.pos += 1;
        n.reset_separator();
        let mut values = Vec::new();
        while !n.done() && source.as_bytes().get(n.pos) != Some(&b')') {
            if values.len() >= 6 {
                return None;
            }
            values.push(n.number(budget)?);
            n.whitespace();
        }
        if source.as_bytes().get(n.pos) != Some(&b')') {
            return None;
        }
        n.pos += 1;
        n.reset_separator();
        let t = match (name, values.as_slice()) {
            ("matrix", [a, b, c, d, e, f]) => SvgTransform {
                a: *a,
                b: *b,
                c: *c,
                d: *d,
                e: *e,
                f: *f,
            },
            ("translate", [x]) => SvgTransform::translate(*x, 0.),
            ("translate", [x, y]) => SvgTransform::translate(*x, *y),
            ("scale", [x]) => SvgTransform::scale(*x, *x),
            ("scale", [x, y]) => SvgTransform::scale(*x, *y),
            ("rotate", [a]) | ("rotate", [a, _, _]) => {
                let radians = a.to_radians();
                let (s, c) = radians.sin_cos();
                let r = SvgTransform {
                    a: c,
                    b: s,
                    c: -s,
                    d: c,
                    e: 0.,
                    f: 0.,
                };
                if let [_, x, y] = values.as_slice() {
                    SvgTransform::translate(*x, *y)
                        .multiply(r)
                        .multiply(SvgTransform::translate(-x, -y))
                } else {
                    r
                }
            }
            ("skewX", [a]) => SvgTransform {
                c: a.to_radians().tan(),
                ..SvgTransform::IDENTITY
            },
            ("skewY", [a]) => SvgTransform {
                b: a.to_radians().tan(),
                ..SvgTransform::IDENTITY
            },
            _ => return None,
        };
        result = result.multiply(t);
        if !result.valid() {
            return None;
        }
    }
    Some(result)
}
