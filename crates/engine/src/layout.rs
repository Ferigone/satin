use crate::{
    css::{
        self, Align, BoxSizing, Color, Display, FlexDirection, GRID_TRACK_LIMIT, GridAutoFlow,
        GridBreadth, Length, Overflow, Style,
    },
    dom::{NodeId, NodeKind},
    fonts::{FontRequest, FontSet, Fonts, Run},
    loader::LoadedPage,
};
use std::{collections::HashMap, sync::Arc};
mod controls;
mod floats;
mod grid_template;
mod intersections;
mod positioning;
mod scroll_flow;
mod svg_content;
use grid_template::{Axis, AxisSize, Tracks};
use intersections::ObservationBox;
pub(crate) use intersections::ObserverRoot;
use positioning::TrackedCommands;
pub use positioning::{Affine2D, ScrollOffset, ScrollRegion, Viewport};
pub(crate) use scroll_flow::ScrollFlow;

#[derive(Clone, Debug)]
pub struct ControlPaintStyle {
    pub font_size: f32,
    pub line_height: f32,
    pub color: Color,
    pub font: FontRequest,
    pub font_set: Arc<FontSet>,
    pub placeholder: String,
    pub listbox: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
    fn translated(self, x: f32, y: f32) -> Self {
        Self {
            x: self.x + x,
            y: self.y + y,
            ..self
        }
    }
    fn union(self, r: Self) -> Self {
        let x = self.x.min(r.x);
        let y = self.y.min(r.y);
        Self {
            x,
            y,
            width: (self.x + self.width).max(r.x + r.width) - x,
            height: (self.y + self.height).max(r.y + r.height) - y,
        }
    }
}
#[derive(Clone, Debug)]
pub enum DrawCommand {
    PushState {
        transform: Affine2D,
        clip: Option<Rect>,
    },
    PopState,
    Control {
        node: NodeId,
        rect: Rect,
        control: Arc<crate::script::ControlSnapshot>,
        style: ControlPaintStyle,
    },
    Rect {
        rect: Rect,
        color: Color,
    },
    RoundedFill {
        rect: Rect,
        radii: [[f32; 2]; 4],
        color: Color,
    },
    RoundedBorder {
        rect: Rect,
        radii: [[f32; 2]; 4],
        widths: [f32; 4],
        colors: [Color; 4],
    },
    UniformBorder {
        rect: Rect,
        widths: [f32; 4],
        color: Color,
    },
    Text {
        x: f32,
        baseline: f32,
        run: Run,
        color: Color,
        underline: bool,
    },
    Image {
        rect: Rect,
        image: Arc<image::RgbaImage>,
    },
    Canvas {
        node: NodeId,
        rect: Rect,
    },
    Svg {
        node: NodeId,
        rect: Rect,
        scene: Arc<crate::svg::SvgScene>,
        /// An image resource never exposes its internal nodes to the host DOM.
        interactive: bool,
    },
}
/// CSS scales all corner radii by a single factor when adjacent curves overlap.
fn used_radii(rect: Rect, style: &Style) -> [[f32; 2]; 4] {
    let Some(corners) = &style.border_radii else {
        return [[0.; 2]; 4];
    };
    let mut radii = corners.map(|[x, y]| {
        [
            x.resolve(rect.width, style.font_size).unwrap_or(0.).max(0.),
            y.resolve(rect.height, style.font_size)
                .unwrap_or(0.)
                .max(0.),
        ]
    });
    let mut scale = 1.0_f64;
    for (side, a, b) in [
        (rect.width, radii[0][0], radii[1][0]),
        (rect.width, radii[3][0], radii[2][0]),
        (rect.height, radii[0][1], radii[3][1]),
        (rect.height, radii[1][1], radii[2][1]),
    ] {
        if a > 0. || b > 0. {
            scale = scale.min(f64::from(side) / (f64::from(a) + f64::from(b)));
        }
    }
    for radius in &mut radii {
        if radius[0] == 0. || radius[1] == 0. {
            *radius = [0.; 2];
        } else {
            *radius = radius.map(|v| (f64::from(v) * scale) as f32);
        }
    }
    radii
}

fn fitted_object(area: Rect, (w, h): (f32, f32), style: &Style) -> Rect {
    use css::ObjectFit;
    if w <= 0. || h <= 0. || area.width <= 0. || area.height <= 0. {
        return area;
    }
    let contain = (area.width / w).min(area.height / h);
    let (width, height) = match style.object_fit {
        ObjectFit::Fill => (area.width, area.height),
        fit => {
            let scale = match fit {
                ObjectFit::Contain => contain,
                ObjectFit::Cover => (area.width / w).max(area.height / h),
                ObjectFit::ScaleDown => contain.min(1.),
                _ => 1.,
            };
            (w * scale, h * scale)
        }
    };
    let [x, y] = style.object_position;
    Rect {
        x: area.x + x.resolve(area.width - width, style.font_size).unwrap_or(0.),
        y: area.y
            + y.resolve(area.height - height, style.font_size)
                .unwrap_or(0.),
        width,
        height,
    }
}

fn background_command(rect: Rect, style: &Style) -> DrawCommand {
    let color = style.background.unwrap_or(Color(0, 0, 0, 0));
    let radii = used_radii(rect, style);
    if color.3 > 0 && radii.iter().any(|r| r[0] > 0.) {
        DrawCommand::RoundedFill { rect, radii, color }
    } else {
        DrawCommand::Rect { rect, color }
    }
}

#[derive(Clone, Debug)]
pub struct LinkUrl(Arc<str>);
impl LinkUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::ops::Deref for LinkUrl {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
impl From<Arc<str>> for LinkUrl {
    fn from(value: Arc<str>) -> Self {
        Self(value)
    }
}
impl From<String> for LinkUrl {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}
impl PartialEq<&str> for LinkUrl {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
impl serde::Serialize for LinkUrl {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
#[derive(Clone, Debug)]
pub struct Link {
    pub node: NodeId,
    pub rect: Rect,
    pub url: LinkUrl,
}
/// Block border boxes and inline fragments in document coordinates. Parent
/// entries precede descendant entries, preserving innermost hit precedence.
#[derive(Clone, Debug)]
pub struct ElementBox {
    pub node: NodeId,
    pub rect: Rect,
}
/// Node IDs remain stable across detach and reparent operations. Only connected
/// elements need their own computed style; text inherits its parent's entry.
#[derive(Debug)]
pub struct ComputedStyles {
    by_node: Vec<usize>,
    entries: Vec<Style>,
}

impl ComputedStyles {
    fn new(node_count: usize) -> Self {
        Self {
            by_node: vec![0; node_count],
            entries: vec![Style::default()],
        }
    }

