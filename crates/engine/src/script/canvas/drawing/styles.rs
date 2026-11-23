use super::super::pixels::{self, ImageStatus, Source};
use super::{Exceptions, Shared, context_node, illegal, inverse, matrix, matrix_finite, numbers};
use crate::{
    css::{self, Color},
    script::{arg, text, webidl},
    svg::{Point, SvgTransform},
};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};
use std::{
    cell::{Cell, RefCell},
    f64::consts::TAU,
    rc::Rc,
};

const STOPS_PER_GRADIENT: usize = 256;
const LIVE_STOPS: usize = 32768;
const PATTERN_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub(super) enum Paint {
    Solid(Solid),
    Gradient(Value, Rc<Gradient>),
    Pattern(Value, Rc<Pattern>),
}

#[derive(Clone, Copy)]
pub(super) struct Solid(pub [u8; 4]);

impl Default for Paint {
    fn default() -> Self {
        Self::Solid(Solid([0, 0, 0, 255]))
    }
}

impl Paint {
    pub fn from_value(
        vm: &mut Vm,
        value: Value,
        current_color: impl FnOnce(&mut Vm) -> Result<Color, JsError>,
    ) -> Result<Option<Self>, JsError> {
        if let Some(gradient) = vm.host_data::<Gradient>(&value) {
            return Ok(Some(Self::Gradient(value, gradient)));
        }
        if let Some(pattern) = vm.host_data::<Pattern>(&value) {
            return Ok(Some(Self::Pattern(value, pattern)));
        }
        let color = webidl::usv_string(vm, &value, 4096)?;
        Ok(Solid::parse_with_current(vm, &color, current_color)?.map(Self::Solid))
    }

    pub fn value(&self) -> Value {
        match self {
            Self::Solid(color) => color.css().into(),
            Self::Gradient(value, _) | Self::Pattern(value, _) => value.clone(),
        }
    }

    pub fn object(&self) -> Option<Value> {
        match self {
            Self::Solid(_) => None,
            Self::Gradient(value, _) | Self::Pattern(value, _) => Some(value.clone()),
        }
    }

    pub fn is_tainted(&self) -> bool {
        matches!(self, Self::Pattern(_, pattern) if !pattern.source.origin_clean)
    }

    pub fn prepare(&self, t: SvgTransform, smoothing: bool) -> Shader<'_> {
        match self {
            Self::Solid(color) => Shader::Solid(color.0),
            Self::Gradient(_, gradient) => {
                if let Some(back) = inverse(t) {
                    Shader::Gradient {
                        back,
                        gradient,
                        stops: gradient.stops.borrow(),
                    }
                } else if let GradientGeometry::Linear(start, end) = gradient.geometry {
                    let (start, end) = (t.map(start), t.map(end));
                    Shader::Linear {
                        start,
                        end,
                        stops: gradient.stops.borrow(),
                    }
                } else {
                    Shader::Solid([0; 4])
                }
            }
            Self::Pattern(_, pattern) => {
                if let Some(back) = inverse(t.multiply(pattern.matrix.get())) {
                    Shader::Pattern {
                        back,
                        pattern,
                        smoothing,
                    }
                } else {
                    Shader::Solid([0; 4])
                }
            }
        }
    }
}

impl Solid {
    fn current(source: &str) -> bool {
        source.trim().eq_ignore_ascii_case("currentcolor")
    }

    pub fn parse_with_current(
        vm: &mut Vm,
        source: &str,
        current_color: impl FnOnce(&mut Vm) -> Result<Color, JsError>,
    ) -> Result<Option<Self>, JsError> {
        if Self::current(source) {
            return Ok(Some(Self::from(current_color(vm)?)));
        }
        Ok(Self::parse(source))
    }

    fn from(Color(r, g, b, a): Color) -> Self {
        Self([r, g, b, a])
    }

    fn parse(source: &str) -> Option<Self> {
        let color = if Self::current(source) {
            Color::BLACK
        } else {
            css::color(source)?
        };
        Some(Self::from(color))
    }

    pub fn css(self) -> String {
        let [r, g, b, a] = self.0;
        if a == 255 {
            return format!("#{r:02x}{g:02x}{b:02x}");
        }
        let target = f64::from(a);
        let alpha = target / 255.;
        let mut shortest = alpha;
        for denominator in [10., 100., 1000.] {
            let candidate = (alpha * denominator).round() / denominator;
            if (candidate * 255.).round() == target {
                shortest = candidate;
                break;
            }
        }
        format!("rgba({r}, {g}, {b}, {shortest})")
    }
}

