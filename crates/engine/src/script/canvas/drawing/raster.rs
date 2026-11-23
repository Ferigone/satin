use super::{Bitmap, Quota, REALM_CLIP_EDGES};
use crate::svg::{
    DevicePath, FillRule, FlatPath, Point, SvgRenderReport, SvgTransform, device_path,
    intersection, intervals,
};
use rust_browser_javascript::{ErrorKind, JsError, Vm};
use std::rc::Rc;

mod shadows;
pub(in crate::script::canvas::drawing) use shadows::Shadow;

const SAMPLES: usize = 8;
const CLIP_EDGES: usize = 8192;
const CLIP_STACK: usize = 64;
const RASTER_SPANS: usize = 262_144;

#[derive(Clone, Copy, Debug, Default)]
struct Bounds {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl Bounds {
    fn of(path: &FlatPath) -> Option<Self> {
        let mut points = path
            .contours
            .iter()
            .filter(|c| c.points.len() >= 3)
            .flat_map(|c| &c.points);
        let first = points.next()?;
        let mut bounds = Self {
            left: first.x,
            right: first.x,
            top: first.y,
            bottom: first.y,
        };
        for p in points {
            bounds.left = bounds.left.min(p.x);
            bounds.top = bounds.top.min(p.y);
            bounds.right = bounds.right.max(p.x);
            bounds.bottom = bounds.bottom.max(p.y);
        }
        Some(bounds)
    }

    fn empty(self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    fn intersect(self, other: Self) -> Self {
        Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }

    fn pixels(self) -> [u32; 4] {
        [
            self.left.floor() as u32,
            self.top.floor() as u32,
            self.right.ceil() as u32,
            self.bottom.ceil() as u32,
        ]
    }

    fn pixel_coverage(self, x: f64, y: f64) -> f32 {
        ((self.right.min(x + 1.) - self.left.max(x)).max(0.)
            * (self.bottom.min(y + 1.) - self.top.max(y)).max(0.)) as f32
    }
}

#[derive(Clone)]
pub(super) struct Shape {
    bounds: Bounds,
    path: Option<Rc<DevicePath>>,
    edges: usize,
    offset: Point,
}

impl Shape {
    pub fn new(vm: &mut Vm, path: &FlatPath, rule: FillRule) -> Result<Option<Self>, JsError> {
        let Some(bounds) = Bounds::of(path) else {
            return Ok(None);
        };
        if bounds.empty() {
            return Ok(None);
        }
        if path.contours.len() == 1 && is_rectangle(&path.contours[0].points, bounds) {
            return Ok(Some(Self {
                bounds,
                path: None,
                edges: 0,
                offset: Point::default(),
            }));
        }
        let mut report = SvgRenderReport::default();
        let device = device_path(path, SvgTransform::IDENTITY, rule, &mut report);
        vm.charge_work(report.work.div_ceil(8))?;
        if report.truncated {
            return Err(limit());
        }
        Ok(device.map(|path| Self {
            bounds,
            edges: report.work,
            path: Some(Rc::new(path)),
            offset: Point::default(),
        }))
    }

