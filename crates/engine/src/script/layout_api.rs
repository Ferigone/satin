//! CSSOM View reads use the same bounded layout and immutable snapshot as paint.
//! A write invalidates a revision; consecutive reads reuse the resulting layout.
use super::*;
use crate::{
    css::{BoxSizing, Length, Overflow, Position, Style, TransformOp},
    fonts::Fonts,
    layout::{Affine2D, Layout, Rect, ScrollOffset, Viewport},
};

#[derive(Clone)]
pub(super) struct LayoutHost(Rc<RefCell<Data>>);
pub(super) type GeometryStamp = (usize, Viewport);

struct Data {
    dom: Shared,
    forms: forms::FormHost,
    page: LoadedPage,
    page_revision: u64,
    layout_revision: Option<u64>,
    fonts: Option<Fonts>,
    layout: Option<Arc<Layout>>,
    viewport: Viewport,
    offsets: HashMap<NodeId, ScrollOffset>,
    root_request: Option<ScrollOffset>,
    events: Vec<Option<NodeId>>,
    resize_pending: bool,
    observe_intersections: bool,
    builds: usize,
}
impl Data {
    fn layout_is_valid(&self) -> bool {
        let dom = self.dom.borrow();
        self.layout_revision == Some(dom.revision) && self.page.url == dom.url.as_str()
    }

    fn sync_page(&mut self) {
        let dom = self.dom.borrow();
        let revision = dom.revision;
        let address_changed = self.page.url != dom.url.as_str();
        if self.page_revision == revision && !address_changed {
            return;
        }
        if address_changed {
            self.page.url = dom.url.as_str().to_string();
            self.layout_revision = None;
        }
        drop(dom);
        if self.page_revision != revision {
            self.page.document = document_snapshot(&mut self.dom.borrow_mut());
            self.page.form_controls = Arc::new(
                self.forms
                    .snapshot()
                    .into_iter()
                    .map(|(n, c)| (n, Arc::new(c)))
                    .collect(),
            );
            self.page.refresh_stylesheets();
        }
        self.page.base = document_base(&self.dom.borrow());
        self.page_revision = revision;
    }
    fn invalidate(&mut self) {
        self.layout_revision = None;
    }
    fn event(&mut self, node: Option<NodeId>) {
        if self.events.len() < 1024 && !self.events.contains(&node) {
            self.events.push(node);
        }
    }
}
impl LayoutHost {
    pub(super) fn with_canvas_fonts<T>(
        &self,
        operation: impl FnOnce(&Fonts, &crate::fonts::FontSet) -> T,
    ) -> Result<T, JsError> {
        let mut d = self.0.borrow_mut();
        if d.fonts.is_none() {
            d.fonts = Some(
                Fonts::new().map_err(|message| JsError::new(ErrorKind::InternalError, message))?,
            );
        }
        Ok(operation(d.fonts.as_ref().unwrap(), &d.page.fonts))
    }

    pub(super) fn canvas_font_basis(
        &self,
        vm: &mut Vm,
        node: NodeId,
    ) -> Result<css::canvas_font::Basis, JsError> {
        let layout = self.layout(Some(vm))?;
        let d = self.0.borrow();
        let document = &d.page.document;
        let mut basis = css::canvas_font::Basis {
            viewport: [d.viewport.width, d.viewport.height],
            ..css::canvas_font::Basis::default()
        };
        if let Some(root) = document.html_elements("html").next() {
            basis.root_size = layout.computed_styles[root.0].font_size;
        }
        let mut ancestor = Some(node);
        while let Some(current) = ancestor {
            let Some(style) = layout.computed_styles.get(current.0) else {
                break;
            };
            if current == document.root {
                let style = &layout.computed_styles[node.0];
                basis.size = style.font_size;
                basis.weight = style.font_weight;
                break;
            }
            if style.display == css::Display::None {
                break;
            }
            ancestor = document.nodes.get(current.0).and_then(|node| node.parent);
        }
        Ok(basis)
    }