#[derive(Clone, Copy)]
enum GradientGeometry {
    Linear(Point, Point),
    Radial(Point, f64, Point, f64),
    Conic(f64, Point),
}

#[derive(Clone, Copy)]
pub(super) struct Stop {
    at: f64,
    color: [u8; 4],
}

pub(super) struct Gradient {
    geometry: GradientGeometry,
    stops: RefCell<Vec<Stop>>,
    resident: Rc<Cell<usize>>,
}

impl Drop for Gradient {
    fn drop(&mut self) {
        self.resident
            .set(self.resident.get() - self.stops.get_mut().len());
    }
}

pub(super) struct Pattern {
    source: Source,
    repeat: [bool; 2],
    matrix: Cell<SvgTransform>,
    resident: Rc<Cell<usize>>,
    bytes: usize,
}

impl Drop for Pattern {
    fn drop(&mut self) {
        self.resident.set(self.resident.get() - self.bytes);
    }
}

pub(super) enum Shader<'a> {
    Solid([u8; 4]),
    Gradient {
        back: SvgTransform,
        gradient: &'a Gradient,
        stops: std::cell::Ref<'a, Vec<Stop>>,
    },
    Linear {
        start: Point,
        end: Point,
        stops: std::cell::Ref<'a, Vec<Stop>>,
    },
    Pattern {
        back: SvgTransform,
        pattern: &'a Pattern,
        smoothing: bool,
    },
}

impl Shader<'_> {
    pub fn sample(&self, x: f64, y: f64) -> [u8; 4] {
        let p = Point::new(x, y);
        match self {
            Self::Solid(color) => *color,
            Self::Gradient {
                back,
                gradient,
                stops,
            } => gradient
                .geometry
                .offset(back.map(p))
                .map_or([0; 4], |t| stop_color(stops, t)),
            Self::Linear { start, end, stops } => {
                linear(*start, *end, p).map_or([0; 4], |t| stop_color(stops, t))
            }
            Self::Pattern {
                back,
                pattern,
                smoothing,
            } => pattern.sample(back.map(p), *smoothing),
        }
    }
}

fn linear(start: Point, end: Point, p: Point) -> Option<f64> {
    let direction = end.minus(start);
    let d = direction.dot(direction);
    (d > 0. && d.is_finite()).then(|| p.minus(start).dot(direction) / d)
}

impl GradientGeometry {
    fn offset(self, p: Point) -> Option<f64> {
        match self {
            Self::Linear(start, end) => linear(start, end, p),
            Self::Radial(start, r0, end, r1) => radial(start, r0, end, r1, p),
            Self::Conic(angle, center) => {
                let p = p.minus(center);
                Some((p.y.atan2(p.x) - angle).rem_euclid(TAU) / TAU)
            }
        }
    }
}

fn radial(start: Point, r0: f64, end: Point, r1: f64, p: Point) -> Option<f64> {
    let d = end.minus(start);
    let r = r1 - r0;
    let p = p.minus(start);
    let (a, b, c) = (
        d.dot(d) - r * r,
        -2. * (d.dot(p) + r0 * r),
        p.dot(p) - r0 * r0,
    );
    let roots = if a == 0. {
        if b == 0. {
            return None;
        }
        [-c / b; 2]
    } else {
        let discriminant = b * b - 4. * a * c;
        if discriminant < 0. {
            return None;
        }
        let sqrt = discriminant.sqrt();
        [(-b + sqrt) / (2. * a), (-b - sqrt) / (2. * a)]
    };
    roots
        .into_iter()
        .filter(|t| t.is_finite() && r0 + t * r >= 0.)
        .max_by(f64::total_cmp)
}

fn stop_color(stops: &[Stop], offset: f64) -> [u8; 4] {
    let Some(first) = stops.first() else {
        return [0; 4];
    };
    let index = stops.partition_point(|stop| stop.at <= offset);
    if index == 0 {
        return first.color;
    }
    let a = &stops[index - 1];
    let Some(b) = stops.get(index) else {
        return a.color;
    };
    let t = (offset - a.at) / (b.at - a.at);
    std::array::from_fn(|i| {
        (f64::from(a.color[i]) * (1. - t) + f64::from(b.color[i]) * t).round() as u8
    })
}

