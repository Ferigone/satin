use super::{Position, Rect, ScrollOffset, Style, Viewport, layout_overflow, padding_box};
use crate::{
    css::{Display, Length},
    dom::{Document, NodeId, NodeKind},
};
use std::collections::HashMap;

pub(super) struct Geometry<'a> {
    pub document: &'a Document,
    pub styles: &'a [Style],
    pub border_boxes: &'a [Option<Rect>],
    pub content_boxes: &'a [Option<Rect>],
    pub scroll_sizes: &'a [Option<(f32, f32)>],
    pub scroll_offsets: &'a [ScrollOffset],
    pub viewport: Viewport,
    pub viewport_overflow_source: Option<NodeId>,
}

#[derive(Clone, Copy, Default)]
struct Ancestors {
    // Translations in document layout coordinates before CSS transforms.
    border_shift: ScrollOffset,
    content_shift: ScrollOffset,
    containing_block_for_children: Option<NodeId>,
    scrollports_for_children: [Option<NodeId>; 2],
}

pub(super) struct StickyPositions<'a> {
    geometry: Geometry<'a>,
    ancestors: Vec<Ancestors>,
    pub uses_viewport: bool,
    pub inset_sizes: HashMap<NodeId, [f32; 2]>,
}

pub(super) fn has_insets(style: &Style) -> bool {
    style.position == Position::Sticky && style.inset.iter().any(|value| *value != Length::Auto)
}

impl<'a> StickyPositions<'a> {
    pub fn new(geometry: Geometry<'a>) -> Self {
        Self {
            ancestors: vec![Ancestors::default(); geometry.document.nodes.len()],
            geometry,
            uses_viewport: false,
            inset_sizes: HashMap::new(),
        }
    }

    pub fn visit(
        &mut self,
        node: NodeId,
        visual_parent: Option<NodeId>,
        relative: ScrollOffset,
    ) -> ScrollOffset {
        let g = &self.geometry;
        let dom_node = &g.document.nodes[node.0];
        let style = &g.styles[node.0];
        let inherited = visual_parent.map_or(Ancestors::default(), |p| self.ancestors[p.0]);
        let containing_block = dom_node
            .parent
            .and_then(|p| self.ancestors[p.0].containing_block_for_children);
        let mut scrollports = inherited.scrollports_for_children;
        let mut shift = ScrollOffset {
            x: inherited.content_shift.x + relative.x,
            y: inherited.content_shift.y + relative.y,
        };
        let establishes_box = matches!(dom_node.kind, NodeKind::Element { .. })
            && g.content_boxes[node.0].is_some()
            && match style.display {
                Display::None => false,
                Display::Inline => dom_node.parent.is_some_and(|parent| {
                    matches!(
                        g.styles[parent.0].display,
                        Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid
                    )
                }),
                _ => true,
            };
        let (delta, uses_viewport) = if has_insets(style)
            && matches!(dom_node.kind, NodeKind::Element { .. })
            && let Some(border) = g.border_boxes[node.0]
        {
            let (delta, uses_viewport, sizes) = self.displacement(
                style,
                border.translated(shift.x, shift.y),
                containing_block,
                scrollports,
            );
            if style.inset.iter().any(|inset| inset.has_percent()) {
                self.inset_sizes.insert(node, sizes);
            }
            (delta, uses_viewport)
        } else {
            (ScrollOffset::default(), false)
        };
        self.uses_viewport |= uses_viewport;
        shift.x += delta.x;
        shift.y += delta.y;
        if establishes_box {
            for (axis, overflow) in layout_overflow(style, node, g.viewport_overflow_source)
                .into_iter()
                .enumerate()
            {
                if overflow.scrollable() {
                    scrollports[axis] = Some(node);
                }
            }
        }
        let scroll = g.scroll_offsets[node.0];
        self.ancestors[node.0] = Ancestors {
            border_shift: shift,
            content_shift: ScrollOffset {
                x: shift.x - scroll.x,
                y: shift.y - scroll.y,
            },
            containing_block_for_children: if establishes_box {
                Some(node)
            } else {
                containing_block
            },
            scrollports_for_children: scrollports,
        };
        delta
    }

