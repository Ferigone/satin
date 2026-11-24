use rust_browser_engine::{Fonts, Rect, css::Color, paint::Canvas};

const HEADER_HEIGHT: f32 = 36.;
const TAB_INSET: f32 = 6.;
const TAB_WIDTH: f32 = 88.;
const PANEL: Color = Color(26, 26, 28, 255);
const TOOLBAR: Color = Color(33, 32, 35, 255);
const ACTIVE_TAB: Color = Color(43, 41, 47, 255);
const BORDER: Color = Color(69, 67, 74, 255);
const TEXT: Color = Color(242, 241, 244, 255);
const MUTED: Color = Color(185, 182, 190, 255);
const ACCENT: Color = Color(179, 158, 250, 255);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Section {
    #[default]
    Console,
    Network,
}

impl Section {
    const ALL: [Self; 2] = [Self::Console, Self::Network];

    fn label(self) -> &'static str {
        match self {
            Self::Console => "Console",
            Self::Network => "Network",
        }
    }
}

pub(super) struct Layout {
    bounds: Rect,
    header: Rect,
    tabs: [Rect; 2],
    pub(super) body: Rect,
}

impl Layout {
    pub(super) fn new(bounds: Rect) -> Option<Self> {
        if !has_area(bounds) {
            return None;
        }
        let header = Rect {
            height: bounds.height.min(HEADER_HEIGHT),
            ..bounds
        };
        let inset = TAB_INSET.min(bounds.width / 12.);
        let width = TAB_WIDTH.min((bounds.width - 2. * inset) / 2.);
        let tab = Rect {
            x: bounds.x + inset,
            y: bounds.y + 1.,
            width,
            height: (header.height - 2.).max(0.),
        };
        Some(Self {
            bounds,
            header,
            tabs: [
                tab,
                Rect {
                    x: tab.x + tab.width,
                    ..tab
                },
            ],
            body: Rect {
                y: bounds.y + header.height,
                height: bounds.height - header.height,
                ..bounds
            },
        })
    }

    pub(super) fn section_at(&self, x: f32, y: f32) -> Option<Section> {
        Section::ALL
            .into_iter()
            .zip(self.tabs)
            .find_map(|(section, rect)| contains(rect, x, y).then_some(section))
    }

    pub(super) fn paint(
        &self,
        canvas: &mut Canvas,
        fonts: &mut Fonts,
        scale: f32,
        active: Section,
    ) {
        let physical = |rect| scaled(rect, scale);
        canvas.clip(physical(self.bounds));
        canvas.fill_rect(physical(self.bounds), PANEL);
        canvas.fill_rect(physical(self.header), TOOLBAR);
        for (section, tab) in Section::ALL.into_iter().zip(self.tabs) {
            if !has_area(tab) {
                continue;
            }
            canvas.clip(physical(tab));
            let selected = section == active;
            if selected {
                canvas.fill_rect(physical(tab), ACTIVE_TAB);
                let height = 2_f32.min(tab.height);
                canvas.fill_rect(
                    physical(Rect {
                        y: tab.y + tab.height - height,
                        height,
                        ..tab
                    }),
                    ACCENT,
                );
            }
            canvas.label(
                section.label(),
                (tab.x + 10_f32.min(tab.width / 8.)) * scale,
                (tab.y + tab.height / 2. + 4.) * scale,
                13. * scale,
                if selected { TEXT } else { MUTED },
                selected,
                fonts,
            );
        }
        canvas.clip(physical(self.header));
        for y in [self.header.y, self.header.y + self.header.height - 1.] {
            canvas.fill_rect(
                physical(Rect {
                    y,
                    height: 1.,
                    ..self.header
                }),
                BORDER,
            );
        }
    }
}

pub(super) fn has_area(rect: Rect) -> bool {
    rect.x.is_finite()
        && rect.y.is_finite()
        && (rect.x + rect.width).is_finite()
        && (rect.y + rect.height).is_finite()
        && rect.width > 0.
        && rect.height > 0.
}

pub(super) fn contains(rect: Rect, x: f32, y: f32) -> bool {
    has_area(rect) && rect.contains(x, y)
}

pub(super) fn scaled(rect: Rect, scale: f32) -> Rect {
    // Snap shared edges, not widths; Canvas otherwise rounds fills outward.
    let x = (rect.x * scale).round();
    let y = (rect.y * scale).round();
    Rect {
        x,
        y,
        width: ((rect.x + rect.width) * scale).round() - x,
        height: ((rect.y + rect.height) * scale).round() - y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_hit_targets_are_disjoint_and_do_not_reach_the_body() {
        let layout = Layout::new(Rect {
            x: 10.,
            y: 120.,
            width: 400.,
            height: 220.,
        })
        .unwrap();
        let [console, network] = layout.tabs;
        assert_eq!(
            layout.section_at(console.x, console.y),
            Some(Section::Console)
        );
        assert_eq!(
            layout.section_at(network.x, network.y),
            Some(Section::Network)
        );
        assert_eq!(
            layout.section_at(network.x + network.width, network.y),
            None
        );
        assert_eq!(layout.section_at(network.x, layout.body.y), None);
        assert!(!contains(layout.bounds, 10., 340.));
    }

    #[test]
    fn narrow_and_short_docks_never_expand_into_the_page() {
        let bounds = Rect {
            x: 0.,
            y: 90.,
            width: 64.,
            height: 18.,
        };
        let layout = Layout::new(bounds).unwrap();
        assert_eq!(layout.body.height, 0.);
        for (section, tab) in Section::ALL.into_iter().zip(layout.tabs) {
            assert!(tab.x + tab.width <= bounds.width);
            assert_eq!(layout.section_at(tab.x, tab.y), Some(section));
        }
        assert!(
            Layout::new(Rect {
                height: 0.,
                ..bounds
            })
            .is_none()
        );
        assert!(
            Layout::new(Rect {
                width: f32::NAN,
                ..bounds
            })
            .is_none()
        );
    }

    #[test]
    fn fractional_device_scale_keeps_a_shared_header_body_edge() {
        let layout = Layout::new(Rect {
            x: 0.3,
            y: 117.8,
            width: 400.,
            height: 200.,
        })
        .unwrap();
        let header = scaled(layout.header, 1.25);
        let body = scaled(layout.body, 1.25);
        assert_eq!(header.y + header.height, body.y);
    }
}
