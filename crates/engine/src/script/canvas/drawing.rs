//! Canvas 2D drawing state, path bindings and software composition.
mod geometry;
mod matrix;
mod raster;
mod styles;

use super::{Bitmap, Exceptions, Shared, context_node, text::TextState};
use crate::svg::{
    Budget, FillRule, FlatPath, LineCap, LineJoin, StrokeStyle, SvgTransform, parse_path, stroke,
};
use crate::{
    css::Color,
    script::{arg, layout_api::LayoutHost, text, webidl},
};
pub(super) use geometry::OutlineCommand;
use geometry::{
    CanvasPath, PATH_METHODS, PathCall, PathQuota, inverse, matrix_finite, path_budget, point,
    rectangle,
};
use raster::{Clips, Composite, Draw, Shadow, Shape};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    rc::Rc,
};
use styles::{Paint, Solid};

const SAVE_LIMIT: usize = 128;
const DASH_LIMIT: usize = 256;
const PATH2D_POINTS: usize = 262_144;
const STYLE_SLOT: u32 = 10;
const REALM_PATH_POINTS: usize = 131_072;
const REALM_CLIP_EDGES: usize = 131_072;
const REALM_SAVED_STATES: usize = 8192;

pub(super) struct Quota {
    current_path: Rc<PathQuota>,
    clip_edges: Cell<usize>,
    saved_states: Cell<usize>,
}

impl Default for Quota {
    fn default() -> Self {
        Self {
            current_path: Rc::new(PathQuota::new(REALM_PATH_POINTS)),
            clip_edges: Cell::new(0),
            saved_states: Cell::new(0),
        }
    }
}

#[derive(Clone)]
struct DrawingState {
    matrix: SvgTransform,
    fill: Paint,
    stroke: Paint,
    composite: Composite,
    alpha: f64,
    clips: Clips,
    width: f64,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: f64,
    dash: Rc<[f64]>,
    dash_offset: f64,
    smoothing: bool,
    smoothing_quality: &'static str,
    shadow: Shadow,
    text: TextState,
}

impl Default for DrawingState {
    fn default() -> Self {
        Self {
            matrix: SvgTransform::IDENTITY,
            fill: Paint::default(),
            stroke: Paint::default(),
            composite: Composite::SourceOver,
            alpha: 1.,
            clips: Clips::default(),
            width: 1.,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.,
            dash: Rc::default(),
            dash_offset: 0.,
            smoothing: true,
            smoothing_quality: "low",
            shadow: Shadow::default(),
            text: TextState::default(),
        }
    }
}

pub(super) struct Drawing {
    state: DrawingState,
    path: CanvasPath,
    saved: Vec<DrawingState>,
    quota: Rc<Quota>,
}

impl Drawing {
    pub fn new(quota: Rc<Quota>) -> Self {
        Self {
            state: DrawingState::default(),
            path: CanvasPath::tracked(quota.current_path.clone()),
            saved: Vec::new(),
            quota,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.quota.clone());
    }

    fn begin_path(&mut self) {
        self.path = CanvasPath::tracked(self.quota.current_path.clone());
    }

    pub fn text(&self) -> &TextState {
        &self.state.text
    }

    pub fn text_mut(&mut self) -> &mut TextState {
        &mut self.state.text
    }

    pub fn transform(&self) -> SvgTransform {
        self.state.matrix
    }

    pub fn image_smoothing_enabled(&self) -> bool {
        self.state.smoothing
    }

    fn outline(&self, vm: &mut Vm, path: &FlatPath) -> Result<FlatPath, JsError> {
        let Some(back) = inverse(self.state.matrix) else {
            return Ok(FlatPath::default());
        };
        // The default path's points were already transformed on insertion.
        // Trace the pen in current user space, then bring the outline back.
        let mut local = path.transformed(back);
        for contour in &mut local.contours {
            contour.points.dedup();
        }
        let mut budget = Budget::default();
        let result = stroke(
            &local,
            &StrokeStyle {
                width: self.state.width,
                cap: self.state.line_cap,
                join: self.state.line_join,
                miter: self.state.miter_limit,
                dash: &self.state.dash,
                dash_offset: self.state.dash_offset,
            },
            self.state.matrix,
            &mut budget,
        );
        vm.charge_work(budget.diagnostics.work.div_ceil(8))?;
        if budget.diagnostics.truncated {
            return Err(geometry::size_error());
        }
        Ok(result.transformed(self.state.matrix))
    }

