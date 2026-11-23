use crate::{
    css::Color,
    fonts::{Fonts, RasterFailure, Run},
    layout::{Affine2D, DrawCommand, Layout, Rect},
};
mod controls;
mod transformed;
use transformed::valid_rect;

#[derive(Clone, Copy)]
struct PaintClip {
    inverse: Affine2D,
    rect: Rect,
}
#[derive(Clone, Copy)]
struct PaintState {
    transform: Affine2D,
    clip: Rect,
    clips: usize,
}

pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
    clip: Rect,
    transform: Affine2D,
    clips: Vec<PaintClip>,
    stack: Vec<PaintState>,
    work_left: usize,
    /// Drawing exhausted a bounded raster work allowance.
    pub limited: bool,
}
impl Canvas {
    pub fn stroke_line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, width: f32, color: Color) {
        let steps = ((x2 - x1).abs().max((y2 - y1).abs()).ceil() as usize).clamp(1, 65536);
        if !self.charge_pixels(steps) {
            return;
        }
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            self.fill_rect(
                Rect {
                    x: x1 + (x2 - x1) * t - width / 2.,
                    y: y1 + (y2 - y1) * t - width / 2.,
                    width,
                    height: width,
                },
                color,
            );
        }
    }
    pub fn new(width: u32, height: u32, color: Color) -> Self {
        let mut c = Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize],
            clip: Rect {
                x: 0.,
                y: 0.,
                width: width as f32,
                height: height as f32,
            },
            transform: Affine2D::IDENTITY,
            clips: Vec::new(),
            stack: Vec::new(),
            work_left: 64 * 1024 * 1024,
            limited: false,
        };
        c.clear(color);
        c
    }
    pub fn clear(&mut self, color: Color) {
        let pixel = (color.0 as u32) << 16 | (color.1 as u32) << 8 | color.2 as u32;
        self.pixels.fill(pixel);
        self.work_left = 64 * 1024 * 1024;
        self.limited = false;
    }
    pub fn clip(&mut self, rect: Rect) {
        self.clip = rect;
    }
    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        if color.3 == 0 {
            return;
        }
        if self.transform.b != 0. || self.transform.c != 0. || !self.clips.is_empty() {
            self.raster_rect(rect, |_, _| Some(color));
            return;
        }
        let rect = self.transform.rect(rect);
        if !valid_rect(rect) {
            return;
        }
        let left = rect.x.max(self.clip.x).max(0.).floor() as u32;
        let right = (rect.x + rect.width)
            .min(self.clip.x + self.clip.width)
            .min(self.width as f32)
            .ceil()
            .max(0.) as u32;
        let top = rect.y.max(self.clip.y).max(0.).floor() as u32;
        let bottom = (rect.y + rect.height)
            .min(self.clip.y + self.clip.height)
            .min(self.height as f32)
            .ceil()
            .max(0.) as u32;
        if left >= right || top >= bottom {
            return;
        }
        if !self.charge_pixels((right - left) as usize * (bottom - top) as usize) {
            return;
        }
        let packed = (color.0 as u32) << 16 | (color.1 as u32) << 8 | color.2 as u32;
        for y in top..bottom {
            let row = &mut self.pixels
                [(y * self.width + left) as usize..(y * self.width + right) as usize];
            if color.3 == 255 {
                row.fill(packed);
            } else {
                for pixel in row {
                    *pixel = blend(*pixel, color);
                }
            }
        }
    }
    pub fn text(&mut self, x: f32, baseline: f32, run: &Run, color: Color, fonts: &mut Fonts) {
        self.text_scaled(x, baseline, run, color, fonts, 1.);
    }
    pub fn text_scaled(
        &mut self,
        x: f32,
        baseline: f32,
        run: &Run,
        color: Color,
        fonts: &mut Fonts,
        scale: f32,
    ) {
        if self.work_left == 0 || color.3 == 0 || !self.text_visible(x, baseline, run, scale) {
            return;
        }
        if self.transform.b != 0.
            || self.transform.c != 0.
            || self.transform.a <= 0.
            || self.transform.a != self.transform.d
            || !self.clips.is_empty()
        {
            self.transformed_text(x, baseline, run, color, fonts, scale);
            return;
        }
        let (x, baseline) = self.transform.point(x, baseline);
        let scale = scale * self.transform.a;
        for glyph in &run.glyphs {
            let face = run.face_for(glyph);
            let x = (x + glyph.x * scale).round();
            let y = (baseline - glyph.y * scale).round();
            if let Some(bounds) = face.ink_bounds(glyph.id, run.size * scale)
                && (x + bounds[2] <= self.clip.x
                    || x + bounds[0] >= self.clip.x + self.clip.width
                    || y + bounds[3] <= self.clip.y
                    || y + bounds[1] >= self.clip.y + self.clip.height)
            {
                continue;
            }
            let (m, bitmap) =
                match fonts.raster_glyph(face, glyph.id, run.size * scale, &mut self.work_left) {
                    Ok(glyph) => glyph,
                    Err(RasterFailure::Invalid) => continue,
                    Err(RasterFailure::Limit) => {
                        self.limited = true;
                        if self.work_left == 0 {
                            break;
                        }
                        continue;
                    }
                };
            let gx = x + m.xmin as f32;
            let gy = y - m.ymin as f32 - m.height as f32;
            if !gx.is_finite()
                || !gy.is_finite()
                || gx + m.width as f32 <= self.clip.x
                || gx >= self.width as f32
                || gy + m.height as f32 <= self.clip.y
                || gy >= self.height as f32
            {
                continue;
            }
            let gx = gx as i32;
            let gy = gy as i32;
            if !self.charge_pixels(m.width.saturating_mul(m.height)) {
                break;
            }
            for row in 0..m.height {
                let py = gy + row as i32;
                if py < 0
                    || py < self.clip.y.ceil() as i32
                    || py >= self.height as i32
                    || py >= (self.clip.y + self.clip.height).ceil() as i32
                {
                    continue;
                }
                for col in 0..m.width {
                    let px = gx + col as i32;
                    if px < 0
                        || px < self.clip.x.ceil() as i32
                        || px >= self.width as i32
                        || px >= (self.clip.x + self.clip.width).ceil() as i32
                    {
                        continue;
                    }
                    let alpha = bitmap[row * m.width + col] as u32 * color.3 as u32 / 255;
                    if alpha == 0 {
                        continue;
                    }
                    let index = py as usize * self.width as usize + px as usize;
                    self.pixels[index] = blend(
                        self.pixels[index],
                        Color(color.0, color.1, color.2, alpha as u8),
                    );
                }
            }
        }
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "The drawing primitive exposes text, coordinates and style explicitly"
    )]
    pub fn label(
        &mut self,
        text: &str,
        x: f32,
        baseline: f32,
        size: f32,
        color: Color,
        bold: bool,
        fonts: &mut Fonts,
    ) {
        let run = fonts.shape(text, size, bold, false, false);
        self.text(x, baseline, &run, color, fonts);
    }
    pub fn document(&mut self, layout: &Layout, fonts: &mut Fonts, offset: f32, scroll: f32) {
        self.document_scaled(layout, fonts, offset, scroll, 1.);
    }
    pub fn document_scaled(
        &mut self,
        layout: &Layout,
        fonts: &mut Fonts,
        offset: f32,
        scroll: f32,
        scale: f32,
    ) {
        fonts.prune_raster_cache();
        let ui_work = self.work_left;
        let depth = self.stack.len();
        self.push_state(
            Affine2D {
                a: scale,
                d: scale,
                e: -layout.viewport.scroll_x * scale,
                f: offset - scroll * scale,
                ..Affine2D::IDENTITY
            },
            None,
        );
        for command in &layout.commands {
            match command {
                DrawCommand::Svg { rect, scene, .. } => self.svg_scene(*rect, scene),
                DrawCommand::PushState { transform, clip } => self.push_state(*transform, *clip),
                DrawCommand::PopState => {
                    if self.stack.len() > depth + 1 {
                        self.pop_state();
                    }
                }
                DrawCommand::Control {
                    rect,
                    control,
                    style,
                    ..
                } => self.form_control(*rect, control, style, fonts, 1.),
                DrawCommand::Rect { rect, color } => self.fill_rect(*rect, *color),
                DrawCommand::Text {
                    x,
                    baseline,
                    run,
                    color,
                    underline,
                } => {
                    self.text(*x, *baseline, run, *color, fonts);
                    if *underline {
                        self.fill_rect(
                            Rect {
                                x: *x,
                                y: baseline + 2.,
                                width: run.advance,
                                height: 1.,
                            },
                            *color,
                        );
                    }
                }
                DrawCommand::Image { rect, image } => self.image(*rect, image),
                DrawCommand::Canvas { node, rect } => match layout.canvas_frames.get(*node) {
                    Some(crate::canvas::Frame::Bitmap(image)) => self.image(*rect, &image),
                    Some(crate::canvas::Frame::OpaqueBlack) => self.fill_rect(*rect, Color::BLACK),
                    None => {}
                },
            }
        }
        while self.stack.len() > depth {
            self.pop_state();
        }
        // Content exhaustion must not prevent drawing the address bar and tabs.
        self.work_left = ui_work;
    }
    pub fn image(&mut self, rect: Rect, image: &image::RgbaImage) {
        if !valid_rect(rect) || image.width() == 0 || image.height() == 0 {
            return;
        }
        if self.transform.b != 0.
            || self.transform.c != 0.
            || self.transform.a <= 0.
            || self.transform.d <= 0.
            || !self.clips.is_empty()
        {
            self.raster_rect(rect, |x, y| {
                let sx = ((x - rect.x) / rect.width * image.width() as f32)
                    .clamp(0., image.width().saturating_sub(1) as f32)
                    as u32;
                let sy = ((y - rect.y) / rect.height * image.height() as f32)
                    .clamp(0., image.height().saturating_sub(1) as f32)
                    as u32;
                let p = image.get_pixel(sx, sy).0;
                Some(Color(p[0], p[1], p[2], p[3]))
            });
            return;
        }
        let rect = self.transform.rect(rect);
        if !valid_rect(rect) {
            return;
        }
        let left = rect.x.max(self.clip.x).max(0.).ceil() as u32;
        let right = (rect.x + rect.width)
            .min(self.clip.x + self.clip.width)
            .min(self.width as f32)
            .ceil()
            .max(0.) as u32;
        let top = rect.y.max(self.clip.y).max(0.).ceil() as u32;
        let bottom = (rect.y + rect.height)
            .min(self.clip.y + self.clip.height)
            .min(self.height as f32)
            .ceil()
            .max(0.) as u32;
        if !self.charge_pixels(
            right.saturating_sub(left) as usize * bottom.saturating_sub(top) as usize,
        ) {
            return;
        }
        for y in top..bottom {
            let sy = (((y as f32 - rect.y) / rect.height) * image.height() as f32)
                .clamp(0., image.height().saturating_sub(1) as f32) as u32;
            for x in left..right {
                let sx = (((x as f32 - rect.x) / rect.width) * image.width() as f32)
                    .clamp(0., image.width().saturating_sub(1) as f32)
                    as u32;
                let p = image.get_pixel(sx, sy).0;
                let i = (y * self.width + x) as usize;
                self.pixels[i] = blend(self.pixels[i], Color(p[0], p[1], p[2], p[3]));
            }
        }
    }
    pub fn save_png(&self, path: &std::path::Path) -> Result<(), image::ImageError> {
        let mut bytes = Vec::with_capacity(self.pixels.len() * 4);
        for &p in &self.pixels {
            bytes.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]);
        }
        image::save_buffer(
            path,
            &bytes,
            self.width,
            self.height,
            image::ColorType::Rgba8,
        )
    }
}

fn blend(background: u32, foreground: Color) -> u32 {
    let a = foreground.3 as u32;
    let inv = 255 - a;
    let r = (((background >> 16) & 255) * inv + foreground.0 as u32 * a + 127) / 255;
    let g = (((background >> 8) & 255) * inv + foreground.1 as u32 * a + 127) / 255;
    let b = ((background & 255) * inv + foreground.2 as u32 * a + 127) / 255;
    r << 16 | g << 8 | b
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipping_and_alpha() {
        let mut c = Canvas::new(8, 8, Color::WHITE);
        c.clip(Rect {
            x: 2.,
            y: 2.,
            width: 2.,
            height: 2.,
        });
        c.fill_rect(
            Rect {
                x: 0.,
                y: 0.,
                width: 8.,
                height: 8.,
            },
            Color(255, 0, 0, 128),
        );
        assert_eq!(c.pixels[0], 0xffffff);
        assert_eq!(c.pixels[18], 0xff7f7f);
        assert_eq!(c.pixels[36], 0xffffff);
    }
}