impl Pattern {
    fn sample(&self, p: Point, smoothing: bool) -> [u8; 4] {
        let [width, height] = self.source.size.map(f64::from);
        if !p.x.is_finite()
            || !p.y.is_finite()
            || (!self.repeat[0] && !(0. ..width).contains(&p.x))
            || (!self.repeat[1] && !(0. ..height).contains(&p.y))
        {
            return [0; 4];
        }
        let x = p.x.rem_euclid(width);
        let y = p.y.rem_euclid(height);
        if !smoothing {
            return self.source.sample(x, y, false);
        }
        let (base_x, base_y) = ((x - 0.5).floor(), (y - 0.5).floor());
        let (x_mix, y_mix) = (x - 0.5 - base_x, y - 0.5 - base_y);
        if x_mix == 0. && y_mix == 0. {
            return self.source.sample(x, y, false);
        }
        let mut premultiplied = [0.; 4];
        for (dy, wy) in [(0., 1. - y_mix), (1., y_mix)] {
            for (dx, wx) in [(0., 1. - x_mix), (1., x_mix)] {
                let color = self.source.sample(
                    (base_x + dx).rem_euclid(width),
                    (base_y + dy).rem_euclid(height),
                    false,
                );
                let alpha = f64::from(color[3]) * wx * wy;
                premultiplied[3] += alpha;
                for i in 0..3 {
                    premultiplied[i] += f64::from(color[i]) * alpha;
                }
            }
        }
        let alpha = premultiplied[3];
        if alpha == 0. {
            return [0; 4];
        }
        std::array::from_fn(|i| {
            if i == 3 {
                alpha.round() as u8
            } else {
                (premultiplied[i] / alpha).round() as u8
            }
        })
    }
}

#[derive(Clone, Copy)]
enum GradientKind {
    Linear,
    Radial,
    Conic,
}

impl GradientKind {
    fn convert(
        self,
        vm: &mut Vm,
        args: &[Value],
        errors: Exceptions,
    ) -> Result<GradientGeometry, JsError> {
        let geometry = match self {
            Self::Linear => {
                let [x0, y0, x1, y1] = restricted::<4>(vm, args)?;
                GradientGeometry::Linear(Point::new(x0, y0), Point::new(x1, y1))
            }
            Self::Radial => {
                let [x0, y0, r0, x1, y1, r1] = restricted::<6>(vm, args)?;
                if r0 < 0. || r1 < 0. {
                    return Err(errors.error(
                        vm,
                        "IndexSizeError",
                        "Gradient radius must be nonnegative",
                    ));
                }
                GradientGeometry::Radial(Point::new(x0, y0), r0, Point::new(x1, y1), r1)
            }
            Self::Conic => {
                let [angle, x, y] = restricted::<3>(vm, args)?;
                GradientGeometry::Conic(angle, Point::new(x, y))
            }
        };
        Ok(geometry)
    }
}