    fn config(&self, clear: bool) -> Draw<'_> {
        Draw {
            clips: &self.state.clips,
            operator: self.state.composite,
            global_alpha: self.state.alpha as f32,
            clear,
            shadow: self.state.shadow,
        }
    }

    fn paint_path(
        &self,
        vm: &mut Vm,
        bitmap: &mut Bitmap,
        path: &FlatPath,
        rule: FillRule,
        kind: DrawKind,
    ) -> Result<bool, JsError> {
        let Some(shape) = Shape::new(vm, path, rule)? else {
            return Ok(false);
        };
        let style = if kind == DrawKind::Stroke {
            &self.state.stroke
        } else {
            &self.state.fill
        };
        let shader = style.prepare(self.state.matrix, self.state.smoothing);
        raster::paint(
            vm,
            bitmap,
            &shape,
            self.config(kind == DrawKind::Clear),
            |x, y| shader.sample(x, y),
        )
    }

    pub fn paint_outline(
        &self,
        vm: &mut Vm,
        bitmap: &mut Bitmap,
        curves: &[OutlineCommand],
        stroke: bool,
    ) -> Result<bool, JsError> {
        let path = CanvasPath::outline(vm, curves, self.state.matrix)?;
        if stroke {
            let outline = self.outline(vm, &path.path)?;
            self.paint_path(vm, bitmap, &outline, FillRule::NonZero, DrawKind::Stroke)
        } else {
            self.paint_path(vm, bitmap, &path.path, FillRule::NonZero, DrawKind::Fill)
        }
    }

    pub fn paint_sampled(
        &self,
        vm: &mut Vm,
        bitmap: &mut Bitmap,
        bounds: [f64; 4],
        mut sample: impl FnMut(f64, f64) -> [u8; 4],
    ) -> Result<bool, JsError> {
        let Some(inverse) = inverse(self.state.matrix) else {
            return Ok(false);
        };
        let Some(path) = rectangle(bounds, false, self.state.matrix) else {
            return Ok(false);
        };
        let Some(shape) = Shape::new(vm, &path, FillRule::NonZero)? else {
            return Ok(false);
        };
        let [x, y, w, h] = bounds;
        let (left, right) = (x.min(x + w), x.max(x + w));
        let (top, bottom) = (y.min(y + h), y.max(y + h));
        raster::paint(vm, bitmap, &shape, self.config(false), |x, y| {
            let p = inverse.map(crate::svg::Point::new(x, y));
            sample(
                p.x.clamp(left, right.next_down()),
                p.y.clamp(top, bottom.next_down()),
            )
        })
    }

    fn retain_styles(
        vm: &mut Vm,
        context: Value,
        current: &DrawingState,
        saved: &[DrawingState],
    ) -> Result<(), JsError> {
        let styles = std::iter::once(current)
            .chain(saved)
            .flat_map(|state| [&state.fill, &state.stroke]);
        let mut seen = std::collections::HashSet::new();
        let objects: Vec<_> = styles
            .filter_map(Paint::object)
            .filter(|object| seen.insert(object.as_object()))
            .collect();
        if objects.is_empty() {
            return clear_hosts(vm, context);
        }
        let slots = vm.try_new_array(&objects)?;
        vm.set_host_slot(context, STYLE_SLOT, slots)
    }
}

impl Drop for Drawing {
    fn drop(&mut self) {
        self.quota
            .saved_states
            .set(self.quota.saved_states.get() - self.saved.len());
    }
}

pub(super) fn clear_hosts(vm: &mut Vm, context: Value) -> Result<(), JsError> {
    if vm.host_slot(&context, STYLE_SLOT).is_some() {
        vm.set_host_slot(context, STYLE_SLOT, Value::Undefined)?;
    }
    Ok(())
}

