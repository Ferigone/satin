use super::{Exceptions, numbers};
use crate::script::{arg, webidl};
use crate::svg::{
    Budget, Contour, FlatPath, MAX_SVG_COORDINATE, MAX_SVG_SEGMENTS, PathWriter, Point,
    SvgTransform,
};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};
use std::cell::Cell;
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::rc::Rc;

pub(super) const PATH_POINTS: usize = 8192;

#[derive(Default)]
pub(super) struct CanvasPath {
    pub path: FlatPath,
    pub points: usize,
    quota: Option<Rc<PathQuota>>,
}

pub(super) struct PathQuota {
    points: Cell<usize>,
    limit: usize,
}

impl PathQuota {
    pub fn new(limit: usize) -> Self {
        Self {
            points: Cell::new(0),
            limit,
        }
    }
}

impl Drop for CanvasPath {
    fn drop(&mut self) {
        if let Some(quota) = &self.quota {
            quota.points.set(quota.points.get() - self.points);
        }
    }
}

pub(in crate::script::canvas) enum OutlineCommand {
    Move([f64; 2]),
    Line([f64; 2]),
    Quadratic([f64; 2], [f64; 2]),
    Cubic([f64; 2], [f64; 2], [f64; 2]),
    Close,
}

pub(super) fn point(value: [f64; 2]) -> Point {
    Point::new(value[0], value[1])
}

pub(super) fn inverse(t: SvgTransform) -> Option<SvgTransform> {
    let det = t.a * t.d - t.b * t.c;
    if det == 0. || !det.is_finite() {
        return None;
    }
    let result = SvgTransform {
        a: t.d / det,
        b: -t.b / det,
        c: -t.c / det,
        d: t.a / det,
        e: (t.c * t.f - t.d * t.e) / det,
        f: (t.b * t.e - t.a * t.f) / det,
    };
    matrix_finite(result).then_some(result)
}

pub(super) fn matrix_finite(t: SvgTransform) -> bool {
    [t.a, t.b, t.c, t.d, t.e, t.f]
        .into_iter()
        .all(f64::is_finite)
}

pub(super) fn size_error() -> JsError {
    JsError::new(
        ErrorKind::ExecutionLimit,
        "Canvas path geometry budget exhausted",
    )
}

impl CanvasPath {
    pub fn tracked(quota: Rc<PathQuota>) -> Self {
        Self {
            path: FlatPath::default(),
            points: 0,
            quota: Some(quota),
        }
    }

    pub fn from_flat(path: FlatPath) -> Result<Self, JsError> {
        let points = path.contours.iter().map(|c| c.points.len()).sum();
        if points > PATH_POINTS {
            return Err(size_error());
        }
        Ok(Self {
            path,
            points,
            quota: None,
        })
    }

    pub fn append(&mut self, mut path: Self) -> Result<(), JsError> {
        self.reserve(path.points)?;
        self.account(path.points);
        self.path.contours.append(&mut path.path.contours);
        Ok(())
    }

    pub fn add_path(&mut self, path: Self) -> Result<(), JsError> {
        if let Some(point) = path.current() {
            self.reserve(path.points + 1)?;
            self.append(path)?;
            self.push(point)?;
        }
        Ok(())
    }