fn restricted<const N: usize>(vm: &mut Vm, args: &[Value]) -> Result<[f64; N], JsError> {
    // Each restricted double is checked during IDL conversion, before any
    // later argument has the opportunity to invoke a script getter.
    let mut values = [0.; N];
    for (i, value) in values.iter_mut().enumerate() {
        *value = numbers::<1>(vm, args, i)?[0];
        if !value.is_finite() {
            return Err(JsError::type_error(
                "CanvasGradient requires finite coordinates",
            ));
        }
    }
    Ok(values)
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    context: &Value,
    errors: Exceptions,
) -> Result<(), JsError> {
    let prototype = vm.new_object();
    vm.root(prototype.clone());
    webidl::interface(vm, prototype.clone(), "CanvasGradient", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal CanvasGradient constructor"))
    })?;
    webidl::operation(
        vm,
        prototype.clone(),
        "addColorStop",
        2,
        move |vm, this, args| {
            let gradient = vm
                .host_data::<Gradient>(&this)
                .ok_or_else(|| illegal("CanvasGradient"))?;
            webidl::required(args, 2, "addColorStop")?;
            let [at] = restricted::<1>(vm, args)?;
            let color = webidl::usv_string(vm, &arg(args, 1), 4096)?;
            if !(0. ..=1.).contains(&at) {
                return Err(errors.error(
                    vm,
                    "IndexSizeError",
                    "Color stop offset is outside [0, 1]",
                ));
            }
            let Some(Solid(color)) = Solid::parse(&color) else {
                return Err(errors.error(vm, "SyntaxError", "Invalid CSS color"));
            };
            let mut stops = gradient.stops.borrow_mut();
            if stops.len() >= STOPS_PER_GRADIENT || gradient.resident.get() >= LIVE_STOPS {
                return Err(JsError::new(
                    ErrorKind::HeapLimit,
                    "Canvas gradient stop limit exceeded",
                ));
            }
            let index = stops.partition_point(|stop| stop.at <= at);
            stops.insert(index, Stop { at, color });
            gradient.resident.set(gradient.resident.get() + 1);
            Ok(Value::Undefined)
        },
    )?;
    let resident = Rc::new(Cell::new(0));
    for (name, arity, kind) in [
        ("createLinearGradient", 4, GradientKind::Linear),
        ("createRadialGradient", 6, GradientKind::Radial),
        ("createConicGradient", 3, GradientKind::Conic),
    ] {
        let state = state.clone();
        let prototype = prototype.clone();
        let resident = resident.clone();
        webidl::operation(vm, context.clone(), name, arity, move |vm, this, args| {
            context_node(&state, &this)?;
            webidl::required(args, arity as usize, name)?;
            let geometry = kind.convert(vm, args, errors)?;
            let gradient = vm.new_object_with_prototype(prototype.clone())?;
            vm.set_host_data(
                gradient.clone(),
                Rc::new(Gradient {
                    geometry,
                    stops: RefCell::default(),
                    resident: resident.clone(),
                }),
            )?;
            Ok(gradient)
        })?;
    }

    let prototype = vm.new_object();
    vm.root(prototype.clone());
    webidl::interface(vm, prototype.clone(), "CanvasPattern", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal CanvasPattern constructor"))
    })?;
    webidl::operation(
        vm,
        prototype.clone(),
        "setTransform",
        0,
        |vm, this, args| {
            let pattern = vm
                .host_data::<Pattern>(&this)
                .ok_or_else(|| illegal("CanvasPattern"))?;
            let transform = matrix::from_dict(vm, arg(args, 0))?;
            if matrix_finite(transform) {
                pattern.matrix.set(transform);
            }
            Ok(Value::Undefined)
        },
    )?;
    let resident = Rc::new(Cell::new(0usize));
    let state = state.clone();
    webidl::operation(
        vm,
        context.clone(),
        "createPattern",
        2,
        move |vm, this, args| {
            context_node(&state, &this)?;
            webidl::required(args, 2, "createPattern")?;
            let id = pixels::source_node(&state, &arg(args, 0))?;
            let repeat = arg(args, 1);
            let repeat = if matches!(repeat, Value::Null) {
                String::new()
            } else {
                text(vm, repeat)?
            };
            let source = match pixels::source(vm, &state, id)? {
                ImageStatus::Ready(source) => source,
                ImageStatus::Pending => return Ok(Value::Null),
                ImageStatus::Broken => {
                    return Err(errors.error(
                        vm,
                        "InvalidStateError",
                        "Canvas pattern has no usable source image",
                    ));
                }
            };
            let repeat = match repeat.as_str() {
                "repeat" | "" => [true, true],
                "repeat-x" => [true, false],
                "repeat-y" => [false, true],
                "no-repeat" => [false, false],
                _ => {
                    return Err(errors.error(
                        vm,
                        "SyntaxError",
                        "Invalid CanvasPattern repetition",
                    ));
                }
            };
            let bytes = source
                .bitmap
                .as_ref()
                .map_or(0, |image| image.as_raw().len());
            if resident.get().saturating_add(bytes) > PATTERN_BYTES {
                return Err(JsError::new(
                    ErrorKind::HeapLimit,
                    "Canvas pattern snapshot limit exceeded",
                ));
            }
            let instance = vm.new_object_with_prototype(prototype.clone())?;
            resident.set(resident.get() + bytes);
            vm.set_host_data(
                instance.clone(),
                Rc::new(Pattern {
                    source,
                    repeat,
                    matrix: Cell::default(),
                    resident: resident.clone(),
                    bytes,
                }),
            )?;
            Ok(instance)
        },
    )?;
    Ok(())
}
