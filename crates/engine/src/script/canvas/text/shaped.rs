use super::{LayoutHost, Metrics, OutlineCommand, TextState, limit};
use crate::{
    css::canvas_font::stretch_factor,
    fonts::{CanvasShaping, Glyph, Run},
};
use rust_browser_javascript::{JsError, Vm};
use rustybuzz::{Feature, ttf_parser as ttf};

const MAX_OUTLINE_COMMANDS: usize = 64 * 1024;

pub(super) struct Prepared {
    run: Run,
    scale: [f64; 2],
    width: f64,
    anchor: [f64; 2],
    font: FontMetrics,
}

pub(super) fn prepare(
    vm: &mut Vm,
    host: &LayoutHost,
    style: &TextState,
    rtl: bool,
    language: Option<&str>,
    text: &str,
) -> Result<Prepared, JsError> {
    vm.charge_work(text.len().saturating_mul(64).saturating_add(64))?;
    let shape_size = style.font.size.min(512.);
    let scale_y = if shape_size == 0. {
        1.
    } else {
        f64::from(style.font.size) / f64::from(shape_size)
    };
    let stretch = if style.stretch == "normal" {
        style.font.stretch
    } else {
        stretch_factor(style.stretch).unwrap_or(1.)
    };
    let scale = [scale_y * stretch, scale_y];
    let viewport = host.viewport().width;
    let (run, font) = host.with_canvas_fonts(|fonts, set| {
        let primary = fonts.shape_text("", shape_size, &style.font.request, set);
        let face = primary.faces()[0].outline_face();
        let size = f64::from(style.font.size);
        let resolve = |spacing: &super::Spacing| {
            let pixels = spacing.resolve(viewport, style.font.size, &face);
            // A very large CSS length can exceed the shaper's finite coordinates.
            (pixels / scale[0]).clamp(-1e9, 1e9) as f32
        };
        let letter_spacing = resolve(&style.letter_spacing);
        let features = font_features(style, letter_spacing != 0.);
        let options = CanvasShaping {
            language,
            features: &features,
            letter_spacing,
            word_spacing: resolve(&style.word_spacing),
        };
        let run = fonts.shape_canvas(text, shape_size, &style.font.request, set, options);
        (run, FontMetrics::new(&face, size))
    })?;
    let width = f64::from(run.advance) * scale[0];
    let x = match style.align {
        "center" => width / 2.,
        "right" => width,
        "start" if rtl => width,
        "end" if !rtl => width,
        _ => 0.,
    };
    let y = match style.baseline {
        "top" => font.em_ascent,
        "hanging" => font.hanging,
        "middle" => (font.em_ascent - font.em_descent) / 2.,
        "ideographic" => font.ideographic,
        "bottom" => -font.em_descent,
        _ => 0.,
    };
    Ok(Prepared {
        run,
        scale,
        width,
        anchor: [x, y],
        font,
    })
}

fn font_features(style: &TextState, tracked: bool) -> Vec<Feature> {
    let mut features = Vec::new();
    let mut feature = |name: &[u8; 4], enabled: bool| {
        features.push(Feature::new(ttf::Tag::from_bytes(name), enabled.into(), ..));
    };
    if style.kerning == "none" || (style.kerning == "auto" && style.rendering == "optimizeSpeed") {
        feature(b"kern", false);
    }
    if tracked {
        feature(b"liga", false);
        feature(b"clig", false);
    }
    let caps = if style.caps == "normal" && style.font.small_caps {
        "small-caps"
    } else {
        style.caps
    };
    match caps {
        "small-caps" => feature(b"smcp", true),
        "all-small-caps" => {
            feature(b"smcp", true);
            feature(b"c2sc", true);
        }
        "petite-caps" => feature(b"pcap", true),
        "all-petite-caps" => {
            feature(b"pcap", true);
            feature(b"c2pc", true);
        }
        "unicase" => feature(b"unic", true),
        "titling-caps" => feature(b"titl", true),
        _ => {}
    }
    features
}

struct FontMetrics {
    size: f64,
    ascent: f64,
    descent: f64,
    em_ascent: f64,
    em_descent: f64,
    hanging: f64,
    ideographic: f64,
}

impl FontMetrics {
    fn new(face: &ttf::Face<'_>, size: f64) -> Self {
        let scale = size / f64::from(face.units_per_em());
        let ascent = f64::from(face.ascender()).max(0.) * scale;
        let descent = -f64::from(face.descender()).min(0.) * scale;
        let typo_ascent = f64::from(face.typographic_ascender().unwrap_or(face.ascender())).max(0.);
        let typo_descent =
            -f64::from(face.typographic_descender().unwrap_or(face.descender())).min(0.);
        let em_ascent = if typo_ascent + typo_descent > 0. {
            size * typo_ascent / (typo_ascent + typo_descent)
        } else {
            size * 0.8
        };
        Self {
            size,
            ascent,
            descent,
            em_ascent,
            em_descent: size - em_ascent,
            hanging: ascent * 0.8,
            ideographic: -descent,
        }
    }
}