    pub fn len(&self) -> usize {
        self.by_node.len()
    }
    pub fn is_empty(&self) -> bool {
        self.by_node.is_empty()
    }
    pub fn get(&self, id: usize) -> Option<&Style> {
        self.by_node.get(id).map(|&entry| &self.entries[entry])
    }
    pub fn iter(&self) -> impl Iterator<Item = &Style> {
        self.by_node.iter().map(|&entry| &self.entries[entry])
    }
}

impl std::ops::Index<usize> for ComputedStyles {
    type Output = Style;
    fn index(&self, id: usize) -> &Style {
        &self.entries[self.by_node[id]]
    }
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub viewport: Viewport,
    pub viewport_overflow: [Overflow; 2],
    pub depends_on_viewport_scroll: bool,
    scroll_flow: Option<Arc<ScrollFlow>>,
    pub computed_styles: Arc<ComputedStyles>,
    pub scroll_regions: Vec<ScrollRegion>,
    flow_elements: Vec<ElementBox>,
    hit_elements: Vec<positioning::HitRegion>,
    node_links: Vec<Option<Arc<str>>>,
    hit_controls: Vec<positioning::ControlRegion>,
    clips: Vec<positioning::ClipRegion>,
    observation_boxes: Option<Vec<ObservationBox>>,
    scroll_sizes: Vec<Option<(f32, f32)>>,
    pub(crate) sticky_inset_sizes: HashMap<NodeId, [f32; 2]>,
    pub commands: Vec<DrawCommand>,
    pub(crate) canvas_frames: crate::canvas::Frames,
    pub links: Vec<Link>,
    pub elements: Vec<ElementBox>,
    pub anchors: HashMap<String, f32>,
    pub height: f32,
    pub content_width: f32,
    pub background: Color,
    pub width: f32,
    /// Recursive formatting or its work budget used the iterative content fallback.
    pub limited: bool,
}
impl Layout {
    pub fn control_option_at(&self, node: NodeId, x: f32, y: f32) -> Option<NodeId> {
        for hit in self.hit_controls.iter().rev() {
            if hit.node != node || !positioning::inside_clips(&self.clips, hit.clip, x, y) {
                continue;
            }
            let Some(inverse) = hit.matrix.inverse() else {
                continue;
            };
            let (x, y) = inverse.point(x, y);
            let command = &hit.command;
            if let DrawCommand::Control {
                node: id,
                rect,
                control,
                style,
            } = command
                && *id == node
                && style.listbox
                && rect.contains(x, y)
            {
                let line = style.line_height.max(1.);
                let rows = (rect.height / line).floor().max(1.) as usize;
                let selected = control
                    .options
                    .iter()
                    .position(|option| option.selected)
                    .unwrap_or(0);
                let first = if control.focused {
                    selected.saturating_sub(rows - 1)
                } else {
                    0
                };
                return control
                    .options
                    .get(first + ((y - rect.y) / line).floor() as usize)
                    .map(|option| option.node);
            }
        }
        None
    }
    pub fn build(page: &LoadedPage, fonts: &Fonts, width: f32) -> Self {
        Self::build_with_viewport(
            page,
            fonts,
            Viewport {
                width,
                ..Viewport::default()
            },
        )
    }
    pub fn build_with_viewport(page: &LoadedPage, fonts: &Fonts, viewport: Viewport) -> Self {
        Self::build_with_scroll(page, fonts, viewport, &HashMap::new())
    }
    pub fn build_with_scroll(
        page: &LoadedPage,
        fonts: &Fonts,
        viewport: Viewport,
        scroll: &HashMap<NodeId, ScrollOffset>,
    ) -> Self {
        Self::build_for_viewport(page, fonts, viewport, scroll, false)
    }
    pub(crate) fn build_for_viewport(
        page: &LoadedPage,
        fonts: &Fonts,
        viewport: Viewport,
        scroll: &HashMap<NodeId, ScrollOffset>,
        observe_intersections: bool,
    ) -> Self {
        Self::build_checked_for_viewport(
            page,
            fonts,
            viewport,
            scroll,
            observe_intersections,
            false,
            |_| Ok::<_, std::convert::Infallible>(()),
        )
        .unwrap()
    }
    pub(crate) fn build_checked_for_viewport<E>(
        page: &LoadedPage,
        fonts: &Fonts,
        mut viewport: Viewport,
        scroll: &HashMap<NodeId, ScrollOffset>,
        observe_intersections: bool,
        retain_flow: bool,
        check_text: impl FnOnce(&ComputedStyles) -> Result<(), E>,
    ) -> Result<Self, E> {
        let width = viewport.width;
        let width = if width.is_finite() {
            width.max(1.)
        } else {
            800.
        };
        viewport.width = width;
        viewport.height = if viewport.height.is_finite() {
            viewport.height.max(1.)
        } else {
            600.
        };
        viewport.scroll_x = if viewport.scroll_x.is_finite() {
            viewport.scroll_x.max(0.)
        } else {
            0.
        };
        viewport.scroll_y = if viewport.scroll_y.is_finite() {
            viewport.scroll_y.max(0.)
        } else {
            0.
        };
        let mut rules = vec![];
        for (index, sheet) in page.stylesheets.iter().enumerate() {
            if !page.stylesheet_applies(index, (width, viewport.height)) {
                continue;
            }
            rules.extend(css::parse_for_viewport(sheet, (width, viewport.height)));
            if rules.len() >= 8192 {
                rules.truncate(8192);
                break;
            }
        }
        let stylesheet = css::Stylesheet::with_viewport(rules, width, viewport.height);
        let mut styles = ComputedStyles::new(page.document.nodes.len());
        // HTML recovery and DOM reparenting can allocate parents after children.
        let mut stack = vec![page.document.root];
        let mut root_font = 16.;
        while let Some(id) = stack.pop() {
            let n = &page.document.nodes[id.0];
            let parent = n.parent.map_or(0, |p| styles.by_node[p.0]);
            styles.by_node[id.0] = if matches!(n.kind, NodeKind::Element { .. }) {
                let style = stylesheet.compute_with_form_state(
                    &page.document,
                    id,
                    &styles.entries[parent],
                    root_font,
                    &|node, pseudo| page.matches_form_pseudo(node, pseudo),
                );
                styles.entries.push(style);
                styles.entries.len() - 1
            } else {
                parent
            };
            if n.parent == Some(page.document.root) && matches!(n.kind, NodeKind::Element { .. }) {
                root_font = styles[id.0].font_size;
            }
            stack.extend(n.children.iter().rev().copied());
        }
        check_text(&styles)?;
        let background = page
            .document
            .elements("html")
            .chain(page.document.elements("body"))
            // A transparent root hides the canvas background, including a background
            // propagated from the body. Body opacity alone does not hide that background.
            .take_while(|&id| page.document.tag(id) != "html" || styles[id.0].opacity > 0.)
            .find_map(|id| styles[id.0].background.filter(|c| c.3 > 0))
            .unwrap_or(Color::WHITE);
        let styles: Arc<ComputedStyles> = styles.into();
        let mut b = Builder::new(page, fonts, &styles, viewport);
        // The root box resolves percentages against the initial containing
        // block. Descendants still receive their containing box's own height.
        let mut line = Line::new(
            0.,
            0.,
            width,
            &styles[page.document.root.0],
            Some(viewport.height),
        );
        for &child in &page.document.nodes[page.document.root.0].children {
            b.child(child, &mut line, None);
        }
        b.flush(&mut line, false);
        // A fixed box can use the viewport as its containing block. Preserve
        // flow before resolving positioned descendants in that case; otherwise
        // their final fragments are independent of the scroll offset as well.
        let viewport_fixed = !b.positioned.is_empty()
            && styles
                .entries
                .iter()
                .any(|s| s.position == css::Position::Fixed);
        if !viewport_fixed {
            b.layout_positioned();
        }
        let candidate = (retain_flow
            && (viewport_fixed
                || page.raw_json.is_some()
                || styles.entries.iter().any(|s| {
                    s.position == css::Position::Sticky
                        || s.overflow.iter().any(|axis| axis.scrollable())
                })))
        .then(|| Arc::new(ScrollFlow::capture(&b, background)));
        let mut layout = b.finish_frame(background, scroll, observe_intersections);
        if layout.depends_on_viewport_scroll
            || layout
                .scroll_regions
                .iter()
                .any(|r| r.max_x > 0. || r.max_y > 0.)
        {
            layout.scroll_flow = candidate;
        }
        Ok(layout)
    }
    /// Update only scroll-dependent geometry for an unchanged page. Callers
    /// discard the prior layout on DOM, resource or form changes. A resize
    /// (including a change in viewport height) always needs new formatting.
    pub fn for_scrolling(
        page: &LoadedPage,
        fonts: &Fonts,
        viewport: Viewport,
        scroll: &HashMap<NodeId, ScrollOffset>,
        previous: Option<&Layout>,
    ) -> Self {
        if let Some(layout) =
            previous.and_then(|l| l.at_scroll(page, fonts, viewport, scroll, false))
        {
            return layout;
        }
        Self::build_checked_for_viewport(page, fonts, viewport, scroll, false, true, |_| {
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap()
    }

    pub(crate) fn at_scroll(
        &self,
        page: &LoadedPage,
        fonts: &Fonts,
        viewport: Viewport,
        scroll: &HashMap<NodeId, ScrollOffset>,
        observe: bool,
    ) -> Option<Self> {
        let flow = self.scroll_flow.as_ref()?;
        if self.viewport.width != viewport.width || self.viewport.height != viewport.height {
            return None;
        }
        let mut layout = flow.at_scroll(page, fonts, viewport, scroll, observe);
        layout.scroll_flow = Some(flow.clone());
        Some(layout)
    }

    pub(crate) fn scroll_work(&self) -> Option<usize> {
        self.scroll_flow.as_ref().map(|flow| flow.replay_work)
    }

    pub fn link_at(&self, x: f32, y: f32) -> Option<&str> {
        let node = self.element_at(x, y)?;
        self.node_links[node.0].as_deref()
    }
    pub fn element_at(&self, x: f32, y: f32) -> Option<NodeId> {
        self.hit_elements
            .iter()
            .rev()
            .find_map(|hit| hit.target(&self.clips, x, y))
    }
    /// Visual AABB union, including positioning/transforms and element scroll,
    /// without clipping. Generated content remains outside this layout subset.
    pub fn rect_for(&self, node: NodeId) -> Option<Rect> {
        self.elements
            .iter()
            .filter(|e| e.node == node)
            .map(|e| e.rect)
            .reduce(Rect::union)
    }
    pub fn rects_for(&self, node: NodeId) -> Vec<Rect> {
        self.elements
            .iter()
            .filter(|e| e.node == node)
            .map(|e| e.rect)
            .collect()
    }
    pub fn flow_rect_for(&self, node: NodeId) -> Option<Rect> {
        self.flow_elements
            .iter()
            .filter(|e| e.node == node)
            .map(|e| e.rect)
            .reduce(Rect::union)
    }
    pub fn scroll_size_for(&self, node: NodeId) -> Option<(f32, f32)> {
        self.scroll_sizes.get(node.0).copied().flatten()
    }
    pub fn computed_svg_styles(
        &self,
        document: &crate::dom::Document,
        root: NodeId,
    ) -> std::collections::BTreeMap<NodeId, crate::svg::SvgStyle> {
        let mut result = std::collections::BTreeMap::new();
        let mut stack = vec![root];
        let mut remaining = crate::svg::MAX_SVG_NODES;
        while let Some(id) = stack.pop() {
            if remaining == 0 {
                break;
            }
            remaining -= 1;
            if document.namespace(id) == crate::dom::Namespace::Svg {
                result.insert(id, self.computed_styles[id.0].svg_style());
            }
            stack.extend(
                document.nodes[id.0]
                    .children
                    .iter()
                    .rev()
                    .take(remaining)
                    .copied(),
            );
        }
        result
    }
}
enum Item {
    Text {
        run: Run,
        color: Color,
        background: Option<Color>,
        underline: bool,
        href: Option<Arc<str>>,
        node: NodeId,
        line_height: f32,
    },
    Fragment(Fragment),
}
impl Item {
    fn advance(&self) -> f32 {
        match self {
            Self::Text { run, .. } => run.advance,
            Self::Fragment(f) => f.width,
        }
    }
    fn extents(&self) -> (f32, f32) {
        match self {
            Self::Text {
                run, line_height, ..
            } => {
                let l = (*line_height - run.ascent - run.descent) / 2.;
                (run.ascent + l, run.descent + l)
            }
            Self::Fragment(f) => (f.height, 0.),
        }
    }
}
struct Line<'a> {
    x: f32,
    y: f32,
    width: f32,
    used: f32,
    items: Vec<Item>,
    pending_space: bool,
    parent: &'a Style,
    containing_height: Option<f32>,
    previous_margin: Option<f32>,
}
impl<'a> Line<'a> {
    fn new(x: f32, y: f32, width: f32, parent: &'a Style, containing_height: Option<f32>) -> Self {
        Self {
            x,
            y,
            width: width.max(0.),
            used: 0.,
            items: vec![],
            pending_space: false,
            parent,
            containing_height,
            previous_margin: None,
        }
    }
}
#[derive(Default)]
struct Fragment {
    commands: TrackedCommands,
    positioned: Vec<positioning::Positioned>,
    links: Vec<Link>,
    elements: Vec<ElementBox>,
    anchors: HashMap<String, f32>,
    width: f32,
    height: f32,
}
#[derive(Clone, Copy, Default)]
struct Constraints {
    width: Option<f32>,
    intrinsic_width: bool,
    height: Option<f32>,
    containing_height: Option<f32>,
    flex_item: bool,
}
#[derive(Clone, Copy)]
struct BoxMetrics {
    margin: [f32; 4],
    padding: [f32; 4],
    border: [f32; 4],
    width: f32,
    left: f32,
}
impl BoxMetrics {
    fn anonymous(width: f32) -> Self {
        Self {
            margin: [0.; 4],
            padding: [0.; 4],
            border: [0.; 4],
            width,
            left: 0.,
        }
    }
    fn new(s: &Style, available: f32, c: Constraints) -> Self {
        let margin = s
            .margin
            .map(|l| l.resolve(available, s.font_size).unwrap_or(0.));
        let padding = s
            .padding
            .map(|l| l.resolve(available, s.font_size).unwrap_or(0.).max(0.));
        let border = s.borders();
        let extra = padding[1] + padding[3] + border[1] + border[3];
        let requested = s
            .width
            .resolve(available, s.font_size)
            .map(|v| to_content(v, extra, s.box_sizing));
        let mut width = c
            .width
            .map(|v| (v - extra).max(0.))
            .or(requested)
            .unwrap_or((available - margin[1] - margin[3] - extra).max(0.));
        if c.width.is_none() {
            if let Some(max) = s.max_width.resolve(available, s.font_size) {
                width = width.min(to_content(max, extra, s.box_sizing));
            }
            if let Some(min) = s.min_width.resolve(available, s.font_size) {
                width = width.max(to_content(min, extra, s.box_sizing));
            }
        }
        let rest = (available - width - extra - margin[1] - margin[3]).max(0.);
        let left = margin[3]
            + if !c.flex_item && s.margin[3] == Length::Auto {
                if s.margin[1] == Length::Auto {
                    rest / 2.
                } else {
                    rest
                }
            } else {
                0.
            };
        Self {
            margin,
            padding,
            border,
            width: width.max(0.),
            left,
        }
    }
    /// Intrinsic measurements are border-box widths. Automatic inline margins are zero here.
    fn fit_content_width(self, s: &Style, available: f32, intrinsic: (f32, f32)) -> f32 {
        let (min_content, max_content) = intrinsic;
        let stretch = available - self.margin[1] - self.margin[3];
        let extra = self.extra_x();
        let mut width = stretch.min(max_content).max(min_content).max(extra);
        if let Some(max) = s.max_width.resolve(available, s.font_size) {
            width = width.min(to_content(max, extra, s.box_sizing) + extra);
        }
        if let Some(min) = s.min_width.resolve(available, s.font_size) {
            width = width.max(to_content(min, extra, s.box_sizing) + extra);
        }
        width
    }
    fn extra_x(self) -> f32 {
        self.padding[1] + self.padding[3] + self.border[1] + self.border[3]
    }
    fn clamp_content_height(self, mut content: f32, style: &Style, basis: Option<f32>) -> f32 {
        if let Some(max) = style.max_height.resolve_with_basis(basis, style.font_size) {
            content = content.min(to_content(max, self.extra_y(), style.box_sizing));
        }
        if let Some(min) = style
            .min_height
            .resolve(basis.unwrap_or(0.), style.font_size)
        {
            content = content.max(to_content(min, self.extra_y(), style.box_sizing));
        }
        content.max(0.)
    }
    fn extra_y(self) -> f32 {
        self.padding[0] + self.padding[2] + self.border[0] + self.border[2]
    }
}
fn to_content(v: f32, extra: f32, sizing: BoxSizing) -> f32 {
    (v - if sizing == BoxSizing::BorderBox {
        extra
    } else {
        0.
    })
    .max(0.)
}
fn collapse_margins(a: f32, b: f32) -> f32 {
    a.max(0.).max(b.max(0.)) + a.min(0.).min(b.min(0.))
}
fn is_css_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}')
}
struct Builder<'a> {
    page: &'a LoadedPage,
    fonts: &'a Fonts,
    // Layout treats computed styles as immutable. Borrow them across recursive
    // calls so the size of a CSS value does not multiply with layout depth.
    styles: &'a Arc<ComputedStyles>,
    commands: TrackedCommands,
    positioned: Vec<positioning::Positioned>,
    viewport: Viewport,
    link_cache: std::cell::RefCell<positioning::LinkCache>,
    svg_work_left: usize,
    svg_images: HashMap<(usize, u32, u32), Arc<crate::svg::SvgScene>>,
    links: Vec<Link>,
    elements: Vec<ElementBox>,
    anchors: HashMap<String, f32>,
    list_ordinals: HashMap<NodeId, i64>,
    intrinsic: Vec<Option<(f32, f32)>>,
    grid_text: HashMap<NodeId, GridText>,
    floats: floats::FloatContext,
    float_work_remaining: usize,
    recursion: usize,
    work_remaining: usize,
    limited: bool,
}
impl<'a> Builder<'a> {
    fn new(
        page: &'a LoadedPage,
        fonts: &'a Fonts,
        styles: &'a Arc<ComputedStyles>,
        viewport: Viewport,
    ) -> Self {
        Self {
            page,
            fonts,
            styles,
            commands: TrackedCommands::default(),
            positioned: vec![],
            viewport,
            link_cache: std::cell::RefCell::new(positioning::LinkCache::default()),
            svg_work_left: crate::svg::MAX_SVG_WORK,
            svg_images: HashMap::new(),
            links: vec![],
            elements: vec![],
            anchors: HashMap::new(),
            list_ordinals: HashMap::new(),
            intrinsic: vec![None; page.document.nodes.len()],
            grid_text: HashMap::new(),
            floats: floats::FloatContext::default(),
            float_work_remaining: page
                .document
                .nodes
                .len()
                .saturating_mul(64)
                .saturating_add(512)
                .min(1_000_000),
            recursion: 0,
            work_remaining: page
                .document
                .nodes
                .len()
                .saturating_mul(8)
                .saturating_add(512)
                .min(1_000_000),
            limited: false,
        }
    }