    fn displacement(
        &self,
        style: &Style,
        border: Rect,
        containing_block: Option<NodeId>,
        scrollports: [Option<NodeId>; 2],
    ) -> (ScrollOffset, bool, [f32; 2]) {
        let g = &self.geometry;
        let viewport = Rect {
            width: g.viewport.width,
            height: g.viewport.height,
            ..Rect::default()
        };
        let containing = containing_block
            .and_then(|node| g.content_boxes[node.0])
            .unwrap_or(viewport);
        let containing_shift = containing_block
            .map(|node| self.ancestors[node.0].content_shift)
            .unwrap_or_default();
        let visible_viewport = viewport.translated(g.viewport.scroll_x, g.viewport.scroll_y);
        let mut delta = [0.; 2];
        let mut sizes = [0.; 2];
        let mut uses_viewport = false;
        for (axis, sides) in [[3, 1], [0, 2]].into_iter().enumerate() {
            if sides.iter().all(|&side| style.inset[side] == Length::Auto) {
                continue;
            }
            let scroller = scrollports[axis];
            let scrollport = if let Some(node) = scroller {
                let t = self.ancestors[node.0].border_shift;
                // Retain a scroller's padding when pinning against its edges.
                g.content_boxes[node.0]
                    .expect("scrollport of a rendered block box")
                    .translated(t.x, t.y)
            } else {
                uses_viewport = true;
                visible_viewport
            };
            let scrollport = Interval::of(scrollport, axis);
            sizes[axis] = scrollport.size();
            let mut limit = Interval::of(containing, axis);
            if let Some(node) = containing_block
                && scroller == Some(node)
                && let Some((width, height)) = g.scroll_sizes[node.0]
            {
                // A direct child is allowed to stick across the scroller's
                // contents, including the portion outside the visible box.
                let padding = padding_box(
                    g.border_boxes[node.0].expect("containing block has a box"),
                    &g.styles[node.0],
                );
                let padding_extent = Interval::of(padding, axis);
                limit.end += ([width, height][axis] - padding_extent.size()).max(0.);
            }
            limit = limit.shifted([containing_shift.x, containing_shift.y][axis]);
            let insets = sides.map(|side| style.inset[side].resolve(sizes[axis], style.font_size));
            let margins = sides.map(|side| {
                style.margin[side]
                    .resolve(containing.width, style.font_size)
                    .unwrap_or(0.)
            });
            delta[axis] = limit_shift(
                Interval::of(border, axis),
                scrollport,
                limit,
                insets,
                margins,
            );
        }
        (
            ScrollOffset {
                x: delta[0],
                y: delta[1],
            },
            uses_viewport,
            sizes,
        )
    }
}

#[derive(Clone, Copy)]
struct Interval {
    start: f32,
    end: f32,
}

impl Interval {
    fn of(rect: Rect, axis: usize) -> Self {
        if axis == 0 {
            Self {
                start: rect.x,
                end: rect.x + rect.width,
            }
        } else {
            Self {
                start: rect.y,
                end: rect.y + rect.height,
            }
        }
    }

    fn size(self) -> f32 {
        self.end - self.start
    }

    fn shifted(self, delta: f32) -> Self {
        Self {
            start: self.start + delta,
            end: self.end + delta,
        }
    }
}

fn limit_shift(
    border: Interval,
    scrollport: Interval,
    containing: Interval,
    [start, end]: [Option<f32>; 2],
    [start_margin, end_margin]: [f32; 2],
) -> f32 {
    let view_start = scrollport.start + start.unwrap_or(0.);
    // Reduce the end inset if needed so the view rectangle fits the box.
    let view_end = (scrollport.end - end.unwrap_or(0.)).max(view_start + border.size());
    let forward = start.map_or(0., |_| (view_start - border.start).max(0.));
    let backward = end.map_or(0., |_| (view_end - border.end).min(0.));
    let max_forward = (containing.end - border.end - end_margin).max(0.);
    let max_backward = (containing.start - border.start + start_margin).min(0.);
    forward.min(max_forward) + backward.max(max_backward)
}