    pub(super) fn snapshot_into(&self, page: &mut LoadedPage) {
        let mut d = self.0.borrow_mut();
        d.sync_page();
        page.url.clone_from(&d.page.url);
        page.document = d.page.document.clone();
        page.form_controls = d.page.form_controls.clone();
        page.base = d.page.base.clone();
        page.stylesheets = d.page.stylesheets.clone();
        page.stylesheet_media = d.page.stylesheet_media.clone();
        page.fonts = d.page.fonts.clone();
    }
    pub(super) fn update_resources(&self, page: &LoadedPage) {
        let mut d = self.0.borrow_mut();
        d.page.fonts = page.fonts.clone();
        d.page.stylesheets = page.stylesheets.clone();
        d.page.stylesheet_media = page.stylesheet_media.clone();
        d.page.linked_stylesheets = page.linked_stylesheets.clone();
        d.page.images = page.images.clone();
        d.page.svg_images = page.svg_images.clone();
        d.page.image_references = page.image_references.clone();
        d.page_revision = u64::MAX;
        d.invalidate();
        d.dom.borrow_mut().dirty = true;
    }
    pub(super) fn set_viewport(&self, mut viewport: Viewport) {
        viewport.width = finite(viewport.width).clamp(1., 16384.);
        viewport.height = finite(viewport.height).clamp(1., 16384.);
        viewport.scroll_x = finite(viewport.scroll_x).max(0.);
        viewport.scroll_y = finite(viewport.scroll_y).max(0.);
        let mut d = self.0.borrow_mut();
        let old = d.viewport;
        if (viewport.width != old.width || viewport.height != old.height)
            && d.dom.borrow().ready_state == "complete"
        {
            d.resize_pending = true;
        }
        if viewport.width != old.width
            || viewport.height != old.height
            || viewport.scroll_x != old.scroll_x
            || (viewport.scroll_y != old.scroll_y
                && d.layout
                    .as_ref()
                    .is_some_and(|l| l.depends_on_viewport_scroll))
        {
            d.invalidate();
        }
        if viewport.scroll_x != old.scroll_x || viewport.scroll_y != old.scroll_y {
            d.event(None);
        }
        d.viewport = viewport;
    }
    pub(super) fn viewport(&self) -> Viewport {
        self.0.borrow().viewport
    }
    pub(super) fn layout(&self, mut vm: Option<&mut Vm>) -> Result<Arc<Layout>, JsError> {
        let mut d = self.0.borrow_mut();
        let revision = d.dom.borrow().revision;
        if d.layout_is_valid()
            && let Some(layout) = &d.layout
        {
            return Ok(layout.clone());
        }
        // Charge native work before copying DOM data, CSS parsing or shaping.
        let source_work = d
            .page
            .stylesheets
            .iter()
            .map(|sheet| sheet.len())
            .sum::<usize>()
            .saturating_add(
                d.page
                    .linked_stylesheets
                    .values()
                    .map(|sheet| sheet.len())
                    .sum::<usize>(),
            )
            .saturating_add(
                d.page
                    .svg_images
                    .values()
                    .map(|a| a.retained_bytes())
                    .sum::<usize>(),
            );
        if let Some(vm) = vm.as_deref_mut() {
            let state = d.dom.borrow();
            vm.charge_work(
                state
                    .document
                    .nodes
                    .len()
                    .saturating_mul(32)
                    .saturating_add(state.dom_bytes)
                    .saturating_add(source_work),
            )?;
        }
        d.sync_page();
        if d.fonts.is_none() {
            d.fonts = Some(Fonts::new().map_err(|m| JsError::new(ErrorKind::InternalError, m))?);
        }
        let mut layout = Arc::new(Layout::build_for_viewport(
            &d.page,
            d.fonts.as_ref().unwrap(),
            d.viewport,
            &d.offsets,
            d.observe_intersections,
        ));
        let mut native_svg_work = svg_work(&layout);
        let x = d
            .viewport
            .scroll_x
            .clamp(0., (layout.content_width - d.viewport.width).max(0.));
        let y = d
            .viewport
            .scroll_y
            .clamp(0., (layout.height - d.viewport.height).max(0.));
        if x != d.viewport.scroll_x || y != d.viewport.scroll_y {
            let rebuild = layout.depends_on_viewport_scroll || x != d.viewport.scroll_x;
            d.viewport.scroll_x = x;
            d.viewport.scroll_y = y;
            d.root_request = Some(ScrollOffset { x, y });
            d.event(None);
            if rebuild {
                if let Some(vm) = vm.as_deref_mut() {
                    vm.charge_work(
                        source_work
                            .saturating_add(d.dom.borrow().document.nodes.len().saturating_mul(32)),
                    )?;
                }
                layout = Arc::new(Layout::build_for_viewport(
                    &d.page,
                    d.fonts.as_ref().unwrap(),
                    d.viewport,
                    &d.offsets,
                    d.observe_intersections,
                ));
                native_svg_work = native_svg_work.saturating_add(svg_work(&layout));
            }
        }
        // Commit effective offsets, so a later growth cannot resurrect a value
        // that was clamped when the scrolling area shrank.
        let old_offsets = std::mem::take(&mut d.offsets);
        for region in &layout.scroll_regions {
            if let Some(old) = old_offsets.get(&region.node)
                && *old != region.offset
            {
                d.event(Some(region.node));
            }
            if region.offset != ScrollOffset::default() {
                d.offsets.insert(region.node, region.offset);
            }
        }
        if let Some(vm) = vm {
            vm.charge_work(native_svg_work)?;
        }
        d.layout_revision = Some(revision);
        d.builds += 1;
        d.layout = Some(layout.clone());
        Ok(layout)
    }
    pub(super) fn geometry_stamp(&self) -> Option<GeometryStamp> {
        let d = self.0.borrow();
        d.layout_is_valid().then_some((d.builds, d.viewport))
    }

