//! Desktop wheel input is sampled once per display interval. Pixel deltas
//! already describe a gesture; only whole wheel detents need interpolation.
use super::{App, Layout, MouseScrollDelta, ScrollOffset, TOP, Viewport};
use rust_browser_engine::dom::NodeId;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DETENT_PIXELS: f32 = 54.;
const SETTLE: Duration = Duration::from_millis(100);
const GESTURE_GAP: Duration = Duration::from_millis(180);

pub(super) struct WheelGesture {
    tab: u64,
    document: u64,
    pointer: (f32, f32),
    target: Option<NodeId>,
    period: Duration,
    allow_animation: bool,
    precision: bool,
    remaining: ScrollOffset,
    ease_left: Duration,
    last_input: Instant,
    last_frame: Instant,
    next_frame: Instant,
}

impl WheelGesture {
    fn new(
        now: Instant,
        period: Duration,
        allow_animation: bool,
        tab: u64,
        document: u64,
        pointer: (f32, f32),
        target: Option<NodeId>,
    ) -> Self {
        Self {
            tab,
            document,
            pointer,
            target,
            period,
            allow_animation,
            precision: false,
            remaining: ScrollOffset::default(),
            ease_left: Duration::ZERO,
            last_input: now,
            last_frame: now - period,
            next_frame: now,
        }
    }

    fn push(&mut self, delta: ScrollOffset, detent: bool, now: Instant) {
        if self.remaining == ScrollOffset::default() {
            self.last_frame = now - self.period;
        }
        // A new direction interrupts the tail of the previous wheel movement.
        // Precision events within a frame are summed, including reversals.
        if !self.ease_left.is_zero() {
            if self.remaining.x * delta.x < 0. {
                self.remaining.x = 0.;
            }
            if self.remaining.y * delta.y < 0. {
                self.remaining.y = 0.;
            }
        }
        self.remaining.x += delta.x;
        self.remaining.y += delta.y;
        self.last_input = now;
        self.precision |= !detent;
        self.ease_left = if !self.precision && self.allow_animation {
            SETTLE
        } else {
            Duration::ZERO
        };
    }

    fn deadline(&self) -> Option<Instant> {
        (self.remaining != ScrollOffset::default()).then_some(self.next_frame)
    }

    fn sample(&mut self, now: Instant) -> Option<ScrollOffset> {
        if now < self.deadline()? {
            return None;
        }
        let elapsed = now.saturating_duration_since(self.last_frame);
        let fraction = if elapsed >= self.ease_left {
            1.
        } else {
            let left = 1. - elapsed.as_secs_f32() / self.ease_left.as_secs_f32();
            1. - left * left * left
        };
        self.last_frame = now;
        self.next_frame = now + self.period;
        self.ease_left = self.ease_left.saturating_sub(elapsed);
        // Keep intermediate detent positions on a binary subpixel grid. Repeated
        // f32 additions at large offsets otherwise leave a small remainder even
        // after equal wheel travel in the opposite direction. The last sample
        // consumes the exact balance, including fractional distances from zoom.
        let advance = |remaining: f32| {
            if fraction == 1. {
                remaining
            } else {
                ((remaining * fraction) * 64.).trunc() / 64.
            }
        };
        let step = ScrollOffset {
            x: advance(self.remaining.x),
            y: advance(self.remaining.y),
        };
        self.remaining.x -= step.x;
        self.remaining.y -= step.y;
        Some(step)
    }
}

pub(super) fn wheel_delta(delta: MouseScrollDelta, scale: f32) -> (ScrollOffset, bool) {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => (
            ScrollOffset {
                x: -x * DETENT_PIXELS,
                y: -y * DETENT_PIXELS,
            },
            x.fract() == 0. && y.fract() == 0.,
        ),
        MouseScrollDelta::PixelDelta(p) => (
            ScrollOffset {
                x: -p.x as f32 / scale,
                y: -p.y as f32 / scale,
            },
            false,
        ),
    }
}

#[cfg(windows)]
fn animations_enabled() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };
    let mut enabled = 1_i32;
    // SPI_GETCLIENTAREAANIMATION writes one Win32 BOOL.
    unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0);
    }
    enabled != 0
}
#[cfg(not(windows))]
fn animations_enabled() -> bool {
    true
}

fn axis_step(value: f32, offset: f32, max: f32, allowed: bool) -> f32 {
    if allowed {
        (offset + value).clamp(0., max) - offset
    } else {
        0.
    }
}

impl App {
    fn scroll_target(&self) -> Option<NodeId> {
        let tab = &self.tabs[self.active];
        let layout = tab.layout.as_ref()?;
        if !self.pointer_in(self.window_regions(self.active).page)
            || !layout
                .scroll_regions
                .iter()
                .any(|r| r.max_x > 0. || r.max_y > 0.)
        {
            return None;
        }
        let page = tab.page.as_ref()?;
        let mut node = layout.element_at(
            self.mouse.0 / tab.zoom + tab.scroll_x,
            (self.mouse.1 - TOP) / tab.zoom + tab.scroll,
        )?;
        loop {
            let overflow = layout.computed_styles.get(node.0)?.overflow;
            if (overflow[0].user_scrollable() || overflow[1].user_scrollable())
                && layout
                    .scroll_regions
                    .iter()
                    .any(|r| r.node == node && (r.max_x > 0. || r.max_y > 0.))
            {
                return Some(node);
            }
            node = page.document.nodes.get(node.0)?.parent?;
        }
    }