    fn finish_frame(
        mut self,
        background: Color,
        scroll: &HashMap<NodeId, ScrollOffset>,
        observe_intersections: bool,
    ) -> Layout {
        self.layout_positioned();
        let (page, fonts) = (self.page, self.fonts);
        let mut layout = self.finish_visual(background, scroll, observe_intersections);
        if let Some(json) = &page.raw_json {
            json.lock().unwrap().paint_rows(&mut layout, fonts);
        }
        layout
    }
}
const MAX_LAYOUT_RECURSION: usize = 48;
struct FlexItem {
    node: NodeId,
    basis: f32, // Border-box base used for free-space accounting.
    inner_basis: f32,
    min: f32,
    max: f32,
    grow: f32,
    shrink: f32,
    margin: [f32; 4],
    auto: [bool; 4],
    fragment: Option<Fragment>,
}
fn distribute_flex(items: &[FlexItem], available: f32) -> Vec<f32> {
    // The outer hypothetical sizes choose the used flex factor. Fixed items
    // and bases on the wrong side of that size start frozen at it.
    let mut sizes: Vec<_> = items.iter().map(|i| i.basis.clamp(i.min, i.max)).collect();
    let growing = sizes.iter().sum::<f32>() < available;
    let factor = |item: &FlexItem| if growing { item.grow } else { item.shrink };
    let weight = |item: &FlexItem| {
        if growing {
            factor(item)
        } else {
            factor(item) * item.inner_basis
        }
    };
    let mut frozen: Vec<_> = items
        .iter()
        .zip(&sizes)
        .map(|(item, &hypothetical)| {
            factor(item) == 0.
                || if growing {
                    item.basis > hypothetical
                } else {
                    item.basis < hypothetical
                }
        })
        .collect();
    let free_space = |sizes: &[f32], frozen: &[bool]| {
        available
            - items
                .iter()
                .enumerate()
                .map(|(i, item)| if frozen[i] { sizes[i] } else { item.basis })
                .sum::<f32>()
    };
    let initial = free_space(&sizes, &frozen);
    let mut violations = vec![0.; items.len()];
    // The pass bound protects pathological author input.
    for _ in 0..64 {
        let (mut factor_sum, mut weight_sum) = (0., 0.);
        for (i, item) in items.iter().enumerate() {
            if !frozen[i] {
                factor_sum += factor(item);
                weight_sum += weight(item);
            }
        }
        let mut remaining = free_space(&sizes, &frozen);
        if factor_sum < 1. {
            let partial = initial * factor_sum;
            if partial.abs() < remaining.abs() {
                remaining = partial;
            }
        }
        if !growing {
            remaining = -remaining.abs();
        }
        let mut total_violation = 0.;
        for (i, item) in items.iter().enumerate() {
            if frozen[i] {
                continue;
            }
            let target = item.basis
                + if weight_sum > 0. {
                    remaining * (weight(item) / weight_sum)
                } else {
                    0.
                };
            sizes[i] = target.clamp(item.min, item.max);
            violations[i] = sizes[i] - target;
            total_violation += violations[i];
        }
        if total_violation == 0. {
            break;
        }
        // Only freeze the constraints on the side of the net violation.
        // Opposite clamps may cease to be needed after redistribution.
        for (is_frozen, &violation) in frozen.iter_mut().zip(&violations) {
            if (total_violation > 0. && violation > 0.) || (total_violation < 0. && violation < 0.)
            {
                *is_frozen = true;
            }
        }
    }
    sizes
}
fn alignment_space(align: Align, extra: f32, count: usize) -> (f32, f32) {
    match align {
        Align::End => (extra, 0.),
        Align::Center => (extra / 2., 0.),
        Align::SpaceBetween if count > 1 => (0., extra / (count - 1) as f32),
        Align::SpaceAround if count > 0 => (extra / (count * 2) as f32, extra / count as f32),
        Align::SpaceEvenly if count > 0 => (extra / (count + 1) as f32, extra / (count + 1) as f32),
        _ => (0., 0.),
    }
}

const GRID_ITEM_LIMIT: usize = 4096;
#[derive(Clone, Copy)]
struct GridAxisPosition {
    start: Option<i32>,
    span: usize,
}
#[derive(Clone, Copy)]
struct GridItem {
    node: NodeId,
    column: usize,
    row: usize,
    columns: usize,
    rows: usize,
}
struct GridPlacement {
    items: Vec<GridItem>,
    columns: usize,
    rows: usize,
    column_origin: i32,
    row_origin: i32,
}
#[derive(Clone)]
struct GridText {
    merged: Option<Arc<str>>,
    visible: bool,
}
fn grid_placement(
    nodes: Vec<NodeId>,
    styles: &ComputedStyles,
    text: impl Fn(NodeId) -> bool,
    container: &Style,
    [rows, columns]: [&Axis<'_>; 2],
    work: &mut usize,
) -> Option<GridPlacement> {
    if nodes.len() > GRID_ITEM_LIMIT {
        return None;
    }
    let transpose = container.grid_auto_flow == GridAutoFlow::Column;
    let explicit = if transpose {
        [columns.explicit, rows.explicit]
    } else {
        [rows.explicit, columns.explicit]
    };
    let mut positions = Vec::with_capacity(nodes.len());
    let mut origins = [0i32; 2];
    let mut ends = [explicit[0] as i32, explicit[1] as i32];
    for node in nodes {
        let style = &styles[node.0];
        let (row, column) = if text(node) {
            let auto = GridAxisPosition {
                start: None,
                span: 1,
            };
            (auto, auto)
        } else {
            (
                rows.position(&style.grid_row, work)?,
                columns.position(&style.grid_column, work)?,
            )
        };
        let axes = if transpose {
            [column, row]
        } else {
            [row, column]
        };
        for (index, axis) in axes.iter().enumerate() {
            if axis.span > GRID_TRACK_LIMIT {
                return None;
            }
            if let Some(start) = axis.start {
                origins[index] = origins[index].min(start);
                ends[index] = ends[index].max(start.checked_add(axis.span as i32)?);
            }
        }
        positions.push((node, axes));
    }
    // An axis can have no tracks. Implicit tracks are added for item
    // placement, not merely because an auto-track sizing function exists.
    let mut counts = [
        (ends[0] - origins[0]) as usize,
        (ends[1] - origins[1]) as usize,
    ];
    if counts.iter().any(|count| *count > GRID_TRACK_LIMIT) {
        return None;
    }
    let mut occupied = vec![false; GRID_TRACK_LIMIT * GRID_TRACK_LIMIT];
    let mut placed = vec![None; positions.len()];
    fn fits(
        occupied: &[bool],
        position: [usize; 2],
        span: [usize; 2],
        work: &mut usize,
    ) -> Option<bool> {
        if position[0].checked_add(span[0])? > GRID_TRACK_LIMIT
            || position[1].checked_add(span[1])? > GRID_TRACK_LIMIT
        {
            return Some(false);
        }
        for row in position[0]..position[0] + span[0] {
            for column in position[1]..position[1] + span[1] {
                *work = work.checked_sub(1)?;
                if occupied[row * GRID_TRACK_LIMIT + column] {
                    return Some(false);
                }
            }
        }
        Some(true)
    }
    fn occupy(
        occupied: &mut [bool],
        position: [usize; 2],
        span: [usize; 2],
        work: &mut usize,
    ) -> Option<()> {
        if position[0].checked_add(span[0])? > GRID_TRACK_LIMIT
            || position[1].checked_add(span[1])? > GRID_TRACK_LIMIT
        {
            return None;
        }
        for row in position[0]..position[0] + span[0] {
            for column in position[1]..position[1] + span[1] {
                *work = work.checked_sub(1)?;
                occupied[row * GRID_TRACK_LIMIT + column] = true;
            }
        }
        Some(())
    }
    // Grid1 §8.5: definite areas first; author-requested overlaps are allowed.
    for (index, (_, axes)) in positions.iter().enumerate() {
        if let [Some(row), Some(column)] = [axes[0].start, axes[1].start] {
            let position = [(row - origins[0]) as usize, (column - origins[1]) as usize];
            let span = [axes[0].span, axes[1].span];
            occupy(&mut occupied, position, span, work)?;
            placed[index] = Some(position);
        }
    }
    // Items locked to a major-axis track can grow the implicit minor axis.
    let mut row_cursors = [0usize; GRID_TRACK_LIMIT];
    for (index, (_, axes)) in positions.iter().enumerate() {
        if placed[index].is_some() {
            continue;
        }
        if let Some(row) = axes[0].start {
            let row = (row - origins[0]) as usize;
            let span = [axes[0].span, axes[1].span];
            let start = if container.grid_dense {
                0
            } else {
                row_cursors[row]
            };
            let mut found = None;
            for column in start..=GRID_TRACK_LIMIT.saturating_sub(span[1]) {
                if fits(&occupied, [row, column], span, work)? {
                    found = Some([row, column]);
                    break;
                }
            }
            let position = found?;
            occupy(&mut occupied, position, span, work)?;
            placed[index] = Some(position);
            row_cursors[row] = position[1] + span[1];
            counts[1] = counts[1].max(position[1] + span[1]);
        }
    }
    for (_, axes) in &positions {
        counts[1] = counts[1].max(axes[1].span);
    }
    if counts[1] > GRID_TRACK_LIMIT {
        return None;
    }
    let mut cursor = [0usize; 2];
    for (index, (_, axes)) in positions.iter().enumerate() {
        if placed[index].is_some() {
            continue;
        }
        if container.grid_dense {
            cursor = [0, 0];
        }
        let span = [axes[0].span, axes[1].span];
        if let Some(column) = axes[1].start {
            let column = (column - origins[1]) as usize;
            if column < cursor[1] {
                cursor[0] += 1;
            }
            cursor[1] = column;
        }
        loop {
            if cursor[0] + span[0] > GRID_TRACK_LIMIT {
                return None;
            }
            if axes[1].start.is_none() && cursor[1] + span[1] > counts[1] {
                cursor[0] += 1;
                cursor[1] = 0;
                continue;
            }
            if fits(&occupied, cursor, span, work)? {
                break;
            }
            if axes[1].start.is_some() {
                cursor[0] += 1;
            } else {
                cursor[1] += 1;
            }
        }
        occupy(&mut occupied, cursor, span, work)?;
        counts[0] = counts[0].max(cursor[0] + span[0]);
        placed[index] = Some(cursor);
    }
    let items = positions
        .into_iter()
        .zip(placed)
        .map(|((node, axes), position)| {
            let position = position.unwrap();
            if transpose {
                GridItem {
                    node,
                    column: position[0],
                    row: position[1],
                    columns: axes[0].span,
                    rows: axes[1].span,
                }
            } else {
                GridItem {
                    node,
                    row: position[0],
                    column: position[1],
                    rows: axes[0].span,
                    columns: axes[1].span,
                }
            }
        })
        .collect();
    Some(if transpose {
        GridPlacement {
            items,
            columns: counts[0],
            rows: counts[1],
            column_origin: origins[0],
            row_origin: origins[1],
        }
    } else {
        GridPlacement {
            items,
            columns: counts[1],
            rows: counts[0],
            column_origin: origins[1],
            row_origin: origins[0],
        }
    })
}

#[derive(Clone, Copy)]
struct GridContribution {
    start: usize,
    span: usize,
    minimum: f32,
    min_content: f32,
    maximum: f32,
    auto_minimum: bool,
    floor: f32,
}
#[derive(Clone, Copy)]
struct GridTrackState {
    base: f32,
    limit: f32,
    flex: f32,
    auto_max: bool,
    min: GridBreadth,
    max: GridBreadth,
    infinitely_growable: bool,
}
fn intrinsic_breadth(value: GridBreadth) -> bool {
    matches!(
        value,
        GridBreadth::Auto | GridBreadth::MinContent | GridBreadth::MaxContent
    )
}

fn grid_intrinsic_phase(
    states: &mut [GridTrackState],
    items: &[GridContribution],
    phase: usize,
    max_constraint: bool,
    flexible: bool,
    gap: f32,
) {
    let growth = phase >= 3;
    let affected = |track: &GridTrackState| {
        if flexible && !matches!(track.max, GridBreadth::Flex(_)) {
            return false;
        }
        match phase {
            0 => intrinsic_breadth(track.min),
            1 => matches!(track.min, GridBreadth::MinContent | GridBreadth::MaxContent),
            2 => {
                track.min == GridBreadth::MaxContent
                    || (max_constraint && track.min == GridBreadth::Auto)
            }
            3 => intrinsic_breadth(track.max),
            _ => matches!(track.max, GridBreadth::Auto | GridBreadth::MaxContent),
        }
    };
    let mut planned = vec![0f32; states.len()];
    let mut touched = vec![false; states.len()];
    for item in items {
        let end = item.start + item.span;
        let candidates: Vec<_> = (item.start..end)
            .filter(|&index| affected(&states[index]))
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let contribution = match phase {
            0 => item.minimum,
            1 | 3 => item.min_content,
            _ => item.maximum,
        };
        let sizes: Vec<_> = states[item.start..end]
            .iter()
            .map(|track| {
                if growth && track.limit.is_finite() {
                    track.limit.max(track.base)
                } else {
                    track.base
                }
            })
            .collect();
        let mut space =
            (contribution - sizes.iter().sum::<f32>() - gap * item.span.saturating_sub(1) as f32)
                .max(0.);
        let mut increases = vec![0f32; item.span];
        let cap = |index: usize| {
            let track = &states[index];
            if growth {
                if track.limit.is_finite() && !track.infinitely_growable {
                    0.
                } else {
                    f32::INFINITY
                }
            } else {
                (track.limit - track.base).max(0.)
            }
        };
        fn distribute(
            candidates: &[usize],
            start: usize,
            caps: impl Fn(usize) -> f32,
            weights: impl Fn(usize) -> f32,
            space: &mut f32,
            increases: &mut [f32],
        ) {
            for _ in 0..GRID_TRACK_LIMIT {
                if *space <= 0.0001 {
                    break;
                }
                let weight: f32 = candidates
                    .iter()
                    .filter(|&&index| increases[index - start] + 0.0001 < caps(index))
                    .map(|&index| weights(index))
                    .sum();
                if weight <= 0. {
                    break;
                }
                let previous = *space;
                let mut used = 0.;
                for &index in candidates {
                    let local = index - start;
                    let add = (previous * weights(index) / weight)
                        .min((caps(index) - increases[local]).max(0.));
                    increases[local] += add;
                    used += add;
                }
                *space = (*space - used).max(0.);
                if used <= 0.0001 {
                    break;
                }
            }
        }
        let factor_sum: f32 = candidates.iter().map(|&index| states[index].flex).sum();
        let weight = |index: usize| {
            if flexible {
                states[index].flex
                    + if factor_sum < 1. {
                        (1. - factor_sum) / candidates.len() as f32
                    } else {
                        0.
                    }
            } else {
                1.
            }
        };
        distribute(
            &candidates,
            item.start,
            cap,
            weight,
            &mut space,
            &mut increases,
        );
        if space > 0.0001 && !flexible {
            let others: Vec<_> = (item.start..end)
                .filter(|&index| !affected(&states[index]))
                .collect();
            distribute(&others, item.start, cap, |_| 1., &mut space, &mut increases);
        }
        if space > 0.0001 {
            let beyond: Vec<_> = candidates
                .iter()
                .copied()
                .filter(|&index| {
                    if growth || phase < 2 {
                        intrinsic_breadth(states[index].max)
                    } else {
                        matches!(
                            states[index].max,
                            GridBreadth::Auto | GridBreadth::MaxContent
                        )
                    }
                })
                .collect();
            let beyond = if beyond.is_empty() && !growth {
                &candidates
            } else {
                &beyond
            };
            distribute(
                beyond,
                item.start,
                |_| f32::INFINITY,
                weight,
                &mut space,
                &mut increases,
            );
        }
        for index in item.start..end {
            planned[index] = planned[index].max(increases[index - item.start]);
        }
        for index in candidates {
            touched[index] = true;
        }
    }
    for (index, track) in states.iter_mut().enumerate() {
        if growth {
            if touched[index] || planned[index] > 0. {
                if !track.limit.is_finite() {
                    track.limit = track.base;
                    if phase == 3 {
                        track.infinitely_growable = true;
                    }
                }
                track.limit = (track.limit + planned[index]).max(track.base);
            }
        } else {
            track.base += planned[index];
            if track.limit.is_finite() {
                track.limit = track.limit.max(track.base);
            }
        }
    }
}
fn grid_track_sizes(
    tracks: &Tracks,
    available: Option<f32>,
    gap: f32,
    font: f32,
    contributions: &[GridContribution],
    align: Align,
    min_constraint: bool,
) -> Vec<f32> {
    let fixed = |value: GridBreadth| match value {
        GridBreadth::Length(value) => value.resolve_with_basis(available, font).map(|n| n.max(0.)),
        _ => None,
    };
    let normalize = |value: GridBreadth| {
        if available.is_none()
            && matches!(value, GridBreadth::Length(length) if length.has_percent())
        {
            GridBreadth::Auto
        } else {
            value
        }
    };
    let mut states: Vec<_> = tracks
        .iter()
        .map(|track| {
            let base = fixed(track.min).unwrap_or(0.);
            GridTrackState {
                base,
                limit: fixed(track.max).unwrap_or(f32::INFINITY).max(base),
                flex: if let GridBreadth::Flex(n) = track.max {
                    n
                } else {
                    0.
                },
                auto_max: matches!(normalize(track.max), GridBreadth::Auto),
                min: normalize(track.min),
                max: normalize(track.max),
                infinitely_growable: false,
            }
        })
        .collect();
    let gaps = tracks.gaps(gap);
    let mut items = contributions.to_vec();
    items.sort_by_key(|item| item.span);
    for item in &mut items {
        let slice = &states[item.start..item.start + item.span];
        let fixed_maximum = slice
            .iter()
            .map(|track| fixed(track.max))
            .collect::<Option<Vec<_>>>()
            .map(|sizes| sizes.iter().sum::<f32>() + gap * item.span.saturating_sub(1) as f32);
        if item.auto_minimum {
            let automatic = slice.iter().any(|track| track.min == GridBreadth::Auto)
                && !(item.span > 1
                    && slice
                        .iter()
                        .any(|track| matches!(track.max, GridBreadth::Flex(_))));
            item.minimum = if automatic {
                item.minimum
                    .min(fixed_maximum.unwrap_or(f32::INFINITY))
                    .max(item.floor)
            } else {
                item.floor
            };
        }
        if available.is_none() {
            item.minimum = item
                .min_content
                .min(fixed_maximum.unwrap_or(f32::INFINITY))
                .max(item.minimum);
        }
    }
    for item in items.iter().filter(|item| item.span == 1) {
        let track = &mut states[item.start];
        let minimum = match track.min {
            GridBreadth::Auto => item.minimum,
            GridBreadth::MinContent => item.min_content,
            GridBreadth::MaxContent => item.maximum,
            _ => 0.,
        };
        track.base = track.base.max(minimum);
        let maximum = match track.max {
            GridBreadth::MinContent => Some(item.min_content),
            GridBreadth::Auto | GridBreadth::MaxContent => Some(item.maximum),
            _ => None,
        };
        if let Some(maximum) = maximum {
            track.limit = if track.limit.is_finite() {
                track.limit.max(maximum)
            } else {
                maximum
            };
        }
        track.limit = track.limit.max(track.base);
    }
    let mut start = 0;
    while start < items.len() {
        let span = items[start].span;
        let mut end = start + 1;
        while end < items.len() && items[end].span == span {
            end += 1;
        }
        if span > 1 {
            let group: Vec<_> = items[start..end]
                .iter()
                .copied()
                .filter(|item| {
                    !states[item.start..item.start + item.span]
                        .iter()
                        .any(|track| matches!(track.max, GridBreadth::Flex(_)))
                })
                .collect();
            for track in &mut states {
                track.infinitely_growable = false;
            }
            for phase in 0..5 {
                grid_intrinsic_phase(
                    &mut states,
                    &group,
                    phase,
                    available.is_none() && !min_constraint,
                    false,
                    gap,
                );
            }
        }
        start = end;
    }
    let flexible: Vec<_> = items
        .iter()
        .copied()
        .filter(|item| {
            item.span > 1
                && states[item.start..item.start + item.span]
                    .iter()
                    .any(|track| matches!(track.max, GridBreadth::Flex(_)))
        })
        .collect();
    for phase in 0..3 {
        grid_intrinsic_phase(
            &mut states,
            &flexible,
            phase,
            available.is_none() && !min_constraint,
            true,
            gap,
        );
    }
    for track in &mut states {
        if !track.limit.is_finite() {
            track.limit = track.base;
        }
        track.limit = track.limit.max(track.base);
    }
    if min_constraint {
        return states
            .into_iter()
            .map(|track| track.base.clamp(0., 1e8))
            .collect();
    }
    if let Some(available) = available {
        let mut free =
            (available - gaps - states.iter().map(|track| track.base).sum::<f32>()).max(0.);
        for _ in 0..GRID_TRACK_LIMIT {
            let candidates: Vec<_> = states
                .iter()
                .enumerate()
                .filter(|(_, track)| {
                    !matches!(track.max, GridBreadth::Flex(_)) && track.base + 0.001 < track.limit
                })
                .map(|(i, _)| i)
                .collect();
            if free <= 0.001 || candidates.is_empty() {
                break;
            }
            let share = free / candidates.len() as f32;
            let mut used = 0.;
            for index in candidates {
                let increase = share.min(states[index].limit - states[index].base);
                states[index].base += increase;
                used += increase;
            }
            free -= used;
        }
        let mut frozen = vec![false; states.len()];
        for _ in 0..=states.len() {
            let taken = states
                .iter()
                .enumerate()
                .filter(|(i, track)| frozen[*i] || !matches!(track.max, GridBreadth::Flex(_)))
                .map(|(_, track)| track.base)
                .sum::<f32>();
            let factors = states
                .iter()
                .enumerate()
                .filter(|(i, track)| !frozen[*i] && matches!(track.max, GridBreadth::Flex(_)))
                .map(|(_, track)| track.flex)
                .sum::<f32>();
            let fraction = ((available - gaps - taken) / factors.max(1.)).max(0.);
            let mut changed = false;
            for (index, track) in states.iter().enumerate() {
                if !frozen[index]
                    && matches!(track.max, GridBreadth::Flex(_))
                    && fraction * track.flex + 0.001 < track.base
                {
                    frozen[index] = true;
                    changed = true;
                }
            }
            if !changed {
                for (index, track) in states.iter_mut().enumerate() {
                    if !frozen[index] && matches!(track.max, GridBreadth::Flex(_)) {
                        track.base = track.base.max(fraction * track.flex);
                    }
                }
                break;
            }
        }
        if align == Align::Stretch {
            let free =
                (available - gaps - states.iter().map(|track| track.base).sum::<f32>()).max(0.);
            let count = states.iter().filter(|track| track.auto_max).count();
            if count > 0 {
                for track in &mut states {
                    if track.auto_max {
                        track.base += free / count as f32;
                    }
                }
            }
        }
    } else {
        let mut fraction = states
            .iter()
            .filter(|track| matches!(track.max, GridBreadth::Flex(_)))
            .map(|track| track.base / track.flex.max(1.))
            .fold(0f32, f32::max);
        // The hypothetical fr size can only decrease when a track freezes.
        // Visit potential freezes in descending base/factor order so each
        // spanning item takes one bounded scan, not repeated restarts.
        let mut freeze_order: Vec<_> = states
            .iter()
            .enumerate()
            .filter(|(_, track)| matches!(track.max, GridBreadth::Flex(_)) && track.base > 0.)
            .map(|(index, track)| (index, track.base / track.flex))
            .collect();
        freeze_order.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        for item in &items {
            let span = item.start..item.start + item.span;
            let slice = &states[span.clone()];
            let mut flex: f32 = slice
                .iter()
                .filter(|track| matches!(track.max, GridBreadth::Flex(_)))
                .map(|track| track.flex)
                .sum();
            if flex <= 0. {
                continue;
            }
            let fixed = slice
                .iter()
                .filter(|track| !matches!(track.max, GridBreadth::Flex(_)))
                .map(|track| track.limit)
                .sum::<f32>();
            let mut leftover = item.maximum - fixed - gap * item.span.saturating_sub(1) as f32;
            let mut hypothetical = leftover.max(0.) / flex.max(1.);
            for &(index, required) in &freeze_order {
                if hypothetical >= required {
                    break;
                }
                if span.contains(&index) {
                    leftover -= states[index].base;
                    flex -= states[index].flex;
                    hypothetical = leftover.max(0.) / flex.max(1.);
                }
            }
            fraction = fraction.max(hypothetical);
        }
        for track in &mut states {
            track.base = if matches!(track.max, GridBreadth::Flex(_)) {
                track.base.max(fraction * track.flex)
            } else {
                track.limit
            };
        }
    }
    states
        .into_iter()
        .map(|track| track.base.clamp(0., 1e8))
        .collect()
}
fn grid_area(sizes: &[f32], start: usize, span: usize, gap: f32) -> f32 {
    sizes[start..start + span].iter().sum::<f32>() + gap * span.saturating_sub(1) as f32
}
fn grid_offsets(
    sizes: &[f32],
    tracks: &Tracks,
    gap: f32,
    available: f32,
    align: Align,
) -> (Vec<f32>, f32) {
    let total = sizes.iter().sum::<f32>() + tracks.gaps(gap);
    let extra = available - total;
    let (offset, spacing) = alignment_space(
        align,
        if matches!(align, Align::Center | Align::End) {
            extra
        } else {
            extra.max(0.)
        },
        tracks.active,
    );
    let mut cursor = offset;
    let mut seen_active = false;
    (
        sizes
            .iter()
            .enumerate()
            .map(|(index, size)| {
                if tracks.collapsed(index) {
                    return cursor;
                }
                if seen_active {
                    cursor += gap + spacing;
                }
                let position = cursor;
                cursor += size;
                seen_active = true;
                position
            })
            .collect(),
        gap + spacing,
    )
}
impl Builder<'_> {
    fn grid_nodes(&mut self, id: NodeId) -> Vec<NodeId> {
        let mut nodes = vec![];
        let mut text = vec![];
        let doc = &self.page.document;
        let mut flush = |nodes: &mut Vec<NodeId>, text: &mut Vec<NodeId>| {
            if let Some(&first) = text.first() {
                let entry = self.grid_text.entry(first).or_insert_with(|| {
                    if text.len() == 1 {
                        let visible = matches!(&doc.nodes[first.0].kind, NodeKind::Text(text) | NodeKind::Cdata(text) if !text.chars().all(is_css_space));
                        GridText { merged: None, visible }
                    } else {
                        let merged: String = text.iter().filter_map(|node| doc.nodes[node.0].kind.text_data()).collect();
                        let visible = !merged.chars().all(is_css_space);
                        self.intrinsic[first.0] = None;
                        GridText { merged: Some(Arc::from(merged)), visible }
                    }
                });
                if entry.visible {
                    nodes.push(first);
                }
            }
            text.clear();
        };
        for &node in &doc.nodes[id.0].children {
            match &doc.nodes[node.0].kind {
                NodeKind::Text(_) | NodeKind::Cdata(_) => text.push(node),
                NodeKind::Comment(_)
                | NodeKind::ProcessingInstruction { .. }
                | NodeKind::Doctype { .. } => {}
                NodeKind::Element { .. } if self.styles[node.0].display == Display::None => {}
                NodeKind::Element { .. } if self.styles[node.0].position.out_of_flow() => {}
                _ => {
                    flush(&mut nodes, &mut text);
                    nodes.push(node);
                }
            }
            if nodes.len() > GRID_ITEM_LIMIT {
                break;
            }
        }
        flush(&mut nodes, &mut text);
        nodes.sort_by_key(|node| {
            if self.page.document.nodes[node.0].kind.text_data().is_some() {
                0
            } else {
                self.styles[node.0].order
            }
        });
        nodes
    }
    fn grid_inline_contribution(&mut self, item: GridItem, width: f32) -> GridContribution {
        let (min_content, max_content) = self.intrinsic_width(item.node);
        if self.page.document.nodes[item.node.0]
            .kind
            .text_data()
            .is_some()
        {
            return GridContribution {
                start: item.column,
                span: item.columns,
                minimum: min_content,
                min_content,
                maximum: max_content,
                auto_minimum: true,
                floor: 0.,
            };
        }
        let style = &self.styles[item.node.0];
        let m = BoxMetrics::new(
            style,
            width,
            Constraints {
                flex_item: true,
                ..Constraints::default()
            },
        );
        let margin = m.margin[1] + m.margin[3];
        let minimum = style
            .min_width
            .resolve(width, style.font_size)
            .map(|value| {
                if style.box_sizing == BoxSizing::ContentBox {
                    value + m.extra_x()
                } else {
                    value.max(m.extra_x())
                }
            })
            .unwrap_or(min_content);
        let maximum = style
            .max_width
            .resolve(width, style.font_size)
            .map(|value| {
                if style.box_sizing == BoxSizing::ContentBox {
                    value + m.extra_x()
                } else {
                    value.max(m.extra_x())
                }
            })
            .unwrap_or(f32::INFINITY)
            .max(minimum);
        GridContribution {
            start: item.column,
            span: item.columns,
            minimum: minimum + margin,
            min_content: min_content.min(maximum) + margin,
            maximum: max_content.min(maximum).max(minimum) + margin,
            auto_minimum: style.min_width == Length::Auto
                && (style.width == Length::Auto || style.width.has_percent()),
            floor: m.extra_x() + margin,
        }
    }
    fn grid_intrinsic(&mut self, id: NodeId) -> (f32, f32) {
        let container = &self.styles[id.0];
        let metrics = BoxMetrics::new(container, 0., Constraints::default());
        let rows = Axis::new(
            container,
            true,
            AxisSize::from_style(container, true, None, metrics),
        );
        let columns = Axis::new(
            container,
            false,
            AxisSize::from_style(container, false, None, metrics),
        );
        self.limited |= rows.limited || columns.limited;
        let nodes = self.grid_nodes(id);
        let doc = &self.page.document;
        let Some(placement) = grid_placement(
            nodes,
            self.styles,
            |node| doc.nodes[node.0].kind.text_data().is_some(),
            container,
            [&rows, &columns],
            &mut self.work_remaining,
        ) else {
            self.limited = true;
            return (0., 0.);
        };
        let tracks = columns.tracks(&container.grid_auto_columns, &placement);
        let gap = container.gap[1]
            .resolve(0., container.font_size)
            .unwrap_or(0.);
        let contributions: Vec<_> = placement
            .items
            .iter()
            .map(|&item| self.grid_inline_contribution(item, 0.))
            .collect();
        let minimum = grid_track_sizes(
            &tracks,
            None,
            gap,
            container.font_size,
            &contributions,
            Align::Start,
            true,
        )
        .iter()
        .sum::<f32>();
        let maximum = grid_track_sizes(
            &tracks,
            None,
            gap,
            container.font_size,
            &contributions,
            Align::Start,
            false,
        )
        .iter()
        .sum::<f32>();
        let extra = metrics.extra_x() + tracks.gaps(gap);
        (minimum + extra, maximum + extra)
    }
    fn grid(
        &mut self,
        id: NodeId,
        x: f32,
        y: f32,
        width: f32,
        [block_size, inline_size]: [AxisSize; 2],
        href: Option<Arc<str>>,
    ) -> f32 {
        let container = &self.styles[id.0];
        let height = block_size
            .preferred
            .map(|height| height.min(block_size.maximum).max(block_size.minimum));
        let row_template = Axis::new(
            container,
            true,
            AxisSize {
                preferred: height,
                ..block_size
            },
        );
        let column_template = Axis::new(container, false, inline_size);
        self.limited |= row_template.limited || column_template.limited;
        let nodes = self.grid_nodes(id);
        let doc = &self.page.document;
        let placement = grid_placement(
            nodes,
            self.styles,
            |node| doc.nodes[node.0].kind.text_data().is_some(),
            container,
            [&row_template, &column_template],
            &mut self.work_remaining,
        );
        let Some(placement) = placement else {
            self.limited = true;
            let mut line = Line::new(x, y, width, container, height);
            for &child in &self.page.document.nodes[id.0].children {
                self.flatten(child, &mut line, href.clone());
            }
            self.flush(&mut line, false);
            return (line.y - y).max(0.);
        };
        let columns = column_template.tracks(&container.grid_auto_columns, &placement);
        let rows = row_template.tracks(&container.grid_auto_rows, &placement);
        let gap_x = container.gap[1]
            .resolve(width, container.font_size)
            .unwrap_or(0.)
            .max(0.);
        let gap_y = container.gap[0]
            .resolve(height.unwrap_or(0.), container.font_size)
            .unwrap_or(0.)
            .max(0.);
        let contributions: Vec<_> = placement
            .items
            .iter()
            .map(|&item| self.grid_inline_contribution(item, width))
            .collect();
        let column_sizes = grid_track_sizes(
            &columns,
            Some(width),
            gap_x,
            container.font_size,
            &contributions,
            container.justify_content,
            false,
        );
        let (column_offsets, used_gap_x) = grid_offsets(
            &column_sizes,
            &columns,
            gap_x,
            width,
            container.justify_content,
        );
        let mut measured = Vec::with_capacity(placement.items.len());
        let mut row_contributions = Vec::with_capacity(placement.items.len());
        for &item in &placement.items {
            let area_width = grid_area(&column_sizes, item.column, item.columns, used_gap_x);
            let style = &self.styles[item.node.0];
            let text = self.page.document.nodes[item.node.0]
                .kind
                .text_data()
                .is_some();
            let m = if text {
                BoxMetrics::anonymous(area_width)
            } else {
                BoxMetrics::new(
                    style,
                    area_width,
                    Constraints {
                        flex_item: true,
                        ..Constraints::default()
                    },
                )
            };
            let justify = if text {
                Align::Stretch
            } else if style.justify_self == Align::Auto {
                container.justify_items
            } else {
                style.justify_self
            };
            let auto_margins = style.margin[1] == Length::Auto || style.margin[3] == Length::Auto;
            let automatic = columns[item.column..item.column + item.columns]
                .iter()
                .any(|track| track.min == GridBreadth::Auto)
                && !(item.columns > 1
                    && columns[item.column..item.column + item.columns]
                        .iter()
                        .any(|track| matches!(track.max, GridBreadth::Flex(_))));
            let (min_content, max_content) = self.intrinsic_width(item.node);
            let available = (area_width - m.margin[1] - m.margin[3]).max(0.);
            let mut border_width = if text {
                available
            } else if style.width == Length::FitContent {
                m.fit_content_width(style, area_width, (min_content, max_content))
            } else if style.width != Length::Auto {
                m.width + m.extra_x()
            } else if justify == Align::Stretch && !auto_margins {
                available
            } else {
                max_content
                    .min(available)
                    .max(if automatic { min_content } else { 0. })
            };
            if !text {
                let maximum = style
                    .max_width
                    .resolve(area_width, style.font_size)
                    .map(|value| {
                        if style.box_sizing == BoxSizing::BorderBox {
                            value
                        } else {
                            value + m.extra_x()
                        }
                    })
                    .unwrap_or(f32::INFINITY);
                let minimum = style
                    .min_width
                    .resolve(area_width, style.font_size)
                    .map(|value| {
                        if style.box_sizing == BoxSizing::BorderBox {
                            value
                        } else {
                            value + m.extra_x()
                        }
                    })
                    .unwrap_or(if automatic {
                        min_content.min(available)
                    } else {
                        0.
                    });
                border_width = border_width.min(maximum).max(minimum).max(m.extra_x());
            }
            let fragment = self.fragment(
                item.node,
                area_width,
                href.clone(),
                Constraints {
                    width: Some(border_width),
                    flex_item: true,
                    ..Constraints::default()
                },
            );
            let auto_height = style.height == Length::Auto || style.height.has_percent();
            let minimum = if !text && auto_height {
                style
                    .min_height
                    .resolve(height.unwrap_or(0.), style.font_size)
                    .map(|value| {
                        if style.box_sizing == BoxSizing::BorderBox {
                            value.max(m.extra_y())
                        } else {
                            value + m.extra_y()
                        }
                    })
                    .map(|value| value + m.margin[0] + m.margin[2])
                    .unwrap_or(fragment.height)
            } else {
                fragment.height
            };
            row_contributions.push(GridContribution {
                start: item.row,
                span: item.rows,
                minimum,
                min_content: fragment.height,
                maximum: fragment.height,
                auto_minimum: text || (style.min_height == Length::Auto && auto_height),
                floor: m.extra_y() + m.margin[0] + m.margin[2],
            });
            measured.push((fragment, border_width, justify));
        }
        let row_sizes = grid_track_sizes(
            &rows,
            height,
            gap_y,
            container.font_size,
            &row_contributions,
            container.align_content,
            false,
        );
        let natural_height = row_sizes.iter().sum::<f32>() + rows.gaps(gap_y);
        let used_height = height.unwrap_or(
            natural_height
                .min(block_size.maximum)
                .max(block_size.minimum),
        );
        // Cyclic tracks behave as auto and the percentage part of gutters uses
        // zero during intrinsic sizing. Resolve both against the resulting height.
        let gap_y = container.gap[0]
            .resolve(used_height, container.font_size)
            .unwrap_or(0.)
            .max(0.);
        let row_sizes = if height.is_none() {
            grid_track_sizes(
                &rows,
                Some(used_height),
                gap_y,
                container.font_size,
                &row_contributions,
                container.align_content,
                false,
            )
        } else {
            row_sizes
        };
        let (row_offsets, used_gap_y) = grid_offsets(
            &row_sizes,
            &rows,
            gap_y,
            used_height,
            container.align_content,
        );
        for (item, (mut fragment, border_width, justify)) in placement.items.iter().zip(measured) {
            let area_width = grid_area(&column_sizes, item.column, item.columns, used_gap_x);
            let area_height = grid_area(&row_sizes, item.row, item.rows, used_gap_y);
            let style = &self.styles[item.node.0];
            let text = self.page.document.nodes[item.node.0]
                .kind
                .text_data()
                .is_some();
            let m = if text {
                BoxMetrics::anonymous(area_width)
            } else {
                BoxMetrics::new(
                    style,
                    area_width,
                    Constraints {
                        flex_item: true,
                        ..Constraints::default()
                    },
                )
            };
            let align = if text {
                Align::Start
            } else if style.align_self == Align::Auto {
                container.align_items
            } else {
                style.align_self
            };
            let auto_y = style.margin[0] == Length::Auto || style.margin[2] == Length::Auto;
            let target_height =
                if !text && style.height == Length::Auto && align == Align::Stretch && !auto_y {
                    let maximum = style
                        .max_height
                        .resolve(area_height, style.font_size)
                        .map(|value| {
                            if style.box_sizing == BoxSizing::BorderBox {
                                value
                            } else {
                                value + m.extra_y()
                            }
                        })
                        .unwrap_or(f32::INFINITY);
                    let minimum = style
                        .min_height
                        .resolve(area_height, style.font_size)
                        .map(|value| {
                            if style.box_sizing == BoxSizing::BorderBox {
                                value
                            } else {
                                value + m.extra_y()
                            }
                        })
                        .unwrap_or(0.);
                    Some(
                        (area_height - m.margin[0] - m.margin[2])
                            .min(maximum)
                            .max(minimum)
                            .max(m.extra_y()),
                    )
                } else {
                    None
                };
            if !text {
                fragment = self.fragment(
                    item.node,
                    area_width,
                    href.clone(),
                    Constraints {
                        width: Some(border_width),
                        height: target_height,
                        containing_height: Some(area_height),
                        flex_item: true,
                        ..Constraints::default()
                    },
                );
            }
            let free_x = area_width - fragment.width;
            let free_y = area_height - fragment.height;
            let margin_x = if text {
                0
            } else {
                usize::from(style.margin[1] == Length::Auto)
                    + usize::from(style.margin[3] == Length::Auto)
            };
            let margin_y = if text {
                0
            } else {
                usize::from(style.margin[0] == Length::Auto)
                    + usize::from(style.margin[2] == Length::Auto)
            };
            let offset = |align, free| match align {
                Align::End => free,
                Align::Center => free / 2.,
                _ => 0.,
            };
            let dx = if margin_x > 0 {
                if style.margin[3] == Length::Auto {
                    free_x.max(0.) / margin_x as f32
                } else {
                    0.
                }
            } else {
                offset(justify, free_x)
            };
            let dy = if margin_y > 0 {
                if style.margin[0] == Length::Auto {
                    free_y.max(0.) / margin_y as f32
                } else {
                    0.
                }
            } else {
                offset(align, free_y)
            };
            self.append_fragment(
                fragment,
                x + column_offsets[item.column] + dx,
                y + row_offsets[item.row] + dy,
            );
        }
        used_height
    }
}
impl Builder<'_> {
    fn is_flex_item(&self, child: NodeId) -> bool {
        match &self.page.document.nodes[child.0].kind {
            NodeKind::Text(text) | NodeKind::Cdata(text) => !text.chars().all(is_css_space),
            NodeKind::Element { .. } => {
                let style = &self.styles[child.0];
                style.display != Display::None && !style.position.out_of_flow()
            }
            _ => false,
        }
    }

