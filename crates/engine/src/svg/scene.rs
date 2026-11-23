use super::{
    geometry::{Contour, FlatPath, Numbers, Point, parse_transform},
    path::{self, Writer},
    *,
};
use crate::dom::{AttributeNamespace, Namespace, NodeKind};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub(crate) enum Command {
    PushOpacity(f32),
    PopOpacity,
    Paint {
        node: NodeId,
        path: Arc<FlatPath>,
        rule: FillRule,
        paint: Paint,
        clips: Vec<Arc<Clip>>,
        bounds: SvgRect,
    },
}
#[derive(Clone, Debug)]
pub(crate) struct Clip {
    pub paths: Vec<(FlatPath, FillRule)>,
}
#[derive(Clone, Debug)]
pub(crate) struct Stop {
    pub offset: f64,
    pub color: Color,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Spread {
    Pad,
    Repeat,
    Reflect,
}
#[derive(Clone, Debug)]
pub(crate) enum Paint {
    Solid(Color),
    Linear {
        inverse: SvgTransform,
        from: Point,
        to: Point,
        stops: Arc<Vec<Stop>>,
        spread: Spread,
        opacity: f32,
    },
    Radial {
        inverse: SvgTransform,
        center: Point,
        focus: Point,
        radius: f64,
        stops: Arc<Vec<Stop>>,
        spread: Spread,
        opacity: f32,
    },
}
#[derive(Clone)]
struct Computed {
    geometry: BTreeMap<String, String>,
    fill: SvgPaint,
    stroke: SvgPaint,
    color: Color,
    fill_opacity: f32,
    stroke_opacity: f32,
    opacity: f32,
    fill_rule: FillRule,
    clip_rule: FillRule,
    stroke_width: f32,
    linecap: LineCap,
    linejoin: LineJoin,
    miter: f32,
    dash: Vec<f32>,
    dash_offset: f32,
    display: bool,
    visibility: bool,
    clip_path: String,
    stop_color: Color,
    stop_current_color: bool,
    stop_opacity: f32,
}
impl Default for Computed {
    fn default() -> Self {
        Self {
            geometry: BTreeMap::new(),
            fill: SvgPaint::Color(Color::BLACK),
            stroke: SvgPaint::None,
            color: Color::BLACK,
            fill_opacity: 1.,
            stroke_opacity: 1.,
            opacity: 1.,
            fill_rule: FillRule::NonZero,
            clip_rule: FillRule::NonZero,
            stroke_width: 1.,
            linecap: LineCap::Butt,
            linejoin: LineJoin::Miter,
            miter: 4.,
            dash: vec![],
            dash_offset: 0.,
            display: true,
            visibility: true,
            clip_path: String::new(),
            stop_color: Color::BLACK,
            stop_current_color: false,
            stop_opacity: 1.,
        }
    }
}
impl Computed {
    fn child(&self) -> Self {
        Self {
            geometry: BTreeMap::new(),
            opacity: 1.,
            display: true,
            clip_path: String::new(),
            stop_color: Color::BLACK,
            stop_current_color: false,
            stop_opacity: 1.,
            ..self.clone()
        }
    }
    fn apply(&mut self, s: &SvgStyle, parent: &Self) {
        self.geometry.extend(s.geometry.clone());
        macro_rules! copy {
            ($field:ident,$source:ident) => {
                if let Some(v) = &s.$source {
                    self.$field = v.clone();
                }
            };
        }
        copy!(fill, fill);
        copy!(stroke, stroke);
        copy!(color, color);
        copy!(fill_opacity, fill_opacity);
        copy!(stroke_opacity, stroke_opacity);
        copy!(opacity, opacity);
        copy!(fill_rule, fill_rule);
        copy!(clip_rule, clip_rule);
        copy!(stroke_width, stroke_width);
        copy!(linecap, stroke_linecap);
        copy!(linejoin, stroke_linejoin);
        copy!(miter, stroke_miterlimit);
        copy!(dash, stroke_dasharray);
        copy!(dash_offset, stroke_dashoffset);
        copy!(display, display);
        copy!(visibility, visibility);
        copy!(clip_path, clip_path);
        copy!(stop_color, stop_color);
        copy!(stop_current_color, stop_current_color);
        copy!(stop_opacity, stop_opacity);
        for name in &s.inherit {
            match name.as_str() {
                "fill" => self.fill = parent.fill.clone(),
                "stroke" => self.stroke = parent.stroke.clone(),
                "color" => self.color = parent.color,
                "fill-opacity" => self.fill_opacity = parent.fill_opacity,
                "stroke-opacity" => self.stroke_opacity = parent.stroke_opacity,
                "opacity" => self.opacity = parent.opacity,
                "fill-rule" => self.fill_rule = parent.fill_rule,
                "clip-rule" => self.clip_rule = parent.clip_rule,
                "stroke-width" => self.stroke_width = parent.stroke_width,
                "stroke-linecap" => self.linecap = parent.linecap,
                "stroke-linejoin" => self.linejoin = parent.linejoin,
                "stroke-miterlimit" => self.miter = parent.miter,
                "stroke-dasharray" => self.dash = parent.dash.clone(),
                "stroke-dashoffset" => self.dash_offset = parent.dash_offset,
                "display" => self.display = parent.display,
                "visibility" => self.visibility = parent.visibility,
                "clip-path" => self.clip_path = parent.clip_path.clone(),
                "stop-color" => {
                    self.stop_color = parent.stop_color;
                    self.stop_current_color = parent.stop_current_color;
                }
                "stop-opacity" => self.stop_opacity = parent.stop_opacity,
                name if super::style::geometry_property(name) => {
                    self.geometry.insert(
                        name.into(),
                        parent.geometry.get(name).cloned().unwrap_or_else(|| {
                            if matches!(name, "rx" | "ry" | "width" | "height") {
                                "auto".into()
                            } else {
                                "0".into()
                            }
                        }),
                    );
                }
                _ => {}
            }
        }
    }
}

pub(super) fn intrinsic_size(doc: &Document, root: NodeId) -> (f32, f32) {
    let mut b = Budget::default();
    let vb = viewbox(doc, root, &mut b).filter(|v| v.width > 0. && v.height > 0.);
    let width = doc
        .attr(root, "width")
        .and_then(|s| length(s, 300.))
        .filter(|v| *v >= 0.);
    let height = doc
        .attr(root, "height")
        .and_then(|s| length(s, 150.))
        .filter(|v| *v >= 0.);
    match (width, height, vb) {
        (Some(w), Some(h), _) => (w as f32, h as f32),
        (Some(w), None, Some(v)) => (
            w as f32,
            (w * f64::from(v.height) / f64::from(v.width)) as f32,
        ),
        (None, Some(h), Some(v)) => (
            (h * f64::from(v.width) / f64::from(v.height)) as f32,
            h as f32,
        ),
        (Some(w), None, None) => (w as f32, 150.),
        (None, Some(h), None) => (300., h as f32),
        (None, None, Some(v)) => {
            let ratio = v.width / v.height;
            let w = 300f32.min(150. * ratio);
            (w, w / ratio)
        }
        _ => (300., 150.),
    }
}
pub(crate) fn length(source: &str, base: f64) -> Option<f64> {
    let s = source.trim();
    let (number, factor) = if let Some(s) = s.strip_suffix('%') {
        (s, base / 100.)
    } else if let Some(s) = s.strip_suffix("px") {
        (s, 1.)
    } else if let Some(s) = s.strip_suffix("pt") {
        (s, 96. / 72.)
    } else if let Some(s) = s.strip_suffix("pc") {
        (s, 16.)
    } else if let Some(s) = s.strip_suffix("mm") {
        (s, 96. / 25.4)
    } else if let Some(s) = s.strip_suffix("cm") {
        (s, 96. / 2.54)
    } else if let Some(s) = s.strip_suffix("in") {
        (s, 96.)
    } else if let Some(s) = s.strip_suffix("em") {
        (s, 16.)
    } else if let Some(s) = s.strip_suffix("ex") {
        (s, 8.)
    } else {
        (s, 1.)
    };
    let v = number.parse::<f64>().ok()? * factor;
    (v.is_finite() && v.abs() <= MAX_SVG_COORDINATE).then_some(v)
}
fn viewbox(doc: &Document, id: NodeId, b: &mut Budget) -> Option<SvgRect> {
    let raw = doc.attr(id, "viewBox")?;
    if !b.work(raw.len()) {
        return None;
    }
    let mut n = Numbers::new(raw);
    let v = [n.number(b)?, n.number(b)?, n.number(b)?, n.number(b)?];
    if !n.done() || v[2] < 0. || v[3] < 0. {
        b.diagnostics.invalid_geometry = true;
        return None;
    }
    Some(SvgRect {
        x: v[0] as f32,
        y: v[1] as f32,
        width: v[2] as f32,
        height: v[3] as f32,
    })
}
fn viewport_transform(
    doc: &Document,
    id: NodeId,
    width: f64,
    height: f64,
    b: &mut Budget,
) -> Option<(SvgTransform, (f64, f64))> {
    let Some(v) = viewbox(doc, id, b) else {
        return Some((SvgTransform::IDENTITY, (width, height)));
    };
    if v.width == 0. || v.height == 0. {
        return None;
    }
    let raw = doc
        .attr(id, "preserveAspectRatio")
        .unwrap_or("xMidYMid meet");
    let mut words = raw.split_ascii_whitespace();
    let mut align = words.next().unwrap_or("xMidYMid");
    if align == "defer" {
        align = words.next().unwrap_or("xMidYMid");
    }
    let mode = words.next().unwrap_or("meet");
    let sx = width / f64::from(v.width);
    let sy = height / f64::from(v.height);
    let (sx, sy, dx, dy) = if align == "none" {
        (sx, sy, 0., 0.)
    } else {
        let scale = if mode == "slice" {
            sx.max(sy)
        } else {
            sx.min(sy)
        };
        let ax = if align.starts_with("xMin") {
            0.
        } else if align.starts_with("xMax") {
            1.
        } else {
            0.5
        };
        let ay = if align.ends_with("YMin") {
            0.
        } else if align.ends_with("YMax") {
            1.
        } else {
            0.5
        };
        (
            scale,
            scale,
            (width - f64::from(v.width) * scale) * ax,
            (height - f64::from(v.height) * scale) * ay,
        )
    };
    Some((
        SvgTransform {
            a: sx,
            d: sy,
            e: dx - f64::from(v.x) * sx,
            f: dy - f64::from(v.y) * sy,
            ..SvgTransform::IDENTITY
        },
        (f64::from(v.width), f64::from(v.height)),
    ))
}

#[derive(Clone)]
struct State {
    transform: SvgTransform,
    viewport: (f64, f64),
    style: Computed,
    clips: Vec<Arc<Clip>>,
    ancestors: Vec<NodeId>,
    refs: Vec<NodeId>,
    instance: Option<NodeId>,
    depth: usize,
    use_size: Option<(f64, f64)>,
}
struct Builder<'a> {
    doc: &'a Document,
    styles: &'a BTreeMap<NodeId, SvgStyle>,
    ids: BTreeMap<String, NodeId>,
    budget: Budget,
    scene: SvgScene,
}
enum Task {
    Visit(NodeId, Box<State>, bool),
    EndOpacity,
}