    pub(super) fn queue_wheel(&mut self, mut delta: ScrollOffset, detent: bool) {
        if !delta.x.is_finite() || !delta.y.is_finite() || delta == ScrollOffset::default() {
            return;
        }
        let now = Instant::now();
        let tab = &self.tabs[self.active];
        delta.x /= tab.zoom;
        delta.y /= tab.zoom;
        let new_gesture = self.wheel.as_ref().is_none_or(|g| {
            g.tab != tab.id
                || g.document != tab.document_navigation
                || now.duration_since(g.last_input) > GESTURE_GAP
        });
        let moved = self.wheel.as_ref().is_some_and(|g| {
            (g.pointer.0 - self.mouse.0).abs() > 3. || (g.pointer.1 - self.mouse.1).abs() > 3.
        });
        if new_gesture || moved {
            self.ensure_layout();
            let target = self.scroll_target();
            if !new_gesture && self.wheel.as_ref().is_some_and(|g| g.target == target) {
                self.wheel.as_mut().unwrap().pointer = self.mouse;
            } else {
                let tab = &self.tabs[self.active];
                let rate = self
                    .window
                    .as_ref()
                    .and_then(|w| w.current_monitor())
                    .and_then(|m| m.refresh_rate_millihertz())
                    .filter(|rate| *rate != 0)
                    .unwrap_or(60_000)
                    .clamp(30_000, 240_000);
                let period = Duration::from_secs_f64(1000. / f64::from(rate));
                self.wheel = Some(WheelGesture::new(
                    now,
                    period,
                    animations_enabled(),
                    tab.id,
                    tab.document_navigation,
                    self.mouse,
                    target,
                ));
            }
        }
        self.address_focus = false;
        self.wheel.as_mut().unwrap().push(delta, detent, now);
    }

    pub(super) fn tick_wheel(&mut self, now: Instant) -> Option<Instant> {
        let tab = &self.tabs[self.active];
        let gesture = self.wheel.as_mut()?;
        if gesture.tab != tab.id
            || gesture.document != tab.document_navigation
            || self.settings_panel.is_some()
            || self.history_menu.is_some()
            || self.chrome_menu.is_some()
        {
            self.wheel = None;
            return None;
        }
        let target = gesture.target;
        if let Some(step) = gesture.sample(now)
            && step != ScrollOffset::default()
        {
            if self.tabs[self.active].layout.is_none() {
                self.ensure_layout();
            }
            if !self.apply_scroll(target, step) {
                self.wheel.as_mut()?.remaining = ScrollOffset::default();
            }
        }
        self.wheel.as_ref()?.deadline()
    }

    pub(super) fn scroll(&mut self, dy: f32) {
        if self.settings_panel.is_some() {
            return;
        }
        if !self.devtools_focused {
            self.address_focus = false;
        }
        self.ensure_layout();
        self.apply_scroll(self.scroll_target(), ScrollOffset { x: 0., y: dy });
    }