    fn intervals(&self, y: f64, report: &mut SvgRenderReport) -> Result<Vec<Span>, JsError> {
        if y < self.bounds.top || y >= self.bounds.bottom || self.bounds.empty() {
            return Ok(Vec::new());
        }
        self.path.as_ref().map_or_else(
            || Ok(vec![(self.bounds.left, self.bounds.right)]),
            |path| {
                let mut spans = intervals(path, y - self.offset.y, report).ok_or_else(limit)?;
                if self.offset.x != 0. {
                    for (a, b) in &mut spans {
                        *a += self.offset.x;
                        *b += self.offset.x;
                    }
                }
                Ok(spans)
            },
        )
    }
}

fn is_rectangle(points: &[Point], b: Bounds) -> bool {
    points.len() == 4
        && points
            .iter()
            .zip(points.iter().cycle().skip(1))
            .all(|(a, z)| {
                (a.x == b.left || a.x == b.right)
                    && (a.y == b.top || a.y == b.bottom)
                    && ((a.x == z.x) != (a.y == z.y))
            })
        && points[0] != points[2]
        && points[1] != points[3]
}

pub(super) fn contains(path: &FlatPath, point: Point, rule: FillRule) -> bool {
    let mut winding = 0i32;
    for contour in &path.contours {
        if contour.points.len() < 2 {
            continue;
        }
        for (a, b) in contour
            .points
            .iter()
            .zip(contour.points.iter().cycle().skip(1))
            .take(contour.points.len())
        {
            if a == b {
                continue;
            }
            let direction = b.minus(*a);
            let delta = point.minus(*a);
            let cross = direction.cross(delta);
            if cross.abs() <= 1e-10 * direction.length()
                && delta.dot(direction) >= 0.
                && point.minus(*b).dot(direction) <= 0.
            {
                return true;
            }
            if a.y <= point.y && point.y < b.y && cross > 0. {
                winding += 1;
            } else if b.y <= point.y && point.y < a.y && cross < 0. {
                winding -= 1;
            }
        }
    }
    match rule {
        FillRule::NonZero => winding != 0,
        FillRule::EvenOdd => winding % 2 != 0,
    }
}

#[derive(Clone, Default)]
pub(super) struct Clips {
    shapes: Rc<Vec<Rc<ClipLease>>>,
    bounds: Option<Bounds>,
    edges: usize,
}

struct ClipLease {
    shape: Shape,
    quota: Rc<Quota>,
}

impl Drop for ClipLease {
    fn drop(&mut self) {
        self.quota
            .clip_edges
            .set(self.quota.clip_edges.get() - self.shape.edges);
    }
}

impl Clips {
    pub fn add(&mut self, shape: Option<Shape>, quota: &Rc<Quota>) -> Result<(), JsError> {
        if self.bounds.is_some_and(Bounds::empty) {
            return Ok(());
        }
        let Some(shape) = shape else {
            self.bounds = Some(Bounds::default());
            self.shapes = Rc::default();
            self.edges = 0;
            return Ok(());
        };
        let bounds = self
            .bounds
            .map_or(shape.bounds, |b| b.intersect(shape.bounds));
        if shape.path.is_some() && !bounds.empty() {
            if self.edges + shape.edges > CLIP_EDGES || self.shapes.len() == CLIP_STACK {
                return Err(limit());
            }
            let resident = quota.clip_edges.get() + shape.edges;
            if resident > REALM_CLIP_EDGES {
                return Err(JsError::new(
                    ErrorKind::HeapLimit,
                    "Canvas live clip geometry exceeds its realm limit",
                ));
            }
            self.edges += shape.edges;
            quota.clip_edges.set(resident);
            Rc::make_mut(&mut self.shapes).push(Rc::new(ClipLease {
                shape,
                quota: quota.clone(),
            }));
        }
        self.bounds = Some(bounds);
        Ok(())
    }

