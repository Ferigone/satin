use super::{Canvas, Color, ICON_SIZE};
use crate::app::{Action, chrome_icon};
use std::f32::consts::TAU;

#[derive(Clone, Copy)]
pub(super) enum Glyph {
    Plus,
    Minus,
    Home,
    History,
    Book,
    Zoom,
    Code,
    Settings,
    Info,
    Keyboard,
}

type Point = (f32, f32);

pub(super) fn draw_glyph(
    canvas: &mut Canvas,
    glyph: Glyph,
    x: f32,
    y: f32,
    scale: f32,
    color: Color,
) {
    if matches!(glyph, Glyph::Home | Glyph::Info) {
        let action = if matches!(glyph, Glyph::Home) {
            Action::Home
        } else {
            Action::Version
        };
        // Reuse the native 20-DIP toolbar glyphs in the reference's 17-DIP slot.
        let factor = ICON_SIZE / 20.;
        chrome_icon(
            canvas,
            x / factor,
            y / factor,
            scale * factor,
            action,
            false,
            color,
        );
        return;
    }

    // The reference's menu icons use a 24-unit view box and a 1.65-unit stroke.
    let unit = ICON_SIZE * scale / 24.;
    let mut stroke = |(x1, y1), (x2, y2)| {
        canvas.stroke_line(
            x * scale + x1 * unit,
            y * scale + y1 * unit,
            x * scale + x2 * unit,
            y * scale + y2 * unit,
            1.65 * unit,
            color,
        );
    };
    match glyph {
        Glyph::Plus | Glyph::Minus => {
            stroke((5., 12.), (19., 12.));
            if matches!(glyph, Glyph::Plus) {
                stroke((12., 5.), (12., 19.));
            }
        }
        Glyph::History => {
            arc(&mut stroke, (12., 12.), 8.5, -0.46, 0.42, 22);
            path(&mut stroke, &[(4., 4.), (4., 10.), (10., 10.)]);
            path(&mut stroke, &[(12., 7.), (12., 12.), (16., 14.)]);
        }
        Glyph::Book => {
            path(
                &mut stroke,
                &[
                    (5., 19.), (5., 5.), (7., 3.), (19., 3.), (19., 21.), (7., 21.),
                    (5., 20.), (5., 18.), (7., 17.), (19., 17.),
                ],
            );
            path(&mut stroke, &[(10., 3.), (10., 12.), (13., 10.), (16., 12.), (16., 3.)]);
        }
        Glyph::Zoom => {
            arc(&mut stroke, (10., 10.), 6., 0., 1., 16);
            stroke((14.5, 14.5), (19.5, 19.5));
            stroke((7., 10.), (13., 10.));
            stroke((10., 7.), (10., 13.));
        }
        Glyph::Code => {
            path(&mut stroke, &[(8., 6.), (2., 12.), (8., 18.)]);
            path(&mut stroke, &[(16., 6.), (22., 12.), (16., 18.)]);
            stroke((14., 3.), (10., 21.));
        }
        Glyph::Settings => {
            path(
                &mut stroke,
                &[
                    (10., 3.), (9.2, 6.1), (6.8, 7.1), (4., 5.8), (2., 9.), (4.4, 11.),
                    (4.4, 13.), (2., 15.), (4., 18.2), (6.8, 16.9), (9.2, 17.9), (10., 21.),
                    (14., 21.), (14.8, 17.9), (17.2, 16.9), (20., 18.2), (22., 15.),
                    (19.6, 13.), (19.6, 11.), (22., 9.), (20., 5.8), (17.2, 7.1),
                    (14.8, 6.1), (14., 3.), (10., 3.),
                ],
            );
            arc(&mut stroke, (12., 12.), 3., 0., 1., 10);
        }
        Glyph::Keyboard => {
            path(
                &mut stroke,
                &[
                    (4., 5.), (20., 5.), (22., 7.), (22., 17.), (20., 19.), (4., 19.),
                    (2., 17.), (2., 7.), (4., 5.),
                ],
            );
            for y in [9., 13.] {
                for x in [6., 10., 14., 18.] {
                    stroke((x, y), (x, y));
                }
            }
            stroke((8., 16.), (16., 16.));
        }
        Glyph::Home | Glyph::Info => unreachable!("native toolbar glyph"),
    }
}

fn path(stroke: &mut impl FnMut(Point, Point), points: &[Point]) {
    for pair in points.windows(2) {
        stroke(pair[0], pair[1]);
    }
}

fn arc(
    stroke: &mut impl FnMut(Point, Point),
    center: Point,
    radius: f32,
    start: f32,
    end: f32,
    segments: u32,
) {
    let point = |step| {
        let angle = (start + (end - start) * step as f32 / segments as f32) * TAU;
        (center.0 + radius * angle.cos(), center.1 + radius * angle.sin())
    };
    for step in 0..segments {
        stroke(point(step), point(step + 1));
    }
}
