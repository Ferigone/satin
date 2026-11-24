use super::panel::scaled;
use rust_browser_engine::{
    Fonts, Layout as PageLayout, LoadedPage, Rect, css::Color, paint::Canvas, script::ScriptRuntime,
};
use std::{collections::VecDeque, time::Duration};

const CAPACITY: usize = 120;
#[derive(Default)]
pub(super) struct Performance {
    samples: VecDeque<(f32, f32)>,
    paused: bool,
    counters: String,
    viewport: String,
}

impl Performance {
    pub(super) fn sample(
        &mut self,
        layout_time: Duration,
        paint_time: Duration,
        page: Option<&LoadedPage>,
        layout: Option<&PageLayout>,
        runtime: Option<&ScriptRuntime>,
    ) {
        if self.paused {
            return;
        }
        if self.samples.len() == CAPACITY {
            self.samples.pop_front();
        }
        self.samples.push_back((
            layout_time.as_secs_f32() * 1000.,
            paint_time.as_secs_f32() * 1000.,
        ));
        self.counters=page.map_or_else(|| "Waiting for document".into(),|p|format!("DOM arena: {} nodes    •    Stylesheets: {}    •    Scripts executed: {}    •    Pending Fetch: {}",p.document.nodes.len(),p.stylesheets.len(),runtime.map_or(0,ScriptRuntime::executed_scripts),runtime.map_or(0,ScriptRuntime::pending_network)));
        self.viewport=layout.map_or_else(String::new,|l|format!("Viewport: {:.0} × {:.0} CSS px    •    Layout boxes: {}    •    Paint commands: {}{}",l.viewport.width,l.viewport.height,l.elements.len(),l.commands.len(),if l.limited {"    •    Layout limit reached"} else {""}));
    }
    pub(super) fn click(&mut self, b: Rect, x: f32, y: f32) {
        if y < b.y + 30. {
            if x < b.x + 86. {
                self.paused = !self.paused;
            } else if x < b.x + 145. {
                self.samples.clear();
            }
        }
    }
    pub(super) fn draw(&self, c: &mut Canvas, f: &mut Fonts, b: Rect, s: f32) {
        let bg = Color(30, 30, 33, 255);
        let fg = Color(229, 228, 236, 255);
        let muted = Color(183, 181, 192, 255);
        let layout_col = Color(177, 151, 236, 255);
        let paint_col = Color(111, 193, 170, 255);
        let rect = |x, y, w, h| {
            scaled(
                Rect {
                    x: b.x + x,
                    y: b.y + y,
                    width: w,
                    height: h,
                },
                s,
            )
        };
        c.fill_rect(scaled(b, s), bg);
        c.fill_rect(
            rect(0., 0., b.width, b.height.min(30.)),
            Color(42, 41, 45, 255),
        );
        let text = |c: &mut Canvas, f: &mut Fonts, v: &str, x: f32, y: f32, col| {
            c.label(v, (b.x + x) * s, (b.y + y) * s, 12. * s, col, false, f)
        };
        text(
            c,
            f,
            if self.paused {
                "▶ Resume"
            } else {
                "Ⅱ Pause"
            },
            8.,
            20.,
            fg,
        );
        text(c, f, "Clear", 94., 20., muted);
        text(
            c,
            f,
            &format!("{} / {} document draws", self.samples.len(), CAPACITY),
            154.,
            20.,
            muted,
        );
        text(c, f, &self.counters, 8., 49., fg);
        text(c, f, &self.viewport, 8., 68., fg);
        let peak = self.samples.iter().map(|(a, b)| a + b).fold(0., f32::max);
        let max = peak.max(f32::EPSILON);
        let (layout, paint) = self.samples.back().copied().unwrap_or_default();
        text(
            c,
            f,
            &format!("Layout check/update: {layout:.2} ms"),
            8.,
            92.,
            layout_col,
        );
        text(
            c,
            f,
            &format!("Document paint: {paint:.2} ms"),
            203.,
            92.,
            paint_col,
        );
        let h = (b.height - 126.).max(0.);
        if h < 10. {
            return;
        }
        let w = (b.width - 16.).max(0.);
        c.fill_rect(rect(8., 102., w, h), Color(38, 38, 42, 255));
        let bar = w / CAPACITY as f32;
        for (i, (a, d)) in self.samples.iter().enumerate() {
            let x = 8. + bar * i as f32;
            let h1 = a / max * h;
            let h2 = d / max * h;
            c.fill_rect(rect(x, 102. + h - h1, bar.max(1.), h1), layout_col);
            c.fill_rect(rect(x, 102. + h - h1 - h2, bar.max(1.), h2), paint_col);
        }
        text(
            c,
            f,
            &format!("Oldest → newest    •    Peak combined time: {peak:.2} ms"),
            8.,
            b.height - 6.,
            muted,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_is_bounded_and_pause_freezes_history() {
        let mut p = Performance::default();
        for _ in 0..CAPACITY + 1 {
            p.sample(Duration::from_millis(1), Duration::ZERO, None, None, None);
        }
        assert_eq!(p.samples.len(), CAPACITY);
        p.click(Rect::default(), 1., 1.);
        p.sample(Duration::MAX, Duration::MAX, None, None, None);
        assert_eq!(p.samples.back(), Some(&(1., 0.)));
        p.click(Rect::default(), 100., 1.);
        assert!(p.samples.is_empty());
    }
}
