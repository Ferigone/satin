//! Paint-only transitions for Quiet Cinder. Input never samples this state.

use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Clone, Copy)]
struct Timing {
    duration: Duration,
    curve: [f64; 4],
}

const CSS_EASE: [f64; 4] = [0.25, 0.1, 0.25, 1.];
const CINDER_EASE: [f64; 4] = [0.22, 0.85, 0.25, 1.];
const TOOLBAR: Timing = Timing::new(130, CSS_EASE);
const CAPTION: Timing = Timing::new(160, CSS_EASE);
const TAB_BACKGROUND: Timing = Timing::new(170, CINDER_EASE);
const TAB_INK: Timing = Timing::new(170, CSS_EASE);
const TAB_CLOSE: Timing = Timing::new(120, CSS_EASE);

impl Timing {
    const fn new(ms: u64, curve: [f64; 4]) -> Self {
        Self {
            duration: Duration::from_millis(ms),
            curve,
        }
    }

    fn ease(self, fraction: f64) -> f64 {
        if !(0. < fraction && fraction < 1.) {
            return fraction.clamp(0., 1.);
        }
        let [x1, y1, x2, y2] = self.curve;
        let bezier = |t: f64, a, b| {
            let inv = 1. - t;
            3. * inv * inv * t * a + 3. * inv * t * t * b + t * t * t
        };
        // CSS cubic-bezier evaluates y at the parameter where x = elapsed.
        let (mut left, mut right) = (0., 1.);
        for _ in 0..20 {
            let mid = (left + right) * 0.5;
            if bezier(mid, x1, x2) < fraction {
                left = mid;
            } else {
                right = mid;
            }
        }
        bezier((left + right) * 0.5, y1, y2)
    }
}

#[derive(Clone, Copy)]
struct Running {
    start: Instant,
    duration: Duration,
    from: f32,
    shortening: f64,
}

impl Running {
    fn end(self) -> Instant {
        self.start + self.duration
    }

    fn progress(self, now: Instant, timing: Timing) -> f64 {
        timing.ease(
            now.saturating_duration_since(self.start).as_secs_f64() / self.duration.as_secs_f64(),
        )
    }
}

#[derive(Clone, Copy, Default)]
struct Track {
    target: Option<bool>,
    running: Option<Running>,
}

impl Track {
    fn sample(&mut self, frame: &mut Frame, target: bool, timing: Timing) -> f32 {
        let end_value = unit(target);
        let previous = self.target.replace(target);
        if !frame.enabled || previous.is_none() {
            self.running = None;
            return end_value;
        }
        self.running = self.running.filter(|run| frame.now < run.end());
        if previous != Some(target) {
            let (from, shortening) = if let Some(run) = self.running {
                let p = run.progress(frame.now, timing);
                // CSS reversing-shortening factor for a binary property.
                let s = (p * run.shortening + (1. - run.shortening)).clamp(0., 1.);
                (mix(run.from, 1. - end_value, p as f32), s)
            } else {
                (1. - end_value, 1.)
            };
            let duration = timing.duration.mul_f64(shortening);
            self.running = (from != end_value && !duration.is_zero()).then_some(Running {
                start: frame.now,
                duration,
                from,
                shortening,
            });
        }
        let Some(run) = self.running else {
            return end_value;
        };
        frame.until(run.end());
        mix(run.from, end_value, run.progress(frame.now, timing) as f32)
    }
}

