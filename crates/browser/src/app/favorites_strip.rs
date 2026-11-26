//! Quiet Cinder's single scrolling flex row, in logical pixels.

pub(crate) const MAX_FAVORITES: usize = 9;
pub(crate) const LABEL_INSET: f32 = 8. + 15. + 6.;
const PADDING: f32 = 11.;
const GAP: f32 = 3.;
const MANAGER_WIDTH: f32 = 31.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Span {
    pub x: f32,
    pub width: f32,
}

impl Span {
    fn clipped(self, width: f32) -> Option<Self> {
        let left = self.x.max(0.);
        let right = (self.x + self.width).min(width);
        (left < right).then_some(Self {
            x: left,
            width: right - left,
        })
    }
}

#[derive(Default)]
pub(crate) struct Scroll {
    offset: f32,
    max: f32,
}

impl Scroll {
    /// Prefer actual horizontal gestures. Use vertical detents as a fallback
    /// over this horizontal-only row, like the native tab strip.
    pub fn wheel(&mut self, dx: f32, dy: f32) -> bool {
        let delta = if dx != 0. { dx } else { dy };
        if !delta.is_finite() {
            return false;
        }
        let next = (self.offset + delta).clamp(0., self.max);
        let changed = next != self.offset;
        self.offset = next;
        changed
    }
}

pub(crate) struct Strip {
    width: f32,
    offset: f32,
    slots: Vec<Span>,
}

impl Strip {
    pub fn layout(
        width: f32,
        label_widths: impl IntoIterator<Item = f32>,
        scroll: &mut Scroll,
    ) -> Self {
        let mut slots = Vec::with_capacity(MAX_FAVORITES + 1);
        slots.push(Span {
            x: PADDING,
            width: MANAGER_WIDTH,
        });
        let mut end = PADDING + MANAGER_WIDTH;
        for label_width in label_widths.into_iter().take(MAX_FAVORITES) {
            let width = LABEL_INSET + label_width + 8.;
            let x = end + GAP;
            slots.push(Span { x, width });
            end = x + width;
        }
        scroll.max = (end + PADDING - width).max(0.);
        scroll.offset = scroll.offset.min(scroll.max);
        Self {
            width,
            offset: scroll.offset,
            slots,
        }
    }

    /// Slot zero is the manager. An item keeps its full paint span even
    /// when only its intersection with the viewport can be hit.
    pub fn visible(&self) -> impl Iterator<Item = (usize, Span, Span)> + '_ {
        self.slots.iter().enumerate().filter_map(|(index, span)| {
            let paint = Span {
                x: span.x - self.offset,
                ..*span
            };
            Some((index, paint, paint.clipped(self.width)?))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TITLES: [f32; 4] = [70., 61., 91., 100.];

    #[test]
    fn layout_preserves_full_labels_at_narrow_and_wide_widths() {
        let mut scroll = Scroll::default();
        let narrow = Strip::layout(390., TITLES, &mut scroll);
        let wide = Strip::layout(800., TITLES, &mut scroll);
        let n = narrow.visible().collect::<Vec<_>>();
        let w = wide.visible().collect::<Vec<_>>();
        assert_eq!(n[3], w[3]);
        assert_eq!(n[3].1.width, 128.);
        assert_eq!(n[4].1, w[4].1);
        assert!(n[4].2.width < n[4].1.width);
        assert_eq!(n[4].2.x + n[4].2.width, 390.);
    }

    #[test]
    fn translated_paint_and_hits_share_the_client_clip_at_all_target_scales() {
        for scale in [1., 1.25, 1.5] {
            // Integer device sizes can yield fractional logical client widths.
            let physical_width = (390_f32 * scale).round();
            let width = physical_width / scale;
            let mut scroll = Scroll::default();
            Strip::layout(width, TITLES, &mut scroll);
            assert!(scroll.wheel(35. / scale, 200.));
            let strip = Strip::layout(width, TITLES, &mut scroll);
            let rows = strip.visible().collect::<Vec<_>>();
            let (index, paint, hit) = rows.first().unwrap();
            assert_eq!(*index, 0);
            assert!(paint.x < 0.);
            assert_eq!(hit.x, 0.);
            let (_, paint, hit) = rows.last().unwrap();
            assert!(paint.width > hit.width);
            assert!(((hit.x + hit.width) * scale - physical_width).abs() < 0.001);
        }
    }

    #[test]
    fn wheel_clamps_both_ends_and_relayout_clamps_a_shrinking_strip() {
        let mut scroll = Scroll::default();
        Strip::layout(250., TITLES, &mut scroll);
        assert!(!scroll.wheel(-5., 80.));
        assert!(scroll.wheel(0., 2000.));
        assert!(!scroll.wheel(50., -500.));
        assert!(!scroll.wheel(f32::INFINITY, 0.));
        let strip = Strip::layout(250., TITLES, &mut scroll);
        let (index, paint, hit) = strip.visible().last().unwrap();
        assert_eq!(index, 4);
        assert_eq!(paint, hit);
        assert_eq!(hit.x + hit.width + PADDING, 250.);
        assert!(scroll.wheel(0., -2000.));
        assert_eq!(scroll.offset, 0.);
        assert!(scroll.wheel(2000., 0.));
        Strip::layout(250., [20.], &mut scroll);
        assert_eq!(scroll.offset, 0.);
        assert!(!scroll.wheel(30., 0.));
    }

    #[test]
    fn no_favorite_slots_are_manufactured_and_the_reference_cap_stays_bounded() {
        let mut scroll = Scroll::default();
        let empty = Strip::layout(1280., [], &mut scroll);
        assert_eq!(empty.visible().count(), 1); // Manager only.
        let populated = Strip::layout(1280., [11.; 128], &mut scroll);
        assert_eq!(populated.visible().count(), 1 + MAX_FAVORITES);
    }
}