    fn reserve(&self, count: usize) -> Result<(), JsError> {
        if self.points + count > PATH_POINTS {
            return Err(size_error());
        }
        if self
            .quota
            .as_ref()
            .is_some_and(|quota| quota.points.get() + count > quota.limit)
        {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Canvas live path geometry exceeds its realm limit",
            ));
        }
        Ok(())
    }

    fn account(&mut self, count: usize) {
        if let Some(quota) = &self.quota {
            quota.points.set(quota.points.get() + count);
        }
        self.points += count;
    }

    fn push(&mut self, p: Point) -> Result<(), JsError> {
        if !p.valid() {
            return Ok(());
        }
        if let Some(last) = self.path.contours.last_mut()
            && !last.closed
            && last.points.len() == 1
        {
            last.points[0] = p;
            return Ok(());
        }
        self.reserve(1)?;
        self.path.contours.push(Contour {
            points: vec![p],
            closed: false,
        });
        self.account(1);
        Ok(())
    }

    fn current(&self) -> Option<Point> {
        let last = self.path.contours.last()?;
        if last.closed {
            last.points.first()
        } else {
            last.points.last()
        }
        .copied()
    }

    fn line(&mut self, p: Point) -> Result<(), JsError> {
        if !p.valid() {
            return Ok(());
        }
        if let Some(last) = self.path.contours.last()
            && last.closed
        {
            let start = last.points[0];
            self.reserve(2)?;
            self.path.contours.push(Contour {
                points: vec![start],
                closed: false,
            });
            self.account(1);
        }
        self.reserve(1)?;
        if let Some(last) = self.path.contours.last_mut() {
            last.points.push(p);
            self.account(1);
            Ok(())
        } else {
            self.push(p)
        }
    }

    fn close(&mut self) {
        if let Some(last) = self.path.contours.last_mut() {
            last.closed = true;
        }
    }

    fn cubic(&mut self, vm: &mut Vm, c1: Point, c2: Point, end: Point) -> Result<(), JsError> {
        if ![c1, c2, end].into_iter().all(Point::valid) {
            return Ok(());
        }
        let start = self.current().unwrap_or(c1);
        let mut writer = PathWriter::default();
        let mut budget = path_budget();
        writer.move_to(start, &mut budget);
        writer.cubic(c1, c2, end, SvgTransform::IDENTITY, &mut budget);
        vm.charge_work(budget.diagnostics.work.div_ceil(8))?;
        if budget.diagnostics.truncated {
            return Err(size_error());
        }
        let arc = writer.path.contours.pop().expect("curve start");
        self.reserve(arc.points.len())?;
        if self.current().is_none() {
            self.push(start)?;
        }
        for p in arc.points.into_iter().skip(1) {
            self.line(p)?;
        }
        Ok(())
    }

    fn quadratic(&mut self, vm: &mut Vm, c: Point, end: Point) -> Result<(), JsError> {
        let start = self.current().unwrap_or(c);
        self.cubic(
            vm,
            start.plus(c.minus(start).times(2. / 3.)),
            end.plus(c.minus(end).times(2. / 3.)),
            end,
        )
    }

    fn outbound(&mut self, x: f64, y: f64, w: f64, h: f64, t: SvgTransform) -> Result<(), JsError> {
        let path = rectangle([x, y, w, h], false, t);
        if let Some(path) = path {
            self.append(Self::from_flat(path)?)?;
        }
        Ok(())
    }

    fn ellipse(&mut self, vm: &mut Vm, ellipse: Ellipse, t: SvgTransform) -> Result<(), JsError> {
        let (sin, cos) = ellipse.rotation.sin_cos();
        let position = |a: f64| {
            let (s, c) = a.sin_cos();
            let x = ellipse.radii[0] * c;
            let y = ellipse.radii[1] * s;
            t.map(
                ellipse
                    .center
                    .plus(Point::new(x * cos - y * sin, x * sin + y * cos)),
            )
        };
        let delta = ellipse.end - ellipse.start;
        let sweep = if ellipse.anticlockwise {
            if delta <= -TAU {
                -TAU
            } else {
                -(-delta).rem_euclid(TAU)
            }
        } else if delta >= TAU {
            TAU
        } else {
            delta.rem_euclid(TAU)
        };
        let start = ellipse.start.rem_euclid(TAU);
        let first = position(start);
        if !first.valid() || !sweep.is_finite() {
            return Ok(());
        }
        // Flatten in device space, including nonuniform scale. Start each piece
        // at most a quadrant apart so the midpoint tests resolve a full circle.
        let pieces = (sweep.abs() / FRAC_PI_2).ceil().max(1.) as usize;
        let mut samples = vec![first];
        let mut work = 0usize;
        if sweep != 0. {
            for piece in 0..pieces {
                let a = start + sweep * piece as f64 / pieces as f64;
                let b = start + sweep * (piece + 1) as f64 / pieces as f64;
                let mut pending = vec![(a, b, 0)];
                while let Some((a, b, depth)) = pending.pop() {
                    work += 1;
                    let middle = (a + b) / 2.;
                    let (p, q, m) = (position(a), position(b), position(middle));
                    if ![p, q, m].into_iter().all(Point::valid) {
                        return Ok(());
                    }
                    if m.minus(p.plus(q).times(0.5)).length() <= 0.1 || depth == 16 {
                        if samples.len() >= PATH_POINTS {
                            vm.charge_work(work)?;
                            return Err(size_error());
                        }
                        samples.push(q);
                    } else {
                        pending.push((middle, b, depth + 1));
                        pending.push((a, middle, depth + 1));
                    }
                }
            }
        }
        vm.charge_work(work)?;
        self.reserve(samples.len() + 1)?;
        for p in samples {
            self.line(p)?;
        }
        Ok(())
    }

    fn arc_to(
        &mut self,
        vm: &mut Vm,
        [x1, y1, x2, y2, r]: [f64; 5],
        t: SvgTransform,
    ) -> Result<(), JsError> {
        let (corner, end) = (Point::new(x1, y1), Point::new(x2, y2));
        let Some(start) = self
            .current()
            .and_then(|p| inverse(t).map(|inverse| inverse.map(p)))
        else {
            return self.line(t.map(corner));
        };
        let (u, v) = (start.minus(corner), end.minus(corner));
        let (lu, lv) = (u.length(), v.length());
        if lu == 0. || lv == 0. || r == 0. {
            return self.line(t.map(corner));
        }
        let (u, v) = (u.times(1. / lu), v.times(1. / lv));
        let turn = u.cross(v);
        if turn.abs() <= f64::EPSILON {
            return self.line(t.map(corner));
        }
        let angle = u.dot(v).clamp(-1., 1.).acos() / 2.;
        let tangent = r / angle.tan();
        let bisector = u.plus(v);
        let center = corner.plus(bisector.times(r / (bisector.length() * angle.sin())));
        let a = corner.plus(u.times(tangent)).minus(center);
        let b = corner.plus(v.times(tangent)).minus(center);
        self.ellipse(
            vm,
            Ellipse {
                center,
                radii: [r, r],
                rotation: 0.,
                start: a.y.atan2(a.x),
                end: b.y.atan2(b.x),
                anticlockwise: turn > 0.,
            },
            t,
        )
    }

    fn round_rect(
        &mut self,
        vm: &mut Vm,
        [x, y, w, h]: [f64; 4],
        mut radii: [Point; 4],
        t: SvgTransform,
    ) -> Result<(), JsError> {
        if w < 0. {
            radii.swap(0, 1);
            radii.swap(2, 3);
        }
        if h < 0. {
            radii.swap(0, 3);
            radii.swap(1, 2);
        }
        let t = t
            .multiply(SvgTransform::translate(x, y))
            .multiply(SvgTransform::scale(w.signum(), h.signum()));
        let (w, h) = (w.abs(), h.abs());
        let scaled = |side: f64, combined: f64| if combined > 0. { side / combined } else { 1. };
        let scale = 1f64
            .min(scaled(w, radii[0].x + radii[1].x))
            .min(scaled(w, radii[2].x + radii[3].x))
            .min(scaled(h, radii[0].y + radii[3].y))
            .min(scaled(h, radii[1].y + radii[2].y));
        let [tl, tr, br, bl] = radii.map(|point| point.times(scale));
        let mut rounded = Self::default();
        for (center, radius, start) in [
            (Point::new(w - tr.x, tr.y), tr, -FRAC_PI_2),
            (Point::new(w - br.x, h - br.y), br, 0.),
            (Point::new(bl.x, h - bl.y), bl, FRAC_PI_2),
            (tl, tl, PI),
        ] {
            rounded.ellipse(
                vm,
                Ellipse {
                    center,
                    radii: [radius.x, radius.y],
                    rotation: 0.,
                    start,
                    end: start + FRAC_PI_2,
                    anticlockwise: false,
                },
                t,
            )?;
        }
        rounded.close();
        self.reserve(rounded.points + 1)?;
        self.append(rounded)?;
        self.push(t.map(Point::new(0., 0.)))
    }

    pub fn outline(
        vm: &mut Vm,
        curves: &[OutlineCommand],
        t: SvgTransform,
    ) -> Result<Self, JsError> {
        let mut result = Self::default();
        let map = |xy| t.map(point(xy));
        for curve in curves {
            match *curve {
                OutlineCommand::Move(p) => result.push(map(p))?,
                OutlineCommand::Line(p) => result.line(map(p))?,
                OutlineCommand::Quadratic(c, end) => result.quadratic(vm, map(c), map(end))?,
                OutlineCommand::Cubic(c1, c2, end) => {
                    result.cubic(vm, map(c1), map(c2), map(end))?
                }
                OutlineCommand::Close => result.close(),
            }
        }
        Ok(result)
    }
}

