//! Intersection geometry uses the same containing blocks, scrolling, and clips as layout.
use super::{Affine2D, Layout, Length, NodeId, Rect, Viewport};

#[derive(Clone, Copy, Debug)]
pub(super) struct ObservationBox {
    pub bounds: Option<Rect>,
    pub client_bounds: Option<Rect>,
    pub matrix: Affine2D,
    pub clip: Option<usize>,
    pub content_clip: Option<usize>,
    pub parent: Option<NodeId>,
}

#[derive(Clone, Copy)]
pub(crate) struct ObserverRoot {
    element: Option<NodeId>,
    rect: Rect,
    matrix: Affine2D,
    clip: Option<usize>,
    viewport: Viewport,
}

#[derive(Default)]
pub(crate) struct IntersectionSample {
    pub target: Rect,
    pub root: Rect,
    pub intersection: Option<Rect>,
}

impl Layout {
    pub(crate) fn observer_root(
        &self,
        element: Option<NodeId>,
        viewport: Viewport,
        margin: [Length; 4],
    ) -> Option<ObserverRoot> {
        let (rect, matrix, clip) = if let Some(node) = element {
            let geometry = self.observation_boxes.as_ref()?.get(node.0)?;
            let mut rect = geometry.bounds?;
            if let Some(index) = geometry.content_clip.filter(|i| Some(*i) != geometry.clip)
                && self.computed_styles[node.0]
                    .overflow
                    .iter()
                    .all(|v| v.clips())
            {
                rect = self.clips[index].rect;
            }
            (rect, geometry.matrix, geometry.content_clip)
        } else {
            let rect = Rect {
                x: viewport.scroll_x,
                y: viewport.scroll_y,
                width: viewport.width,
                height: viewport.height,
            };
            (rect, Affine2D::IDENTITY, None)
        };
        Some(ObserverRoot {
            element,
            rect: expand(rect, margin),
            matrix,
            clip,
            viewport,
        })
    }

    pub(crate) fn intersection_sample(
        &self,
        root: &ObserverRoot,
        target: NodeId,
    ) -> IntersectionSample {
        let Some(boxes) = &self.observation_boxes else {
            return IntersectionSample::default();
        };
        let Some(&target) = boxes.get(target.0) else {
            return IntersectionSample::default();
        };
        let Some(bounds) = target.client_bounds else {
            return IntersectionSample::default();
        };
        if let Some(element) = root.element {
            let mut parent = target.parent;
            while let Some(node) = parent {
                if node == element {
                    break;
                }
                parent = boxes[node.0].parent;
            }
            if parent.is_none() {
                return IntersectionSample::default();
            }
        }
        let client = |r: Rect| r.translated(-root.viewport.scroll_x, -root.viewport.scroll_y);
        IntersectionSample {
            target: client(bounds),
            root: client(root.matrix.rect(root.rect)),
            intersection: self.clipped_intersection(root, target).map(client),
        }
    }

    fn clipped_intersection(&self, root: &ObserverRoot, target: ObservationBox) -> Option<Rect> {
        let mut rect = target.bounds?;
        let mut matrix = target.matrix;
        let mut next = target.clip;
        while let Some(index) = next.filter(|i| Some(*i) != root.clip) {
            let clip = &self.clips[index];
            rect = intersect(map_rect(rect, matrix, clip.matrix)?, clip.rect)?;
            matrix = clip.matrix;
            next = clip.parent;
        }
        let rect = intersect(map_rect(rect, matrix, root.matrix)?, root.rect)?;
        Some(root.matrix.rect(rect))
    }
}

fn expand(rect: Rect, margin: [Length; 4]) -> Rect {
    // Chromium and WPT resolve percentages against each side's corresponding axis.
    let [top, right, bottom, left] = std::array::from_fn(|i| {
        let base = if i % 2 == 0 { rect.height } else { rect.width };
        margin[i].resolve(base, 16.).unwrap_or_default().trunc()
    });
    Rect {
        x: rect.x - left,
        y: rect.y - top,
        width: (rect.width + left + right).max(0.),
        height: (rect.height + top + bottom).max(0.),
    }
}

fn map_rect(rect: Rect, from: Affine2D, to: Affine2D) -> Option<Rect> {
    if from == to {
        Some(rect)
    } else {
        Some(to.inverse()?.multiply(from).rect(rect))
    }
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let width = (a.x + a.width).min(b.x + b.width) - x;
    let height = (a.y + a.height).min(b.y + b.height) - y;
    // A point or line on a clip edge is still an intersection.
    (width >= 0. && height >= 0.).then_some(Rect {
        x,
        y,
        width,
        height,
    })
}
