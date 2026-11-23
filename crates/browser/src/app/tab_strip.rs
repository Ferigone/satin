use std::ops::Range;

const LEFT: f32 = 18.;
const GAP: f32 = 4.;
const NEW_TAB_SPACE: f32 = 58.;
pub(super) const NEW_TAB_WIDTH: f32 = 42.;
const MIN_ACTIVE_SLOT: f32 = 88.;
const MIN_CLOSE_FACE: f32 = 58.;

pub(super) struct TabStrip {
    pub(super) slot: f32,
    pub(super) new_tab_x: f32,
    extra: f32,
    active: usize,
}

pub(super) struct TabSlot {
    pub(super) x: f32,
    pub(super) face: f32,
    close_width: f32,
}

impl TabStrip {
    pub(super) fn new(width: f32, caption: f32, count: usize, active: usize) -> Self {
        debug_assert!(active < count);
        let available = (width - caption - LEFT - NEW_TAB_SPACE).max(0.);
        let count = count as f32;
        let average = available / count;
        let extra = (average - 80.).clamp(0., 32.);
        let slot = ((available - extra) / count).min(216.);
        let (slot, extra) = if slot + extra < MIN_ACTIVE_SLOT && count > 1. {
            // Keep every tab in order and reserve room for the active title and X.
            let active_slot = MIN_ACTIVE_SLOT.min(available);
            let inactive_slot = (available - active_slot) / (count - 1.);
            (inactive_slot, active_slot - inactive_slot)
        } else {
            (slot, extra)
        };
        Self {
            slot,
            new_tab_x: LEFT + 11. + count * slot + extra,
            extra,
            active,
        }
    }

    pub(super) fn tab(&self, index: usize) -> TabSlot {
        let offset = if index > self.active { self.extra } else { 0. };
        let bonus = if index == self.active { self.extra } else { 0. };
        let face = (self.slot + bonus - GAP).max(0.);
        TabSlot {
            x: LEFT + index as f32 * self.slot + offset,
            face,
            // A narrow face is selection-only: no invisible click target and no
            // hover-dependent change in what the same point will do.
            close_width: if face >= MIN_CLOSE_FACE {
                (face * 0.42).min(42.)
            } else {
                0.
            },
        }
    }
}

impl TabSlot {
    pub(super) fn select(&self) -> Range<f32> {
        self.x..self.x + self.face - self.close_width
    }

    pub(super) fn close(&self) -> Option<Range<f32>> {
        (self.close_width > 0.).then(|| self.select().end..self.x + self.face)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_tab_geometry_is_unchanged() {
        // (width, caption, count, active, faces as (x, width), new tab x)
        for (width, caption, count, active, faces, plus) in [
            (1586., 177., 1, 0, &[(18., 244.)][..], 277.),
            (1586., 177., 2, 0, &[(18., 244.), (266., 212.)][..], 493.),
            (1586., 177., 2, 1, &[(18., 212.), (234., 244.)][..], 493.),
            (640., 177., 2, 1, &[(18., 173.5), (195.5, 205.5)][..], 416.),
            (640., 0., 2, 0, &[(18., 244.), (266., 212.)][..], 493.),
        ] {
            let strip = TabStrip::new(width, caption, count, active);
            assert_eq!(strip.new_tab_x, plus);
            for (index, &(x, face)) in faces.iter().enumerate() {
                let tab = strip.tab(index);
                assert_eq!((tab.x, tab.face), (x, face));
                assert_eq!(tab.close(), Some(x + face - 42. ..x + face));
            }
        }
    }

    #[test]
    fn crowded_tabs_keep_the_active_title_and_close_space() {
        // Logical widths include 640 px rounded down slightly at fractional DPI.
        for (width, caption, count) in [
            (639.5, 177., 24),
            (640., 177., 24),
            (640., 177., 8),
            (800., 177., 12),
            (1280., 177., 24),
            (1586., 177., 24),
            (640., 0., 24),
        ] {
            for active in [0, count / 2, count - 1] {
                let strip = TabStrip::new(width, caption, count, active);
                let mut end = 0.;
                for index in 0..count {
                    let tab = strip.tab(index);
                    let select = tab.select();
                    assert!(select.start >= end && select.end - select.start >= 8.);
                    let right = tab.x + tab.face;
                    if index == active {
                        let close = tab.close().expect("active tab has a close target");
                        assert!(close.end - close.start >= 24.);
                        assert!(select.end - select.start - 5. - 3. >= 40.);
                    } else if tab.face < MIN_CLOSE_FACE {
                        assert!(tab.close().is_none());
                        assert_eq!(select, tab.x..right);
                    }
                    if let Some(close) = tab.close() {
                        assert_eq!(select.end, close.start);
                        assert!(!select.contains(&close.start));
                        assert!(close.contains(&close.start));
                        assert!(!close.contains(&right));
                        assert_eq!(close.end, right);
                    }
                    end = right;
                }
                assert!(end < strip.new_tab_x);
                assert!(strip.new_tab_x + NEW_TAB_WIDTH <= width - caption);
            }
        }
    }

    #[test]
    fn close_target_uses_a_face_width_threshold() {
        for (width, inactive_close) in [(588., false), (589., true), (640., true)] {
            let strip = TabStrip::new(width, 177., 5, 0);
            assert!(strip.tab(0).close().is_some());
            assert_eq!(strip.tab(1).close().is_some(), inactive_close);
        }
    }
}