pub(super) fn path_budget() -> Budget {
    let mut budget = Budget::default();
    budget.diagnostics.segments = MAX_SVG_SEGMENTS - PATH_POINTS;
    budget
}

pub(super) fn rectangle(
    [x, y, w, h]: [f64; 4],
    for_stroke: bool,
    t: SvgTransform,
) -> Option<FlatPath> {
    let mut vertices = [
        Point::new(x, y),
        Point::new(x + w, y),
        Point::new(x + w, y + h),
        Point::new(x, y + h),
    ]
    .map(|p| t.map(p));
    if !vertices.iter().all(|p| p.valid()) {
        // An axis-aligned rectangle can extend beyond the rasterizer's
        // coordinate range while still covering the entire canvas.
        if t.b != 0. || t.c != 0. {
            return None;
        }
        let [x0, x1] = rectangle_axis(x, w, t.a, t.e)?;
        let [y0, y1] = rectangle_axis(y, h, t.d, t.f)?;
        vertices = [
            Point::new(x0, y0),
            Point::new(x1, y0),
            Point::new(x1, y1),
            Point::new(x0, y1),
        ];
    }
    let mut vertices = vertices.to_vec();
    if w == 0. && h == 0. {
        vertices.truncate(1);
    } else if for_stroke && (w == 0. || h == 0.) {
        vertices[1] = vertices[2];
        vertices.truncate(2);
        return Some(FlatPath {
            contours: vec![Contour {
                points: vertices,
                closed: false,
            }],
        });
    }
    Some(FlatPath::polygon(vertices))
}

