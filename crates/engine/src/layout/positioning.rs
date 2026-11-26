use super::*;
use crate::{
    css::{Position, TransformOp},
    dom::Document,
};
use std::{
    collections::HashSet,
    ops::{Deref, DerefMut},
};

mod sticky;

#[derive(Clone, Copy, Debug)]
pub(super) struct CommandMeta {
    node: NodeId,
    background: bool,
}
#[derive(Clone)]
pub(super) struct TrackedCommands {
    items: Vec<DrawCommand>,
    meta: Vec<CommandMeta>,
    pub owner: NodeId,
    pub background: bool,
}
impl Default for TrackedCommands {
    fn default() -> Self {
        Self {
            items: vec![],
            meta: vec![],
            owner: NodeId(0),
            background: false,
        }
    }
}
impl TrackedCommands {
    pub fn push(&mut self, command: DrawCommand) {
        self.meta.push(CommandMeta {
            node: self.owner,
            background: self.background,
        });
        self.items.push(command);
    }
    pub fn push_object(&mut self, command: DrawCommand, area: Rect, object: Rect) {
        let clip = object.x < area.x
            || object.y < area.y
            || object.x + object.width > area.x + area.width
            || object.y + object.height > area.y + area.height;
        if clip {
            self.push(DrawCommand::PushState {
                transform: Affine2D::IDENTITY,
                clip: Some(area),
            });
        }
        self.push(command);
        if clip {
            self.push(DrawCommand::PopState);
        }
    }
    pub fn extend(&mut self, other: Self) {
        self.items.extend(other.items);
        self.meta.extend(other.meta);
    }
}
impl Deref for TrackedCommands {
    type Target = Vec<DrawCommand>;
    fn deref(&self) -> &Self::Target {
        &self.items
    }
}
impl DerefMut for TrackedCommands {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.items
    }
}
impl<'a> IntoIterator for &'a mut TrackedCommands {
    type Item = &'a mut DrawCommand;
    type IntoIter = std::slice::IterMut<'a, DrawCommand>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter_mut()
    }
}

#[derive(Clone, Debug)]
pub(super) struct Positioned {
    pub node: NodeId,
    pub x: f32,
    pub y: f32,
    pub href: Option<Arc<str>>,
}
#[derive(Clone, Default)]
pub(super) struct LinkCache {
    values: HashMap<NodeId, Option<Arc<str>>>,
    bytes: usize,
    limited: bool,
}
impl Builder<'_> {
    pub(super) fn resolve_href(&self, node: NodeId) -> Option<Arc<str>> {
        let mut cache = self.link_cache.borrow_mut();
        if let Some(value) = cache.values.get(&node) {
            return value.clone();
        }
        let doc = &self.page.document;
        let raw = doc.attr(node, "href").or_else(|| {
            (doc.namespace(node) == crate::dom::Namespace::Svg)
                .then(|| doc.attr_ns(node, crate::dom::AttributeNamespace::XLink, "href"))
                .flatten()
        })?;
        let value = if raw.len() > 65536 || self.page.base.as_str().len() > 65536 {
            cache.limited = true;
            None
        } else {
            self.page.resolve(raw).and_then(|url| {
                if url.len() > 65536 || cache.bytes.saturating_add(url.len()) > 8 * 1024 * 1024 {
                    cache.limited = true;
                    None
                } else {
                    cache.bytes += url.len();
                    Some(Arc::from(url))
                }
            })
        };
        cache.values.insert(node, value.clone());
        value
    }
}
#[derive(Clone, Debug)]
pub(super) struct ClipRegion {
    pub(super) rect: Rect,
    pub(super) matrix: Affine2D,
    pub(super) parent: Option<usize>,
}
#[derive(Clone, Debug)]
pub(super) struct HitRegion {
    pub node: NodeId,
    rect: Rect,
    pub matrix: Affine2D,
    pub clip: Option<usize>,
    rank: usize,
    svg: Option<SvgHit>,
}
#[derive(Clone, Debug)]
struct SvgHit {
    root_visible: bool,
    rect: Rect,
    scene: Arc<crate::svg::SvgScene>,
    matrix: Affine2D,
    clip: Option<usize>,
}
impl HitRegion {
    pub fn target(&self, clips: &[ClipRegion], x: f32, y: f32) -> Option<NodeId> {
        let inside = self.matrix.inverse().is_some_and(|m| {
            let (x, y) = m.point(x, y);
            self.rect.contains(x, y)
        }) && inside_clips(clips, self.clip, x, y);
        if !inside {
            return None;
        }
        if let Some(svg) = &self.svg
            && svg.rect.width > 0.
            && svg.rect.height > 0.
            && inside_clips(clips, svg.clip, x, y)
            && let Some(inverse) = svg.matrix.inverse()
        {
            let (x, y) = inverse.point(x, y);
            if svg.rect.contains(x, y)
                && let Some(node) = svg.scene.hit_test(
                    (x - svg.rect.x) * svg.scene.width / svg.rect.width,
                    (y - svg.rect.y) * svg.scene.height / svg.rect.height,
                )
            {
                return Some(node);
            }
        }
        self.svg
            .as_ref()
            .is_none_or(|svg| svg.root_visible)
            .then_some(self.node)
    }
}
#[derive(Clone, Debug)]
pub(super) struct ControlRegion {
    pub node: NodeId,
    pub matrix: Affine2D,
    pub clip: Option<usize>,
    pub command: DrawCommand,
}
pub(super) fn inside_clips(clips: &[ClipRegion], mut clip: Option<usize>, x: f32, y: f32) -> bool {
    let mut budget = 256;
    while let Some(index) = clip {
        if budget == 0 {
            return false;
        }
        budget -= 1;
        let c = &clips[index];
        if !c.matrix.inverse().is_some_and(|m| {
            let (x, y) = m.point(x, y);
            c.rect.contains(x, y)
        }) {
            return false;
        }
        clip = c.parent;
    }
    true
}

