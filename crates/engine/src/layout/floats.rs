use super::{BoxMetrics, Builder, Constraints, Line, NodeId, to_content};
use crate::css::Display;
use std::sync::Arc;

/// Exclusions in the current block formatting context's layout coordinates.
#[derive(Default)]
pub(super) struct FloatContext {
    boxes: Vec<Placed>,
    bottom: [Option<f32>; 2],
}

struct Placed {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    right_side: bool,
}

pub(super) struct Interval {
    pub(super) left: f32,
    pub(super) right: f32,
    next_bottom: Option<f32>,
}

impl Interval {
    pub(super) fn width(&self) -> f32 {
        (self.right - self.left).max(0.)
    }
}

impl FloatContext {
    pub(super) fn clearance(&self, sides: [bool; 2]) -> Option<f32> {
        self.bottom
            .into_iter()
            .zip(sides)
            .filter_map(|(bottom, side)| side.then_some(bottom).flatten())
            .reduce(f32::max)
    }

    fn interval(&self, mut slot: Interval, y: f32, work: &mut usize) -> Option<Interval> {
        if self.bottom.into_iter().flatten().all(|bottom| bottom <= y) {
            return Some(slot);
        }
        for f in &self.boxes {
            *work = work.checked_sub(1)?;
            if f.top > y {
                // Float tops never go backwards in source order.
                break;
            }
            if f.bottom <= y {
                continue;
            }
            if (!f.right_side && f.right <= slot.left) || (f.right_side && f.left >= slot.right) {
                continue;
            }
            if f.right_side {
                slot.right = slot.right.min(f.left);
            } else {
                slot.left = slot.left.max(f.right);
            }
            slot.next_bottom = Some(slot.next_bottom.map_or(f.bottom, |v| v.min(f.bottom)));
        }
        Some(slot)
    }

    fn push(&mut self, f: Placed) {
        let bottom = &mut self.bottom[f.right_side as usize];
        *bottom = Some(bottom.map_or(f.bottom, |value| value.max(f.bottom)));
        self.boxes.push(f);
    }
}

impl Builder<'_> {
    fn float_interval_at(&mut self, line: &Line<'_>, y: f32) -> Interval {
        let unconstrained = || Interval {
            left: line.x,
            right: line.x + line.width,
            next_bottom: None,
        };
        self.floats
            .interval(unconstrained(), y, &mut self.float_work_remaining)
            .unwrap_or_else(|| {
                self.limited = true;
                unconstrained()
            })
    }

    pub(super) fn float_interval(&mut self, line: &Line<'_>) -> Interval {
        self.float_interval_at(line, line.y)
    }

    pub(super) fn fit_empty_line(&mut self, line: &mut Line<'_>, advance: f32) {
        if !line.items.is_empty() {
            return;
        }
        loop {
            let slot = self.float_interval(line);
            if slot.width() >= advance {
                break;
            }
            let Some(next) = slot.next_bottom else { break };
            line.y = next;
        }
    }

    pub(super) fn place_float(&mut self, id: NodeId, line: &mut Line<'_>, href: Option<Arc<str>>) {
        let s = &self.styles[id.0];
        let right_side = s.float.is_right(line.parent.rtl);
        let mut constraints = Constraints {
            // Auto margins resolve to zero, as with the other atomic fragments.
            flex_item: true,
            containing_height: line.containing_height,
            ..Constraints::default()
        };
        let m = BoxMetrics::new(s, line.width, constraints);
        if s.width.resolve(line.width, s.font_size).is_none() {
            constraints.width = self.replaced_auto_border_width(id, line.width, constraints, m);
            if constraints.width.is_none() {
                let (min, max) = self.intrinsic_width(id);
                let intrinsic_extra = BoxMetrics::new(s, 0., constraints).extra_x();
                let available = line.width - m.margin[3] - m.margin[1] - m.extra_x();
                let mut width = (max - intrinsic_extra).min(available.max(min - intrinsic_extra));
                if let Some(max) = s.max_width.resolve(line.width, s.font_size) {
                    width = width.min(to_content(max, m.extra_x(), s.box_sizing));
                }
                if let Some(min) = s.min_width.resolve(line.width, s.font_size) {
                    width = width.max(to_content(min, m.extra_x(), s.box_sizing));
                }
                constraints.width = Some(width.max(0.) + m.extra_x());
                constraints.intrinsic_width = s.display == Display::Grid;
            }
        }
        let outer_width =
            constraints.width.unwrap_or(m.width + m.extra_x()) + m.margin[3] + m.margin[1];
        let mut y = line.y;
        if let Some(previous) = self.floats.boxes.last() {
            y = y.max(previous.top);
        }
        if let Some(bottom) = self.floats.clearance(s.clear.sides(line.parent.rtl)) {
            y = y.max(bottom);
        }
        let x = loop {
            let slot = self.float_interval_at(line, y);
            let inline_advance = if y == line.y { line.used } else { 0. };
            if outer_width + inline_advance > slot.right - slot.left {
                if inline_advance > 0. {
                    self.flush(line, false);
                    y = y.max(line.y);
                    continue;
                }
                if let Some(next) = slot.next_bottom {
                    y = next;
                    continue;
                }
            }
            break if right_side {
                slot.right - outer_width
            } else {
                slot.left
            };
        };
        let height = self.block(id, x, y, line.width, href, constraints);
        self.floats.push(Placed {
            left: x,
            right: x + outer_width,
            top: y,
            bottom: y + height,
            right_side,
        });
    }
}