    /// The children's cached intrinsic sizes include their padding and borders.
    /// A row places all preferred widths on one line. A wrapping row can break
    /// between items when computing its minimum width.
    fn intrinsic_flex_content(&self, id: NodeId) -> (f32, f32) {
        let style = &self.styles[id.0];
        let row = matches!(
            style.flex_direction,
            FlexDirection::Row | FlexDirection::RowReverse
        );
        let gap = style.gap[1]
            .resolve_with_basis(None, style.font_size)
            .unwrap_or(0.);
        let mut min_width = 0.0_f32;
        let mut max_width = 0.0_f32;
        let mut first = true;
        for &child in &self.page.document.nodes[id.0].children {
            if !self.is_flex_item(child) {
                continue;
            }
            let (mut child_min, mut child_max) = self.intrinsic[child.0].unwrap_or_default();
            if self.page.document.nodes[child.0].kind.text_data().is_none() {
                let item = &self.styles[child.0];
                let metrics = BoxMetrics::new(item, 0., Constraints::default());
                let to_border = |value: f32| {
                    if item.box_sizing == BoxSizing::BorderBox {
                        value.max(metrics.extra_x())
                    } else {
                        value + metrics.extra_x()
                    }
                };
                let min = item
                    .min_width
                    .resolve_with_basis(None, item.font_size)
                    .map(to_border)
                    .unwrap_or(0.);
                let max = item
                    .max_width
                    .resolve_with_basis(None, item.font_size)
                    .map(to_border)
                    .unwrap_or(f32::INFINITY)
                    .max(min);
                let margin = metrics.margin[1] + metrics.margin[3];
                child_min = child_min.clamp(min, max) + margin;
                child_max = child_max.clamp(min, max) + margin;
            }
            let spacing = if first { 0. } else { gap };
            if row {
                max_width += spacing + child_max;
            } else {
                max_width = max_width.max(child_max);
            }
            if row && !style.flex_wrap {
                min_width += spacing + child_min;
            } else {
                min_width = min_width.max(child_min);
            }
            first = false;
        }
        (min_width.max(0.), max_width.max(0.))
    }

