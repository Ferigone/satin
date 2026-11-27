use super::{
    FillRule, FlatPath, SvgScene, SvgTransform,
    scene::{Command, Paint, Spread, Stop},
};
use crate::css::Color;
use ::vello::{
    Scene,
    kurbo::{Affine, BezPath, Rect},
    peniko::{Color as VelloColor, ColorStop, Extend, Fill, Gradient, Mix},
};

fn shape(path: &FlatPath) -> BezPath {
    let mut result = BezPath::new();
    for contour in &path.contours {
        let Some((first, remaining)) = contour.points.split_first() else {
            continue;
        };
        result.move_to((first.x, first.y));
        for point in remaining {
            result.line_to((point.x, point.y));
        }
        result.close_path();
    }
    result
}

fn fill(rule: FillRule) -> Fill {
    match rule {
        FillRule::NonZero => Fill::NonZero,
        FillRule::EvenOdd => Fill::EvenOdd,
    }
}

fn color(Color(red, green, blue, alpha): Color) -> VelloColor {
    VelloColor::from_rgba8(red, green, blue, alpha)
}

fn affine(transform: SvgTransform) -> Affine {
    Affine::new([
        transform.a, transform.b, transform.c,
        transform.d, transform.e, transform.f,
    ])
}

fn gradient(gradient: Gradient, stops: &[Stop], spread: Spread, opacity: f32) -> Gradient {
    let extend = match spread {
        Spread::Pad => Extend::Pad,
        Spread::Repeat => Extend::Repeat,
        Spread::Reflect => Extend::Reflect,
    };
    let stops: Vec<_> = stops.iter().map(|stop| {
        let mut value = stop.color;
        value.3 = (f32::from(value.3) * opacity).round() as u8;
        ColorStop::from((stop.offset as f32, color(value)))
    }).collect();
    gradient.with_extend(extend).with_stops(stops.as_slice())
}

impl SvgScene {
    pub(crate) fn record_vello(&self, scene: &mut Scene, transform: Affine) -> Result<(), String> {
        let viewport = Rect::new(0., 0., f64::from(self.width), f64::from(self.height));
        for command in &self.commands {
            match command {
                Command::PushOpacity(opacity) => {
                    scene.push_layer(Fill::NonZero, Mix::Normal, *opacity, transform, &viewport);
                }
                Command::PopOpacity => scene.pop_layer(),
                Command::Paint { path, rule, paint, clips, .. } => {
                    for clip in clips {
                        let [(path, rule)] = clip.paths.as_slice() else {
                            return Err("Vello SVG clip unions are not integrated".into());
                        };
                        scene.push_clip_layer(fill(*rule), transform, &shape(path));
                    }
                    let path = shape(path);
                    let rule = fill(*rule);
                    match paint {
                        Paint::Solid(value) => scene.fill(rule, transform, color(*value), None, &path),
                        Paint::Linear { inverse, from, to, stops, spread, opacity } => {
                            let paint = gradient(
                                Gradient::new_linear((from.x, from.y), (to.x, to.y)),
                                stops, *spread, *opacity,
                            );
                            scene.fill(rule, transform, &paint, inverse.inverse().map(affine), &path);
                        }
                        Paint::Radial { inverse, center, focus, radius, stops, spread, opacity } => {
                            let paint = gradient(
                                Gradient::new_two_point_radial(
                                    (focus.x, focus.y), 0., (center.x, center.y), *radius as f32,
                                ),
                                stops, *spread, *opacity,
                            );
                            scene.fill(rule, transform, &paint, inverse.inverse().map(affine), &path);
                        }
                    }
                    for _ in clips {
                        scene.pop_layer();
                    }
                }
            }
        }
        Ok(())
    }
}