    pub(super) fn observer_layout(&self) -> Result<(Arc<Layout>, GeometryStamp), JsError> {
        {
            let mut d = self.0.borrow_mut();
            if !d.observe_intersections {
                d.observe_intersections = true;
                d.invalidate();
            }
        }
        let layout = self.layout(None)?;
        let d = self.0.borrow();
        Ok((layout, (d.builds, d.viewport)))
    }
    fn root_element(&self, node: NodeId) -> bool {
        let d = self.0.borrow();
        scrolling_element(&d.page.document) == Some(node)
    }
    fn scroll(
        &self,
        vm: &mut Vm,
        node: Option<NodeId>,
        x: Option<f32>,
        y: Option<f32>,
        relative: bool,
    ) -> Result<(), JsError> {
        let layout = self.layout(Some(vm))?;
        let node = node.filter(|n| !self.root_element(*n));
        let mut d = self.0.borrow_mut();
        if let Some(node) = node {
            let Some(region) = layout.scroll_regions.iter().find(|r| r.node == node) else {
                return Ok(());
            };
            let old = region.offset;
            let next = ScrollOffset {
                x: x.map_or(old.x, |v| finite(v) + if relative { old.x } else { 0. })
                    .clamp(0., region.max_x),
                y: y.map_or(old.y, |v| finite(v) + if relative { old.y } else { 0. })
                    .clamp(0., region.max_y),
            };
            if next != old {
                d.offsets.insert(node, next);
                d.invalidate();
                d.event(Some(node));
            }
        } else {
            let old = d.viewport;
            let max_x = (layout.content_width - old.width).max(0.);
            let next = ScrollOffset {
                x: x.map_or(old.scroll_x, |v| {
                    finite(v) + if relative { old.scroll_x } else { 0. }
                })
                .clamp(0., max_x),
                y: y.map_or(old.scroll_y, |v| {
                    finite(v) + if relative { old.scroll_y } else { 0. }
                })
                .clamp(0., (layout.height - old.height).max(0.)),
            };
            if next.x != old.scroll_x || next.y != old.scroll_y {
                d.viewport.scroll_x = next.x;
                d.viewport.scroll_y = next.y;
                if layout.depends_on_viewport_scroll || next.x != old.scroll_x {
                    d.invalidate();
                }
                d.root_request = Some(next);
                d.event(None);
            }
        }
        Ok(())
    }
    pub(super) fn take_root_request(&self) -> Option<ScrollOffset> {
        self.0.borrow_mut().root_request.take()
    }
    pub(super) fn has_events(&self) -> bool {
        let d = self.0.borrow();
        d.resize_pending || !d.events.is_empty()
    }
    pub(super) fn take_resize(&self) -> bool {
        std::mem::take(&mut self.0.borrow_mut().resize_pending)
    }
    pub(super) fn take_events(&self) -> Vec<Option<NodeId>> {
        std::mem::take(&mut self.0.borrow_mut().events)
    }
    pub(super) fn install(
        vm: &mut Vm,
        state: &Shared,
        forms: forms::FormHost,
        page: &LoadedPage,
    ) -> Result<Self, JsError> {
        let host = Self(Rc::new(RefCell::new(Data {
            dom: state.clone(),
            forms,
            page: LoadedPage {
                canvas_frames: state.borrow().canvases.frames.clone(),
                ..page.clone()
            },
            page_revision: 0,
            layout_revision: None,
            fonts: None,
            layout: None,
            viewport: Viewport::default(),
            offsets: HashMap::new(),
            root_request: None,
            events: vec![],
            resize_pending: false,
            observe_intersections: false,
            builds: 0,
        })));
        let prototype = state.borrow().prototype.clone();
        let window = state.borrow().window.clone();
        let document_root = state.borrow().document.root;
        let document = wrap(vm, state, Some(document_root))?;
        let s = state.clone();
        getter(vm, document, "scrollingElement", move |vm, _, _| {
            let element = scrolling_element(&s.borrow().document);
            wrap(vm, &s, element)
        })?;
        let array = vm.get_global("Array")?;
        let array_proto = vm.get_property(array, "prototype")?;
        let rect_values = vm.get_property(array_proto, "values")?;
        // Retain the intrinsic across replacement of page globals/prototypes.
        vm.root(rect_values.clone());
        for name in ["getBoundingClientRect", "getClientRects"] {
            let h = host.clone();
            let s = state.clone();
            let rect_values = rect_values.clone();
            method(vm, prototype.clone(), name, move |vm, this, _| {
                let id = element(&s, &this)?;
                let layout = h.layout(Some(vm))?;
                let view = h.viewport();
                let rects = layout
                    .rects_for(id)
                    .into_iter()
                    .map(|r| Rect {
                        x: r.x - view.scroll_x,
                        y: r.y - view.scroll_y,
                        ..r
                    })
                    .collect::<Vec<_>>();
                if name == "getBoundingClientRect" {
                    return dom_rect(vm, bounding_rect(&rects));
                }
                let object = vm.new_object();
                host_tag(vm, object.clone(), "DOMRectList")?;
                for (i, rect) in rects.iter().enumerate() {
                    let value = dom_rect(vm, *rect)?;
                    vm.define_data_property(
                        object.clone(),
                        &i.to_string(),
                        value,
                        false,
                        true,
                        false,
                    )?;
                }
                vm.define_data_property(
                    object.clone(),
                    "length",
                    rects.len().into(),
                    false,
                    false,
                    true,
                )?;
                method(vm, object.clone(), "item", |vm, this, args| {
                    let index = vm.to_number(&arg(args, 0))?;
                    let index = if index.is_finite() {
                        index.trunc().rem_euclid(4294967296.) as u32
                    } else {
                        0
                    };
                    let value = vm.get_property(this, &index.to_string())?;
                    Ok(if matches!(value, Value::Undefined) {
                        Value::Null
                    } else {
                        value
                    })
                })?;
                if let Some(symbol) = vm.well_known_symbol("iterator") {
                    let key = vm.to_property_key(&symbol)?;
                    vm.define_data_property_key(
                        object.clone(),
                        key,
                        rect_values.clone(),
                        false,
                        false,
                        true,
                    )?;
                }
                Ok(object)
            })?;
        }
        for name in [
            "offsetWidth",
            "offsetHeight",
            "offsetLeft",
            "offsetTop",
            "offsetParent",
            "clientWidth",
            "clientHeight",
            "clientLeft",
            "clientTop",
            "scrollWidth",
            "scrollHeight",
        ] {
            let h = host.clone();
            let s = state.clone();
            getter(vm, prototype.clone(), name, move |vm, this, _| {
                let id = element(&s, &this)?;
                let layout = h.layout(Some(vm))?;
                let d = h.0.borrow();
                let doc = &d.page.document;
                if name == "offsetParent" {
                    let parent = offset_parent(doc, &layout, id);
                    drop(d);
                    return wrap(vm, &s, parent);
                }
                if h.root_element(id)
                    && matches!(
                        name,
                        "clientWidth" | "clientHeight" | "scrollWidth" | "scrollHeight"
                    )
                {
                    let v = d.viewport;
                    return Ok(number(
                        (match name {
                            "clientWidth" => v.width,
                            "clientHeight" => v.height,
                            "scrollHeight" => layout.height.max(v.height),
                            _ => layout.content_width.max(v.width),
                        })
                        .round(),
                    ));
                }
                let Some(rect) = layout.flow_rect_for(id) else {
                    return Ok(0.into());
                };
                let style = &layout.computed_styles[id.0];
                let border = style.borders();
                if doc.tag(id) == "body" && matches!(name, "offsetLeft" | "offsetTop") {
                    return Ok(0.into());
                }
                let parent = offset_parent(doc, &layout, id)
                    // An unpositioned body returned as offsetParent uses the
                    // initial containing block's origin for its offsets.
                    .filter(|p| {
                        let body = &layout.computed_styles[p.0];
                        doc.tag(*p) != "body"
                            || body.position != Position::Static
                            || !body.transform.is_empty()
                    })
                    .and_then(|p| layout.flow_rect_for(p).map(|r| (p, r)));
                let content = layout.scroll_size_for(id).unwrap_or((
                    (rect.width - border[1] - border[3]).max(0.),
                    (rect.height - border[0] - border[2]).max(0.),
                ));
                let n = match name {
                    "offsetWidth" => rect.width,
                    "offsetHeight" => rect.height,
                    "offsetLeft" => {
                        rect.x
                            - parent
                                .map_or(0., |(p, r)| r.x + layout.computed_styles[p.0].borders()[3])
                    }
                    "offsetTop" => {
                        rect.y
                            - parent
                                .map_or(0., |(p, r)| r.y + layout.computed_styles[p.0].borders()[0])
                    }
                    "clientWidth" => (rect.width - border[1] - border[3]).max(0.),
                    "clientHeight" => (rect.height - border[0] - border[2]).max(0.),
                    "clientLeft" => border[3],
                    "clientTop" => border[0],
                    "scrollWidth" => content.0,
                    "scrollHeight" => content.1,
                    _ => 0.,
                };
                Ok(number(n.round()))
            })?;
        }
        for name in ["scrollTop", "scrollLeft"] {
            let h = host.clone();
            let s = state.clone();
            let get = vm.new_native_function(name, move |vm, this, _| {
                let id = element(&s, &this)?;
                let l = h.layout(Some(vm))?;
                if h.root_element(id) {
                    let v = h.viewport();
                    return Ok(number(if name == "scrollTop" {
                        v.scroll_y
                    } else {
                        v.scroll_x
                    }));
                }
                let n = l
                    .scroll_regions
                    .iter()
                    .find(|r| r.node == id)
                    .map_or(0., |r| {
                        if name == "scrollTop" {
                            r.offset.y
                        } else {
                            r.offset.x
                        }
                    });
                Ok(number(n))
            });
            let h = host.clone();
            let s = state.clone();
            let set = vm.new_native_function(name, move |vm, this, args| {
                let id = element(&s, &this)?;
                let n = vm.to_number(&arg(args, 0))? as f32;
                h.scroll(
                    vm,
                    Some(id),
                    if name == "scrollLeft" { Some(n) } else { None },
                    if name == "scrollTop" { Some(n) } else { None },
                    false,
                )?;
                Ok(Value::Undefined)
            });
            vm.define_accessor(prototype.clone(), name, Some(get), Some(set))?;
        }
        for object in [prototype, window.clone()] {
            let is_window = object == window;
            for name in ["scroll", "scrollTo", "scrollBy"] {
                let h = host.clone();
                let s = state.clone();
                method(vm, object.clone(), name, move |vm, this, args| {
                    let target = if is_window && this.is_nullish() {
                        None
                    } else {
                        target(&s, &this)?
                    };
                    let (x, y) = scroll_arguments(vm, args)?;
                    h.scroll(vm, target, x, y, name == "scrollBy")?;
                    Ok(Value::Undefined)
                })?;
            }
        }
        for name in [
            "innerWidth",
            "innerHeight",
            "scrollX",
            "scrollY",
            "pageXOffset",
            "pageYOffset",
        ] {
            let h = host.clone();
            getter(vm, window.clone(), name, move |vm, _, _| {
                if !matches!(name, "innerWidth" | "innerHeight") {
                    h.layout(Some(vm))?;
                }
                let v = h.viewport();
                Ok(number(match name {
                    "innerWidth" => v.width,
                    "innerHeight" => v.height,
                    "scrollX" | "pageXOffset" => v.scroll_x,
                    _ => v.scroll_y,
                }))
            })?;
        }
        let h = host.clone();
        let s = state.clone();
        method(vm, window, "getComputedStyle", move |vm, _, args| {
            let id = element(&s, &arg(args, 0))?;
            if !arg(args, 1).is_nullish() && !text(vm, arg(args, 1))?.is_empty() {
                return Err(unsupported(
                    "Computed pseudo-element styles are not implemented",
                ));
            }
            h.layout(Some(vm))?;
            computed_style(vm, h.clone(), id)
        })?;
        Ok(host)
    }
}