fn rectangle_axis(origin: f64, length: f64, scale: f64, translation: f64) -> Option<[f64; 2]> {
    let [first, last] = [origin, origin + length].map(|value| {
        if scale == 0. {
            translation
        } else {
            value.mul_add(scale, translation)
        }
    });
    if first.is_nan()
        || last.is_nan()
        || first.min(last) > MAX_SVG_COORDINATE
        || first.max(last) < -MAX_SVG_COORDINATE
    {
        return None;
    }
    Some([first, last].map(|value| value.clamp(-MAX_SVG_COORDINATE, MAX_SVG_COORDINATE)))
}

struct Ellipse {
    center: Point,
    radii: [f64; 2],
    rotation: f64,
    start: f64,
    end: f64,
    anticlockwise: bool,
}

#[derive(Clone, Copy)]
pub(super) enum PathMethod {
    Move,
    Line,
    Close,
    Quadratic,
    Cubic,
    Arc,
    ArcTo,
    Ellipse,
    Rect,
    RoundRect,
}

pub(super) const PATH_METHODS: &[(&str, u32, PathMethod)] = &[
    ("moveTo", 2, PathMethod::Move),
    ("lineTo", 2, PathMethod::Line),
    ("closePath", 0, PathMethod::Close),
    ("quadraticCurveTo", 4, PathMethod::Quadratic),
    ("bezierCurveTo", 6, PathMethod::Cubic),
    ("arc", 5, PathMethod::Arc),
    ("arcTo", 5, PathMethod::ArcTo),
    ("ellipse", 7, PathMethod::Ellipse),
    ("rect", 4, PathMethod::Rect),
    ("roundRect", 4, PathMethod::RoundRect),
];

pub(super) struct PathCall {
    method: PathMethod,
    numbers: [f64; 7],
    anticlockwise: bool,
    radii: [Point; 4],
}

impl PathCall {
    pub fn convert(
        vm: &mut Vm,
        method: PathMethod,
        name: &str,
        arity: usize,
        args: &[Value],
        errors: Exceptions,
    ) -> Result<Option<Self>, JsError> {
        webidl::required(args, arity, name)?;
        let mut result = Self {
            method,
            numbers: [0.; 7],
            anticlockwise: false,
            radii: [Point::default(); 4],
        };
        match arity {
            2 => result.numbers[..2].copy_from_slice(&numbers::<2>(vm, args, 0)?),
            4 => result.numbers[..4].copy_from_slice(&numbers::<4>(vm, args, 0)?),
            5 => result.numbers[..5].copy_from_slice(&numbers::<5>(vm, args, 0)?),
            6 => result.numbers[..6].copy_from_slice(&numbers::<6>(vm, args, 0)?),
            7 => result.numbers = numbers::<7>(vm, args, 0)?,
            _ => {}
        }
        if matches!(method, PathMethod::Arc | PathMethod::Ellipse) {
            result.anticlockwise = arg(args, arity).is_truthy();
        }
        if matches!(method, PathMethod::RoundRect) {
            let Some(radii) = round_radii(
                vm,
                arg(args, 4),
                result.numbers.iter().all(|n| n.is_finite()),
            )?
            else {
                return Ok(None);
            };
            result.radii = radii;
        }
        let n = result.numbers;
        if !n.into_iter().all(f64::is_finite) {
            return Ok(None);
        }
        let negative_radius = match method {
            PathMethod::Arc => n[2] < 0.,
            PathMethod::ArcTo => n[4] < 0.,
            PathMethod::Ellipse => n[2] < 0. || n[3] < 0.,
            _ => false,
        };
        if negative_radius {
            return Err(errors.error(
                vm,
                "IndexSizeError",
                "Canvas path radius must be nonnegative",
            ));
        }
        Ok(Some(result))
    }

