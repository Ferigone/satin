//! Reuse formatting within a document revision at a constant viewport size.
//! Scroll changes only visual positioning, clipping and hit/observer geometry.
use super::*;

pub(crate) struct ScrollFlow {
    styles: Arc<ComputedStyles>,
    background: Color,
    commands: TrackedCommands,
    links: Vec<Link>,
    elements: Vec<ElementBox>,
    anchors: HashMap<String, f32>,
    link_cache: positioning::LinkCache,
    limited: bool,
    positioned: Option<PositionedFlow>,
}

// Only viewport-fixed subtrees need to enter the positioned formatter again.
// Keep its intrinsic measurements, SVGs and remaining budgets from the pass
// that laid out normal flow. Floats begin a fresh context in each fragment.
struct PositionedFlow {
    queue: Vec<positioning::Positioned>,
    intrinsic: Vec<Option<(f32, f32)>>,
    grid_text: HashMap<NodeId, GridText>,
    list_ordinals: HashMap<NodeId, i64>,
    svg_images: HashMap<(usize, u32, u32), Arc<crate::svg::SvgScene>>,
    svg_work_left: usize,
    float_work_remaining: usize,
    work_remaining: usize,
}

impl std::fmt::Debug for ScrollFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrollFlow")
            .field("commands", &self.commands.len())
            .finish_non_exhaustive()
    }
}

impl ScrollFlow {
    pub(super) fn capture(b: &Builder<'_>, background: Color) -> Self {
        Self {
            styles: b.styles.clone(),
            background,
            commands: b.commands.clone(),
            elements: b.elements.clone(),
            links: b.links.clone(),
            anchors: b.anchors.clone(),
            link_cache: b.link_cache.borrow().clone(),
            limited: b.limited,
            positioned: (!b.positioned.is_empty()).then(|| PositionedFlow {
                queue: b.positioned.clone(),
                intrinsic: b.intrinsic.clone(),
                grid_text: b.grid_text.clone(),
                list_ordinals: b.list_ordinals.clone(),
                svg_images: b.svg_images.clone(),
                svg_work_left: b.svg_work_left,
                float_work_remaining: b.float_work_remaining,
                work_remaining: b.work_remaining,
            }),
        }
    }

    pub(crate) fn work(&self) -> usize {
        self.styles
            .len()
            .saturating_add(self.commands.len())
            .saturating_add(self.elements.len())
    }

    pub(crate) fn at_scroll(
        &self,
        page: &LoadedPage,
        fonts: &Fonts,
        viewport: Viewport,
        offsets: &HashMap<NodeId, ScrollOffset>,
        observe: bool,
    ) -> Layout {
        let mut b = Builder::new(page, fonts, &self.styles, viewport);
        b.commands = self.commands.clone();
        b.links = self.links.clone();
        b.elements = self.elements.clone();
        b.anchors = self.anchors.clone();
        b.link_cache = self.link_cache.clone().into();
        b.limited = self.limited;
        if let Some(p) = &self.positioned {
            b.positioned.clone_from(&p.queue);
            b.intrinsic.clone_from(&p.intrinsic);
            b.grid_text.clone_from(&p.grid_text);
            b.list_ordinals.clone_from(&p.list_ordinals);
            b.svg_images.clone_from(&p.svg_images);
            b.svg_work_left = p.svg_work_left;
            b.float_work_remaining = p.float_work_remaining;
            b.work_remaining = p.work_remaining;
        }
        b.finish_frame(self.background, offsets, observe)
    }
}