fn svg_work(layout: &Layout) -> usize {
    let mut seen = std::collections::HashSet::new();
    layout
        .commands
        .iter()
        .filter_map(|c| {
            if let crate::layout::DrawCommand::Svg { scene, .. } = c {
                seen.insert(Arc::as_ptr(scene) as usize)
                    .then_some(scene.diagnostics.work)
            } else {
                None
            }
        })
        .fold(0usize, usize::saturating_add)
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0. }
}
fn number(v: f32) -> Value {
    Value::Number(v as f64)
}
fn bounding_rect(rects: &[Rect]) -> Rect {
    let mut nonempty = rects.iter().filter(|r| r.width != 0. || r.height != 0.);
    let Some(first) = nonempty.next().copied() else {
        return rects.first().copied().unwrap_or_default();
    };
    nonempty.fold(first, |a, b| {
        let x = a.x.min(b.x);
        let y = a.y.min(b.y);
        Rect {
            x,
            y,
            width: (a.x + a.width).max(b.x + b.width) - x,
            height: (a.y + a.height).max(b.y + b.height) - y,
        }
    })
}
fn scrolling_element(doc: &Document) -> Option<NodeId> {
    let part = if doc.quirks_mode == crate::dom::QuirksMode::Quirks {
        "body"
    } else {
        "documentElement"
    };
    document_part(doc, part)
}