    /// Useful horizontal-writing Flexbox subset: row/column, reverse directions,
    /// row wrapping, order, gaps, constrained grow/shrink, auto margins and main/
    /// cross alignment. No baselines, column wrapping or full intrinsic sizing.
    fn flex(
        &mut self,
        id: NodeId,
        x: f32,
        y: f32,
        width: f32,
        height: Option<f32>,
        href: Option<Arc<str>>,
    ) -> f32 {
        let container = &self.styles[id.0];
        let row = matches!(
            container.flex_direction,
            FlexDirection::Row | FlexDirection::RowReverse
        );
        let reverse = matches!(
            container.flex_direction,
            FlexDirection::RowReverse | FlexDirection::ColumnReverse
        );
        let gap = container.gap[if row { 1 } else { 0 }]
            .resolve(
                if row { width } else { height.unwrap_or(0.) },
                container.font_size,
            )
            .unwrap_or(0.)
            .max(0.);
        let cross_gap = container.gap[0]
            .resolve(height.unwrap_or(0.), container.font_size)
            .unwrap_or(0.)
            .max(0.);
        let mut nodes: Vec<_> = self.page.document.nodes[id.0]
            .children
            .iter()
            .copied()
            .filter(|&child| self.is_flex_item(child))
            .collect();
        nodes.sort_by_key(|node| {
            if self.page.document.nodes[node.0].kind.text_data().is_some() {
                0
            } else {
                self.styles[node.0].order
            }
        });
        let mut items = vec![];
        for node in nodes {
            let text = self.page.document.nodes[node.0].kind.text_data().is_some();
            let s = &self.styles[node.0];
            let m = BoxMetrics::new(
                s,
                width,
                Constraints {
                    flex_item: true,
                    containing_height: height,
                    ..Constraints::default()
                },
            );
            let (min_content, max_content) = self.intrinsic_width(node);
            let margin = if text { [0.; 4] } else { m.margin };
            let auto = if text {
                [false; 4]
            } else {
                s.margin.map(|v| v == Length::Auto)
            };
            let extra = if text {
                0.
            } else if row {
                m.extra_x()
            } else {
                m.extra_y()
            };
            let (basis, min, max, fragment) = if row {
                let to_border = |v: f32| {
                    if !text && s.box_sizing == BoxSizing::BorderBox {
                        v.max(extra)
                    } else {
                        v + extra
                    }
                };
                let specified = if text {
                    None
                } else if s.width == Length::FitContent {
                    Some(m.fit_content_width(s, width, (min_content, max_content)))
                } else {
                    s.width.resolve(width, s.font_size).map(to_border)
                };
                let basis = if text {
                    max_content
                } else {
                    s.flex_basis
                        .resolve(width, s.font_size)
                        .map(to_border)
                        .or(specified)
                        .unwrap_or(max_content)
                };
                let min = if text {
                    min_content
                } else {
                    s.min_width
                        .resolve(width, s.font_size)
                        .map(to_border)
                        .unwrap_or_else(|| {
                            if s.overflow[0].scrollable() {
                                // The automatic content-box minimum is zero.
                                extra
                            } else {
                                min_content.min(specified.unwrap_or(f32::INFINITY))
                            }
                        })
                }
                .max(extra);
                let max = if text {
                    f32::INFINITY
                } else {
                    s.max_width
                        .resolve(width, s.font_size)
                        .map(to_border)
                        .unwrap_or(f32::INFINITY)
                }
                .max(min);
                (basis.max(extra), min, max, None)
            } else {
                let align = if s.align_self == Align::Auto {
                    container.align_items
                } else {
                    s.align_self
                };
                let cross = if text {
                    max_content.min(width).max(min_content)
                } else if s.width == Length::FitContent {
                    m.fit_content_width(s, width, (min_content, max_content))
                } else if s.width != Length::Auto || align == Align::Stretch {
                    m.width + m.extra_x()
                } else {
                    max_content.min((width - margin[1] - margin[3]).max(min_content))
                };
                let f = self.fragment(
                    node,
                    width,
                    href.clone(),
                    Constraints {
                        width: Some(cross),
                        containing_height: height,
                        flex_item: true,
                        ..Constraints::default()
                    },
                );
                let natural = (f.height - margin[0] - margin[2]).max(0.);
                let to_border = |v: f32| {
                    if !text && s.box_sizing == BoxSizing::BorderBox {
                        v.max(extra)
                    } else {
                        v + extra
                    }
                };
                let specified = if text {
                    None
                } else {
                    s.height
                        .resolve_with_basis(height, s.font_size)
                        .map(to_border)
                };
                let basis = if text {
                    natural
                } else {
                    s.flex_basis
                        .resolve_with_basis(height, s.font_size)
                        .map(to_border)
                        .or(specified)
                        .unwrap_or(natural)
                };
                let min = if text {
                    natural
                } else {
                    s.min_height
                        .resolve(height.unwrap_or(0.), s.font_size)
                        .map(to_border)
                        .unwrap_or_else(|| {
                            if s.overflow[1].scrollable() {
                                extra
                            } else {
                                natural.min(specified.unwrap_or(f32::INFINITY))
                            }
                        })
                }
                .max(extra);
                let max = if text {
                    f32::INFINITY
                } else {
                    s.max_height
                        .resolve_with_basis(height, s.font_size)
                        .map(to_border)
                        .unwrap_or(f32::INFINITY)
                }
                .max(min);
                (basis.max(extra), min, max, Some(f))
            };
            items.push(FlexItem {
                node,
                basis,
                inner_basis: basis - extra,
                min,
                max,
                grow: if text { 0. } else { s.flex_grow },
                shrink: if text { 1. } else { s.flex_shrink },
                margin,
                auto,
                fragment,
            });
        }
        if items.is_empty() {
            return 0.;
        }
        let mut ranges = vec![];
        let (mut start, mut used) = (0, 0.);
        for (index, item) in items.iter().enumerate() {
            let outer = item.basis.clamp(item.min, item.max)
                + if row {
                    item.margin[1] + item.margin[3]
                } else {
                    item.margin[0] + item.margin[2]
                };
            if row && container.flex_wrap && index > start && used + gap + outer > width {
                ranges.push(start..index);
                start = index;
                used = 0.;
            }
            if index > start {
                used += gap;
            }
            used += outer;
        }
        ranges.push(start..items.len());
        let mut lines = vec![];
        for range in ranges {
            let group = &mut items[range.clone()];
            let margins: f32 = group
                .iter()
                .map(|i| {
                    if row {
                        i.margin[1] + i.margin[3]
                    } else {
                        i.margin[0] + i.margin[2]
                    }
                })
                .sum();
            let gaps = gap * group.len().saturating_sub(1) as f32;
            let main_limit = if row {
                width
            } else {
                height.unwrap_or_else(|| {
                    group
                        .iter()
                        .map(|i| i.basis.clamp(i.min, i.max))
                        .sum::<f32>()
                        + margins
                        + gaps
                })
            };
            let sizes = distribute_flex(group, main_limit - margins - gaps);
            let mut cross = 0f32;
            for (item, &size) in group.iter_mut().zip(&sizes) {
                if row {
                    item.fragment = Some(self.fragment(
                        item.node,
                        width,
                        href.clone(),
                        Constraints {
                            width: Some(size),
                            containing_height: height,
                            flex_item: true,
                            ..Constraints::default()
                        },
                    ));
                } else {
                    let old = item.fragment.as_ref().unwrap();
                    let actual = (old.height - item.margin[0] - item.margin[2]).max(0.);
                    if (size - actual).abs() > 0.01 {
                        let cross_width = (old.width - item.margin[1] - item.margin[3]).max(0.);
                        item.fragment = Some(self.fragment(
                            item.node,
                            width,
                            href.clone(),
                            Constraints {
                                width: Some(cross_width),
                                height: Some(size),
                                containing_height: height,
                                flex_item: true,
                                ..Constraints::default()
                            },
                        ));
                    }
                }
                let f = item.fragment.as_ref().unwrap();
                cross = cross.max(if row { f.height } else { f.width });
            }
            if !container.flex_wrap
                && row
                && let Some(height) = height
            {
                cross = height;
            }
            if !row {
                cross = width;
            }
            lines.push((range, sizes, cross, main_limit));
        }
        if row
            && container.flex_wrap
            && let Some(height) = height
        {
            let natural = lines.iter().map(|line| line.2).sum::<f32>()
                + cross_gap * lines.len().saturating_sub(1) as f32;
            let extra = (height - natural).max(0.) / lines.len() as f32;
            for line in &mut lines {
                line.2 += extra;
            }
        }
        let mut cross_cursor = 0f32;
        let mut column_height = 0f32;
        for (range, sizes, cross, main_limit) in lines {
            let group = &mut items[range];
            let (before, after) = if row {
                if reverse { (1, 3) } else { (3, 1) }
            } else if reverse {
                (2, 0)
            } else {
                (0, 2)
            };
            let (cross_before, cross_after) = if row { (0, 2) } else { (3, 1) };
            let occupied = group
                .iter()
                .zip(&sizes)
                .map(|(i, size)| size + i.margin[before] + i.margin[after])
                .sum::<f32>()
                + gap * group.len().saturating_sub(1) as f32;
            let extra = (main_limit - occupied).max(0.);
            let autos = group
                .iter()
                .map(|i| usize::from(i.auto[before]) + usize::from(i.auto[after]))
                .sum::<usize>();
            let auto_share = if autos > 0 { extra / autos as f32 } else { 0. };
            let (mut cursor, distributed_gap) = if autos > 0 {
                (0., 0.)
            } else {
                alignment_space(container.justify_content, extra, group.len())
            };
            for (item, &size) in group.iter_mut().zip(&sizes) {
                let s = &self.styles[item.node.0];
                let text = self.page.document.nodes[item.node.0]
                    .kind
                    .text_data()
                    .is_some();
                let align = if s.align_self == Align::Auto {
                    container.align_items
                } else {
                    s.align_self
                };
                let has_cross_auto = item.auto[cross_before] || item.auto[cross_after];
                if row
                    && !text
                    && align == Align::Stretch
                    && !has_cross_auto
                    && s.height == Length::Auto
                {
                    let m = BoxMetrics::new(s, width, Constraints::default());
                    let to_border = |v: f32| {
                        if s.box_sizing == BoxSizing::BorderBox {
                            v.max(m.extra_y())
                        } else {
                            v + m.extra_y()
                        }
                    };
                    let mut target = (cross - item.margin[0] - item.margin[2]).max(0.);
                    if let Some(max) = s.max_height.resolve_with_basis(height, s.font_size) {
                        target = target.min(to_border(max));
                    }
                    if let Some(min) = s.min_height.resolve(height.unwrap_or(0.), s.font_size) {
                        target = target.max(to_border(min));
                    }
                    let current =
                        (item.fragment.as_ref().unwrap().height - item.margin[0] - item.margin[2])
                            .max(0.);
                    if (target - current).abs() > 0.01 {
                        item.fragment = Some(self.fragment(
                            item.node,
                            width,
                            href.clone(),
                            Constraints {
                                width: Some(size),
                                height: Some(target),
                                containing_height: Some(cross),
                                flex_item: true,
                                ..Constraints::default()
                            },
                        ));
                    }
                }
                let f = item.fragment.take().unwrap();
                let outer_main = if row { f.width } else { f.height };
                let outer_cross = if row { f.height } else { f.width };
                let cross_extra = (cross - outer_cross).max(0.);
                let cross_offset = if has_cross_auto {
                    if item.auto[cross_before] {
                        if item.auto[cross_after] {
                            cross_extra / 2.
                        } else {
                            cross_extra
                        }
                    } else {
                        0.
                    }
                } else {
                    match align {
                        Align::End => cross_extra,
                        Align::Center => cross_extra / 2.,
                        _ => 0.,
                    }
                };
                if item.auto[before] {
                    cursor += auto_share;
                }
                let position = if reverse {
                    main_limit - cursor - outer_main
                } else {
                    cursor
                };
                let (fx, fy) = if row {
                    (x + position, y + cross_cursor + cross_offset)
                } else {
                    (x + cross_offset, y + position)
                };
                self.append_fragment(f, fx, fy);
                cursor += outer_main + gap + distributed_gap;
                if item.auto[after] {
                    cursor += auto_share;
                }
            }
            cross_cursor += cross + cross_gap;
            column_height = column_height.max(main_limit);
        }
        if row {
            (cross_cursor - cross_gap).max(0.)
        } else {
            column_height
        }
    }
}
impl Builder<'_> {
    fn shape(&self, text: &str, style: &Style) -> Run {
        self.fonts.shape_text(
            text,
            style.font_size,
            &style.font_request(),
            &self.page.fonts,
        )
    }
    fn child(&mut self, id: NodeId, line: &mut Line<'_>, href: Option<Arc<str>>) {
        if self.recursion >= MAX_LAYOUT_RECURSION || self.work_remaining == 0 {
            self.flatten(id, line, href);
            return;
        }
        self.work_remaining -= 1;
        self.recursion += 1;
        self.child_inner(id, line, href);
        self.recursion -= 1;
    }
    fn child_inner(&mut self, id: NodeId, line: &mut Line<'_>, href: Option<Arc<str>>) {
        let doc = &self.page.document;
        let s = &self.styles[id.0];
        match &doc.nodes[id.0].kind {
            NodeKind::Comment(_)
            | NodeKind::ProcessingInstruction { .. }
            | NodeKind::Doctype { .. } => return,
            NodeKind::Text(text) | NodeKind::Cdata(text) => {
                self.text(text, s, line, href, doc.nodes[id.0].parent.unwrap_or(id));
                return;
            }
            NodeKind::Document => {
                for &child in &doc.nodes[id.0].children {
                    self.child(child, line, href.clone());
                }
                return;
            }
            NodeKind::Element { .. } => {}
        }
        if s.display == Display::None {
            return;
        }
        if s.position.out_of_flow() {
            self.positioned.push(positioning::Positioned {
                node: id,
                x: line.x + line.used,
                y: line.y,
                href,
            });
            return;
        }
        if let Some(name) = doc.attr(id, "id") {
            self.anchors.entry(name.into()).or_insert(line.y);
        }
        let tag = doc.tag(id);
        if tag == "br" {
            self.flush(line, true);
            return;
        }
        let href = if tag == "a" {
            doc.attr(id, "href")
                .and_then(|_| self.resolve_href(id))
                .or(href)
        } else {
            href
        };
        // Flex/grid items are laid out by their own formatter, not this
        // normal-flow child path.
        if s.float != css::Float::None && doc.nodes[id.0].parent != Some(doc.root) {
            self.place_float(id, line, href);
            return;
        }
        if matches!(
            s.display,
            Display::Block | Display::ListItem | Display::Flex | Display::Grid
        ) {
            self.flush(line, false);
            let top = s.margin[0].resolve(line.width, s.font_size).unwrap_or(0.);
            let hypothetical = line.y
                - line.previous_margin.map_or(0., |previous| {
                    previous + top - collapse_margins(previous, top)
                });
            line.y = match self.floats.clearance(s.clear.sides(line.parent.rtl)) {
                Some(bottom) if hypothetical + top < bottom => bottom - top,
                _ => hypothetical,
            };
            line.y += self.block(
                id,
                line.x,
                line.y,
                line.width,
                href,
                Constraints {
                    containing_height: line.containing_height,
                    ..Constraints::default()
                },
            );
            line.previous_margin = Some(s.margin[2].resolve(line.width, s.font_size).unwrap_or(0.));
            return;
        }
        if self.is_replaced_image(id)
            || self.is_replaced_control(id)
            || matches!(
                s.display,
                Display::InlineBlock | Display::InlineFlex | Display::InlineGrid
            )
        {
            let m = BoxMetrics::new(s, line.width, Constraints::default());
            let (min, max) = self.intrinsic_width(id);
            let mut width = s
                .width
                .resolve(line.width, s.font_size)
                .map(|v| {
                    if s.box_sizing == BoxSizing::BorderBox {
                        v
                    } else {
                        v + m.extra_x()
                    }
                })
                .unwrap_or(max.min((line.width - m.margin[1] - m.margin[3]).max(min)));
            if let Some(max) = s.max_width.resolve(line.width, s.font_size) {
                width = width.min(if s.box_sizing == BoxSizing::BorderBox {
                    max
                } else {
                    max + m.extra_x()
                });
            }
            if let Some(min) = s.min_width.resolve(line.width, s.font_size) {
                width = width.max(if s.box_sizing == BoxSizing::BorderBox {
                    min
                } else {
                    min + m.extra_x()
                });
            }
            let f = self.fragment(
                id,
                line.width,
                href,
                Constraints {
                    width: Some(width),
                    intrinsic_width: s.display == Display::InlineGrid && s.width == Length::Auto,
                    containing_height: line.containing_height,
                    flex_item: true,
                    ..Constraints::default()
                },
            );
            self.inline_fragment(f, s, line, id);
            return;
        }
        if matches!(tag, "input" | "button" | "select" | "textarea")
            && let Some(label) = doc.attr(id, "value").or(doc.attr(id, "placeholder"))
        {
            self.text(label, s, line, href.clone(), id);
        }
        for &child in &doc.nodes[id.0].children {
            self.child(child, line, href.clone());
        }
    }
    fn block(
        &mut self,
        id: NodeId,
        x: f32,
        y: f32,
        available: f32,
        href: Option<Arc<str>>,
        c: Constraints,
    ) -> f32 {
        if self.recursion >= MAX_LAYOUT_RECURSION || self.work_remaining == 0 {
            let mut line = Line::new(
                x,
                y,
                c.width.unwrap_or(available),
                &self.styles[id.0],
                c.containing_height,
            );
            self.flatten(id, &mut line, href);
            self.flush(&mut line, false);
            return (line.y - y).max(0.);
        }
        self.work_remaining -= 1;
        self.recursion += 1;
        let height = self.block_inner(id, x, y, available, href, c);
        self.recursion -= 1;
        height
    }
    fn replaced_auto_border_width(
        &mut self,
        id: NodeId,
        available: f32,
        c: Constraints,
        m: BoxMetrics,
    ) -> Option<f32> {
        let s = &self.styles[id.0];
        let width = if self.is_replaced_image(id) {
            let (nw, nh) = self.image_size(id);
            s.height
                .resolve_with_basis(c.containing_height, s.font_size)
                .filter(|_| !self.is_canvas(id) || nh > 0.)
                .map(|h| to_content(h, m.extra_y(), s.box_sizing) * nw / nh.max(1.))
                .unwrap_or(nw)
        } else if self.is_replaced_control(id) {
            self.control_intrinsic(id).0
        } else {
            return None;
        };
        let extra = m.extra_x();
        let mut width = width.max(0.);
        if let Some(max) = s.max_width.resolve(available, s.font_size) {
            width = width.min(to_content(max, extra, s.box_sizing));
        }
        if let Some(min) = s.min_width.resolve(available, s.font_size) {
            width = width.max(to_content(min, extra, s.box_sizing));
        }
        Some(width + extra)
    }
    fn block_inner(
        &mut self,
        id: NodeId,
        x: f32,
        y: f32,
        available: f32,
        href: Option<Arc<str>>,
        mut c: Constraints,
    ) -> f32 {
        let s = &self.styles[id.0];
        let tag = self.page.document.tag(id);
        let href = if tag == "a" {
            self.page
                .document
                .attr(id, "href")
                .and_then(|_| self.resolve_href(id))
                .or(href)
        } else {
            href
        };
        let replaced_image = self.is_replaced_image(id);
        let replaced_control = self.is_replaced_control(id);
        if c.width.is_none() && s.width == Length::FitContent {
            let metrics = BoxMetrics::new(s, available, c);
            c.width = Some(metrics.fit_content_width(s, available, self.intrinsic_width(id)));
        }
        if (replaced_image || replaced_control) && c.width.is_none() && s.width == Length::Auto {
            let m = BoxMetrics::new(s, available, c);
            c.width = self.replaced_auto_border_width(id, available, c, m);
        }
        let m = BoxMetrics::new(s, available, c);
        let ox = x + m.left;
        let oy = y + m.margin[0];
        let cx = ox + m.padding[3] + m.border[3];
        let cy = oy + m.padding[0] + m.border[0];
        let explicit = c.height.map(|v| (v - m.extra_y()).max(0.)).or_else(|| {
            s.height
                .resolve_with_basis(c.containing_height, s.font_size)
                .map(|v| to_content(v, m.extra_y(), s.box_sizing))
        });
        if let Some(name) = self.page.document.attr(id, "id") {
            self.anchors.insert(name.into(), oy);
        }
        let background = self.commands.len();
        self.commands.owner = id;
        self.commands.background = true;
        self.commands.push(DrawCommand::Rect {
            rect: Rect::default(),
            color: s.background.unwrap_or(Color(0, 0, 0, 0)),
        });
        self.commands.background = false;
        let element = self.elements.len();
        self.elements.push(ElementBox {
            node: id,
            rect: Rect::default(),
        });
        let link_index = self.links.len();
        if tag == "a"
            && let Some(url) = &href
        {
            self.links.push(Link {
                node: id,
                rect: Rect::default(),
                url: url.clone().into(),
            });
        }
        if s.display == Display::ListItem && s.marker != css::MarkerType::None {
            self.list_marker(id, cx, cy, s);
        }
        let natural = if replaced_control {
            self.control_intrinsic(id).1
        } else if replaced_image {
            let (nw, nh) = self.image_size(id);
            let h = explicit.unwrap_or_else(|| {
                if self.is_canvas(id) && nw == 0. {
                    nh
                } else {
                    nh * m.width / nw.max(1.)
                }
            });
            let rect = Rect {
                x: cx,
                y: cy,
                width: m.width,
                height: h,
            };
            if self.is_svg_root(id) {
                self.draw_svg(id, rect);
            } else {
                self.draw_image(id, rect, href.clone());
            }
            h
        } else if matches!(s.display, Display::Flex | Display::InlineFlex) {
            self.queue_positioned_children(id, cx, cy, href.clone());
            self.flex(id, cx, cy, m.width, explicit, href.clone())
        } else if matches!(s.display, Display::Grid | Display::InlineGrid) {
            self.queue_positioned_children(id, cx, cy, href.clone());
            let minimum = if c.height.is_some() {
                0.
            } else {
                s.min_height
                    .resolve(c.containing_height.unwrap_or(0.), s.font_size)
                    .map(|value| to_content(value, m.extra_y(), s.box_sizing))
                    .unwrap_or(0.)
            };
            let maximum = if c.height.is_some() {
                f32::INFINITY
            } else {
                s.max_height
                    .resolve_with_basis(c.containing_height, s.font_size)
                    .map(|value| to_content(value, m.extra_y(), s.box_sizing))
                    .unwrap_or(f32::INFINITY)
                    .max(minimum)
            };
            let block = AxisSize {
                preferred: explicit,
                minimum,
                maximum,
            };
            let inline = if c.intrinsic_width {
                AxisSize::from_style(s, false, Some(available), m)
            } else {
                AxisSize::fixed(m.width)
            };
            self.grid(id, cx, cy, m.width, [block, inline], href.clone())
        } else {
            let independent = c.flex_item
                || s.position.out_of_flow()
                || s.float != css::Float::None
                || s.overflow.iter().any(|o| o.scrollable())
                || self.page.document.nodes[id.0].parent == Some(self.page.document.root);
            let outer = independent.then(|| std::mem::take(&mut self.floats));
            let mut line = Line::new(cx, cy, m.width, s, explicit);
            if matches!(tag, "input" | "button" | "select" | "textarea")
                && let Some(label) = self
                    .page
                    .document
                    .attr(id, "value")
                    .or(self.page.document.attr(id, "placeholder"))
            {
                self.text(label, s, &mut line, href.clone(), id);
            }
            for &child in &self.page.document.nodes[id.0].children {
                self.child(child, &mut line, href.clone());
            }
            self.flush(&mut line, false);
            if let Some(outer) = outer {
                if let Some(bottom) = self.floats.clearance([true, true]) {
                    line.y = line.y.max(bottom);
                }
                self.floats = outer;
            }
            (line.y - cy).max(0.)
        };
        let content = explicit.unwrap_or(natural);
        let content = if c.height.is_none() {
            m.clamp_content_height(content, s, c.containing_height)
        } else {
            content.max(0.)
        };
        if replaced_control {
            self.control_command(
                id,
                Rect {
                    x: cx,
                    y: cy,
                    width: m.width,
                    height: content,
                },
                s,
            );
        }
        let height = content + m.extra_y();
        let rect = Rect {
            x: ox,
            y: oy,
            width: m.width + m.extra_x(),
            height,
        };
        self.elements[element].rect = rect;
        self.commands[background] = background_command(rect, s);
        if tag == "a" && href.is_some() {
            self.links[link_index].rect = rect;
        }
        self.commands.owner = id;
        self.draw_borders(rect, s, m.border);
        m.margin[0] + height + m.margin[2]
    }
    fn fragment(
        &mut self,
        id: NodeId,
        available: f32,
        href: Option<Arc<str>>,
        c: Constraints,
    ) -> Fragment {
        let outer_floats = std::mem::take(&mut self.floats);
        let old = Fragment {
            commands: std::mem::take(&mut self.commands),
            positioned: std::mem::take(&mut self.positioned),
            links: std::mem::take(&mut self.links),
            elements: std::mem::take(&mut self.elements),
            anchors: std::mem::take(&mut self.anchors),
            ..Fragment::default()
        };
        let s = &self.styles[id.0];
        let m = BoxMetrics::new(s, available, c);
        let text = self.page.document.nodes[id.0].kind.text_data().is_some();
        let height = if let Some(t) = self.page.document.nodes[id.0].kind.text_data() {
            let merged = self.grid_text.get(&id).and_then(|text| text.merged.clone());
            let t = merged.as_deref().unwrap_or(t);
            let mut line = Line::new(0., 0., c.width.unwrap_or(available), s, c.containing_height);
            self.text(
                t,
                s,
                &mut line,
                href,
                self.page.document.nodes[id.0].parent.unwrap_or(id),
            );
            self.flush(&mut line, false);
            line.y
        } else {
            self.block(id, 0., 0., available, href, c)
        };
        self.floats = outer_floats;
        let width = if text {
            c.width.unwrap_or(available)
        } else {
            m.width + m.extra_x() + m.margin[1] + m.margin[3]
        };
        Fragment {
            commands: std::mem::replace(&mut self.commands, old.commands),
            positioned: std::mem::replace(&mut self.positioned, old.positioned),
            links: std::mem::replace(&mut self.links, old.links),
            elements: std::mem::replace(&mut self.elements, old.elements),
            anchors: std::mem::replace(&mut self.anchors, old.anchors),
            width,
            height,
        }
    }
    fn append_fragment(&mut self, mut f: Fragment, x: f32, y: f32) {
        for command in &mut f.commands {
            match command {
                DrawCommand::PushState { transform, clip } => {
                    *transform = Affine2D::translation(x, y)
                        .multiply(*transform)
                        .multiply(Affine2D::translation(-x, -y));
                    if let Some(rect) = clip {
                        *rect = rect.translated(x, y);
                    }
                }
                DrawCommand::PopState => {}
                DrawCommand::Rect { rect, .. }
                | DrawCommand::RoundedFill { rect, .. }
                | DrawCommand::RoundedBorder { rect, .. }
                | DrawCommand::UniformBorder { rect, .. }
                | DrawCommand::Image { rect, .. }
                | DrawCommand::Canvas { rect, .. }
                | DrawCommand::Svg { rect, .. }
                | DrawCommand::Control { rect, .. } => *rect = rect.translated(x, y),
                DrawCommand::Text {
                    x: tx, baseline, ..
                } => {
                    *tx += x;
                    *baseline += y;
                }
            }
        }
        for l in &mut f.links {
            l.rect = l.rect.translated(x, y);
        }
        for e in &mut f.elements {
            e.rect = e.rect.translated(x, y);
        }
        self.commands.extend(f.commands);
        self.positioned
            .extend(f.positioned.into_iter().map(|mut p| {
                p.x += x;
                p.y += y;
                p
            }));
        self.links.extend(f.links);
        self.elements.extend(f.elements);
        for (name, pos) in f.anchors {
            self.anchors.insert(name, pos + y);
        }
    }
    fn inline_fragment(&mut self, f: Fragment, s: &Style, line: &mut Line<'_>, node: NodeId) {
        let space = if line.pending_space && !line.items.is_empty() {
            self.shape(" ", s).advance
        } else {
            0.
        };
        if s.wrap
            && !line.items.is_empty()
            && line.used + space + f.width > self.float_interval(line).width()
        {
            self.flush(line, false);
        }
        self.fit_empty_line(line, f.width);
        self.pending_space(s, line, None, node);
        line.previous_margin = None;
        line.used += f.width;
        line.items.push(Item::Fragment(f));
        line.pending_space = false;
    }
    fn list_ordinal(&mut self, id: NodeId) -> i64 {
        if let Some(&n) = self.list_ordinals.get(&id) {
            return n;
        }
        let doc = &self.page.document;
        let Some(parent) = doc.nodes[id.0].parent else {
            return 1;
        };
        let ordered = doc.tag(parent) == "ol";
        let reversed = ordered && doc.attr(parent, "reversed").is_some();
        let items = doc.nodes[parent.0]
            .children
            .iter()
            .copied()
            .filter(|&child| {
                matches!(doc.nodes[child.0].kind, NodeKind::Element { .. })
                    && self.styles[child.0].display == Display::ListItem
            });
        let mut n = ordered
            .then(|| doc.attr(parent, "start"))
            .flatten()
            .and_then(|start| start.trim().parse().ok())
            .unwrap_or_else(|| {
                if reversed {
                    items.clone().count() as i64
                } else {
                    1
                }
            });
        for item in items {
            if ordered
                && doc.tag(item) == "li"
                && let Some(value) = doc.attr(item, "value").and_then(|v| v.trim().parse().ok())
            {
                n = value;
            }
            self.list_ordinals.insert(item, n);
            n = n.saturating_add(if reversed { -1 } else { 1 });
        }
        self.list_ordinals.get(&id).copied().unwrap_or(1)
    }
    fn list_marker(&mut self, id: NodeId, x: f32, y: f32, s: &Style) {
        self.commands.owner = id;
        let label = match s.marker {
            css::MarkerType::None => return,
            css::MarkerType::Disc => "•".into(),
            css::MarkerType::Circle => "◦".into(),
            css::MarkerType::Square => "▪".into(),
            css::MarkerType::Decimal => format!("{}.", self.list_ordinal(id)),
        };
        let run = self.shape(&label, s);
        self.commands.push(DrawCommand::Text {
            x: x - run.advance - 8.,
            baseline: y + run.ascent,
            run,
            color: s.color,
            underline: false,
        });
    }
    fn draw_borders(&mut self, r: Rect, s: &Style, b: [f32; 4]) {
        self.commands.background = true;
        let colors = s.border_colors.map(|color| color.unwrap_or(s.color));
        let mut active = colors
            .into_iter()
            .zip(b)
            .filter_map(|(color, width)| (width > 0.).then_some(color));
        let radii = used_radii(r, s);
        let rounded = radii.iter().any(|corner| corner[0] > 0.);
        if rounded && b.iter().any(|&w| w > 0.) {
            self.commands.push(DrawCommand::RoundedBorder {
                rect: r,
                radii,
                widths: b,
                colors,
            });
        } else if (b[0] > 0. || b[2] > 0.)
            && (b[1] > 0. || b[3] > 0.)
            && let Some(color) = active.next()
            && (1..255).contains(&color.3)
            && active.all(|side| side == color)
        {
            self.commands.push(DrawCommand::UniformBorder {
                rect: r,
                widths: b,
                color,
            });
        } else {
            for (i, rect) in [
                Rect { height: b[0], ..r },
                Rect {
                    x: r.x + r.width - b[1],
                    width: b[1],
                    ..r
                },
                Rect {
                    y: r.y + r.height - b[2],
                    height: b[2],
                    ..r
                },
                Rect { width: b[3], ..r },
            ]
            .into_iter()
            .enumerate()
            {
                if b[i] > 0. {
                    self.commands.push(DrawCommand::Rect {
                        rect,
                        color: colors[i],
                    });
                }
            }
        }
        if s.outline_visible && s.outline_width > 0. {
            let width = s.outline_width;
            let offset = s.outline_offset.max(-r.width.min(r.height) / 2.);
            let amount = offset + width;
            let rect = Rect {
                x: r.x - amount,
                y: r.y - amount,
                width: r.width + 2. * amount,
                height: r.height + 2. * amount,
            };
            let color = s.outline_color.unwrap_or(s.color);
            if rounded {
                let radii = radii
                    .map(|corner| corner.map(|v| if v > 0. { (v + amount).max(0.) } else { 0. }));
                self.commands.push(DrawCommand::RoundedBorder {
                    rect,
                    radii,
                    widths: [width; 4],
                    colors: [color; 4],
                });
            } else {
                self.commands.push(DrawCommand::UniformBorder {
                    rect,
                    widths: [width; 4],
                    color,
                });
            }
        }
        self.commands.background = false;
    }
    fn image_size(&self, id: NodeId) -> (f32, f32) {
        if self.is_canvas(id) {
            let [width, height] = crate::canvas::size(&self.page.document, id);
            return (width as f32, height as f32);
        }
        if self.is_svg_root(id) {
            return crate::svg::SvgScene::intrinsic_size(&self.page.document, id);
        }
        let d = &self.page.document;
        let uri = self.page.image_asset_key(id);
        let (nw, nh) = if self.is_video(id) {
            match self.page.canvas_frames.get(id) {
                Some(crate::canvas::Frame::Bitmap(frame)) => {
                    (frame.width() as f32, frame.height() as f32)
                }
                _ => (300., 150.),
            }
        } else {
            uri.as_ref()
                .and_then(|u| self.page.svg_images.get(u))
                .map(|asset| asset.intrinsic_size())
                .or_else(|| {
                    uri.as_ref()
                        .and_then(|u| self.page.images.get(u))
                        .map(|i| (i.width() as f32, i.height() as f32))
                })
                .unwrap_or((120., 28.))
        };
        let w = d
            .attr(id, "width")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v >= 0.);
        let h = d
            .attr(id, "height")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v >= 0.);
        match (w, h) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, nh * w / nw.max(1.)),
            (None, Some(h)) => (nw * h / nh.max(1.), h),
            _ => (nw, nh),
        }
    }
    fn is_canvas(&self, id: NodeId) -> bool {
        self.page.document.scripting_enabled && crate::canvas::is_canvas(&self.page.document, id)
    }
    fn is_replaced_image(&self, id: NodeId) -> bool {
        self.page.document.tag(id) == "img"
            || self.is_svg_root(id)
            || self.is_canvas(id)
            || self.is_video(id)
    }
    fn is_video(&self, id: NodeId) -> bool {
        let document = &self.page.document;
        document.namespace(id) == crate::dom::Namespace::Html && document.tag(id) == "video"
    }
    fn image_command(&mut self, id: NodeId, area: Rect) -> Option<(DrawCommand, Rect)> {
        let style = &self.styles[id.0];
        if self.is_canvas(id) || self.is_video(id) {
            let intrinsic = if self.is_canvas(id) {
                let [w, h] = crate::canvas::size(&self.page.document, id);
                (w as f32, h as f32)
            } else if let Some(crate::canvas::Frame::Bitmap(frame)) =
                self.page.canvas_frames.get(id)
            {
                (frame.width() as f32, frame.height() as f32)
            } else {
                (300., 150.)
            };
            let rect = fitted_object(area, intrinsic, style);
            return Some((DrawCommand::Canvas { node: id, rect }, rect));
        }
        let key = self.page.image_asset_key(id)?;
        if let Some(image) = self.page.images.get(&key) {
            let rect = fitted_object(area, (image.width() as f32, image.height() as f32), style);
            return Some((
                DrawCommand::Image {
                    rect,
                    image: image.clone(),
                },
                rect,
            ));
        }
        let asset = self.page.svg_images.get(&key)?;
        let rect = fitted_object(area, asset.intrinsic_size(), style);
        let scene = self.svg_image(id, rect)?;
        Some((
            DrawCommand::Svg {
                node: id,
                rect,
                scene,
                interactive: false,
            },
            rect,
        ))
    }
    fn draw_image(&mut self, id: NodeId, rect: Rect, href: Option<Arc<str>>) {
        self.commands.owner = id;
        if let Some((command, object)) = self.image_command(id, rect) {
            self.commands.push_object(command, rect, object);
        } else {
            self.commands.push(DrawCommand::Rect {
                rect,
                color: Color(238, 240, 244, 255),
            });
            let run = self.shape(
                self.page.document.attr(id, "alt").unwrap_or("obraz"),
                &self.styles[id.0],
            );
            self.commands.push(DrawCommand::Text {
                x: rect.x + 4.,
                baseline: rect.y + 18.,
                run,
                color: Color(95, 105, 120, 255),
                underline: false,
            });
        }
        if let Some(url) = href {
            self.links.push(Link {
                node: id,
                rect,
                url: url.into(),
            });
        }
    }
    fn is_svg_root(&self, id: NodeId) -> bool {
        let d = &self.page.document;
        d.namespace(id) == crate::dom::Namespace::Svg
            && d.tag(id) == "svg"
            && d.nodes[id.0]
                .parent
                .is_none_or(|p| d.namespace(p) != crate::dom::Namespace::Svg)
    }
}