    pub fn apply(self, vm: &mut Vm, path: &mut CanvasPath, t: SvgTransform) -> Result<(), JsError> {
        let [a, b, c, d, e, f, g] = self.numbers;
        let map = |x, y| t.map(Point::new(x, y));
        match self.method {
            PathMethod::Move => path.push(map(a, b)),
            PathMethod::Line => path.line(map(a, b)),
            PathMethod::Close => {
                path.close();
                Ok(())
            }
            PathMethod::Quadratic => path.quadratic(vm, map(a, b), map(c, d)),
            PathMethod::Cubic => path.cubic(vm, map(a, b), map(c, d), map(e, f)),
            PathMethod::Arc => path.ellipse(
                vm,
                Ellipse {
                    center: Point::new(a, b),
                    radii: [c, c],
                    rotation: 0.,
                    start: d,
                    end: e,
                    anticlockwise: self.anticlockwise,
                },
                t,
            ),
            PathMethod::ArcTo => path.arc_to(vm, [a, b, c, d, e], t),
            PathMethod::Ellipse => path.ellipse(
                vm,
                Ellipse {
                    center: Point::new(a, b),
                    radii: [c, d],
                    rotation: e,
                    start: f,
                    end: g,
                    anticlockwise: self.anticlockwise,
                },
                t,
            ),
            PathMethod::Rect => path.outbound(a, b, c, d, t),
            PathMethod::RoundRect => path.round_rect(vm, [a, b, c, d], self.radii, t),
        }
    }
}

fn sized_point(vm: &mut Vm, value: Value) -> Result<Point, JsError> {
    if value.as_object().is_some() || value.is_nullish() {
        let mut out = [0.; 2];
        if !value.is_nullish() {
            for (slot, key) in out.iter_mut().zip(["x", "y"]) {
                let part = vm.get_property(value.clone(), key)?;
                if !part.is_undefined() {
                    *slot = vm.to_number(&part)?;
                }
            }
        }
        return Ok(point(out));
    }
    let n = vm.to_number(&value)?;
    Ok(Point::new(n, n))
}

fn round_radii(
    vm: &mut Vm,
    value: Value,
    finite_rect: bool,
) -> Result<Option<[Point; 4]>, JsError> {
    let default = [Point::default(); 4];
    if value.is_undefined() {
        return Ok(Some(default));
    }
    let mut values = Vec::new();
    if value.as_object().is_some() {
        let iterator = webidl::iterator_method(vm, &value)?;
        if !iterator.is_nullish() {
            values = webidl::sequence(vm, value.clone(), iterator, 128, sized_point)?;
            if !(1..=4).contains(&values.len()) && finite_rect {
                return Err(JsError::new(
                    ErrorKind::RangeError,
                    "roundRect expects one to four radii",
                ));
            }
        } else {
            values.push(sized_point(vm, value)?);
        }
    } else {
        values.push(sized_point(vm, value)?);
    }
    if !finite_rect || values.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
        return Ok(None);
    }
    if values.iter().any(|p| p.x < 0. || p.y < 0.) {
        return Err(JsError::new(
            ErrorKind::RangeError,
            "roundRect radii must be nonnegative",
        ));
    }
    Ok(Some(match values.as_slice() {
        [a] => [*a; 4],
        [a, b] => [*a, *b, *a, *b],
        [a, b, c] => [*a, *b, *c, *b],
        [a, b, c, d] => [*a, *b, *c, *d],
        _ => default,
    }))
}