fn offset_parent(doc: &Document, layout: &Layout, id: NodeId) -> Option<NodeId> {
    if layout.flow_rect_for(id).is_none() || matches!(doc.tag(id), "html" | "body") {
        return None;
    }
    let fixed = layout.computed_styles[id.0].position == Position::Fixed;
    let mut parent = doc.nodes[id.0].parent;
    while let Some(p) = parent {
        let style = &layout.computed_styles[p.0];
        if fixed {
            if !style.transform.is_empty() {
                return Some(p);
            }
            parent = doc.nodes[p.0].parent;
            continue;
        }
        if doc.tag(p) == "body"
            || style.position != Position::Static
            || !style.transform.is_empty()
            || (layout.computed_styles[id.0].position == Position::Static
                && matches!(doc.tag(p), "td" | "th" | "table"))
        {
            return Some(p);
        }
        parent = doc.nodes[p.0].parent;
    }
    None
}
fn containing_box(doc: &Document, layout: &Layout, id: NodeId, viewport: Viewport) -> Rect {
    let style = &layout.computed_styles[id.0];
    let mut parent = doc.nodes[id.0].parent;
    while let Some(node) = parent {
        let s = &layout.computed_styles[node.0];
        let transformed = !s.transform.is_empty()
            && (s.display != css::Display::Inline
                || doc.tag(node) == "img"
                || doc.tag(node) == "svg");
        let qualifies = if style.position == Position::Fixed {
            transformed
        } else if style.position == Position::Absolute {
            s.position != Position::Static || transformed
        } else {
            !matches!(s.display, css::Display::Inline | css::Display::None)
        };
        if qualifies && let Some(r) = layout.flow_rect_for(node) {
            let b = s.borders();
            let padding = if style.position.out_of_flow() {
                [0.; 4]
            } else {
                s.padding
                    .map(|l| l.resolve(r.width, s.font_size).unwrap_or(0.))
            };
            return Rect {
                x: r.x + b[3] + padding[3],
                y: r.y + b[0] + padding[0],
                width: (r.width - b[1] - b[3] - padding[1] - padding[3]).max(0.),
                height: (r.height - b[0] - b[2] - padding[0] - padding[2]).max(0.),
            };
        }
        parent = doc.nodes[node.0].parent;
    }
    Rect {
        x: 0.,
        y: 0.,
        width: viewport.width,
        height: viewport.height,
    }
}
fn scroll_arguments(vm: &mut Vm, args: &[Value]) -> Result<(Option<f32>, Option<f32>), JsError> {
    let first = arg(args, 0);
    if first.as_object().is_some() {
        let left = vm.get_property(first.clone(), "left")?;
        let top = vm.get_property(first.clone(), "top")?;
        let behavior = vm.get_property(first, "behavior")?;
        if !matches!(behavior, Value::Undefined) {
            let behavior = text(vm, behavior)?;
            if !matches!(behavior.as_str(), "auto" | "instant" | "smooth") {
                return Err(JsError::type_error("Invalid ScrollBehavior"));
            }
            if behavior == "smooth" {
                return Err(unsupported("Smooth scroll animation is not implemented"));
            }
        }
        Ok((
            if matches!(left, Value::Undefined) {
                None
            } else {
                Some(vm.to_number(&left)? as f32)
            },
            if matches!(top, Value::Undefined) {
                None
            } else {
                Some(vm.to_number(&top)? as f32)
            },
        ))
    } else if args.len() >= 2 {
        Ok((
            Some(vm.to_number(&first)? as f32),
            Some(vm.to_number(&arg(args, 1))? as f32),
        ))
    } else {
        Ok((None, None))
    }
}
fn host_tag(vm: &mut Vm, object: Value, name: &str) -> Result<(), JsError> {
    if let Some(symbol) = vm.well_known_symbol("toStringTag") {
        let key = vm.to_property_key(&symbol)?;
        vm.define_data_property_key(object, key, name.into(), false, false, true)?;
    }
    Ok(())
}
fn dom_rect(vm: &mut Vm, rect: Rect) -> Result<Value, JsError> {
    let object = vm.new_object();
    host_tag(vm, object.clone(), "DOMRect")?;
    for (key, n) in [
        ("x", rect.x),
        ("y", rect.y),
        ("width", rect.width),
        ("height", rect.height),
    ] {
        vm.set_property(object.clone(), key, number(n))?;
    }
    for name in ["left", "top", "right", "bottom"] {
        getter(vm, object.clone(), name, move |vm, this, _| {
            let (a, b) = if matches!(name, "left" | "right") {
                ("x", "width")
            } else {
                ("y", "height")
            };
            let av = vm.get_property(this.clone(), a)?;
            let bv = vm.get_property(this, b)?;
            let a = vm.to_number(&av)?;
            let b = vm.to_number(&bv)?;
            Ok(if matches!(name, "left" | "top") {
                a.min(a + b)
            } else {
                a.max(a + b)
            }
            .into())
        })?;
    }
    method(vm, object.clone(), "toJSON", |vm, this, _| {
        let result = vm.new_object();
        for key in [
            "x", "y", "width", "height", "top", "right", "bottom", "left",
        ] {
            let value = vm.get_property(this.clone(), key)?;
            vm.set_property(result.clone(), key, value)?;
        }
        Ok(result)
    })?;
    Ok(object)
}