pub(super) fn build(
    doc: &Document,
    root: NodeId,
    width: f32,
    height: f32,
    styles: &BTreeMap<NodeId, SvgStyle>,
) -> SvgScene {
    let width = if width.is_finite() {
        width.clamp(0., MAX_SVG_COORDINATE as f32)
    } else {
        0.
    };
    let height = if height.is_finite() {
        height.clamp(0., MAX_SVG_COORDINATE as f32)
    } else {
        0.
    };
    let mut builder = Builder {
        doc,
        styles,
        ids: BTreeMap::new(),
        budget: Budget::default(),
        scene: SvgScene {
            width,
            height,
            diagnostics: SvgDiagnostics::default(),
            node_bounds: BTreeMap::new(),
            geometry_bounds: BTreeMap::new(),
            commands: vec![],
        },
    };
    if root.0 >= doc.nodes.len() || doc.namespace(root) != Namespace::Svg || doc.tag(root) != "svg"
    {
        builder.budget.diagnostics.unsupported = true;
        builder.scene.diagnostics = builder.budget.diagnostics;
        return builder.scene;
    }
    if width <= 0. || height <= 0. {
        return builder.scene;
    }
    for id in doc.connected_nodes() {
        if !builder.budget.work(1) {
            break;
        }
        if doc.namespace(id) == Namespace::Svg
            && let Some(name) = doc.attr(id, "id")
            && name.len() <= 1024
        {
            if builder.ids.len() >= MAX_SVG_NODES || !builder.budget.work(name.len()) {
                builder.budget.diagnostics.truncated = true;
                break;
            }
            builder.ids.entry(name.into()).or_insert(id);
        }
    }
    let outer = SvgRect {
        x: 0.,
        y: 0.,
        width,
        height,
    };
    let state = State {
        transform: SvgTransform::IDENTITY,
        viewport: (width as f64, height as f64),
        style: Computed::default(),
        clips: vec![Arc::new(Clip {
            paths: vec![(FlatPath::rect(outer), FillRule::NonZero)],
        })],
        ancestors: vec![],
        refs: vec![],
        instance: None,
        depth: 0,
        use_size: None,
    };
    let mut pending = vec![Task::Visit(root, Box::new(state), true)];
    while let Some(task) = pending.pop() {
        match task {
            Task::EndOpacity => builder.scene.commands.push(Command::PopOpacity),
            Task::Visit(id, mut state, referenced) => {
                if builder.budget.diagnostics.truncated {
                    continue;
                }
                if state.depth >= MAX_SVG_DEPTH
                    || builder.budget.diagnostics.nodes >= MAX_SVG_NODES
                    || builder.scene.commands.len() + pending.len() >= MAX_SVG_COMMANDS
                {
                    builder.budget.diagnostics.truncated = true;
                    continue;
                }
                if !builder.budget.work(1) {
                    continue;
                }
                builder.budget.diagnostics.nodes += 1;
                if doc.namespace(id) != Namespace::Svg
                    || !matches!(doc.nodes[id.0].kind, NodeKind::Element { .. })
                {
                    continue;
                }
                let tag = doc.tag(id);
                if matches!(
                    tag,
                    "defs"
                        | "linearGradient"
                        | "radialGradient"
                        | "clipPath"
                        | "stop"
                        | "title"
                        | "desc"
                        | "metadata"
                        | "style"
                        | "script"
                ) || tag == "symbol" && !referenced
                {
                    continue;
                }
                let parent = state.style.clone();
                let style = builder.computed(id, &parent);
                if builder.budget.diagnostics.truncated {
                    continue;
                }
                if !style.display {
                    continue;
                }
                state.style = style.clone();
                state.depth += 1;
                if state.instance.is_none() {
                    state.ancestors.push(id);
                }
                if let Some(raw) = doc.attr(id, "transform") {
                    if let Some(t) = parse_transform(raw, &mut builder.budget) {
                        state.transform = state.transform.multiply(t);
                    } else {
                        builder.budget.diagnostics.invalid_geometry = true;
                        continue;
                    }
                }
                if !state.transform.valid() {
                    builder.budget.diagnostics.invalid_geometry = true;
                    continue;
                }
                if matches!(tag, "svg" | "symbol") {
                    let raw = |name: &str| {
                        style
                            .geometry
                            .get(name)
                            .map(String::as_str)
                            .or_else(|| doc.attr(id, name))
                    };
                    let (w, h) = if id == root {
                        (width as f64, height as f64)
                    } else {
                        state.use_size.take().unwrap_or_else(|| {
                            (
                                raw("width")
                                    .and_then(|s| length(s, state.viewport.0))
                                    .unwrap_or(state.viewport.0),
                                raw("height")
                                    .and_then(|s| length(s, state.viewport.1))
                                    .unwrap_or(state.viewport.1),
                            )
                        })
                    };
                    if w <= 0. || h <= 0. {
                        continue;
                    }
                    let x = if id == root {
                        0.
                    } else {
                        raw("x")
                            .and_then(|s| length(s, state.viewport.0))
                            .unwrap_or(0.)
                    };
                    let y = if id == root {
                        0.
                    } else {
                        raw("y")
                            .and_then(|s| length(s, state.viewport.1))
                            .unwrap_or(0.)
                    };
                    state.transform = state.transform.multiply(SvgTransform::translate(x, y));
                    if id != root && doc.attr(id, "overflow") != Some("visible") {
                        state.clips.push(Arc::new(Clip {
                            paths: vec![(
                                FlatPath::rect(SvgRect {
                                    x: 0.,
                                    y: 0.,
                                    width: w as f32,
                                    height: h as f32,
                                })
                                .transformed(state.transform),
                                FillRule::NonZero,
                            )],
                        }));
                    }
                    let Some((v, units)) = viewport_transform(doc, id, w, h, &mut builder.budget)
                    else {
                        continue;
                    };
                    state.transform = state.transform.multiply(v);
                    if !state.transform.valid() {
                        builder.budget.diagnostics.truncated = true;
                        continue;
                    }
                    state.viewport = units;
                }
                let geometry = builder.geometry(id, state.viewport, state.transform, &style);
                let bbox = geometry.as_ref().and_then(FlatPath::bounds);
                if let Some(bounds) = geometry
                    .as_ref()
                    .and_then(|path| path.transformed(state.transform).bounds())
                {
                    for node in state
                        .ancestors
                        .iter()
                        .copied()
                        .chain(std::iter::once(state.instance.unwrap_or(id)))
                    {
                        builder
                            .scene
                            .geometry_bounds
                            .entry(node)
                            .and_modify(|old| *old = old.union(bounds))
                            .or_insert(bounds);
                    }
                }
                if !style.clip_path.is_empty()
                    && let Some(clip) = builder.clip(&style.clip_path, &state, bbox)
                {
                    state.clips.push(Arc::new(clip));
                }
                let layer = style.opacity < 1.;
                if layer {
                    builder
                        .scene
                        .commands
                        .push(Command::PushOpacity(style.opacity.clamp(0., 1.)));
                    pending.push(Task::EndOpacity);
                }
                if style.visibility
                    && let Some(path) = geometry
                {
                    builder.draw(id, &path, &state);
                }
                if tag == "use" {
                    let raw = |name: &str| {
                        style
                            .geometry
                            .get(name)
                            .map(String::as_str)
                            .or_else(|| doc.attr(id, name))
                    };
                    let href = href(doc, id).unwrap_or("");
                    let target = href
                        .strip_prefix('#')
                        .and_then(|s| builder.ids.get(s))
                        .copied();
                    if let Some(target) = target {
                        if state.refs.contains(&target) {
                            builder.budget.diagnostics.unsupported = true;
                            continue;
                        }
                        state.refs.push(target);
                        state.instance = Some(state.instance.unwrap_or(id));
                        let x = raw("x")
                            .and_then(|s| length(s, state.viewport.0))
                            .unwrap_or(0.);
                        let y = raw("y")
                            .and_then(|s| length(s, state.viewport.1))
                            .unwrap_or(0.);
                        state.transform = state.transform.multiply(SvgTransform::translate(x, y));
                        if matches!(doc.tag(target), "symbol" | "svg") {
                            state.use_size = Some((
                                raw("width")
                                    .and_then(|s| length(s, state.viewport.0))
                                    .or_else(|| {
                                        doc.attr(target, "width")
                                            .and_then(|s| length(s, state.viewport.0))
                                    })
                                    .unwrap_or(state.viewport.0),
                                raw("height")
                                    .and_then(|s| length(s, state.viewport.1))
                                    .or_else(|| {
                                        doc.attr(target, "height")
                                            .and_then(|s| length(s, state.viewport.1))
                                    })
                                    .unwrap_or(state.viewport.1),
                            ));
                        }
                        pending.push(Task::Visit(target, state, true));
                    } else {
                        builder.budget.diagnostics.unsupported = true;
                    }
                } else if matches!(tag, "svg" | "symbol" | "g" | "a" | "switch") {
                    let children = &doc.nodes[id.0].children;
                    let selected = if tag == "switch" {
                        children
                            .iter()
                            .position(|&child| {
                                matches!(doc.nodes[child.0].kind, NodeKind::Element { .. })
                            })
                            .map(|index| &children[index..index + 1])
                            .unwrap_or(&[])
                    } else {
                        children.as_slice()
                    };
                    for &child in selected.iter().rev() {
                        if pending.len() >= MAX_SVG_NODES {
                            builder.budget.diagnostics.truncated = true;
                            break;
                        }
                        pending.push(Task::Visit(child, state.clone(), false));
                    }
                } else if geometry_tag(tag).is_none() {
                    builder.budget.diagnostics.unsupported = true;
                }
            }
        }
    }
    builder.scene.diagnostics = builder.budget.diagnostics;
    builder.scene
}