impl Prepared {
    fn charge_outlines(&self, vm: &mut Vm) -> Result<(), JsError> {
        let work = self.run.glyphs.iter().fold(0usize, |work, glyph| {
            work.saturating_add(self.run.face_for(glyph).outline_cost(glyph.id) + 16)
        });
        vm.charge_work(work)
    }

    pub(super) fn metrics(&self, vm: &mut Vm) -> Result<Metrics, JsError> {
        self.charge_outlines(vm)?;
        let faces = self
            .run
            .faces()
            .iter()
            .map(|face| face.outline_face())
            .collect::<Vec<_>>();
        let mut bounds: Option<[f64; 4]> = None;
        for glyph in &self.run.glyphs {
            let face = &faces[usize::from(glyph.face_slot)];
            let Some(ink) = face.glyph_bounding_box(ttf::GlyphId(glyph.id)) else {
                continue;
            };
            let k = self.font.size / f64::from(face.units_per_em());
            let stretch = self.scale[0] / self.scale[1];
            let [left, right] = [ink.x_min, ink.x_max]
                .map(|x| f64::from(glyph.x) * self.scale[0] + f64::from(x) * k * stretch);
            let [top, bottom] = [ink.y_max, ink.y_min]
                .map(|y| -f64::from(glyph.y) * self.scale[1] - f64::from(y) * k);
            if let Some(old) = &mut bounds {
                *old = [
                    old[0].min(left),
                    old[1].min(top),
                    old[2].max(right),
                    old[3].max(bottom),
                ];
            } else {
                bounds = Some([left, top, right, bottom]);
            }
        }
        let [x, y] = self.anchor;
        let [left, right, ascent, descent] = bounds.map_or([0.; 4], |ink| {
            [x - ink[0], ink[2] - x, -ink[1] - y, ink[3] + y]
        });
        Ok(Metrics([
            self.width,
            left,
            right,
            self.font.ascent - y,
            self.font.descent + y,
            ascent,
            descent,
            self.font.em_ascent - y,
            self.font.em_descent + y,
            self.font.hanging - y,
            -y,
            self.font.ideographic - y,
        ]))
    }

    pub(super) fn outline(
        &self,
        vm: &mut Vm,
        x: f64,
        y: f64,
        max_width: Option<f64>,
    ) -> Result<Vec<OutlineCommand>, JsError> {
        if self.font.size == 0. {
            return Ok(vec![]);
        }
        self.charge_outlines(vm)?;
        let compress = max_width
            .filter(|&max| max < self.width)
            .map_or(1., |max| max / self.width);
        let faces = self
            .run
            .faces()
            .iter()
            .map(|face| face.outline_face())
            .collect::<Vec<_>>();
        let mut commands = Vec::new();
        for glyph in &self.run.glyphs {
            let Glyph {
                face_slot,
                id,
                x: gx,
                y: gy,
            } = *glyph;
            let face = &faces[usize::from(face_slot)];
            let scale = self.font.size / f64::from(face.units_per_em());
            let previous = commands.len();
            let mut builder = Builder {
                origin: [
                    x + (f64::from(gx) * self.scale[0] - self.anchor[0]) * compress,
                    y + self.anchor[1] - f64::from(gy) * self.scale[1],
                ],
                scale: [scale * (self.scale[0] / self.scale[1]) * compress, -scale],
                commands: &mut commands,
                overflow: false,
            };
            let has_outline = face.outline_glyph(ttf::GlyphId(id), &mut builder).is_some();
            if builder.overflow {
                return Err(limit("Canvas text outline exceeds its segment budget"));
            }
            if !has_outline {
                commands.truncate(previous);
            }
        }
        Ok(commands)
    }
}

struct Builder<'a> {
    origin: [f64; 2],
    scale: [f64; 2],
    commands: &'a mut Vec<OutlineCommand>,
    overflow: bool,
}

impl Builder<'_> {
    fn point(&self, x: f32, y: f32) -> [f64; 2] {
        [
            self.origin[0] + f64::from(x) * self.scale[0],
            self.origin[1] + f64::from(y) * self.scale[1],
        ]
    }

    fn push(&mut self, command: OutlineCommand) {
        if self.commands.len() < MAX_OUTLINE_COMMANDS {
            self.commands.push(command);
        } else {
            self.overflow = true;
        }
    }
}

impl ttf::OutlineBuilder for Builder<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.push(OutlineCommand::Move(self.point(x, y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.push(OutlineCommand::Line(self.point(x, y)));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.push(OutlineCommand::Quadratic(
            self.point(cx, cy),
            self.point(x, y),
        ));
    }
    fn curve_to(&mut self, a: f32, b: f32, c: f32, d: f32, x: f32, y: f32) {
        self.push(OutlineCommand::Cubic(
            self.point(a, b),
            self.point(c, d),
            self.point(x, y),
        ));
    }
    fn close(&mut self) {
        self.push(OutlineCommand::Close);
    }
}