const PROPERTIES: &[(&str, &str)] = &[
    ("display", "display"),
    ("position", "position"),
    ("top", "top"),
    ("right", "right"),
    ("bottom", "bottom"),
    ("left", "left"),
    ("zIndex", "z-index"),
    ("overflowX", "overflow-x"),
    ("overflowY", "overflow-y"),
    ("transform", "transform"),
    ("transformOrigin", "transform-origin"),
    ("color", "color"),
    ("backgroundColor", "background-color"),
    ("fontSize", "font-size"),
    ("fontWeight", "font-weight"),
    ("fontStyle", "font-style"),
    ("fontFamily", "font-family"),
    ("lineHeight", "line-height"),
    ("width", "width"),
    ("height", "height"),
    ("minWidth", "min-width"),
    ("maxWidth", "max-width"),
    ("minHeight", "min-height"),
    ("maxHeight", "max-height"),
    ("boxSizing", "box-sizing"),
    ("marginTop", "margin-top"),
    ("marginRight", "margin-right"),
    ("marginBottom", "margin-bottom"),
    ("marginLeft", "margin-left"),
    ("paddingTop", "padding-top"),
    ("paddingRight", "padding-right"),
    ("paddingBottom", "padding-bottom"),
    ("paddingLeft", "padding-left"),
    ("borderTopWidth", "border-top-width"),
    ("borderRightWidth", "border-right-width"),
    ("borderBottomWidth", "border-bottom-width"),
    ("borderLeftWidth", "border-left-width"),
];
fn computed_style(vm: &mut Vm, host: LayoutHost, id: NodeId) -> Result<Value, JsError> {
    let object = vm.new_object();
    host_tag(vm, object.clone(), "CSSStyleProperties")?;
    for (property, css_name) in PROPERTIES {
        for name in [*property, *css_name] {
            let h = host.clone();
            getter(vm, object.clone(), name, move |vm, _, _| {
                computed_value(vm, &h, id, css_name)
            })?;
            if property == css_name {
                break;
            }
        }
    }
    let h = host.clone();
    method(
        vm,
        object.clone(),
        "getPropertyValue",
        move |vm, _, args| {
            let name = text(vm, arg(args, 0))?;
            computed_value(vm, &h, id, &name)
        },
    )?;
    method(vm, object.clone(), "getPropertyPriority", |_, _, _| {
        Ok("".into())
    })?;
    for name in ["setProperty", "removeProperty"] {
        method(vm, object.clone(), name, |vm, _, _| {
            let value = vm.new_object();
            vm.set_property(value.clone(), "name", "NoModificationAllowedError".into())?;
            vm.set_property(
                value.clone(),
                "message",
                "Computed styles are read-only".into(),
            )?;
            Err(JsError {
                kind: ErrorKind::Error,
                message: "Computed styles are read-only".into(),
                line: None,
                column: None,
                thrown: Some(value),
            })
        })?;
    }
    vm.define_data_property(
        object.clone(),
        "length",
        PROPERTIES.len().into(),
        false,
        false,
        true,
    )?;
    for (i, (_, name)) in PROPERTIES.iter().enumerate() {
        vm.define_data_property(
            object.clone(),
            &i.to_string(),
            (*name).into(),
            false,
            true,
            true,
        )?;
    }
    method(vm, object.clone(), "item", |vm, this, args| {
        let i = vm.to_number(&arg(args, 0))? as u32;
        let v = vm.get_property(this, &i.to_string())?;
        Ok(if matches!(v, Value::Undefined) {
            "".into()
        } else {
            v
        })
    })?;
    getter(vm, object.clone(), "cssText", |_, _, _| Ok("".into()))?;
    Ok(object)
}
fn px(n: f32) -> String {
    format!("{}px", if n == 0. { 0. } else { n })
}
fn length(v: Length, base: f32, font: f32) -> String {
    v.resolve(base, font)
        .map(px)
        .unwrap_or_else(|| "auto".into())
}
fn unresolved_percentage(v: Length) -> Option<String> {
    match v {
        Length::Percent(n) => Some(format!("{n}%")),
        _ => unresolved_calculation(v),
    }
}
fn unresolved_calculation(v: Length) -> Option<String> {
    let (pixels, percent) = v.percentage_calculation()?;
    let percent = percent + 0.;
    if pixels == 0. {
        Some(format!("{percent}%"))
    } else {
        let operator = if pixels < 0. { '-' } else { '+' };
        Some(format!("calc({percent}% {operator} {})", px(pixels.abs())))
    }
}
fn color(v: css::Color) -> String {
    if v.3 == 255 {
        format!("rgb({}, {}, {})", v.0, v.1, v.2)
    } else {
        format!("rgba({}, {}, {}, {})", v.0, v.1, v.2, v.3 as f32 / 255.)
    }
}
fn overflow(v: Overflow) -> &'static str {
    match v {
        Overflow::Visible => "visible",
        Overflow::Hidden => "hidden",
        Overflow::Clip => "clip",
        Overflow::Auto => "auto",
        Overflow::Scroll => "scroll",
    }
}
fn transform(style: &Style, rect: Rect) -> Affine2D {
    style.transform.iter().fold(Affine2D::IDENTITY, |m, op| {
        m.multiply(match *op {
            TransformOp::Matrix([a, b, c, d, e, f]) => Affine2D { a, b, c, d, e, f },
            TransformOp::Translate([x, y]) => Affine2D::translation(
                x.resolve(rect.width, style.font_size).unwrap_or(0.),
                y.resolve(rect.height, style.font_size).unwrap_or(0.),
            ),
            TransformOp::Scale([a, d]) => Affine2D {
                a,
                d,
                ..Affine2D::IDENTITY
            },
            TransformOp::Rotate(angle) => Affine2D {
                a: angle.cos(),
                b: angle.sin(),
                c: -angle.sin(),
                d: angle.cos(),
                e: 0.,
                f: 0.,
            },
            TransformOp::Skew([x, y]) => Affine2D {
                b: y.tan(),
                c: x.tan(),
                ..Affine2D::IDENTITY
            },
        })
    })
}
fn computed_value(
    vm: &mut Vm,
    host: &LayoutHost,
    id: NodeId,
    name: &str,
) -> Result<Value, JsError> {
    let layout = host.layout(Some(vm))?;
    let d = host.0.borrow();
    let doc = &d.page.document;
    let mut root = id;
    while let Some(parent) = doc.nodes[root.0].parent {
        root = parent;
    }
    if root != doc.root {
        return Ok("".into());
    }
    let style = &layout.computed_styles[id.0];
    let rect = layout.flow_rect_for(id);
    let containing = containing_box(doc, &layout, id, d.viewport);
    let parent_width = containing.width;
    let border = style.borders();
    let padding = style.padding.map(|l| {
        l.resolve(parent_width, style.font_size)
            .unwrap_or(0.)
            .max(0.)
    });
    let value = match name {
        "display" => format!("{:?}", style.display)
            .to_ascii_lowercase()
            .replace("inlineblock", "inline-block")
            .replace("inlineflex", "inline-flex")
            .replace("inlinegrid", "inline-grid"),
        "position" => format!("{:?}", style.position).to_ascii_lowercase(),
        "color" => color(style.color),
        "background-color" => color(style.background.unwrap_or(css::Color(0, 0, 0, 0))),
        "font-size" => px(style.font_size),
        "font-weight" => style.font_weight.to_string(),
        "font-style" => style.font_style.as_css().to_owned(),
        "font-family" => style.font_family_css(),
        "line-height" => px(style.line_height_px()),
        "z-index" => style
            .z_index
            .map(|n| n.to_string())
            .unwrap_or_else(|| "auto".into()),
        "overflow-x" => overflow(style.overflow[0]).into(),
        "overflow-y" => overflow(style.overflow[1]).into(),
        "box-sizing" => if style.box_sizing == BoxSizing::BorderBox {
            "border-box"
        } else {
            "content-box"
        }
        .into(),
        "width" | "height" => {
            if let Some(rect) = rect {
                let value = if name == "width" {
                    rect.width
                } else {
                    rect.height
                };
                let subtract = if style.box_sizing == BoxSizing::ContentBox {
                    if name == "width" {
                        border[1] + border[3] + padding[1] + padding[3]
                    } else {
                        border[0] + border[2] + padding[0] + padding[2]
                    }
                } else {
                    0.
                };
                px((value - subtract).max(0.))
            } else {
                let size = if name == "width" {
                    style.width
                } else {
                    style.height
                };
                unresolved_percentage(size)
                    .unwrap_or_else(|| length(size, parent_width, style.font_size))
            }
        }
        "min-width" | "max-width" | "min-height" | "max-height" => {
            let (size, base) = match name {
                "min-width" => (style.min_width, parent_width),
                "max-width" => (style.max_width, parent_width),
                "min-height" => (style.min_height, d.viewport.height),
                _ => (style.max_height, d.viewport.height),
            };
            if size == Length::Auto && name.starts_with("max") {
                "none".into()
            } else if let Some(value) = unresolved_calculation(size) {
                value
            } else {
                length(size, base, style.font_size)
            }
        }
        "top" | "right" | "bottom" | "left" => {
            let i = ["top", "right", "bottom", "left"]
                .iter()
                .position(|k| *k == name)
                .unwrap();
            let inset = style.inset[i];
            let axis = usize::from(i % 2 == 0);
            if style.position == Position::Sticky {
                if let Some(sizes) = layout.sticky_inset_sizes.get(&id) {
                    length(inset, sizes[axis], style.font_size)
                } else {
                    unresolved_percentage(inset)
                        .unwrap_or_else(|| length(inset, 0., style.font_size))
                }
            } else if style.position == Position::Static
                && let Some(value) = unresolved_percentage(inset)
            {
                value
            } else {
                length(
                    inset,
                    [containing.width, containing.height][axis],
                    style.font_size,
                )
            }
        }
        "transform" => {
            if style.transform.is_empty() {
                "none".into()
            } else {
                let m = transform(style, rect.unwrap_or_default());
                format!(
                    "matrix({}, {}, {}, {}, {}, {})",
                    m.a, m.b, m.c, m.d, m.e, m.f
                )
            }
        }
        "transform-origin" => {
            let r = rect.unwrap_or_default();
            format!(
                "{} {}",
                length(style.transform_origin[0], r.width, style.font_size),
                length(style.transform_origin[1], r.height, style.font_size)
            )
        }
        _ => {
            if let Some(i) = ["top", "right", "bottom", "left"]
                .iter()
                .position(|side| name == format!("margin-{side}"))
            {
                length(style.margin[i], parent_width, style.font_size)
            } else if let Some(i) = ["top", "right", "bottom", "left"]
                .iter()
                .position(|side| name == format!("padding-{side}"))
            {
                px(padding[i])
            } else if let Some(i) = ["top", "right", "bottom", "left"]
                .iter()
                .position(|side| name == format!("border-{side}-width"))
            {
                px(border[i])
            } else if name.starts_with("--") {
                style.custom_property(name).unwrap_or("").to_owned()
            } else {
                String::new()
            }
        }
    };
    Ok(value.into())
}