    fn apply_scroll(&mut self, target: Option<NodeId>, mut delta: ScrollOffset) -> bool {
        let viewport = self.tab_viewport(self.active);
        let tab = &mut self.tabs[self.active];
        let (Some(layout), Some(page)) = (&tab.layout, &tab.page) else {
            return false;
        };
        let mut current = target.filter(|n| n.0 < page.document.nodes.len());
        let mut nested_changed = false;
        while let Some(node) = current {
            let overflow = layout.computed_styles.get(node.0).map(|s| s.overflow);
            if let Some(overflow) = overflow
                && (overflow[0].user_scrollable() || overflow[1].user_scrollable())
                && let Some(r) = layout.scroll_regions.iter().find(|r| r.node == node)
            {
                let dx = axis_step(delta.x, r.offset.x, r.max_x, overflow[0].user_scrollable());
                let dy = axis_step(delta.y, r.offset.y, r.max_y, overflow[1].user_scrollable());
                if dx != 0. || dy != 0. {
                    let changed = if let Some(runtime) = &mut tab.runtime {
                        match runtime.scroll_element(node, dx, dy) {
                            Ok(changed) => changed,
                            Err(error) => {
                                self.status = error.to_string();
                                false
                            }
                        }
                    } else {
                        tab.nested_scroll.insert(
                            node,
                            ScrollOffset {
                                x: r.offset.x + dx,
                                y: r.offset.y + dy,
                            },
                        );
                        true
                    };
                    if changed {
                        nested_changed = true;
                        delta.x -= dx;
                        delta.y -= dy;
                        if delta == ScrollOffset::default() {
                            break;
                        }
                    }
                }
            }
            current = page.document.nodes[node.0].parent;
        }
        let dx = axis_step(
            delta.x,
            tab.scroll_x,
            (layout.content_width - viewport.width).max(0.),
            layout.viewport_overflow[0].user_scrollable(),
        );
        let dy = axis_step(
            delta.y,
            tab.scroll,
            (layout.height - viewport.height).max(0.),
            layout.viewport_overflow[1].user_scrollable(),
        );
        if nested_changed || dx != 0. || dy != 0. {
            tab.scroll_x += dx;
            tab.scroll += dy;
            if nested_changed && tab.runtime.is_none() {
                let viewport = Viewport {
                    scroll_x: tab.scroll_x,
                    scroll_y: tab.scroll,
                    ..viewport
                };
                tab.layout = Some(Arc::new(Layout::for_scrolling(
                    page,
                    &self.fonts,
                    viewport,
                    &tab.nested_scroll,
                    Some(layout),
                )));
            }
            self.ensure_layout();
            self.redraw();
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gesture(now: Instant, animate: bool) -> WheelGesture {
        WheelGesture::new(
            now,
            Duration::from_millis(10),
            animate,
            1,
            2,
            (24., 50.),
            None,
        )
    }

    #[test]
    fn detents_start_immediately_settle_and_preserve_distance() {
        let start = Instant::now();
        let mut g = gesture(start, true);
        for _ in 0..3 {
            g.push(ScrollOffset { x: 0., y: 54. }, true, start);
        }
        let first = g.sample(start).unwrap().y;
        assert!(first > 0. && first < 162.);
        assert!(g.sample(start + Duration::from_millis(2)).is_none());
        let mut total = first;
        let mut frames = 1;
        while let Some(deadline) = g.deadline() {
            assert!(deadline <= start + SETTLE);
            total += g.sample(deadline).unwrap().y;
            frames += 1;
        }
        assert!((total - 162.).abs() < 0.001 && frames > 1);
    }

    #[test]
    fn long_runs_of_animated_detents_return_to_the_exact_starting_offset() {
        let start = Instant::now();
        let mut g = gesture(start, true);
        let mut offset = 3_584.;
        for (direction, expected) in [(1., 6_176.), (-1., 3_584.)] {
            let begin = g.last_input + Duration::from_secs(1);
            for i in 0..24 {
                let now = begin + Duration::from_micros(i * 16_701);
                g.push(
                    ScrollOffset {
                        x: 0.,
                        y: direction * 108.,
                    },
                    true,
                    now,
                );
                offset += g.sample(now).unwrap().y;
            }
            while let Some(deadline) = g.deadline() {
                offset += g.sample(deadline).unwrap().y;
            }
            assert_eq!(offset, expected);
        }
    }

    #[test]
    fn precision_input_is_coalesced_without_an_animation_tail() {
        let start = Instant::now();
        let mut g = gesture(start, true);
        g.push(ScrollOffset { x: 2.5, y: 0.25 }, false, start);
        assert_eq!(g.sample(start).unwrap(), ScrollOffset { x: 2.5, y: 0.25 });
        g.push(
            ScrollOffset { x: -1., y: 8. },
            false,
            start + Duration::from_millis(2),
        );
        g.push(
            ScrollOffset { x: 0., y: -2. },
            true,
            start + Duration::from_millis(3),
        );
        assert!(g.sample(start + Duration::from_millis(5)).is_none());
        assert_eq!(
            g.sample(start + Duration::from_millis(10)).unwrap(),
            ScrollOffset { x: -1., y: 6. }
        );
        assert!(g.deadline().is_none());
    }

    #[test]
    fn reversal_interrupts_old_tail_and_late_frames_finish_in_one_step() {
        let start = Instant::now();
        let mut g = gesture(start, true);
        g.push(ScrollOffset { x: 0., y: 54. }, true, start);
        assert!(g.sample(start).unwrap().y > 0.);
        g.push(
            ScrollOffset { x: 0., y: -54. },
            true,
            start + Duration::from_millis(3),
        );
        assert_eq!(
            g.sample(start + Duration::from_millis(200)).unwrap().y,
            -54.
        );
        assert!(g.deadline().is_none());
        let mut reduced = gesture(start, false);
        reduced.push(ScrollOffset { x: 0., y: 54. }, true, start);
        assert_eq!(reduced.sample(start).unwrap().y, 54.);
        assert!(reduced.deadline().is_none());
    }

    #[test]
    fn native_units_and_axis_clamping() {
        assert_eq!(
            wheel_delta(MouseScrollDelta::LineDelta(0., 0.25), 1.),
            (ScrollOffset { x: 0., y: -13.5 }, false)
        );
        assert_eq!(
            wheel_delta(MouseScrollDelta::LineDelta(0., -2.), 1.),
            (ScrollOffset { x: 0., y: 108. }, true)
        );
        let pixel = MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(4., -8.));
        assert_eq!(
            wheel_delta(pixel, 2.),
            (ScrollOffset { x: -2., y: 4. }, false)
        );
        assert_eq!(axis_step(20., 95., 100., true), 5.);
        assert_eq!(axis_step(-20., 6., 100., true), -6.);
        assert_eq!(axis_step(25., 0., 100., false), 0.);
    }
}