    fn intervals(
        &self,
        y: f64,
        bounds: Bounds,
        report: &mut SvgRenderReport,
    ) -> Result<Vec<Span>, JsError> {
        if y < bounds.top || y >= bounds.bottom {
            return Ok(Vec::new());
        }
        let mut result = vec![(bounds.left, bounds.right)];
        for clip in self.shapes.iter() {
            let spans = clip.shape.intervals(y, report)?;
            result = intersection(&result, &spans);
            if result.is_empty() {
                break;
            }
        }
        Ok(result)
    }
}

type Span = (f64, f64);

#[derive(Default)]
struct Row {
    shape: Vec<Span>,
    clip: Vec<Span>,
}

fn limit() -> JsError {
    JsError::new(
        ErrorKind::ExecutionLimit,
        "Canvas rasterization budget exhausted",
    )
}

fn scan_rows(
    shape: &Shape,
    clips: &Clips,
    bounds: Bounds,
    top: u32,
    bottom: u32,
    full: bool,
) -> Result<(Vec<Row>, usize), JsError> {
    let mut report = SvgRenderReport::default();
    let mut rows = Vec::with_capacity((bottom - top) as usize);
    let mut span_count = 0;
    for y in top..bottom {
        let mut row = Row::default();
        for part in 0..SAMPLES {
            let sample_y = f64::from(y) + (part as f64 + 0.5) / SAMPLES as f64;
            let clip = clips.intervals(sample_y, bounds, &mut report)?;
            if clip.is_empty() {
                continue;
            }
            let outline = shape.intervals(sample_y, &mut report)?;
            row.shape.extend(intersection(&clip, &outline));
            if full {
                row.clip.extend(clip);
            }
        }
        span_count += row.shape.len() + row.clip.len();
        if span_count > RASTER_SPANS {
            return Err(limit());
        }
        rows.push(row);
    }
    Ok((rows, report.work))
}

fn span_coverage(spans: &[Span], coverage: &mut [f32], origin: i32) {
    coverage.fill(0.);
    for &(a, b) in spans {
        let (a, b) = (a - f64::from(origin), b - f64::from(origin));
        let left = a.floor() as usize;
        let right = coverage.len().min(b.ceil() as usize);
        for (x, coverage) in coverage.iter_mut().enumerate().take(right).skip(left) {
            let left_edge = a.max(x as f64);
            let right_edge = b.min((x + 1) as f64);
            *coverage += ((right_edge - left_edge) / SAMPLES as f64) as f32;
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Draw<'a> {
    pub clips: &'a Clips,
    pub operator: Composite,
    pub global_alpha: f32,
    pub clear: bool,
    pub shadow: Shadow,
}

pub(super) fn paint(
    vm: &mut Vm,
    bitmap: &mut Bitmap,
    shape: &Shape,
    draw: Draw<'_>,
    mut sample: impl FnMut(f64, f64) -> [u8; 4],
) -> Result<bool, JsError> {
    if !bitmap.supported() && bitmap.width() != 0 && bitmap.height() != 0 {
        return Err(JsError::new(
            ErrorKind::HeapLimit,
            "Canvas backing bitmap budget exceeded",
        ));
    }
    if draw.clear && bitmap.image().is_none() {
        return Ok(false);
    }
    let mut clip_bounds = Bounds {
        left: 0.,
        top: 0.,
        right: f64::from(bitmap.width()),
        bottom: f64::from(bitmap.height()),
    };
    if let Some(bounds) = draw.clips.bounds {
        clip_bounds = clip_bounds.intersect(bounds);
    }
    let plan = Plan::prepare(vm, clip_bounds, shape, draw)?;
    let shadow = shadows::prepare(vm, clip_bounds, shape, draw, &mut sample)?;
    if plan.is_none() && shadow.is_none() {
        return Ok(false);
    }
    let opaque = !bitmap.alpha();
    let Some(image) = bitmap.image_mut()? else {
        return Ok(false);
    };
    let mut changed = shadow.is_some_and(|shadow| shadow.paint(image, draw, opaque, &mut sample));
    if let Some(plan) = plan {
        changed |= plan.paint(image, draw, opaque, sample);
    }
    Ok(changed)
}

struct Plan {
    pixels: [u32; 4],
    shape_bounds: Bounds,
    clip_bounds: Bounds,
    rows: Option<Vec<Row>>,
    full: bool,
}

impl Plan {
    fn prepare(
        vm: &mut Vm,
        clip_bounds: Bounds,
        shape: &Shape,
        draw: Draw<'_>,
    ) -> Result<Option<Self>, JsError> {
        let shape_bounds = clip_bounds.intersect(shape.bounds);
        let full = !draw.clear && draw.operator.changes_uncovered();
        let bounds = if full { clip_bounds } else { shape_bounds };
        if bounds.empty() {
            return Ok(None);
        }
        let pixels = bounds.pixels();
        let [left, top, right, bottom] = pixels;
        let pixel_work = (right - left) as usize * (bottom - top) as usize;
        vm.charge_work(pixel_work.div_ceil(16))?;
        let rows = if shape.path.is_none() && draw.clips.shapes.is_empty() {
            None
        } else {
            let (rows, work) = scan_rows(shape, draw.clips, bounds, top, bottom, full)?;
            vm.charge_work(work.div_ceil(8))?;
            if rows
                .iter()
                .all(|row| row.shape.is_empty() && row.clip.is_empty())
            {
                return Ok(None);
            }
            Some(rows)
        };
        Ok(Some(Self {
            pixels,
            shape_bounds,
            clip_bounds,
            rows,
            full,
        }))
    }

    fn paint(
        self,
        image: &mut image::RgbaImage,
        draw: Draw<'_>,
        opaque: bool,
        mut sample: impl FnMut(f64, f64) -> [u8; 4],
    ) -> bool {
        let [left, top, right, bottom] = self.pixels;
        let row_width = (right - left) as usize;
        let mut shape_coverage = vec![0.; row_width];
        let mut clip_coverage = vec![0.; row_width];
        let mut changed = false;
        let mode = if draw.clear {
            Composite::Copy
        } else {
            draw.operator
        };
        for y in top..bottom {
            if let Some(rows) = &self.rows {
                let row = &rows[(y - top) as usize];
                span_coverage(&row.shape, &mut shape_coverage, left as i32);
                if self.full {
                    span_coverage(&row.clip, &mut clip_coverage, left as i32);
                }
            }
            for x in left..right {
                let index = (x - left) as usize;
                let xx = f64::from(x);
                let yy = f64::from(y);
                let geometry = if self.rows.is_none() {
                    self.shape_bounds.pixel_coverage(xx, yy)
                } else {
                    shape_coverage[index].min(1.)
                };
                let clip = if self.full {
                    if self.rows.is_none() {
                        self.clip_bounds.pixel_coverage(xx, yy)
                    } else {
                        clip_coverage[index].min(1.)
                    }
                } else {
                    geometry
                };
                if clip == 0. {
                    continue;
                }
                let src = if draw.clear || geometry == 0. {
                    [0; 4]
                } else {
                    sample(xx + 0.5, yy + 0.5)
                };
                let alpha = if self.full {
                    (geometry / clip).min(1.) * draw.global_alpha
                } else {
                    draw.global_alpha
                };
                let pixel = image.get_pixel_mut(x, y);
                let result = mode.apply(pixel.0, src, alpha, clip, opaque);
                changed |= result != pixel.0;
                pixel.0 = result;
            }
        }
        changed
    }
}

macro_rules! composite_modes {
    ($( $name:ident => $value:literal ),+ $(,)?) => {
        #[derive(Clone, Copy, Eq, PartialEq)]
        pub(super) enum Composite { $( $name ),+ }

        impl Composite {
            pub fn parse(value: &str) -> Option<Self> {
                match value { $( $value => Some(Self::$name), )+ _ => None }
            }

            pub fn name(self) -> &'static str {
                match self { $( Self::$name => $value, )+ }
            }
        }
    };
}

composite_modes! {
    SourceOver => "source-over", SourceIn => "source-in", SourceOut => "source-out", SourceAtop => "source-atop",
    DestinationOver => "destination-over", DestinationIn => "destination-in", DestinationOut => "destination-out", DestinationAtop => "destination-atop",
    Lighter => "lighter", Copy => "copy", Xor => "xor", Multiply => "multiply", Screen => "screen", Overlay => "overlay",
    Darken => "darken", Lighten => "lighten", ColorDodge => "color-dodge", ColorBurn => "color-burn", HardLight => "hard-light",
    SoftLight => "soft-light", Difference => "difference", Exclusion => "exclusion", Hue => "hue", Saturation => "saturation",
    Color => "color", Luminosity => "luminosity",
}

impl Composite {
    fn changes_uncovered(self) -> bool {
        matches!(
            self,
            Self::SourceIn
                | Self::SourceOut
                | Self::DestinationIn
                | Self::DestinationAtop
                | Self::Copy
        )
    }

    fn apply(
        self,
        dst: [u8; 4],
        src: [u8; 4],
        opacity: f32,
        coverage: f32,
        opaque: bool,
    ) -> [u8; 4] {
        let sa = f32::from((f32::from(src[3]) * opacity).round() as u8) / 255.;
        if self == Self::SourceOver {
            if sa == 0. {
                return dst;
            }
            if sa == 1. && coverage == 1. {
                return src;
            }
        }
        let da = f32::from(dst[3]) / 255.;
        let (s, d) = (rgb(src), rgb(dst));
        let (sf, df) = match self {
            Self::SourceIn => (da, 0.),
            Self::SourceOut => (1. - da, 0.),
            Self::SourceAtop => (da, 1. - sa),
            Self::DestinationOver => (1. - da, 1.),
            Self::DestinationIn => (0., sa),
            Self::DestinationOut => (0., 1. - sa),
            Self::DestinationAtop => (1. - da, sa),
            Self::Lighter => (1., 1.),
            Self::Copy => (1., 0.),
            Self::Xor => (1. - da, 1. - sa),
            _ => (1., 1. - sa),
        };
        let blended = self.blend(d, s);
        let src_factor = sa * sf * coverage;
        let dst_factor = da * (df * coverage + 1. - coverage);
        let alpha = if opaque {
            1.
        } else {
            (src_factor + dst_factor).min(1.)
        };
        if alpha <= 0. {
            return [0; 4];
        }
        let mut out = [0; 4];
        for i in 0..3 {
            let s = s[i] + (blended[i] - s[i]) * da;
            out[i] = byte((s * src_factor + d[i] * dst_factor) / alpha);
        }
        out[3] = byte(alpha);
        out
    }

    fn blend(self, d: [f32; 3], s: [f32; 3]) -> [f32; 3] {
        match self {
            Self::Hue => set_lum(set_sat(s, saturation(d)), lum(d)),
            Self::Saturation => set_lum(set_sat(d, saturation(s)), lum(d)),
            Self::Color => set_lum(s, lum(d)),
            Self::Luminosity => set_lum(d, lum(s)),
            _ => std::array::from_fn(|i| self.channel(d[i], s[i])),
        }
    }

    fn channel(self, d: f32, s: f32) -> f32 {
        match self {
            Self::Multiply => d * s,
            Self::Screen => d + s - d * s,
            Self::Overlay => hard_light(s, d),
            Self::Darken => d.min(s),
            Self::Lighten => d.max(s),
            Self::ColorDodge => {
                if d == 0. {
                    0.
                } else if s == 1. {
                    1.
                } else {
                    (d / (1. - s)).min(1.)
                }
            }
            Self::ColorBurn => {
                if d == 1. {
                    1.
                } else if s == 0. {
                    0.
                } else {
                    1. - ((1. - d) / s).min(1.)
                }
            }
            Self::HardLight => hard_light(d, s),
            Self::SoftLight => {
                if s <= 0.5 {
                    d - (1. - 2. * s) * d * (1. - d)
                } else {
                    let curve = if d <= 0.25 {
                        ((16. * d - 12.) * d + 4.) * d
                    } else {
                        d.sqrt()
                    };
                    d + (2. * s - 1.) * (curve - d)
                }
            }
            Self::Difference => (d - s).abs(),
            Self::Exclusion => d + s - 2. * d * s,
            _ => s,
        }
    }
}

fn hard_light(d: f32, s: f32) -> f32 {
    if s <= 0.5 {
        2. * d * s
    } else {
        1. - 2. * (1. - d) * (1. - s)
    }
}

fn rgb(value: [u8; 4]) -> [f32; 3] {
    [value[0], value[1], value[2]].map(|value| f32::from(value) / 255.)
}

fn byte(value: f32) -> u8 {
    (value.clamp(0., 1.) * 255.).round() as u8
}

fn lum([r, g, b]: [f32; 3]) -> f32 {
    0.3 * r + 0.59 * g + 0.11 * b
}

fn min_max([r, g, b]: [f32; 3]) -> (f32, f32) {
    (r.min(g).min(b), r.max(g).max(b))
}

fn saturation(c: [f32; 3]) -> f32 {
    let (low, high) = min_max(c);
    high - low
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let delta = l - lum(c);
    let mut adjusted = c.map(|value| value + delta);
    let (low, high) = min_max(adjusted);
    if low < 0. {
        adjusted = adjusted.map(|value| l + (value - l) * l / (l - low));
    }
    if high > 1. {
        adjusted = adjusted.map(|value| l + (value - l) * (1. - l) / (high - l));
    }
    adjusted
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let (low, high) = min_max(c);
    if low == high {
        [0.; 3]
    } else {
        c.map(|value| (value - low) * s / (high - low))
    }
}