impl ScriptRuntime {
    /// Resource completion can change font metrics or CSS without a DOM write.
    pub fn update_resources(&self, page: &LoadedPage) {
        self.state.borrow_mut().canvases.update_sources(page);
        self.layout.update_resources(page);
    }
    pub fn set_viewport(&self, viewport: Viewport) {
        self.layout.set_viewport(viewport);
    }
    pub fn viewport(&self) -> Viewport {
        self.layout.viewport()
    }
    pub fn layout_for_viewport(&self, viewport: Viewport) -> Result<Arc<Layout>, JsError> {
        self.publish_canvas_frames();
        self.layout.set_viewport(viewport);
        self.layout.layout(None)
    }
    pub fn take_scroll_request(&self) -> Option<ScrollOffset> {
        self.layout.take_root_request()
    }
    pub fn layout_needs_refresh(&self) -> bool {
        let d = self.layout.0.borrow();
        d.root_request.is_some() || !d.layout_is_valid()
    }

    /// Compare the painter's snapshot even when a script read has already rebuilt layout.
    pub fn layout_is_current(&self, snapshot: &Arc<Layout>) -> bool {
        !self.layout_needs_refresh()
            && self
                .layout
                .0
                .borrow()
                .layout
                .as_ref()
                .is_some_and(|layout| Arc::ptr_eq(layout, snapshot))
    }
    pub fn scroll_element(&mut self, node: NodeId, dx: f32, dy: f32) -> Result<bool, JsError> {
        let host = self.layout.clone();
        let before = host
            .0
            .borrow()
            .offsets
            .get(&node)
            .copied()
            .unwrap_or_default();
        self.vm
            .with_task(|vm| host.scroll(vm, Some(node), Some(dx), Some(dy), true))?;
        Ok(before
            != host
                .0
                .borrow()
                .offsets
                .get(&node)
                .copied()
                .unwrap_or_default())
    }
}