/// Column-vector affine matrix: (a*x+c*y+e, b*x+d*y+f).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2D {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}
impl Default for Affine2D {
    fn default() -> Self {
        Self::IDENTITY
    }
}
impl Affine2D {
    pub const IDENTITY: Self = Self {
        a: 1.,
        b: 0.,
        c: 0.,
        d: 1.,
        e: 0.,
        f: 0.,
    };
    pub fn translation(x: f32, y: f32) -> Self {
        Self {
            e: x,
            f: y,
            ..Self::IDENTITY
        }
    }
    pub fn multiply(self, rhs: Self) -> Self {
        Self {
            a: self.a * rhs.a + self.c * rhs.b,
            b: self.b * rhs.a + self.d * rhs.b,
            c: self.a * rhs.c + self.c * rhs.d,
            d: self.b * rhs.c + self.d * rhs.d,
            e: self.a * rhs.e + self.c * rhs.f + self.e,
            f: self.b * rhs.e + self.d * rhs.f + self.f,
        }
    }
    pub fn point(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
    pub fn rect(self, r: Rect) -> Rect {
        let p = [
            self.point(r.x, r.y),
            self.point(r.x + r.width, r.y),
            self.point(r.x, r.y + r.height),
            self.point(r.x + r.width, r.y + r.height),
        ];
        let x = p.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
        let y = p.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
        let right = p.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
        let bottom = p.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
        Rect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }
    pub fn inverse(self) -> Option<Self> {
        let det = self.a * self.d - self.b * self.c;
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let v = Self {
            a: self.d / det,
            b: -self.b / det,
            c: -self.c / det,
            d: self.a / det,
            e: (self.c * self.f - self.d * self.e) / det,
            f: (self.b * self.e - self.a * self.f) / det,
        };
        [v.a, v.b, v.c, v.d, v.e, v.f]
            .iter()
            .all(|v| v.is_finite())
            .then_some(v)
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    pub scroll_x: f32,
    pub scroll_y: f32,
}
impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 800.,
            height: 600.,
            scroll_x: 0.,
            scroll_y: 0.,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollOffset {
    pub x: f32,
    pub y: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ScrollRegion {
    pub node: NodeId,
    pub rect: Rect,
    pub content_width: f32,
    pub content_height: f32,
    pub offset: ScrollOffset,
    pub min_x: f32,
    pub max_x: f32,
    pub min_y: f32,
    pub max_y: f32,
}

impl ScrollRegion {
    pub fn can_scroll(&self) -> bool {
        self.max_x > self.min_x || self.max_y > self.min_y
    }
}

// In horizontal writing, flex-direction reverses the main overflow direction,
// flex-wrap reverses the cross direction and RTL reverses the inline axis.
fn reversed_flex_axes(style: &Style) -> (bool, bool) {
    if !matches!(style.display, Display::Flex | Display::InlineFlex) {
        return (false, false);
    }
    let wrap = style.flex_wrap == FlexWrap::WrapReverse;
    match style.flex_direction {
        FlexDirection::Row => (style.rtl, wrap),
        FlexDirection::RowReverse => (!style.rtl, wrap),
        FlexDirection::Column => (style.rtl ^ wrap, false),
        FlexDirection::ColumnReverse => (style.rtl ^ wrap, true),
    }
}

fn containing_blocks(builder: &Builder<'_>) -> (Vec<Option<NodeId>>, Vec<Option<NodeId>>) {
    let doc = &builder.page.document;
    let mut abs = vec![None; doc.nodes.len()];
    let mut fixed = abs.clone();
    for node in doc.connected_nodes() {
        if let Some(parent) = doc.nodes[node.0].parent {
            let style = &builder.styles[parent.0];
            let transformed = !style.transform.is_empty()
                && (style.display != Display::Inline || builder.is_replaced_image(parent));
            abs[node.0] = if style.position != Position::Static || transformed {
                Some(parent)
            } else {
                abs[parent.0]
            };
            fixed[node.0] = if transformed {
                Some(parent)
            } else {
                fixed[parent.0]
            };
        }
    }
    (abs, fixed)
}
fn padding_box(rect: Rect, style: &Style) -> Rect {
    let b = style.borders();
    Rect {
        x: rect.x + b[3],
        y: rect.y + b[0],
        width: (rect.width - b[1] - b[3]).max(0.),
        height: (rect.height - b[0] - b[2]).max(0.),
    }
}

fn viewport_overflow_source(doc: &Document, styles: &ComputedStyles) -> Option<NodeId> {
    use crate::dom::Namespace;

    let html_element = |node, tag| doc.namespace(node) == Namespace::Html && doc.tag(node) == tag;
    let html = doc.nodes[doc.root.0]
        .children
        .iter()
        .copied()
        .find(|&node| html_element(node, "html") && styles[node.0].display != Display::None)?;
    let source =
        if styles[html.0].overflow == [Overflow::Visible; 2] {
            doc.nodes[html.0].children.iter().copied().find(|&node| {
                html_element(node, "body") && styles[node.0].display != Display::None
            })?
        } else {
            html
        };
    (styles[source.0].overflow != [Overflow::Visible; 2]).then_some(source)
}

fn layout_overflow(style: &Style, node: NodeId, viewport_source: Option<NodeId>) -> [Overflow; 2] {
    if viewport_source == Some(node) {
        [Overflow::Visible; 2]
    } else {
        style.overflow
    }
}

enum PositionAxis {
    Inline,
    Block,
}

fn axis_position(
    axis: PositionAxis,
    [start, end]: [Option<f32>; 2],
    extent: f32,
    size: f32,
    margins: [f32; 2],
    auto: [bool; 2],
    static_pos: f32,
) -> f32 {
    if let Some(start) = start {
        let free = end
            .map(|end| extent - start - end - size - margins.iter().sum::<f32>())
            .unwrap_or(0.);
        start
            + margins[0]
            + if end.is_some() && auto[0] {
                if auto[1] {
                    // Only the inline axis assigns negative space to the end margin.
                    match axis {
                        PositionAxis::Inline => free.max(0.) / 2.,
                        PositionAxis::Block => free / 2.,
                    }
                } else {
                    free
                }
            } else {
                0.
            }
    } else if let Some(end) = end {
        extent - end - size - margins[1]
    } else {
        static_pos + margins[0]
    }
}
impl Builder<'_> {
    pub(super) fn queue_positioned_children(
        &mut self,
        id: NodeId,
        x: f32,
        y: f32,
        href: Option<Arc<str>>,
    ) {
        for &node in &self.page.document.nodes[id.0].children {
            if matches!(
                self.page.document.nodes[node.0].kind,
                NodeKind::Element { .. }
            ) && self.styles[node.0].display != Display::None
                && self.styles[node.0].position.out_of_flow()
            {
                self.positioned.push(Positioned {
                    node,
                    x,
                    y,
                    href: href.clone(),
                });
            }
        }
    }
    pub(super) fn layout_positioned(&mut self) {
        if self.positioned.is_empty() {
            return;
        }
        let (abs, fixed) = containing_blocks(self);
        let mut boxes = vec![None; self.styles.len()];
        for e in &self.elements {
            boxes[e.node.0] = Some(boxes[e.node.0].map_or(e.rect, |r: Rect| r.union(e.rect)));
        }
        let mut queue = std::mem::take(&mut self.positioned);
        let mut seen = HashSet::new();
        let mut cursor = 0;
        while cursor < queue.len() {
            if self.work_remaining == 0 {
                self.limited = true;
                break;
            }
            self.work_remaining -= 1;
            let item = queue[cursor].clone();
            cursor += 1;
            if !seen.insert(item.node) {
                continue;
            }
            let id = item.node;
            let s = &self.styles[id.0];
            let cb_id = if s.position == Position::Fixed {
                fixed[id.0]
            } else {
                abs[id.0]
            };
            let cb = cb_id
                .and_then(|id| boxes[id.0].map(|r| padding_box(r, &self.styles[id.0])))
                .unwrap_or(Rect {
                    x: if s.position == Position::Fixed {
                        self.viewport.scroll_x
                    } else {
                        0.
                    },
                    y: if s.position == Position::Fixed {
                        self.viewport.scroll_y
                    } else {
                        0.
                    },
                    width: self.viewport.width,
                    height: self.viewport.height,
                });
            let m = BoxMetrics::new(
                s,
                cb.width,
                Constraints {
                    flex_item: true,
                    ..Constraints::default()
                },
            );
            let inset = [
                s.inset[0].resolve(cb.height, s.font_size),
                s.inset[1].resolve(cb.width, s.font_size),
                s.inset[2].resolve(cb.height, s.font_size),
                s.inset[3].resolve(cb.width, s.font_size),
            ];
            let to_border = |n: f32, extra: f32| {
                if s.box_sizing == BoxSizing::BorderBox {
                    n.max(extra)
                } else {
                    n + extra
                }
            };
            let mut width = s
                .width
                .resolve(cb.width, s.font_size)
                .map(|n| to_border(n, m.extra_x()));
            if s.width == Length::FitContent {
                let (min, max) = self.intrinsic_width(id);
                let available = cb.width - inset[3].unwrap_or(0.) - inset[1].unwrap_or(0.);
                width = Some(max.min(available - m.margin[1] - m.margin[3]).max(min));
            } else if width.is_none() {
                width = if let (Some(left), Some(right)) = (inset[3], inset[1]) {
                    Some((cb.width - left - right - m.margin[1] - m.margin[3]).max(m.extra_x()))
                } else {
                    let (min, max) = self.intrinsic_width(id);
                    Some(
                        max.min(
                            (cb.width
                                - inset[3].or(inset[1]).unwrap_or(0.)
                                - m.margin[1]
                                - m.margin[3])
                                .max(min),
                        ),
                    )
                };
            }
            let mut width = width.unwrap_or(0.);
            if let Some(max) = s.max_width.resolve(cb.width, s.font_size) {
                width = width.min(to_border(max, m.extra_x()));
            }
            if let Some(min) = s.min_width.resolve(cb.width, s.font_size) {
                width = width.max(to_border(min, m.extra_x()));
            }
            let mut height = s
                .height
                .resolve(cb.height, s.font_size)
                .map(|n| to_border(n, m.extra_y()));
            if height.is_none()
                && let (Some(top), Some(bottom)) = (inset[0], inset[2])
            {
                height =
                    Some((cb.height - top - bottom - m.margin[0] - m.margin[2]).max(m.extra_y()));
            }
            if let Some(h) = &mut height {
                if let Some(max) = s.max_height.resolve(cb.height, s.font_size) {
                    *h = h.min(to_border(max, m.extra_y()));
                }
                if let Some(min) = s.min_height.resolve(cb.height, s.font_size) {
                    *h = h.max(to_border(min, m.extra_y()));
                }
            }
            let fragment = self.fragment(
                id,
                cb.width,
                item.href,
                Constraints {
                    width: Some(width),
                    height,
                    containing_height: Some(cb.height),
                    flex_item: true,
                    ..Constraints::default()
                },
            );
            let local = fragment
                .elements
                .iter()
                .find(|e| e.node == id)
                .map(|e| e.rect)
                .unwrap_or(Rect {
                    width,
                    height: fragment.height,
                    ..Rect::default()
                });
            let x = cb.x
                + axis_position(
                    PositionAxis::Inline,
                    [inset[3], inset[1]],
                    cb.width,
                    local.width,
                    [m.margin[3], m.margin[1]],
                    [s.margin[3] == Length::Auto, s.margin[1] == Length::Auto],
                    item.x - cb.x,
                );
            let y = cb.y
                + axis_position(
                    PositionAxis::Block,
                    [inset[0], inset[2]],
                    cb.height,
                    local.height,
                    [m.margin[0], m.margin[2]],
                    [s.margin[0] == Length::Auto, s.margin[2] == Length::Auto],
                    item.y - cb.y,
                );
            let first = self.elements.len();
            self.append_fragment(fragment, x - local.x, y - local.y);
            for e in &self.elements[first..] {
                boxes[e.node.0] = Some(boxes[e.node.0].map_or(e.rect, |r: Rect| r.union(e.rect)));
            }
            queue.append(&mut self.positioned);
        }
    }
}

fn relative_transform(style: &Style, parent: Rect, parent_definite: bool) -> Affine2D {
    let mut matrix = Affine2D::IDENTITY;
    if style.position == Position::Relative {
        let x = style.inset[3]
            .resolve(parent.width, style.font_size)
            .or_else(|| {
                style.inset[1]
                    .resolve(parent.width, style.font_size)
                    .map(|n| -n)
            })
            .unwrap_or(0.);
        let vertical = |v: Length| {
            v.resolve_with_basis(parent_definite.then_some(parent.height), style.font_size)
        };
        let y = vertical(style.inset[0])
            .or_else(|| vertical(style.inset[2]).map(|n| -n))
            .unwrap_or(0.);
        matrix = Affine2D::translation(x, y);
    }
    matrix
}
fn local_transform(
    style: &Style,
    rect: Rect,
    parent: Rect,
    parent_definite: bool,
    transformable: bool,
) -> Affine2D {
    let mut matrix = relative_transform(style, parent, parent_definite);
    if !style.transform.is_empty() && transformable {
        let x = rect.x
            + style.transform_origin[0]
                .resolve(rect.width, style.font_size)
                .unwrap_or(rect.width / 2.);
        let y = rect.y
            + style.transform_origin[1]
                .resolve(rect.height, style.font_size)
                .unwrap_or(rect.height / 2.);
        matrix = matrix.multiply(Affine2D::translation(x, y));
        for op in style.transform.iter() {
            let op = match *op {
                TransformOp::Matrix([a, b, c, d, e, f]) => Affine2D { a, b, c, d, e, f },
                TransformOp::Translate(v) => Affine2D::translation(
                    v[0].resolve(rect.width, style.font_size).unwrap_or(0.),
                    v[1].resolve(rect.height, style.font_size).unwrap_or(0.),
                ),
                TransformOp::Scale([a, d]) => Affine2D {
                    a,
                    d,
                    ..Affine2D::IDENTITY
                },
                TransformOp::Rotate(angle) => {
                    let (sin, cos) = angle.sin_cos();
                    Affine2D {
                        a: cos,
                        b: sin,
                        c: -sin,
                        d: cos,
                        ..Affine2D::IDENTITY
                    }
                }
                TransformOp::Skew([x, y]) => Affine2D {
                    b: y.tan(),
                    c: x.tan(),
                    ..Affine2D::IDENTITY
                },
            };
            matrix = matrix.multiply(op);
        }
        matrix = matrix.multiply(Affine2D::translation(-x, -y));
    }
    matrix
}

#[derive(Clone, Copy)]
struct Visual {
    matrix: Affine2D,
    content: Affine2D,
    clip: Option<usize>,
    content_clip: Option<usize>,
}
impl Default for Visual {
    fn default() -> Self {
        Self {
            matrix: Affine2D::IDENTITY,
            content: Affine2D::IDENTITY,
            clip: None,
            content_clip: None,
        }
    }
}
#[derive(Clone, Copy)]
enum PaintItem {
    Command(usize),
    Group(usize),
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PaintPhase {
    OwnDecoration,
    Negative,
    BlockDecoration,
    Float,
    Inline,
    Positioned,
    Positive,
}
#[derive(Clone, Copy)]
struct PaintEntry {
    phase: PaintPhase,
    z: i32,
    order: usize,
    minor: usize,
    item: PaintItem,
}
#[derive(Default)]
struct PaintGroup {
    entries: Vec<PaintEntry>,
    owner: Option<NodeId>,
    has_float: bool,
    inline_parent: Option<usize>,
}

fn push_paint_group(
    groups: &mut Vec<PaintGroup>,
    parent: usize,
    owner: NodeId,
    phase: PaintPhase,
    z: i32,
    order: usize,
) -> usize {
    let group = groups.len();
    let inline_parent = (phase == PaintPhase::Inline).then_some(parent);
    groups.push(PaintGroup {
        owner: Some(owner),
        inline_parent,
        ..PaintGroup::default()
    });
    if inline_parent.is_none() {
        groups[parent].has_float |= phase == PaintPhase::Float;
        groups[parent].entries.push(PaintEntry {
            phase,
            z,
            order,
            minor: 0,
            item: PaintItem::Group(group),
        });
    }
    group
}

impl Builder<'_> {
    pub(super) fn finish_visual(
        mut self,
        background: Color,
        scroll: &HashMap<NodeId, ScrollOffset>,
        observe_intersections: bool,
    ) -> Layout {
        let doc = &self.page.document;
        let count = doc.nodes.len();
        let order: Vec<_> = doc.connected_nodes().collect();
        let viewport_source = viewport_overflow_source(doc, self.styles);
        let viewport_overflow = viewport_source.map_or([Overflow::Auto; 2], |node| {
            self.styles[node.0].overflow.map(|value| match value {
                Overflow::Visible => Overflow::Auto,
                Overflow::Clip => Overflow::Hidden,
                value => value,
            })
        });
        let mut boxes = vec![None; count];
        for e in &self.elements {
            boxes[e.node.0] = Some(boxes[e.node.0].map_or(e.rect, |r: Rect| r.union(e.rect)));
        }
        let (abs, fixed) = containing_blocks(&self);
        let mut parents = vec![None; count];
        let mut locals = vec![Affine2D::IDENTITY; count];
        let mut initial = vec![Affine2D::IDENTITY; count];
        let mut offset_matrices = vec![Affine2D::IDENTITY; count];
        let viewport = Rect {
            width: self.viewport.width,
            height: self.viewport.height,
            ..Rect::default()
        };
        for &id in &order {
            let node = &doc.nodes[id.0];
            let is_element = matches!(node.kind, NodeKind::Element { .. });
            let style = &self.styles[id.0];
            let parent = if is_element && style.position == Position::Fixed {
                fixed[id.0]
            } else if is_element && style.position == Position::Absolute {
                abs[id.0].or(Some(doc.root))
            } else {
                node.parent
            };
            parents[id.0] = parent;
            let parent_rect = node.parent.and_then(|p| boxes[p.0]).unwrap_or(viewport);
            let parent_rect = if let Some(p) = node.parent {
                let ps = &self.styles[p.0];
                let b = ps.borders();
                let pad = ps
                    .padding
                    .map(|n| n.resolve(parent_rect.width, ps.font_size).unwrap_or(0.));
                Rect {
                    x: parent_rect.x + b[3] + pad[3],
                    y: parent_rect.y + b[0] + pad[0],
                    width: (parent_rect.width - b[1] - b[3] - pad[1] - pad[3]).max(0.),
                    height: (parent_rect.height - b[0] - b[2] - pad[0] - pad[2]).max(0.),
                }
            } else {
                parent_rect
            };
            let local = if is_element {
                local_transform(
                    style,
                    boxes[id.0].unwrap_or_default(),
                    parent_rect,
                    node.parent.is_none_or(|p| {
                        self.styles[p.0].height != Length::Auto || doc.tag(p) == "html"
                    }),
                    style.display != Display::Inline || self.is_replaced_image(id),
                )
            } else {
                Affine2D::IDENTITY
            };
            locals[id.0] = local;
            let relative = if is_element {
                relative_transform(
                    style,
                    parent_rect,
                    node.parent.is_none_or(|p| {
                        self.styles[p.0].height != Length::Auto || doc.tag(p) == "html"
                    }),
                )
            } else {
                Affine2D::IDENTITY
            };
            offset_matrices[id.0] = relative;
            initial[id.0] = parent
                .map_or(Affine2D::IDENTITY, |p| initial[p.0])
                .multiply(local);
        }
        // Overflow propagation follows the actual containing-block parent of
        // positioned boxes. Viewport-fixed descendants never enlarge the page.
        let mut bounds = vec![None; count];
        for &id in &order {
            if let Some(rect) = boxes[id.0] {
                let style = &self.styles[id.0];
                let area = padding_box(rect, style);
                let base = doc.nodes[id.0]
                    .parent
                    .and_then(|p| boxes[p.0])
                    .map_or(viewport.width, |r| r.width);
                let p = style
                    .padding
                    .map(|n| n.resolve(base, style.font_size).unwrap_or(0.));
                bounds[id.0] = Some(Rect {
                    x: area.x + p[3],
                    y: area.y + p[0],
                    width: (area.width - p[1] - p[3]).max(0.),
                    height: (area.height - p[0] - p[2]).max(0.),
                });
            }
        }
        let sticky_content_boxes = order
            .iter()
            .any(|id| boxes[id.0].is_some() && sticky::has_insets(&self.styles[id.0]))
            .then(|| bounds.clone());
        for &id in order.iter().rev() {
            if let Some(parent) = parents[id.0]
                && let Some(rect) = bounds[id.0]
            {
                let style = &self.styles[id.0];
                let overflow = layout_overflow(style, id, viewport_source);
                let own = boxes[id.0].unwrap_or(rect);
                let rect = rect.union(own);
                let rect = Rect {
                    x: if overflow[0].clips() { own.x } else { rect.x },
                    y: if overflow[1].clips() { own.y } else { rect.y },
                    width: if overflow[0].clips() {
                        own.width
                    } else {
                        rect.width
                    },
                    height: if overflow[1].clips() {
                        own.height
                    } else {
                        rect.height
                    },
                };
                let relative = initial[parent.0]
                    .inverse()
                    .map(|m| m.multiply(initial[id.0]))
                    .unwrap_or(Affine2D::IDENTITY);
                let rect = relative.rect(rect);
                bounds[parent.0] = Some(bounds[parent.0].map_or(rect, |r| r.union(rect)));
            }
        }
        let mut offsets = vec![ScrollOffset::default(); count];
        let mut scroll_sizes = vec![None; count];
        let mut scroll_regions = vec![];
        for &id in &order {
            let style = &self.styles[id.0];
            let Some(rect) = boxes[id.0] else {
                continue;
            };
            let area = padding_box(rect, style);
            let bound = bounds[id.0].unwrap_or(rect);
            let pad = style.padding.map(|n| {
                n.resolve(
                    doc.nodes[id.0]
                        .parent
                        .and_then(|p| boxes[p.0])
                        .map_or(viewport.width, |r| r.width),
                    style.font_size,
                )
                .unwrap_or(0.)
            });
            let (reverse_x, reverse_y) = reversed_flex_axes(style);
            let content_width = if reverse_x {
                area.x + area.width - bound.x + pad[3]
            } else {
                bound.x + bound.width + pad[1] - area.x
            }
            .max(area.width);
            let content_height = if reverse_y {
                area.y + area.height - bound.y + pad[0]
            } else {
                bound.y + bound.height + pad[2] - area.y
            }
            .max(area.height);
            scroll_sizes[id.0] = Some((content_width, content_height));
            let overflow = layout_overflow(style, id, viewport_source);
            if overflow.iter().all(|v| !v.clips()) {
                continue;
            }
            let range = |axis: Overflow, extent: f32, reverse: bool| {
                if !axis.scrollable() {
                    (0., 0.)
                } else if reverse {
                    (-extent, 0.)
                } else {
                    (0., extent)
                }
            };
            let (min_x, max_x) = range(overflow[0], content_width - area.width, reverse_x);
            let (min_y, max_y) = range(overflow[1], content_height - area.height, reverse_y);
            let requested = scroll.get(&id).copied().unwrap_or_default();
            let clean =
                |v: f32, min: f32, max: f32| if v.is_finite() { v.clamp(min, max) } else { 0. };
            let offset = ScrollOffset {
                x: clean(requested.x, min_x, max_x),
                y: clean(requested.y, min_y, max_y),
            };
            offsets[id.0] = offset;
            scroll_regions.push(ScrollRegion {
                node: id,
                rect: area,
                content_width,
                content_height,
                offset,
                min_x,
                max_x,
                min_y,
                max_y,
            });
        }
        let mut sticky = sticky_content_boxes.as_ref().map(|content_boxes| {
            sticky::StickyPositions::new(sticky::Geometry {
                document: doc,
                styles: self.styles,
                border_boxes: &boxes,
                content_boxes,
                scroll_sizes: &scroll_sizes,
                scroll_offsets: &offsets,
                viewport: self.viewport,
                viewport_overflow_source: viewport_source,
            })
        });
        let mut visual = vec![Visual::default(); count];
        let mut clips = vec![];
        for &id in &order {
            let parent = parents[id.0];
            let inherited = parent.map(|p| visual[p.0]).unwrap_or_default();
            let relative = offset_matrices[id.0];
            let delta = sticky.as_mut().map_or(ScrollOffset::default(), |sticky| {
                sticky.visit(
                    id,
                    parent,
                    ScrollOffset {
                        x: relative.e,
                        y: relative.f,
                    },
                )
            });
            let mut offset_matrix = parent
                .map_or(Affine2D::IDENTITY, |p| offset_matrices[p.0])
                .multiply(relative);
            let style = &self.styles[id.0];
            if style.position == Position::Fixed
                && fixed[id.0].is_none()
                && matches!(doc.nodes[id.0].kind, NodeKind::Element { .. })
            {
                offset_matrix =
                    Affine2D::translation(-self.viewport.scroll_x, -self.viewport.scroll_y)
                        .multiply(offset_matrix);
            }
            let mut local = locals[id.0];
            if delta != ScrollOffset::default() {
                let translate = Affine2D::translation(delta.x, delta.y);
                local = translate.multiply(local);
                offset_matrix = translate.multiply(offset_matrix);
            }
            offset_matrices[id.0] = offset_matrix;
            let matrix = inherited.content.multiply(local);
            let mut content_clip = inherited.content_clip;
            let overflow = layout_overflow(style, id, viewport_source);
            if let Some(rect) = boxes[id.0]
                && overflow.iter().any(|v| v.clips())
            {
                let mut rect = padding_box(rect, style);
                if !overflow[0].clips() {
                    rect.x = -1e9;
                    rect.width = 2e9;
                }
                if !overflow[1].clips() {
                    rect.y = -1e9;
                    rect.height = 2e9;
                }
                let index = clips.len();
                clips.push(ClipRegion {
                    rect,
                    matrix,
                    parent: content_clip,
                });
                content_clip = Some(index);
            }
            let offset = offsets[id.0];
            visual[id.0] = Visual {
                matrix,
                content: matrix.multiply(Affine2D::translation(-offset.x, -offset.y)),
                clip: inherited.content_clip,
                content_clip,
            };
        }
        let (sticky_uses_viewport, sticky_inset_sizes) = sticky
            .map(|sticky| (sticky.uses_viewport, sticky.inset_sizes))
            .unwrap_or_default();
        let flex_or_grid_item = |parent: Option<NodeId>| {
            parent.is_some_and(|p| {
                matches!(
                    self.styles[p.0].display,
                    Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid
                )
            })
        };
        let static_float = |id: NodeId| {
            let style = &self.styles[id.0];
            let parent = doc.nodes[id.0].parent;
            style.float != css::Float::None
                && style.position == Position::Static
                && style.transform.is_empty()
                && parent != Some(doc.root)
                && !flex_or_grid_item(parent)
                && boxes[id.0].is_some()
        };
        let mut groups = vec![PaintGroup::default()];
        let mut context_for = vec![0usize; count];
        let mut transparent = vec![false; count];
        let mut float_contexts = HashSet::new();
        let mut pseudo = vec![None; count];
        for (index, &id) in order.iter().enumerate() {
            let style = &self.styles[id.0];
            let parent = doc.nodes[id.0].parent;
            transparent[id.0] = style.opacity == 0. || parent.is_some_and(|p| transparent[p.0]);
            let ctx = parent.map_or(0, |p| context_for[p.0]);
            context_for[id.0] = ctx;
            if !matches!(doc.nodes[id.0].kind, NodeKind::Element { .. }) {
                pseudo[id.0] = parent.and_then(|p| pseudo[p.0]);
                continue;
            }
            if doc.tag(id) == "html" {
                context_for[id.0] = 0;
                groups[0].owner = Some(id);
                continue;
            }
            let flex_item = flex_or_grid_item(parent);
            let creates = style.opacity < 1.
                || matches!(style.position, Position::Fixed | Position::Sticky)
                || !style.transform.is_empty()
                    && (style.display != Display::Inline || self.is_replaced_image(id))
                || style.z_index.is_some() && (style.position != Position::Static || flex_item);
            if creates {
                let z = if style.position != Position::Static || flex_item {
                    style.z_index.unwrap_or(0)
                } else {
                    0
                };
                let phase = if z < 0 {
                    PaintPhase::Negative
                } else if z > 0 {
                    PaintPhase::Positive
                } else {
                    PaintPhase::Positioned
                };
                let next = push_paint_group(&mut groups, ctx, id, phase, z, index);
                context_for[id.0] = next;
            } else {
                if static_float(id) {
                    float_contexts.insert(ctx);
                }
                pseudo[id.0] = parent
                    .and_then(|p| pseudo[p.0])
                    .or_else(|| (style.position != Position::Static).then_some(index));
            }
        }
        // CSS 2.2 Appendix E: floats paint as a group between in-flow block
        // decorations and inline content. Form those groups only in affected
        // contexts. Positioned descendants go to the true context, just as
        // actual stacking contexts did in the first pass.
        let group_for = (!float_contexts.is_empty()).then(|| {
            let mut group_for = context_for.clone();
            for (index, &id) in order.iter().enumerate() {
                let ctx = context_for[id.0];
                if !float_contexts.contains(&ctx) || groups[ctx].owner == Some(id) {
                    continue;
                }
                let node = &doc.nodes[id.0];
                let local = node.parent.map_or(ctx, |p| group_for[p.0]);
                group_for[id.0] = local;
                pseudo[id.0] = None;
                if !matches!(node.kind, NodeKind::Element { .. }) {
                    continue;
                }
                let style = &self.styles[id.0];
                if style.position != Position::Static {
                    group_for[id.0] =
                        push_paint_group(&mut groups, ctx, id, PaintPhase::Positioned, 0, index);
                } else if static_float(id) {
                    group_for[id.0] =
                        push_paint_group(&mut groups, local, id, PaintPhase::Float, 0, index);
                } else if boxes[id.0].is_some()
                    && (flex_or_grid_item(node.parent)
                        || matches!(
                            style.display,
                            Display::InlineBlock | Display::InlineFlex | Display::InlineGrid
                        ))
                {
                    group_for[id.0] =
                        push_paint_group(&mut groups, local, id, PaintPhase::Inline, 0, index);
                }
            }
            group_for
        });
        for (index, meta) in self.commands.meta.iter().enumerate() {
            let group = group_for
                .as_ref()
                .map_or(context_for[meta.node.0], |g| g[meta.node.0]);
            // Attach atomic inline groups at their first in-flow command so
            // they keep their line (or flex/grid item) order among siblings.
            let mut ancestor = group;
            while let Some(parent) = groups[ancestor].inline_parent.take() {
                groups[parent].entries.push(PaintEntry {
                    phase: PaintPhase::Inline,
                    z: 0,
                    order: index,
                    minor: index,
                    item: PaintItem::Group(ancestor),
                });
                ancestor = parent;
            }
            let own_background = meta.background && groups[group].owner == Some(meta.node);
            let p = pseudo[meta.node.0];
            let block_decoration = groups[group].has_float
                && meta.background
                && matches!(
                    self.styles[meta.node.0].display,
                    Display::Block | Display::ListItem | Display::Flex | Display::Grid
                );
            groups[group].entries.push(PaintEntry {
                phase: if own_background {
                    PaintPhase::OwnDecoration
                } else if p.is_some() {
                    PaintPhase::Positioned
                } else if block_decoration {
                    PaintPhase::BlockDecoration
                } else {
                    PaintPhase::Inline
                },
                z: 0,
                order: if own_background {
                    index
                } else {
                    p.unwrap_or(index)
                },
                minor: index,
                item: PaintItem::Command(index),
            });
        }
        for group in &mut groups {
            group
                .entries
                .sort_by_key(|e| (e.phase, e.z, e.order, e.minor));
        }
        let mut paint_order = vec![];
        let mut stack = vec![(0usize, 0usize)];
        while let Some((group, index)) = stack.pop() {
            if let Some(entry) = groups[group].entries.get(index) {
                stack.push((group, index + 1));
                match entry.item {
                    PaintItem::Command(index) => paint_order.push(index),
                    PaintItem::Group(group) => stack.push((group, 0)),
                }
            }
        }
        let mut ranks = vec![usize::MAX; count];
        for (rank, &index) in paint_order.iter().enumerate() {
            let id = self.commands.meta[index].node;
            ranks[id.0] = ranks[id.0].min(rank);
        }
        // An escaped negative-z descendant can paint before the float's own
        // decoration. It must not drag the float's hit region below that paint
        // phase. Keep descendant ranks as a fallback for unpainted ancestors.
        let own_ranks = group_for.as_ref().map(|_| {
            ranks
                .iter()
                .map(|&rank| rank != usize::MAX)
                .collect::<Vec<_>>()
        });
        for &id in order.iter().rev() {
            if let Some(parent) = doc.nodes[id.0].parent
                && own_ranks.as_ref().is_none_or(|own| {
                    !own[parent.0] || !float_contexts.contains(&context_for[parent.0])
                })
            {
                ranks[parent.0] = ranks[parent.0].min(ranks[id.0]);
            }
        }
        let mut hit_elements = vec![];
        for e in &self.elements {
            let v = visual[e.node.0];
            hit_elements.push(HitRegion {
                node: e.node,
                rect: e.rect,
                matrix: v.matrix,
                clip: v.clip,
                rank: ranks[e.node.0],
                svg: None,
            });
        }
        hit_elements.sort_by_key(|h| h.rank);
        let mut node_links: Vec<Option<Arc<str>>> = vec![None; count];
        for &id in &order {
            node_links[id.0] = doc.nodes[id.0].parent.and_then(|p| node_links[p.0].clone());
            if matches!(
                doc.namespace(id),
                crate::dom::Namespace::Html | crate::dom::Namespace::Svg
            ) && doc.tag(id) == "a"
                && (doc.attr(id, "href").is_some()
                    || (doc.namespace(id) == crate::dom::Namespace::Svg
                        && doc
                            .attr_ns(id, crate::dom::AttributeNamespace::XLink, "href")
                            .is_some()))
            {
                node_links[id.0] = self.resolve_href(id);
            }
        }
        let mut commands = vec![];
        let mut svg_hits = HashMap::new();
        let mut hit_controls = vec![];
        let mut open: Vec<usize> = vec![];
        let mut emission_budget = 1_000_000usize;
        let mut path = vec![];
        let mut path_clip = None;
        let mut last_emitted = None;
        let mut originals: Vec<_> = self.commands.items.into_iter().map(Some).collect();
        for (rank, index) in paint_order.into_iter().enumerate() {
            let meta = self.commands.meta[index];
            let visible = self.styles[meta.node.0].visibility == crate::css::Visibility::Visible;
            // An inline SVG emits a single scene for its whole subtree, which
            // can contain visible descendants of a hidden SVG root.
            if !visible
                && !matches!(
                    &originals[index],
                    Some(DrawCommand::Svg {
                        interactive: true,
                        ..
                    })
                )
            {
                continue;
            }
            let v = visual[meta.node.0];
            let matrix = if meta.background { v.matrix } else { v.content };
            let clip = if meta.background {
                v.clip
            } else {
                v.content_clip
            };
            if matrix.inverse().is_none() {
                continue;
            }
            let changed = path_clip != clip;
            if changed {
                path.clear();
                let mut next = clip;
                while let Some(i) = next {
                    if path.len() >= 256 {
                        self.limited = true;
                        break;
                    }
                    path.push(i);
                    next = clips[i].parent;
                }
                path.reverse();
                path_clip = clip;
            }
            let common = open.iter().zip(&path).take_while(|(a, b)| a == b).count();
            let work = open.len() - common + path.len() - common
                + if changed { path.len() } else { 0 }
                + if matrix == Affine2D::IDENTITY { 1 } else { 3 };
            if emission_budget < work {
                self.limited = true;
                break;
            }
            emission_budget -= work;
            for _ in common..open.len() {
                commands.push(DrawCommand::PopState);
            }
            open.truncate(common);
            let mut previous = open.last().map_or(Affine2D::IDENTITY, |&i| clips[i].matrix);
            for &i in &path[common..] {
                let clip = &clips[i];
                let relative = previous
                    .inverse()
                    .map(|m| m.multiply(clip.matrix))
                    .unwrap_or(clip.matrix);
                commands.push(DrawCommand::PushState {
                    transform: relative,
                    clip: Some(clip.rect),
                });
                open.push(i);
                previous = clip.matrix;
            }
            let relative = previous
                .inverse()
                .map(|m| m.multiply(matrix))
                .unwrap_or(matrix);
            let scoped = relative != Affine2D::IDENTITY;
            if scoped {
                commands.push(DrawCommand::PushState {
                    transform: relative,
                    clip: None,
                });
            }
            let command = originals[index].take().unwrap();
            if let DrawCommand::Svg {
                node,
                rect,
                scene,
                interactive: true,
            } = &command
            {
                svg_hits.insert(
                    *node,
                    SvgHit {
                        root_visible: visible,
                        rect: *rect,
                        scene: scene.clone(),
                        matrix,
                        clip,
                    },
                );
            }
            if let DrawCommand::Control { node, .. } = &command {
                hit_controls.push(ControlRegion {
                    node: *node,
                    matrix,
                    clip,
                    command: command.clone(),
                });
            }
            // A fully transparent stacking context has no visual output. Keep
            // geometry, paint order and input regions. Intermediate HTML opacity
            // needs group compositing, not per-command alpha multiplication.
            if !transparent[meta.node.0] {
                commands.push(command);
            }
            last_emitted = Some(rank);
            if scoped {
                commands.push(DrawCommand::PopState);
            }
        }
        for _ in open {
            commands.push(DrawCommand::PopState);
        }
        hit_elements.retain_mut(|hit| {
            hit.svg = svg_hits.get(&hit.node).cloned();
            last_emitted.is_some_and(|last| hit.rank <= last)
                && (self.styles[hit.node.0].visibility == crate::css::Visibility::Visible
                    || hit.svg.is_some())
        });
        let mut elements = self.elements.clone();
        for e in &mut elements {
            e.rect = visual[e.node.0].matrix.rect(e.rect);
        }
        let mut links = self.links;
        for l in &mut links {
            l.rect = visual[l.node.0].content.rect(l.rect);
        }
        let height = bounds[doc.root.0]
            .map(|r| r.y + r.height)
            .unwrap_or(1.)
            .max(1.);
        let mut ids = HashMap::new();
        for &id in &order {
            if let Some(name) = doc.attr(id, "id") {
                ids.entry(name).or_insert(id);
            }
        }
        for (name, y) in &mut self.anchors {
            if let Some(&id) = ids.get(name.as_str())
                && let Some(rect) = boxes[id.0]
            {
                *y = visual[id.0].matrix.rect(rect).y;
            }
        }
        let mut flow_elements = self.elements;
        for e in &mut flow_elements {
            e.rect = offset_matrices[e.node.0].rect(e.rect);
        }
        let viewport_fixed = order.iter().any(|id| {
            boxes[id.0].is_some()
                && self.styles[id.0].position == Position::Fixed
                && fixed[id.0].is_none()
                && matches!(doc.nodes[id.0].kind, NodeKind::Element { .. })
        });
        let mut observation_boxes = observe_intersections.then(|| {
            boxes
                .into_iter()
                .zip(visual)
                .zip(parents)
                .map(|((bounds, v), parent)| ObservationBox {
                    bounds,
                    matrix: v.matrix,
                    clip: v.clip,
                    content_clip: v.content_clip,
                    parent,
                    client_bounds: None,
                })
                .collect::<Vec<_>>()
        });
        for (&root, svg) in &svg_hits {
            for (&node, b) in &svg.scene.geometry_bounds {
                if node == root {
                    continue;
                }
                let local = Rect {
                    x: svg.rect.x + b.x,
                    y: svg.rect.y + b.y,
                    width: b.width,
                    height: b.height,
                };
                let rect = svg.matrix.rect(local);
                if let Some(boxes) = &mut observation_boxes {
                    let observation = &mut boxes[node.0];
                    observation.bounds = Some(local);
                    observation.matrix = svg.matrix;
                    observation.clip = svg.clip;
                    observation.content_clip = svg.clip;
                }
                elements.push(ElementBox { node, rect });
                flow_elements.push(ElementBox {
                    node,
                    rect: offset_matrices[root.0].rect(local),
                });
                if let Some(name) = doc.attr(node, "id")
                    && ids.get(name) == Some(&node)
                {
                    self.anchors.entry(name.to_owned()).or_insert(rect.y);
                }
            }
        }
        if let Some(boxes) = &mut observation_boxes {
            for e in &elements {
                let bounds = &mut boxes[e.node.0].client_bounds;
                *bounds = Some(match *bounds {
                    Some(r) if e.rect.width != 0. || e.rect.height != 0. => {
                        if r.width == 0. && r.height == 0. {
                            e.rect
                        } else {
                            r.union(e.rect)
                        }
                    }
                    Some(r) => r,
                    None => e.rect,
                });
            }
        }
        Layout {
            viewport: self.viewport,
            viewport_overflow,
            depends_on_viewport_scroll: sticky_uses_viewport || viewport_fixed,
            scroll_flow: None,
            computed_styles: Arc::clone(self.styles),
            scroll_regions,
            flow_elements,
            hit_elements,
            node_links,
            hit_controls,
            clips,
            observation_boxes,
            scroll_sizes,
            sticky_inset_sizes,
            commands,
            canvas_frames: self.page.canvas_frames.clone(),
            links,
            elements,
            anchors: self.anchors,
            subgrid_spans: self.subgrid_spans,
            height,
            content_width: bounds[doc.root.0]
                .map(|r| r.x + r.width)
                .unwrap_or(self.viewport.width)
                .max(self.viewport.width),
            background,
            width: self.viewport.width,
            limited: self.limited || self.link_cache.borrow().limited,
        }
    }
}