fn mix(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

fn unit(value: bool) -> f32 {
    if value { 1. } else { 0. }
}

struct Frame {
    enabled: bool,
    now: Instant,
    next: Option<Instant>,
}

impl Frame {
    fn until(&mut self, end: Instant) {
        let next = end.min(self.now + FRAME_INTERVAL);
        self.next = Some(self.next.map_or(next, |pending| pending.min(next)));
    }
}

#[derive(Clone, Copy)]
pub(super) enum Button {
    Back,
    Forward,
    Reload,
    Home,
    Downloads,
    Menu,
    Bookmark,
    NewTab,
    TabList,
}

#[derive(Clone, Copy, Default)]
struct Caption {
    hot: Track,
    pressed: Track,
}

#[derive(Default)]
struct Tab {
    id: u64,
    seen: bool,
    hover: Track,
    text: Track,
    border: Track,
    close: Track,
    close_hover: Track,
}

pub(super) struct TabInk {
    pub(super) hover: f32,
    pub(super) text: f32,
    pub(super) border: f32,
    pub(super) close: f32,
    pub(super) close_hover: f32,
}

pub(super) struct ChromeMotion<const MAX_TABS: usize> {
    frame: Frame,
    scheduled: Option<Instant>,
    buttons: [Track; Button::TabList as usize + 1],
    captions: [Caption; 3],
    // During a frame this holds at most the previous and current visible
    // sets. IDs prevent a removed tab donating its transition to a new slot.
    tabs: Vec<Tab>,
}

impl<const MAX_TABS: usize> ChromeMotion<MAX_TABS> {
    pub(super) fn new(now: Instant, enabled: bool) -> Self {
        Self {
            frame: Frame {
                enabled,
                now,
                next: None,
            },
            scheduled: None,
            buttons: Default::default(),
            captions: Default::default(),
            tabs: Vec::with_capacity(2 * MAX_TABS),
        }
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) -> bool {
        if enabled == self.frame.enabled {
            return false;
        }
        self.frame.enabled = enabled;
        self.scheduled = None;
        true
    }

    pub(super) fn begin_frame(&mut self, now: Instant) {
        self.frame.now = now;
        self.frame.next = None;
        self.scheduled = None;
        for tab in &mut self.tabs {
            tab.seen = false;
        }
    }

    pub(super) fn button(&mut self, button: Button, hot: bool) -> f32 {
        self.buttons[button as usize].sample(&mut self.frame, hot, TOOLBAR)
    }

    pub(super) fn caption(&mut self, index: usize, hovered: bool, pressed: bool) -> (f32, f32) {
        let caption = &mut self.captions[index];
        (
            caption
                .hot
                .sample(&mut self.frame, hovered || pressed, CAPTION),
            caption
                .pressed
                .sample(&mut self.frame, index == 2 && pressed, CAPTION),
        )
    }

    pub(super) fn tab(
        &mut self,
        id: u64,
        active: bool,
        hovered: bool,
        close_hover: Option<bool>,
    ) -> TabInk {
        let tab = if let Some(i) = self.tabs.iter().position(|tab| tab.id == id) {
            &mut self.tabs[i]
        } else if self.tabs.len() < 2 * MAX_TABS {
            self.tabs.push(Tab {
                id,
                ..Tab::default()
            });
            self.tabs.last_mut().unwrap()
        } else {
            // The App also caps the number of live tabs. Snap if an
            // unexpected caller exceeds the allocation bound.
            return TabInk {
                hover: unit(hovered && !active),
                text: unit(active || hovered),
                border: unit(active),
                close: unit(close_hover.is_some() && (active || hovered)),
                close_hover: unit(close_hover == Some(true)),
            };
        };
        tab.seen = true;
        let (close, close_hover) = if let Some(hot) = close_hover {
            (
                tab.close
                    .sample(&mut self.frame, active || hovered, TAB_CLOSE),
                tab.close_hover.sample(&mut self.frame, hot, TAB_CLOSE),
            )
        } else {
            tab.close = Track::default();
            tab.close_hover = Track::default();
            (0., 0.)
        };
        let hover = if active {
            // An opaque selected gradient replaces the hover fill. Do not
            // wake the event loop for a transition hidden behind it.
            tab.hover = Track {
                target: Some(false),
                running: None,
            };
            0.
        } else {
            tab.hover.sample(&mut self.frame, hovered, TAB_BACKGROUND)
        };
        TabInk {
            hover,
            text: tab.text.sample(&mut self.frame, active || hovered, TAB_INK),
            border: tab.border.sample(&mut self.frame, active, TAB_INK),
            close,
            close_hover,
        }
    }

    pub(super) fn finish_frame(&mut self, presented: bool) {
        self.tabs.retain(|tab| tab.seen);
        self.scheduled = if presented { self.frame.next } else { None };
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.scheduled
    }

    /// Consume a due wake before asking the OS to redraw. If no frame is
    /// delivered (e.g. minimized), no future wake is armed by this request.
    pub(super) fn take_due(&mut self, now: Instant) -> bool {
        if self.scheduled.is_some_and(|next| next <= now) {
            self.scheduled = None;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests;