impl Builder<'_> {
    /// Paint resources inherit through their DOM ancestors, never from the shape
    /// referencing them. Template stop instances inherit from the referencing
    /// gradient below, after this host style has been resolved.
    fn computed_at(&mut self, mut id: NodeId) -> Computed {
        let mut chain = vec![];
        loop {
            if self.doc.namespace(id) != Namespace::Svg {
                break;
            }
            if chain.len() >= MAX_SVG_DEPTH || !self.budget.work(1) {
                self.budget.diagnostics.truncated = true;
                break;
            }
            chain.push(id);
            let Some(parent) = self.doc.nodes[id.0].parent else {
                break;
            };
            id = parent;
        }
        let mut computed = Computed::default();
        for node in chain.into_iter().rev() {
            computed = self.computed(node, &computed);
        }
        computed
    }
    fn computed(&mut self, id: NodeId, parent: &Computed) -> Computed {
        let mut result = parent.child();
        let mut raw = SvgStyle::default();
        if let NodeKind::Element { attrs, .. } = &self.doc.nodes[id.0].kind {
            for (name, value) in attrs {
                if self.budget.work(name.len() + value.len()) {
                    raw.parse_property(name, value);
                    if unsupported_effect(name, value) {
                        self.budget.diagnostics.unsupported = true;
                    }
                }
            }
            if let Some(source) = attrs.get("style") {
                for declaration in source.split(';').take(256) {
                    if let Some((name, value)) = declaration.split_once(':') {
                        if unsupported_effect(name.trim(), value.trim()) {
                            self.budget.diagnostics.unsupported = true;
                        }
                        raw.parse_property(
                            name,
                            value
                                .trim()
                                .strip_suffix("!important")
                                .unwrap_or(value)
                                .trim(),
                        );
                    }
                }
            }
        }
        result.apply(&raw, parent);
        if let Some(style) = self.styles.get(&id) {
            result.apply(style, parent);
        }
        result
    }
    fn geometry(
        &mut self,
        id: NodeId,
        v: (f64, f64),
        t: SvgTransform,
        style: &Computed,
    ) -> Option<FlatPath> {
        let tag = self.doc.tag(id);
        let raw = |name: &str| {
            style
                .geometry
                .get(name)
                .map(String::as_str)
                .or_else(|| self.doc.attr(id, name))
        };
        let get = |name: &str, base: f64, default: f64| {
            raw(name).and_then(|s| length(s, base)).unwrap_or(default)
        };
        let mut writer = Writer::default();
        match tag {
            "path" => Some(path::parse(
                self.doc.attr(id, "d").unwrap_or(""),
                t,
                &mut self.budget,
            )),
            "circle" | "ellipse" => {
                let cx = get("cx", v.0, 0.);
                let cy = get("cy", v.1, 0.);
                let (rx, ry) = if tag == "circle" {
                    let r = get("r", (v.0 * v.0 + v.1 * v.1).sqrt() / 2f64.sqrt(), 0.);
                    (r, r)
                } else {
                    let rx = raw("rx").and_then(|s| length(s, v.0));
                    let ry = raw("ry").and_then(|s| length(s, v.1));
                    (rx.or(ry).unwrap_or(0.), ry.or(rx).unwrap_or(0.))
                };
                if rx < 0. || ry < 0. {
                    self.budget.diagnostics.invalid_geometry = true;
                }
                Some(path::ellipse(cx, cy, rx, ry, t, &mut self.budget))
            }
            "rect" => {
                let (x, y, w, h) = (
                    get("x", v.0, 0.),
                    get("y", v.1, 0.),
                    get("width", v.0, 0.),
                    get("height", v.1, 0.),
                );
                if w <= 0. || h <= 0. {
                    if w < 0. || h < 0. {
                        self.budget.diagnostics.invalid_geometry = true;
                    }
                    return Some(FlatPath::default());
                }
                let rx = raw("rx").and_then(|s| length(s, v.0));
                let ry = raw("ry").and_then(|s| length(s, v.1));
                let rx = rx.or(ry).unwrap_or(0.).max(0.).min(w / 2.);
                let ry = ry.or(Some(rx)).unwrap_or(0.).max(0.).min(h / 2.);
                writer.move_to(Point::new(x + rx, y), &mut self.budget);
                writer.line_to(Point::new(x + w - rx, y), &mut self.budget);
                writer.arc(
                    rx,
                    ry,
                    0.,
                    false,
                    true,
                    Point::new(x + w, y + ry),
                    t,
                    &mut self.budget,
                );
                writer.line_to(Point::new(x + w, y + h - ry), &mut self.budget);
                writer.arc(
                    rx,
                    ry,
                    0.,
                    false,
                    true,
                    Point::new(x + w - rx, y + h),
                    t,
                    &mut self.budget,
                );
                writer.line_to(Point::new(x + rx, y + h), &mut self.budget);
                writer.arc(
                    rx,
                    ry,
                    0.,
                    false,
                    true,
                    Point::new(x, y + h - ry),
                    t,
                    &mut self.budget,
                );
                writer.line_to(Point::new(x, y + ry), &mut self.budget);
                writer.arc(
                    rx,
                    ry,
                    0.,
                    false,
                    true,
                    Point::new(x + rx, y),
                    t,
                    &mut self.budget,
                );
                writer.close();
                Some(writer.path)
            }
            "line" => {
                writer.move_to(
                    Point::new(get("x1", v.0, 0.), get("y1", v.1, 0.)),
                    &mut self.budget,
                );
                writer.line_to(
                    Point::new(get("x2", v.0, 0.), get("y2", v.1, 0.)),
                    &mut self.budget,
                );
                Some(writer.path)
            }
            "polyline" | "polygon" => {
                let raw = self.doc.attr(id, "points").unwrap_or("");
                if !self.budget.work(raw.len()) {
                    return Some(writer.path);
                }
                let mut n = Numbers::new(raw);
                while !n.done() {
                    let Some(x) = n.number(&mut self.budget) else {
                        self.budget.diagnostics.invalid_geometry = true;
                        break;
                    };
                    let Some(y) = n.number(&mut self.budget) else {
                        self.budget.diagnostics.invalid_geometry = true;
                        break;
                    };
                    let okay = if writer.path.contours.is_empty() {
                        writer.move_to(Point::new(x, y), &mut self.budget)
                    } else {
                        writer.line_to(Point::new(x, y), &mut self.budget)
                    };
                    if !okay {
                        break;
                    }
                }
                if tag == "polygon" {
                    writer.close();
                }
                Some(writer.path)
            }
            _ => None,
        }
    }
    fn draw(&mut self, id: NodeId, path: &FlatPath, state: &State) {
        let Some(bbox) = path.bounds() else {
            return;
        };
        let node = state.instance.unwrap_or(id);
        if let Some(paint) = self.paint(&state.style.fill, state, bbox, state.style.fill_opacity) {
            self.command(
                node,
                path.transformed(state.transform),
                state.style.fill_rule,
                paint,
                state,
            );
        }
        if state.style.stroke_width > 0.
            && let Some(paint) =
                self.paint(&state.style.stroke, state, bbox, state.style.stroke_opacity)
        {
            let mut stroke_style = state.style.clone();
            if let Some(calibrated) = self
                .doc
                .attr(id, "pathLength")
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.)
            {
                let actual = path
                    .contours
                    .iter()
                    .map(|c| {
                        c.points
                            .windows(2)
                            .map(|p| p[1].minus(p[0]).length())
                            .sum::<f64>()
                            + if c.closed && c.points.len() > 1 {
                                c.points.last().unwrap().minus(c.points[0]).length()
                            } else {
                                0.
                            }
                    })
                    .sum::<f64>();
                let factor = actual / calibrated;
                for dash in &mut stroke_style.dash {
                    *dash = (*dash as f64 * factor) as f32;
                }
                stroke_style.dash_offset = (stroke_style.dash_offset as f64 * factor) as f32;
                if !stroke_style.dash_offset.is_finite()
                    || stroke_style.dash.iter().any(|value| !value.is_finite())
                {
                    self.budget.diagnostics.truncated = true;
                    return;
                }
            }
            let outlined = stroke(
                path,
                &StrokeStyle {
                    width: stroke_style.stroke_width as f64,
                    cap: stroke_style.linecap,
                    join: stroke_style.linejoin,
                    miter: stroke_style.miter as f64,
                    dash: &stroke_style
                        .dash
                        .iter()
                        .map(|&n| n as f64)
                        .collect::<Vec<_>>(),
                    dash_offset: stroke_style.dash_offset as f64,
                },
                state.transform,
                &mut self.budget,
            );
            self.command(
                node,
                outlined.transformed(state.transform),
                FillRule::NonZero,
                paint,
                state,
            );
        }
    }
    fn command(
        &mut self,
        node: NodeId,
        path: FlatPath,
        rule: FillRule,
        paint: Paint,
        state: &State,
    ) {
        if self.scene.commands.len() >= MAX_SVG_COMMANDS {
            self.budget.diagnostics.truncated = true;
            return;
        }
        let Some(mut bounds) = path.bounds() else {
            return;
        };
        for clip in &state.clips {
            let cb = clip
                .paths
                .iter()
                .filter_map(|(p, _)| p.bounds())
                .reduce(SvgRect::union)
                .unwrap_or_default();
            bounds = bounds.intersect(cb);
        }
        for &id in &state.ancestors {
            self.scene
                .node_bounds
                .entry(id)
                .and_modify(|old| *old = old.union(bounds))
                .or_insert(bounds);
        }
        self.scene
            .node_bounds
            .entry(node)
            .and_modify(|old| *old = old.union(bounds))
            .or_insert(bounds);
        self.scene.commands.push(Command::Paint {
            node,
            path: Arc::new(path),
            rule,
            paint,
            clips: state.clips.clone(),
            bounds,
        });
    }
    fn paint(
        &mut self,
        paint: &SvgPaint,
        state: &State,
        bbox: SvgRect,
        opacity: f32,
    ) -> Option<Paint> {
        match paint {
            SvgPaint::None => None,
            SvgPaint::Color(c) => Some(Paint::Solid(alpha(*c, opacity))),
            SvgPaint::CurrentColor => Some(Paint::Solid(alpha(state.style.color, opacity))),
            SvgPaint::Url { id, fallback } => {
                self.gradient(id, state, bbox, opacity).or_else(|| {
                    fallback
                        .as_ref()
                        .and_then(|p| self.paint(p, state, bbox, opacity))
                })
            }
        }
    }
    fn gradient(&mut self, id: &str, state: &State, bbox: SvgRect, opacity: f32) -> Option<Paint> {
        let mut target = *self.ids.get(id)?;
        let host_style = self.computed_at(target);
        let mut seen = HashSet::new();
        let mut attrs = BTreeMap::new();
        let mut stop_nodes = vec![];
        let mut kind = "";
        for depth in 0..16 {
            if !seen.insert(target) {
                self.budget.diagnostics.unsupported = true;
                return None;
            }
            let tag = self.doc.tag(target);
            if !matches!(tag, "linearGradient" | "radialGradient") {
                return None;
            }
            if kind.is_empty() {
                kind = tag;
            }
            if let NodeKind::Element { attrs: a, .. } = &self.doc.nodes[target.0].kind {
                for (k, v) in a {
                    if !self.budget.work(k.len() + v.len()) {
                        return None;
                    }
                    attrs.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            if stop_nodes.is_empty() {
                stop_nodes = self.doc.nodes[target.0]
                    .children
                    .iter()
                    .copied()
                    .filter(|&n| {
                        self.doc.namespace(n) == Namespace::Svg && self.doc.tag(n) == "stop"
                    })
                    .take(257)
                    .collect();
            }
            let href = href(self.doc, target);
            if href.is_some_and(|value| !value.starts_with('#')) {
                self.budget.diagnostics.unsupported = true;
            }
            let Some(next) = href
                .and_then(|s| s.strip_prefix('#'))
                .and_then(|s| self.ids.get(s))
                .copied()
            else {
                break;
            };
            if depth == 15 {
                self.budget.diagnostics.truncated = true;
                return None;
            }
            target = next;
        }
        if stop_nodes.len() > 256 {
            stop_nodes.truncate(256);
            self.budget.diagnostics.truncated = true;
        }
        let mut stops = vec![];
        let mut previous = 0f64;
        for id in stop_nodes {
            if !self.budget.work(1) {
                break;
            }
            let s = self.computed(id, &host_style);
            let offset = self
                .doc
                .attr(id, "offset")
                .and_then(|v| length(v, 1.))
                .unwrap_or(0.)
                .clamp(previous, 1.);
            previous = offset;
            let color = if s.stop_current_color {
                s.color
            } else {
                s.stop_color
            };
            stops.push(Stop {
                offset,
                color: alpha(color, s.stop_opacity),
            });
        }
        if stops.is_empty() {
            return None;
        }
        if stops.len() == 1 {
            return Some(Paint::Solid(alpha(stops[0].color, opacity)));
        }
        let object = attrs
            .get("gradientUnits")
            .is_none_or(|s| s != "userSpaceOnUse");
        if object && (bbox.width <= 0. || bbox.height <= 0.) {
            return None;
        }
        let units = if object { (1., 1.) } else { state.viewport };
        let base = if object {
            SvgTransform::translate(bbox.x as f64, bbox.y as f64)
                .multiply(SvgTransform::scale(bbox.width as f64, bbox.height as f64))
        } else {
            SvgTransform::IDENTITY
        };
        let transform = if let Some(raw) = attrs.get("gradientTransform") {
            let Some(transform) = parse_transform(raw, &mut self.budget) else {
                self.budget.diagnostics.invalid_geometry = true;
                return None;
            };
            transform
        } else {
            SvgTransform::IDENTITY
        };
        let inverse = state
            .transform
            .multiply(base)
            .multiply(transform)
            .inverse()?;
        let get = |name: &str, axis: f64, default: f64| {
            attrs
                .get(name)
                .and_then(|s| length(s, axis))
                .unwrap_or(default)
        };
        let spread = match attrs.get("spreadMethod").map(String::as_str) {
            Some("repeat") => Spread::Repeat,
            Some("reflect") => Spread::Reflect,
            _ => Spread::Pad,
        };
        let stops = Arc::new(stops);
        if kind == "linearGradient" {
            Some(Paint::Linear {
                inverse,
                from: Point::new(get("x1", units.0, 0.), get("y1", units.1, 0.)),
                to: Point::new(get("x2", units.0, units.0), get("y2", units.1, 0.)),
                stops,
                spread,
                opacity,
            })
        } else {
            if attrs
                .get("fr")
                .is_some_and(|value| length(value, 1.).is_none_or(|value| value != 0.))
            {
                self.budget.diagnostics.unsupported = true;
            }
            let center = Point::new(
                get("cx", units.0, units.0 / 2.),
                get("cy", units.1, units.1 / 2.),
            );
            let radius = get(
                "r",
                (units.0 * units.0 + units.1 * units.1).sqrt() / 2f64.sqrt(),
                if object {
                    0.5
                } else {
                    (units.0 * units.0 + units.1 * units.1).sqrt() / 2f64.sqrt() / 2.
                },
            );
            if radius <= 0. {
                return Some(Paint::Solid(alpha(stops.last()?.color, opacity)));
            }
            let mut focus = Point::new(get("fx", units.0, center.x), get("fy", units.1, center.y));
            let delta = focus.minus(center);
            if delta.length() >= radius {
                self.budget.diagnostics.unsupported = true;
                focus = center.plus(delta.times(radius * (1. - 1e-7) / delta.length()));
            }
            Some(Paint::Radial {
                inverse,
                center,
                focus,
                radius,
                stops,
                spread,
                opacity,
            })
        }
    }
    fn clip(&mut self, id: &str, state: &State, bbox: Option<SvgRect>) -> Option<Clip> {
        let root = *self.ids.get(id)?;
        if self.doc.tag(root) != "clipPath" {
            return None;
        }
        let mut transform = state.transform;
        let mut viewport = state.viewport;
        if self.doc.attr(root, "clipPathUnits") == Some("objectBoundingBox") {
            let Some(b) = bbox else {
                self.budget.diagnostics.unsupported = true;
                return Some(Clip { paths: vec![] });
            };
            transform = transform
                .multiply(SvgTransform::translate(b.x as f64, b.y as f64))
                .multiply(SvgTransform::scale(b.width as f64, b.height as f64));
            viewport = (1., 1.);
        }
        let parent = self.doc.nodes[root.0]
            .parent
            .map(|node| self.computed_at(node))
            .unwrap_or_default();
        let mut pending = vec![(root, transform, parent, 0usize)];
        let mut paths = vec![];
        while let Some((node, mut t, parent, depth)) = pending.pop() {
            if depth >= MAX_SVG_DEPTH || !self.budget.work(1) {
                self.budget.diagnostics.truncated = true;
                break;
            }
            if let Some(raw) = self.doc.attr(node, "transform") {
                let Some(v) = parse_transform(raw, &mut self.budget) else {
                    self.budget.diagnostics.invalid_geometry = true;
                    continue;
                };
                t = t.multiply(v);
            }
            let style = self.computed(node, &parent);
            if !style.clip_path.is_empty() {
                self.budget.diagnostics.unsupported = true;
            }
            if !style.display {
                continue;
            }
            if let Some(p) = self.geometry(node, viewport, t, &style) {
                if style.visibility {
                    paths.push((p.transformed(t), style.clip_rule));
                }
            } else if matches!(self.doc.tag(node), "clipPath" | "g") {
                for &child in self.doc.nodes[node.0].children.iter().rev() {
                    if pending.len() >= MAX_SVG_NODES {
                        self.budget.diagnostics.truncated = true;
                        break;
                    }
                    if self.doc.namespace(child) == Namespace::Svg {
                        pending.push((child, t, style.clone(), depth + 1));
                    }
                }
            } else {
                self.budget.diagnostics.unsupported = true;
            }
        }
        Some(Clip { paths })
    }
}
fn unsupported_effect(name: &str, value: &str) -> bool {
    match name {
        "filter" | "mask" | "marker" | "marker-start" | "marker-mid" | "marker-end"
        | "vector-effect" => value != "none",
        "paint-order" => value != "normal",
        "color-interpolation" => !matches!(value, "auto" | "sRGB"),
        "pointer-events" => !matches!(value, "auto" | "visiblePainted"),
        "requiredExtensions" | "systemLanguage" => true,
        _ => false,
    }
}
fn href(doc: &Document, id: NodeId) -> Option<&str> {
    doc.attr(id, "href").or_else(|| {
        if let NodeKind::Element { attrs, .. } = &doc.nodes[id.0].kind {
            attrs.iter().find_map(|(name, value)| {
                (doc.attribute_namespace(id, name) == Some(AttributeNamespace::XLink)
                    && doc.attribute_local_name(id, name) == "href")
                    .then_some(value.as_str())
            })
        } else {
            None
        }
    })
}
fn geometry_tag(name: &str) -> Option<()> {
    matches!(
        name,
        "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "path"
    )
    .then_some(())
}

fn polygon(out: &mut FlatPath, mut points: Vec<Point>, budget: &mut Budget) {
    if points.len() < 3 || !budget.work(points.len()) {
        return;
    }
    let area = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| a.cross(*b))
        .sum::<f64>();
    if area < 0. {
        points.reverse();
    }
    for _ in &points {
        if !budget.segment() {
            return;
        }
    }
    out.contours.push(Contour {
        points,
        closed: true,
    });
}
fn circle(out: &mut FlatPath, p: Point, r: f64, t: SvgTransform, b: &mut Budget) {
    let flat = path::ellipse(p.x, p.y, r, r, t, b);
    out.contours.extend(flat.contours);
}
pub(crate) struct StrokeStyle<'a> {
    pub width: f64,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter: f64,
    pub dash: &'a [f64],
    pub dash_offset: f64,
}