impl Builder<'_> {
    fn flatten(&mut self, id: NodeId, line: &mut Line<'_>, href: Option<Arc<str>>) {
        self.limited = true;
        enum Task {
            Visit(NodeId, Option<Arc<str>>),
            End {
                element: usize,
                background: usize,
                y: f32,
                link: Option<usize>,
            },
        }
        let mut stack = vec![Task::Visit(id, href)];
        while let Some(task) = stack.pop() {
            match task {
                Task::End {
                    element,
                    background,
                    y,
                    link,
                } => {
                    self.flush(line, false);
                    let rect = Rect {
                        x: line.x,
                        y,
                        width: line.width,
                        height: (line.y - y).max(0.),
                    };
                    self.elements[element].rect = rect;
                    let style = &self.styles[self.elements[element].node.0];
                    self.commands[background] = background_command(rect, style);
                    if let Some(link) = link {
                        self.links[link].rect = rect;
                    }
                }
                Task::Visit(node, href) => {
                    self.commands.owner = node;
                    let d = &self.page.document;
                    let s = &self.styles[node.0];
                    match &d.nodes[node.0].kind {
                        NodeKind::Text(text) | NodeKind::Cdata(text) => {
                            self.text(text, s, line, href, d.nodes[node.0].parent.unwrap_or(node));
                            continue;
                        }
                        NodeKind::Comment(_)
                        | NodeKind::ProcessingInstruction { .. }
                        | NodeKind::Doctype { .. } => continue,
                        _ => {}
                    }
                    if s.display == Display::None {
                        continue;
                    }
                    let tag = d.tag(node);
                    let href = if tag == "a" {
                        d.attr(node, "href")
                            .and_then(|_| self.resolve_href(node))
                            .or(href)
                    } else {
                        href
                    };
                    if let Some(name) = d.attr(node, "id") {
                        self.anchors.insert(name.into(), line.y);
                    }
                    if tag == "br" {
                        self.flush(line, true);
                        continue;
                    }
                    if self.is_replaced_control(node) {
                        self.flush(line, false);
                        let (width, height) = self.control_intrinsic(node);
                        let metrics = BoxMetrics::new(
                            s,
                            line.width,
                            Constraints {
                                width: Some(width),
                                ..Constraints::default()
                            },
                        );
                        let content_height = s
                            .height
                            .resolve_with_basis(line.containing_height, s.font_size)
                            .map(|h| to_content(h, metrics.extra_y(), s.box_sizing))
                            .unwrap_or(height);
                        let content_height =
                            metrics.clamp_content_height(content_height, s, line.containing_height);
                        let outer = Rect {
                            x: line.x,
                            y: line.y,
                            width: metrics.width + metrics.extra_x(),
                            height: content_height + metrics.extra_y(),
                        };
                        self.elements.push(ElementBox { node, rect: outer });
                        self.commands.push(background_command(outer, s));
                        self.control_command(
                            node,
                            Rect {
                                x: outer.x + metrics.padding[3] + metrics.border[3],
                                y: outer.y + metrics.padding[0] + metrics.border[0],
                                width: metrics.width,
                                height: content_height,
                            },
                            s,
                        );
                        self.draw_borders(outer, s, metrics.border);
                        line.y += outer.height;
                        continue;
                    }
                    if tag == "img" || self.is_canvas(node) || self.is_video(node) {
                        let (nw, nh) = self.image_size(node);
                        let width = s
                            .width
                            .resolve(line.width, s.font_size)
                            .unwrap_or(nw)
                            .max(0.);
                        let height = s
                            .height
                            .resolve(0., s.font_size)
                            .unwrap_or_else(|| {
                                if self.is_canvas(node) && nw == 0. {
                                    nh
                                } else {
                                    nh * width / nw.max(1.)
                                }
                            })
                            .max(0.);
                        let rect = Rect {
                            x: 0.,
                            y: 0.,
                            width,
                            height,
                        };
                        let mut commands = TrackedCommands::default();
                        commands.owner = node;
                        if let Some((command, object)) = self.image_command(node, rect) {
                            commands.push_object(command, rect, object);
                        } else {
                            commands.push(DrawCommand::Rect {
                                rect,
                                color: Color(238, 240, 244, 255),
                            });
                        }
                        let links = href
                            .map(|url| {
                                vec![Link {
                                    node,
                                    rect,
                                    url: url.into(),
                                }]
                            })
                            .unwrap_or_default();
                        let f = Fragment {
                            commands,
                            links,
                            elements: vec![ElementBox { node, rect }],
                            width,
                            height,
                            ..Fragment::default()
                        };
                        self.inline_fragment(f, s, line, node);
                        continue;
                    }
                    if matches!(
                        s.display,
                        Display::Block
                            | Display::ListItem
                            | Display::Flex
                            | Display::Grid
                            | Display::InlineBlock
                            | Display::InlineFlex
                            | Display::InlineGrid
                    ) {
                        self.flush(line, false);
                        let element = self.elements.len();
                        let background = self.commands.len();
                        self.elements.push(ElementBox {
                            node,
                            rect: Rect::default(),
                        });
                        self.commands.push(DrawCommand::Rect {
                            rect: Rect::default(),
                            color: s.background.unwrap_or(Color(0, 0, 0, 0)),
                        });
                        let link = if tag == "a" {
                            href.as_ref().map(|url| {
                                let index = self.links.len();
                                self.links.push(Link {
                                    node,
                                    rect: Rect::default(),
                                    url: url.clone().into(),
                                });
                                index
                            })
                        } else {
                            None
                        };
                        stack.push(Task::End {
                            element,
                            background,
                            y: line.y,
                            link,
                        });
                    }
                    if matches!(tag, "input" | "button" | "select" | "textarea")
                        && let Some(label) = d.attr(node, "value").or(d.attr(node, "placeholder"))
                    {
                        let safe = if tag == "input"
                            && d.attr(node, "type")
                                .is_some_and(|kind| kind.eq_ignore_ascii_case("password"))
                        {
                            "•".repeat(label.encode_utf16().count())
                        } else {
                            label.to_owned()
                        };
                        self.text(&safe, s, line, href.clone(), node);
                    }
                    for &child in d.nodes[node.0].children.iter().rev() {
                        stack.push(Task::Visit(child, href.clone()));
                    }
                }
            }
        }
    }
    fn record_inline(&mut self, node: NodeId, rect: Rect) {
        let mut ancestors = vec![];
        let mut current = Some(node);
        while let Some(id) = current {
            if self.page.document.tag(id).is_empty() || self.styles[id.0].display != Display::Inline
            {
                break;
            }
            ancestors.push(id);
            current = self.page.document.nodes[id.0].parent;
        }
        for node in ancestors.into_iter().rev() {
            self.elements.push(ElementBox { node, rect });
        }
    }
    fn text(
        &mut self,
        text: &str,
        s: &Style,
        line: &mut Line<'_>,
        href: Option<Arc<str>>,
        node: NodeId,
    ) {
        if s.font_size <= 0. {
            return;
        }
        if s.pre {
            for (index, part) in text.split('\n').enumerate() {
                if index > 0 {
                    self.flush(line, true);
                }
                if s.wrap {
                    for token in part.split_inclusive([' ', '\t']) {
                        self.word(
                            &token.replace('\t', "    "),
                            s,
                            line,
                            href.clone(),
                            node,
                            true,
                        );
                    }
                } else if !part.is_empty() {
                    self.word(
                        &part.replace('\t', "    "),
                        s,
                        line,
                        href.clone(),
                        node,
                        false,
                    );
                }
            }
            return;
        }
        for token in text.split_inclusive(is_css_space) {
            let word = token.trim_end_matches(is_css_space);
            if !word.is_empty() {
                self.word(word, s, line, href.clone(), node, s.wrap);
            }
            if word.len() < token.len() {
                line.pending_space = true;
            }
        }
    }
    fn pending_space(
        &mut self,
        s: &Style,
        line: &mut Line<'_>,
        href: Option<Arc<str>>,
        node: NodeId,
    ) {
        if line.pending_space && !line.items.is_empty() {
            let run = self.shape(" ", s);
            line.used += run.advance;
            line.items.push(Item::Text {
                run,
                color: s.color,
                background: s.background,
                underline: s.underline,
                href,
                node,
                line_height: s.line_height_px(),
            });
        }
        line.pending_space = false;
    }
    fn word(
        &mut self,
        word: &str,
        s: &Style,
        line: &mut Line<'_>,
        href: Option<Arc<str>>,
        node: NodeId,
        wrap: bool,
    ) {
        if word.is_empty() {
            return;
        }
        // Emergency wrapping never shapes every growing prefix. Pathological
        // tokens use bounded chunks; ordinary words preserve shaping clusters.
        if wrap && s.break_words && word.len() > 4096 {
            if !line.items.is_empty() {
                self.flush(line, false);
            }
            let width = self.float_interval(line).width();
            let runs = self.fonts.break_word_text(
                word,
                s.font_size,
                &s.font_request(),
                &self.page.fonts,
                width,
            );
            let count = runs.len();
            for (i, run) in runs.into_iter().enumerate() {
                self.push_run(run, s, line, href.clone(), node);
                if i + 1 < count {
                    self.flush(line, false);
                }
            }
            return;
        }
        if word.len() > crate::fonts::MAX_SHAPE_INPUT_BYTES {
            self.limited = true;
        }
        let run = self.shape(word, s);
        let space = if line.pending_space && !line.items.is_empty() {
            self.shape(" ", s).advance
        } else {
            0.
        };
        if wrap
            && !line.items.is_empty()
            && line.used + space + run.advance > self.float_interval(line).width()
        {
            self.flush(line, false);
        }
        self.fit_empty_line(line, run.advance);
        let width = self.float_interval(line).width();
        if wrap && s.break_words && run.advance > width && word.chars().nth(1).is_some() {
            let runs = self.fonts.break_word_text(
                word,
                s.font_size,
                &s.font_request(),
                &self.page.fonts,
                width,
            );
            let count = runs.len();
            for (i, run) in runs.into_iter().enumerate() {
                self.push_run(run, s, line, href.clone(), node);
                if i + 1 < count {
                    self.flush(line, false);
                }
            }
            return;
        }
        self.pending_space(s, line, href.clone(), node);
        self.push_run(run, s, line, href, node);
    }
    fn push_run(
        &mut self,
        run: Run,
        s: &Style,
        line: &mut Line<'_>,
        href: Option<Arc<str>>,
        node: NodeId,
    ) {
        line.previous_margin = None;
        line.pending_space = false;
        line.used += run.advance;
        line.items.push(Item::Text {
            run,
            color: s.color,
            background: s.background,
            underline: s.underline,
            href,
            node,
            line_height: s.line_height_px(),
        });
    }
    fn flush(&mut self, line: &mut Line<'_>, force: bool) {
        if line.items.is_empty() {
            if force {
                line.y += line.parent.line_height_px();
                line.previous_margin = None;
            }
            line.pending_space = false;
            return;
        }
        let strut = self.shape("", line.parent);
        let leading = (line.parent.line_height_px() - strut.ascent - strut.descent) / 2.;
        let (mut ascent, mut descent) = (strut.ascent + leading, strut.descent + leading);
        for item in &line.items {
            let (a, d) = item.extents();
            ascent = ascent.max(a);
            descent = descent.max(d);
        }
        let height = (ascent + descent).max(0.);
        let baseline = line.y + ascent;
        let slot = self.float_interval(line);
        let extra = (slot.width() - line.used).max(0.);
        let mut x = slot.left
            + match line.parent.align.as_str() {
                "center" => extra / 2.,
                "right" => extra,
                _ => 0.,
            };
        for item in line.items.drain(..) {
            let advance = item.advance();
            match item {
                Item::Text {
                    run,
                    color,
                    background,
                    underline,
                    href,
                    node,
                    ..
                } => {
                    self.commands.owner = node;
                    let rect = Rect {
                        x,
                        y: line.y,
                        width: run.advance,
                        height,
                    };
                    if let Some(color) = background {
                        self.commands.push(DrawCommand::Rect { rect, color });
                    }
                    if let Some(url) = href {
                        self.links.push(Link {
                            node,
                            rect,
                            url: url.into(),
                        });
                    }
                    self.record_inline(node, rect);
                    self.commands.push(DrawCommand::Text {
                        x,
                        baseline,
                        run,
                        color,
                        underline,
                    });
                }
                Item::Fragment(f) => {
                    let y = baseline - f.height;
                    self.append_fragment(f, x, y);
                }
            }
            x += advance;
        }
        line.y += height;
        line.used = 0.;
        line.pending_space = false;
        line.previous_margin = None;
    }
    /// Cached intrinsic estimates for shrink-to-fit and flex auto bases. This
    /// subset handles horizontal text and ordinary blocks; tables, bidi inline
    /// formatting and complete Unicode line breaking need separate algorithms.
    fn intrinsic_width(&mut self, id: NodeId) -> (f32, f32) {
        if self.intrinsic[id.0].is_none() {
            let mut stack = vec![(id, false)];
            while let Some((node, ready)) = stack.pop() {
                if self.intrinsic[node.0].is_some() {
                    continue;
                }
                if ready {
                    self.intrinsic_node(node);
                } else {
                    stack.push((node, true));
                    for &child in self.page.document.nodes[node.0].children.iter().rev() {
                        if self.intrinsic[child.0].is_none() {
                            stack.push((child, false));
                        }
                    }
                }
            }
        }
        self.intrinsic[id.0].unwrap_or((0., 0.))
    }
    fn intrinsic_node(&mut self, id: NodeId) -> (f32, f32) {
        if let Some(size) = self.intrinsic[id.0] {
            return size;
        }
        let s = &self.styles[id.0];
        let d = &self.page.document;
        if s.display == Display::None && d.nodes[id.0].kind.text_data().is_none() {
            self.intrinsic[id.0] = Some((0., 0.));
            return (0., 0.);
        }
        if matches!(s.display, Display::Grid | Display::InlineGrid)
            && (matches!(s.width, Length::Auto | Length::FitContent) || s.width.has_percent())
        {
            let result = self.grid_intrinsic(id);
            self.intrinsic[id.0] = Some(result);
            return result;
        }
        let result = if let Some(text) = d.nodes[id.0].kind.text_data() {
            let merged = self.grid_text.get(&id).and_then(|text| text.merged.clone());
            let text = merged.as_deref().unwrap_or(text);
            let mut min = 0f32;
            let mut max = 0f32;
            let mut current = 0f32;
            let space = self.shape(" ", s).advance;
            // Whitespace at block edges is removed when the line is laid out.
            // Inline wrappers can still share a line with text outside them.
            let (trim_start, trim_end) = d.nodes[id.0]
                .parent
                .filter(|parent| {
                    self.styles[parent.0].display != Display::Inline
                        || self.styles[parent.0].float != css::Float::None
                })
                .map(|parent| {
                    let children = &d.nodes[parent.0].children;
                    (children.first() == Some(&id), children.last() == Some(&id))
                })
                .unwrap_or_default();
            let mut pending_space = false;
            for token in text.split_inclusive(is_css_space) {
                let word = token.trim_end_matches(is_css_space);
                if !word.is_empty() {
                    if pending_space && (!trim_start || current > 0.) {
                        current += space;
                    }
                    pending_space = false;
                    let mut advance = 0.;
                    let mut offset = 0;
                    while offset < word.len() {
                        let mut end = (offset + 4096).min(word.len());
                        while !word.is_char_boundary(end) {
                            end -= 1;
                        }
                        advance += self.shape(&word[offset..end], s).advance;
                        offset = end;
                    }
                    min = min.max(if s.break_words { s.font_size } else { advance });
                    current += advance;
                }
                if word.len() < token.len() {
                    if s.pre && token.ends_with('\n') {
                        max = max.max(current);
                        current = 0.;
                    } else if s.pre {
                        current += space;
                    } else {
                        pending_space = true;
                    }
                }
            }
            if pending_space && !trim_end && (!trim_start || current > 0.) {
                current += space;
            }
            max = max.max(current);
            (if s.wrap { min } else { max }, max)
        } else {
            let m = BoxMetrics::new(s, 0., Constraints::default());
            let extra = m.extra_x();
            if self.is_replaced_control(id) {
                let (width, _) = self.control_intrinsic(id);
                let width = s
                    .width
                    .resolve_with_basis(None, s.font_size)
                    .unwrap_or(width);
                let width = if s.box_sizing == BoxSizing::BorderBox {
                    width.max(extra)
                } else {
                    width + extra
                };
                (width, width)
            } else if self.is_replaced_image(id) {
                let (nw, nh) = self.image_size(id);
                let width = s
                    .width
                    .resolve_with_basis(None, s.font_size)
                    .map(|v| {
                        if s.box_sizing == BoxSizing::BorderBox {
                            v
                        } else {
                            v + extra
                        }
                    })
                    .unwrap_or_else(|| {
                        s.height
                            .resolve_with_basis(None, s.font_size)
                            .filter(|_| !self.is_canvas(id) || nh > 0.)
                            .map(|h| {
                                to_content(h, m.extra_y(), s.box_sizing) * nw / nh.max(1.) + extra
                            })
                            .unwrap_or(nw + extra)
                    });
                (width, width)
            } else if let Some(width) = s.width.resolve_with_basis(None, s.font_size) {
                let width = if s.box_sizing == BoxSizing::BorderBox {
                    width.max(extra)
                } else {
                    width + extra
                };
                (width, width)
            } else if matches!(s.display, Display::Flex | Display::InlineFlex) {
                let (min, max) = self.intrinsic_flex_content(id);
                (min + extra, max + extra)
            } else {
                let (mut min, mut max, mut current) = (0f32, 0f32, 0f32);
                let mut float_sides = [false; 2];
                let mut has_inline_content = false;
                for &child in &d.nodes[id.0].children {
                    let cs = &self.styles[child.0];
                    let text = d.nodes[child.0].kind.text_data().is_some();
                    if !text && (cs.position.out_of_flow() || cs.display == Display::None) {
                        continue;
                    }
                    if !cs.pre
                        && !has_inline_content
                        && matches!(&d.nodes[child.0].kind, NodeKind::Text(t) | NodeKind::Cdata(t) if t.chars().all(is_css_space))
                    {
                        continue;
                    }
                    let block = !text
                        && matches!(
                            cs.display,
                            Display::Block | Display::ListItem | Display::Flex | Display::Grid
                        );
                    let floated = !text && cs.float != css::Float::None;
                    if (block || floated)
                        && cs
                            .clear
                            .sides(s.rtl)
                            .iter()
                            .zip(float_sides)
                            .any(|(clear, occupied)| *clear && occupied)
                    {
                        max = max.max(current);
                        current = 0.;
                        float_sides = [false; 2];
                        has_inline_content = false;
                    }
                    let (a, b) = self.intrinsic[child.0].unwrap_or((0., 0.));
                    if floated {
                        // Adjacent floats share a maximum-content line, but can
                        // wrap separately when sizing to minimum content.
                        let margins = [cs.margin[1], cs.margin[3]]
                            .iter()
                            .filter_map(|edge| edge.resolve_with_basis(None, cs.font_size))
                            .sum::<f32>();
                        min = min.max(a + margins);
                        current += (b + margins).max(0.);
                        float_sides[usize::from(cs.float.is_right(s.rtl))] = true;
                    } else {
                        if block {
                            max = max.max(current).max(b);
                            current = 0.;
                            float_sides = [false; 2];
                            has_inline_content = false;
                        } else {
                            current += b;
                            has_inline_content |= b > 0.;
                        }
                        min = min.max(a);
                    }
                }
                max = max.max(current);
                (min + extra, max + extra)
            }
        };
        self.intrinsic[id.0] = Some(result);
        result
    }
}