fn numbers<const N: usize>(vm: &mut Vm, args: &[Value], from: usize) -> Result<[f64; N], JsError> {
    let mut values = [0.; N];
    for (i, value) in values.iter_mut().enumerate() {
        *value = vm.to_number(&arg(args, from + i))?;
    }
    Ok(values)
}

fn illegal(name: &str) -> JsError {
    JsError::type_error(format!("Illegal {name} receiver"))
}

fn fill_rule(vm: &mut Vm, value: Value) -> Result<FillRule, JsError> {
    if value.is_undefined() {
        return Ok(FillRule::NonZero);
    }
    match text(vm, value)?.as_str() {
        "nonzero" => Ok(FillRule::NonZero),
        "evenodd" => Ok(FillRule::EvenOdd),
        _ => Err(JsError::type_error("Invalid CanvasFillRule")),
    }
}

struct PathData {
    path: RefCell<CanvasPath>,
}

fn install_path2d(vm: &mut Vm, errors: Exceptions) -> Result<(), JsError> {
    let prototype = vm.new_object();
    let intrinsic = prototype.clone();
    let realm_points = Rc::new(PathQuota::new(PATH2D_POINTS));
    webidl::interface(
        vm,
        prototype.clone(),
        "Path2D",
        0,
        move |vm, _, args, target| {
            let target =
                target.ok_or_else(|| JsError::type_error("Path2D constructor requires new"))?;
            let value = arg(args, 0);
            let flat = if value.is_undefined() {
                FlatPath::default()
            } else if let Some(other) = vm.host_data::<PathData>(&value) {
                let other = other.path.borrow();
                vm.charge_work(other.points)?;
                other.path.clone()
            } else {
                let string = webidl::usv_string(vm, &value, 256 * 1024)?;
                let mut budget = path_budget();
                let path = parse_path(&string, SvgTransform::IDENTITY, &mut budget);
                vm.charge_work(budget.diagnostics.work.div_ceil(8))?;
                if budget.diagnostics.truncated {
                    return Err(geometry::size_error());
                }
                path
            };
            let mut path = CanvasPath::tracked(realm_points.clone());
            path.append(CanvasPath::from_flat(flat)?)?;
            let data = Rc::new(PathData {
                path: RefCell::new(path),
            });
            let object = webidl::instance(vm, target, &intrinsic)?;
            vm.set_host_data(object.clone(), data)?;
            Ok(object)
        },
    )?;
    for &(name, arity, method) in PATH_METHODS {
        webidl::operation(vm, prototype.clone(), name, arity, move |vm, this, args| {
            let data = vm
                .host_data::<PathData>(&this)
                .ok_or_else(|| illegal("Path2D"))?;
            if let Some(call) = PathCall::convert(vm, method, name, arity as usize, args, errors)? {
                call.apply(vm, &mut data.path.borrow_mut(), SvgTransform::IDENTITY)?;
            }
            Ok(Value::Undefined)
        })?;
    }
    webidl::operation(vm, prototype, "addPath", 1, |vm, this, args| {
        let data = vm
            .host_data::<PathData>(&this)
            .ok_or_else(|| illegal("Path2D"))?;
        webidl::required(args, 1, "addPath")?;
        let other = vm
            .host_data::<PathData>(&arg(args, 0))
            .ok_or_else(|| JsError::type_error("Expected Path2D"))?;
        let matrix = matrix::from_dict(vm, arg(args, 1))?;
        if matrix_finite(matrix) {
            let other = other.path.borrow();
            vm.charge_work(other.points)?;
            let flat = other.path.transformed(matrix);
            drop(other);
            if flat
                .contours
                .iter()
                .flat_map(|contour| &contour.points)
                .all(|p| p.valid())
            {
                data.path
                    .borrow_mut()
                    .add_path(CanvasPath::from_flat(flat)?)?;
            }
        }
        Ok(Value::Undefined)
    })?;
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DrawKind {
    Fill,
    Stroke,
    Clear,
}

#[derive(Clone, Copy)]
enum PathAction {
    Draw(DrawKind),
    Clip,
    Hit(DrawKind),
}

fn install_paths(
    vm: &mut Vm,
    state: &Shared,
    prototype: &Value,
    errors: Exceptions,
) -> Result<(), JsError> {
    for &(name, arity, method) in PATH_METHODS {
        let state = state.clone();
        webidl::operation(vm, prototype.clone(), name, arity, move |vm, this, args| {
            let id = context_node(&state, &this)?;
            if let Some(command) =
                PathCall::convert(vm, method, name, arity as usize, args, errors)?
            {
                let mut state = state.borrow_mut();
                let drawing = &mut state.canvases.entries.get_mut(&id).unwrap().drawing;
                let transform = drawing.transform();
                command.apply(vm, &mut drawing.path, transform)?;
            }
            Ok(Value::Undefined)
        })?;
    }
    let host = state.clone();
    webidl::operation(vm, prototype.clone(), "beginPath", 0, move |_, this, _| {
        let id = context_node(&host, &this)?;
        host.borrow_mut()
            .canvases
            .entries
            .get_mut(&id)
            .unwrap()
            .drawing
            .begin_path();
        Ok(Value::Undefined)
    })?;
    for (name, arity, action) in [
        ("fill", 0, PathAction::Draw(DrawKind::Fill)),
        ("stroke", 0, PathAction::Draw(DrawKind::Stroke)),
        ("clip", 0, PathAction::Clip),
        ("isPointInPath", 2, PathAction::Hit(DrawKind::Fill)),
        ("isPointInStroke", 2, PathAction::Hit(DrawKind::Stroke)),
    ] {
        let state = state.clone();
        webidl::operation(vm, prototype.clone(), name, arity, move |vm, this, args| {
            let id = context_node(&state, &this)?;
            let explicit = vm.host_data::<PathData>(&arg(args, 0));
            let uses_stroke = matches!(
                action,
                PathAction::Draw(DrawKind::Stroke) | PathAction::Hit(DrawKind::Stroke)
            );
            if explicit.is_none() {
                let path_required = match action {
                    PathAction::Draw(DrawKind::Stroke) => !args.is_empty(),
                    PathAction::Draw(_) | PathAction::Clip => args.len() >= 2,
                    PathAction::Hit(DrawKind::Stroke) => args.len() >= 3,
                    PathAction::Hit(_) => args.len() >= 4,
                };
                if path_required {
                    return Err(JsError::type_error(format!("{name} expects Path2D")));
                }
            }
            let offset = usize::from(explicit.is_some());
            let test = if matches!(action, PathAction::Hit(_)) {
                webidl::required(args, offset + 2, name)?;
                Some(numbers::<2>(vm, args, offset)?)
            } else {
                None
            };
            let rule = if uses_stroke {
                FillRule::NonZero
            } else {
                fill_rule(vm, arg(args, offset + 2 * usize::from(test.is_some())))?
            };
            if test.is_some_and(|n| !n.into_iter().all(f64::is_finite)) {
                return Ok(false.into());
            }
            let mut state = state.borrow_mut();
            let entry = state.canvases.entries.get_mut(&id).unwrap();
            let drawing = &mut entry.drawing;
            let path = if let Some(explicit) = explicit {
                let path = explicit.path.borrow();
                vm.charge_work(path.points.div_ceil(8))?;
                Cow::Owned(path.path.transformed(drawing.state.matrix))
            } else {
                Cow::Borrowed(&drawing.path.path)
            };
            let path = if uses_stroke {
                Cow::Owned(drawing.outline(vm, &path)?)
            } else {
                path
            };
            match action {
                PathAction::Draw(kind) => {
                    if drawing.paint_path(vm, &mut entry.bitmap, &path, rule, kind)? {
                        state.canvases.changed(id);
                    }
                }
                PathAction::Clip => drawing
                    .state
                    .clips
                    .add(Shape::new(vm, &path, rule)?, &drawing.quota)?,
                PathAction::Hit(_) => {
                    let work = path
                        .contours
                        .iter()
                        .map(|contour| contour.points.len())
                        .sum::<usize>();
                    vm.charge_work(work.div_ceil(8))?;
                    return Ok(raster::contains(
                        &path,
                        point(test.expect("hit coordinates")),
                        rule,
                    )
                    .into());
                }
            }
            Ok(Value::Undefined)
        })?;
    }
    for (name, kind) in [
        ("fillRect", DrawKind::Fill),
        ("strokeRect", DrawKind::Stroke),
        ("clearRect", DrawKind::Clear),
    ] {
        let state = state.clone();
        webidl::operation(vm, prototype.clone(), name, 4, move |vm, this, args| {
            let id = context_node(&state, &this)?;
            webidl::required(args, 4, name)?;
            let bounds = numbers::<4>(vm, args, 0)?;
            if !bounds.into_iter().all(f64::is_finite) || bounds[2] == 0. && bounds[3] == 0. {
                return Ok(Value::Undefined);
            }
            let mut state = state.borrow_mut();
            let entry = state.canvases.entries.get_mut(&id).unwrap();
            let drawing = &entry.drawing;
            let Some(mut path) = rectangle(bounds, kind == DrawKind::Stroke, drawing.state.matrix)
            else {
                return Ok(Value::Undefined);
            };
            if kind == DrawKind::Stroke {
                path = drawing.outline(vm, &path)?;
            }
            if drawing.paint_path(vm, &mut entry.bitmap, &path, FillRule::NonZero, kind)? {
                state.canvases.changed(id);
            }
            Ok(Value::Undefined)
        })?;
    }
    Ok(())
}

fn install_state(
    vm: &mut Vm,
    state: &Shared,
    prototype: &Value,
    layout: &LayoutHost,
) -> Result<(), JsError> {
    let host = state.clone();
    webidl::operation(vm, prototype.clone(), "save", 0, move |_, this, _| {
        let id = context_node(&host, &this)?;
        let mut state = host.borrow_mut();
        let drawing = &mut state.canvases.entries.get_mut(&id).unwrap().drawing;
        let saved = drawing.quota.saved_states.get();
        if drawing.saved.len() == SAVE_LIMIT || saved == REALM_SAVED_STATES {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Canvas saved state limit exceeded",
            ));
        }
        drawing.saved.push(drawing.state.clone());
        drawing.quota.saved_states.set(saved + 1);
        Ok(Value::Undefined)
    })?;
    let host = state.clone();
    webidl::operation(vm, prototype.clone(), "restore", 0, move |vm, this, _| {
        let id = context_node(&host, &this)?;
        let mut state = host.borrow_mut();
        let drawing = &mut state.canvases.entries.get_mut(&id).unwrap().drawing;
        if let Some((restored, saved)) = drawing.saved.split_last() {
            Drawing::retain_styles(vm, this, restored, saved)?;
            drawing.state = drawing.saved.pop().expect("saved state");
            drawing
                .quota
                .saved_states
                .set(drawing.quota.saved_states.get() - 1);
        }
        Ok(Value::Undefined)
    })?;
    let host = state.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "getLineDash",
        0,
        move |vm, this, _| {
            let id = context_node(&host, &this)?;
            let store = host.borrow();
            let dash = &store.canvases.entries[&id].drawing.state.dash;
            let items: Vec<_> = dash.iter().copied().map(Value::from).collect();
            vm.try_new_array(&items)
        },
    )?;
    let host = state.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "setLineDash",
        1,
        move |vm, this, args| {
            let id = context_node(&host, &this)?;
            webidl::required(args, 1, "setLineDash")?;
            let input = arg(args, 0);
            if input.as_object().is_none() {
                return Err(JsError::type_error("Expected a dash sequence"));
            }
            let iterator = webidl::iterator_method(vm, &input)?;
            let mut values = webidl::sequence(vm, input, iterator, DASH_LIMIT, |vm, input| {
                vm.to_number(&input)
            })?;
            if values.iter().all(|value| value.is_finite() && *value >= 0.) {
                if values.len() % 2 == 1 {
                    values.extend_from_within(..);
                }
                host.borrow_mut()
                    .canvases
                    .entries
                    .get_mut(&id)
                    .unwrap()
                    .drawing
                    .state
                    .dash = values.into();
            }
            Ok(Value::Undefined)
        },
    )?;
    for (name, property) in PROPERTIES {
        let host = state.clone();
        let property = *property;
        let get = vm.new_native_function(&format!("get {name}"), move |_, this, _| {
            let id = context_node(&host, &this)?;
            Ok(property.get(&host.borrow().canvases.entries[&id].drawing.state))
        });
        let host = state.clone();
        let layout = layout.clone();
        let set = vm.new_native_function(&format!("set {name}"), move |vm, this, args| {
            let id = context_node(&host, &this)?;
            let change = property.convert(vm, arg(args, 0), |vm| {
                let layout = layout.layout(Some(vm))?;
                Ok(layout
                    .computed_styles
                    .get(id.0)
                    .map_or(Color::BLACK, |style| style.color))
            })?;
            if let Some(change) = change {
                let mut host = host.borrow_mut();
                let entry = host.canvases.entries.get_mut(&id).unwrap();
                if matches!(change, Change::Fill(_) | Change::Stroke(_)) {
                    let mut candidate = entry.drawing.state.clone();
                    let tainted = change.is_tainted();
                    change.apply(&mut candidate);
                    Drawing::retain_styles(vm, this, &candidate, &entry.drawing.saved)?;
                    entry.drawing.state = candidate;
                    entry.origin_clean &= !tainted;
                } else {
                    change.apply(&mut entry.drawing.state);
                }
            }
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype.clone(), name, Some(get), Some(set))?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Property {
    Fill,
    Stroke,
    Alpha,
    Composite,
    Width,
    Cap,
    Join,
    Miter,
    DashOffset,
    Smoothing,
    Quality,
    ShadowX,
    ShadowY,
    ShadowBlur,
    ShadowColor,
}

const PROPERTIES: &[(&str, Property)] = &[
    ("fillStyle", Property::Fill),
    ("strokeStyle", Property::Stroke),
    ("globalAlpha", Property::Alpha),
    ("globalCompositeOperation", Property::Composite),
    ("lineWidth", Property::Width),
    ("lineCap", Property::Cap),
    ("lineJoin", Property::Join),
    ("miterLimit", Property::Miter),
    ("lineDashOffset", Property::DashOffset),
    ("imageSmoothingEnabled", Property::Smoothing),
    ("imageSmoothingQuality", Property::Quality),
    ("shadowOffsetX", Property::ShadowX),
    ("shadowOffsetY", Property::ShadowY),
    ("shadowBlur", Property::ShadowBlur),
    ("shadowColor", Property::ShadowColor),
];

impl Property {
    fn get(self, state: &DrawingState) -> Value {
        match self {
            Self::Fill => state.fill.value(),
            Self::Stroke => state.stroke.value(),
            Self::Alpha => state.alpha.into(),
            Self::Composite => state.composite.name().into(),
            Self::Width => state.width.into(),
            Self::Cap => match state.line_cap {
                LineCap::Butt => "butt",
                LineCap::Round => "round",
                LineCap::Square => "square",
            }
            .into(),
            Self::Join => match state.line_join {
                LineJoin::Miter => "miter",
                LineJoin::Round => "round",
                LineJoin::Bevel => "bevel",
            }
            .into(),
            Self::Miter => state.miter_limit.into(),
            Self::DashOffset => state.dash_offset.into(),
            Self::Smoothing => state.smoothing.into(),
            Self::Quality => state.smoothing_quality.into(),
            Self::ShadowX => state.shadow.offset.x.into(),
            Self::ShadowY => state.shadow.offset.y.into(),
            Self::ShadowBlur => state.shadow.blur.into(),
            Self::ShadowColor => Solid(state.shadow.color).css().into(),
        }
    }

    fn convert(
        self,
        vm: &mut Vm,
        value: Value,
        current_color: impl FnOnce(&mut Vm) -> Result<Color, JsError>,
    ) -> Result<Option<Change>, JsError> {
        Ok(match self {
            Self::Fill => Paint::from_value(vm, value, current_color)?.map(Change::Fill),
            Self::Stroke => Paint::from_value(vm, value, current_color)?.map(Change::Stroke),
            Self::Alpha => {
                let number = vm.to_number(&value)?;
                (0. ..=1.)
                    .contains(&number)
                    .then_some(Change::Alpha(number))
            }
            Self::Width
            | Self::Miter
            | Self::DashOffset
            | Self::ShadowX
            | Self::ShadowY
            | Self::ShadowBlur => {
                let number = vm.to_number(&value)?;
                if !number.is_finite() {
                    None
                } else {
                    match self {
                        Self::Width if number > 0. => Some(Change::Width(number)),
                        Self::Miter if number > 0. => Some(Change::Miter(number)),
                        Self::DashOffset => Some(Change::DashOffset(number)),
                        Self::ShadowX => Some(Change::ShadowX(number)),
                        Self::ShadowY => Some(Change::ShadowY(number)),
                        Self::ShadowBlur if number >= 0. => Some(Change::ShadowBlur(number)),
                        _ => None,
                    }
                }
            }
            Self::Composite => Composite::parse(&text(vm, value)?).map(Change::Composite),
            Self::Cap => match text(vm, value)?.as_str() {
                "butt" => Some(Change::Cap(LineCap::Butt)),
                "round" => Some(Change::Cap(LineCap::Round)),
                "square" => Some(Change::Cap(LineCap::Square)),
                _ => None,
            },
            Self::Join => match text(vm, value)?.as_str() {
                "miter" => Some(Change::Join(LineJoin::Miter)),
                "round" => Some(Change::Join(LineJoin::Round)),
                "bevel" => Some(Change::Join(LineJoin::Bevel)),
                _ => None,
            },
            Self::Smoothing => Some(Change::Smoothing(value.is_truthy())),
            Self::Quality => match text(vm, value)?.as_str() {
                "low" => Some(Change::Quality("low")),
                "medium" => Some(Change::Quality("medium")),
                "high" => Some(Change::Quality("high")),
                _ => None,
            },
            Self::ShadowColor => {
                let source = webidl::usv_string(vm, &value, 4096)?;
                Solid::parse_with_current(vm, &source, current_color)?.map(Change::ShadowColor)
            }
        })
    }
}

enum Change {
    Fill(Paint),
    Stroke(Paint),
    Alpha(f64),
    Composite(Composite),
    Width(f64),
    Cap(LineCap),
    Join(LineJoin),
    Miter(f64),
    DashOffset(f64),
    Smoothing(bool),
    Quality(&'static str),
    ShadowX(f64),
    ShadowY(f64),
    ShadowBlur(f64),
    ShadowColor(Solid),
}

impl Change {
    fn is_tainted(&self) -> bool {
        match self {
            Self::Fill(paint) | Self::Stroke(paint) => paint.is_tainted(),
            _ => false,
        }
    }

    fn apply(self, state: &mut DrawingState) {
        match self {
            Self::Fill(paint) => state.fill = paint,
            Self::Stroke(paint) => state.stroke = paint,
            Self::Alpha(value) => state.alpha = value,
            Self::Composite(value) => state.composite = value,
            Self::Width(value) => state.width = value,
            Self::Cap(value) => state.line_cap = value,
            Self::Join(value) => state.line_join = value,
            Self::Miter(value) => state.miter_limit = value,
            Self::DashOffset(value) => state.dash_offset = value,
            Self::Smoothing(value) => state.smoothing = value,
            Self::Quality(value) => state.smoothing_quality = value,
            Self::ShadowX(value) => state.shadow.offset.x = value,
            Self::ShadowY(value) => state.shadow.offset.y = value,
            Self::ShadowBlur(value) => state.shadow.blur = value,
            Self::ShadowColor(value) => state.shadow.color = value.0,
        }
    }
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    prototype: Value,
    layout: &LayoutHost,
    errors: Exceptions,
) -> Result<(), JsError> {
    install_path2d(vm, errors)?;
    install_paths(vm, state, &prototype, errors)?;
    install_state(vm, state, &prototype, layout)?;
    matrix::install(vm, state, &prototype, errors)?;
    styles::install(vm, state, &prototype, errors)
}