pub(crate) fn stroke(
    path: &FlatPath,
    s: &StrokeStyle<'_>,
    t: SvgTransform,
    b: &mut Budget,
) -> FlatPath {
    let source = if s.dash.is_empty() || s.dash.iter().all(|&n| n == 0.) {
        path.clone()
    } else {
        dashed(path, s.dash, s.dash_offset, b)
    };
    let mut out = FlatPath::default();
    let r = s.width / 2.;
    for c in &source.contours {
        if b.diagnostics.truncated {
            break;
        }
        if c.points.len() < 2 {
            continue;
        }
        let mut segments = vec![];
        for pair in c.points.windows(2) {
            let d = pair[1].minus(pair[0]);
            if d.length() > 1e-12 {
                segments.push((pair[0], pair[1], d.times(1. / d.length())));
            }
        }
        if c.closed {
            let a = *c.points.last().unwrap();
            let end = c.points[0];
            let d = end.minus(a);
            if d.length() > 1e-12 {
                segments.push((a, end, d.times(1. / d.length())));
            }
        }
        if segments.is_empty() {
            match s.cap {
                LineCap::Round => circle(&mut out, c.points[0], r, t, b),
                LineCap::Square => polygon(
                    &mut out,
                    FlatPath::rect(SvgRect {
                        x: (c.points[0].x - r) as f32,
                        y: (c.points[0].y - r) as f32,
                        width: (2. * r) as f32,
                        height: (2. * r) as f32,
                    })
                    .contours
                    .remove(0)
                    .points,
                    b,
                ),
                LineCap::Butt => {}
            }
            continue;
        }
        for (i, &(a, end, d)) in segments.iter().enumerate() {
            if b.diagnostics.truncated {
                break;
            }
            let n = Point::new(-d.y * r, d.x * r);
            let mut a = a;
            let mut end = end;
            if !c.closed && s.cap == LineCap::Square {
                if i == 0 {
                    a = a.minus(d.times(r));
                }
                if i + 1 == segments.len() {
                    end = end.plus(d.times(r));
                }
            }
            polygon(
                &mut out,
                vec![a.plus(n), end.plus(n), end.minus(n), a.minus(n)],
                b,
            );
            if !c.closed && s.cap == LineCap::Round {
                if i == 0 {
                    circle(&mut out, a, r, t, b);
                }
                if i + 1 == segments.len() {
                    circle(&mut out, end, r, t, b);
                }
            }
            if i + 1 < segments.len() || c.closed {
                let next = segments[(i + 1) % segments.len()].2;
                let turn = d.cross(next);
                if turn.abs() < 1e-12 {
                    continue;
                }
                if s.join == LineJoin::Round {
                    circle(&mut out, end, r, t, b);
                    continue;
                }
                let side = if turn > 0. { -1. } else { 1. };
                let a = end.plus(Point::new(-d.y, d.x).times(r * side));
                let z = end.plus(Point::new(-next.y, next.x).times(r * side));
                let m = a.plus(d.times(z.minus(a).cross(next) / turn));
                if s.join == LineJoin::Miter && m.minus(end).length() <= r * s.miter {
                    polygon(&mut out, vec![a, m, z, end], b);
                } else {
                    polygon(&mut out, vec![a, z, end], b);
                }
            }
        }
    }
    out
}
fn dashed(path: &FlatPath, pattern: &[f64], offset: f64, b: &mut Budget) -> FlatPath {
    let mut pattern = pattern.to_vec();
    if pattern.len() % 2 == 1 {
        let copy = pattern.clone();
        pattern.extend(copy);
    }
    let total = pattern.iter().sum::<f64>();
    if total <= 0. {
        return path.clone();
    }
    let mut out = FlatPath::default();
    for c in &path.contours {
        let first_output = out.contours.len();
        let mut index = 0;
        let mut phase = offset.rem_euclid(total);
        while phase > 0. && phase >= pattern[index] {
            phase -= pattern[index];
            index = (index + 1) % pattern.len();
            if !b.work(1) {
                return out;
            }
        }
        let mut remaining = pattern[index] - phase;
        let mut current: Option<Contour> = None;
        let mut pairs = c
            .points
            .windows(2)
            .map(|p| (p[0], p[1]))
            .collect::<Vec<_>>();
        if c.closed && c.points.len() > 1 {
            pairs.push((*c.points.last().unwrap(), c.points[0]));
        }
        for (a, z) in pairs {
            let delta = z.minus(a);
            let length = delta.length();
            if length <= 1e-12 {
                continue;
            }
            let direction = delta.times(1. / length);
            let mut done = 0.;
            loop {
                if !b.work(1) {
                    return out;
                }
                if remaining <= 1e-12 {
                    if let Some(c) = current.take() {
                        out.contours.push(c);
                    }
                    if index % 2 == 0 && pattern[index] == 0. {
                        let p = a.plus(direction.times(done));
                        if !b.segment() || !b.segment() {
                            return out;
                        }
                        // A zero-length dash is observable with round/square
                        // caps; retaining two equal points lets stroke() draw it.
                        out.contours.push(Contour {
                            points: vec![p, p],
                            closed: false,
                        });
                    }
                    index = (index + 1) % pattern.len();
                    remaining = pattern[index];
                    continue;
                }
                if done >= length - 1e-12 {
                    break;
                }
                let take = remaining.min(length - done);
                if index % 2 == 0 {
                    let p = a.plus(direction.times(done));
                    let end = a.plus(direction.times(done + take));
                    let item = current.get_or_insert_with(|| Contour {
                        points: vec![p],
                        closed: false,
                    });
                    if !b.segment() {
                        return out;
                    }
                    item.points.push(end);
                }
                done += take;
                remaining -= take;
            }
        }
        if let Some(c) = current {
            out.contours.push(c);
        }
        if c.closed && out.contours.len() > first_output {
            let start = c.points.first().copied();
            let first = &out.contours[first_output];
            let last = out.contours.last().unwrap();
            if first.points.first().copied() == start && last.points.last().copied() == start {
                if out.contours.len() == first_output + 1 {
                    let contour = &mut out.contours[first_output];
                    contour.closed = true;
                    contour.points.pop();
                } else {
                    let mut tail = out.contours.pop().unwrap();
                    let head = std::mem::take(&mut out.contours[first_output]);
                    tail.points.extend(head.points.into_iter().skip(1));
                    out.contours[first_output] = tail;
                }
            }
        }
    }
    out
}
