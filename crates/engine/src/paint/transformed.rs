use super::*;

pub(super) fn valid_rect(rect: Rect) -> bool {
    [
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        rect.x + rect.width,
        rect.y + rect.height,
    ]
    .iter()
    .all(|v| v.is_finite())
        && rect.width > 0.
        && rect.height > 0.
}
fn intersection(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    Rect {
        x,
        y,
        width: ((a.x + a.width).min(b.x + b.width) - x).max(0.),
        height: ((a.y + a.height).min(b.y + b.height) - y).max(0.),
    }
}
pub(super) fn inside_clips(clips: &[PaintClip], x: f32, y: f32) -> bool {
    clips.iter().all(|clip| {
        let (x, y) = clip.inverse.point(x, y);
        clip.rect.contains(x, y)
    })
}
impl Canvas {
    fn visible(&self, rect: Rect) -> bool {
        let clip = intersection(
            self.clip,
            Rect {
                x: 0.,
                y: 0.,
                width: self.width as f32,
                height: self.height as f32,
            },
        );
        valid_rect(rect)
            && valid_rect(clip)
            && valid_rect(intersection(self.transform.rect(rect), clip))
    }
    pub(super) fn text_visible(&self, x: f32, baseline: f32, run: &Run, scale: f32) -> bool {
        let margin = run.size * 2. * scale;
        let rect = Rect {
            x: x - margin,
            y: baseline - run.ascent * scale - margin,
            width: run.advance * scale + margin * 2.,
            height: (run.ascent + run.descent) * scale + margin * 2.,
        };
        self.visible(rect)
    }
    pub(super) fn svg_scene(&mut self, rect: Rect, scene: &crate::svg::SvgScene) {
        if !valid_rect(rect)
            || !valid_rect(self.clip)
            || scene.width <= 0.
            || scene.height <= 0.
            || self.work_left == 0
        {
            return;
        }
        let matrix = self.transform.multiply(Affine2D {
            a: rect.width / scene.width,
            d: rect.height / scene.height,
            e: rect.x,
            f: rect.y,
            ..Affine2D::IDENTITY
        });
        let clips = &self.clips;
        let clip = self.clip;
        let clip_work = std::cell::Cell::new(0usize);
        let clip_exhausted = std::cell::Cell::new(false);
        let quota = self.work_left;
        let report = scene.paint_transformed(
            &mut self.pixels,
            self.width,
            self.height,
            [matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f],
            crate::svg::SvgRect {
                x: clip.x,
                y: clip.y,
                width: clip.width,
                height: clip.height,
            },
            |x, y| {
                let next = clip_work.get().saturating_add(clips.len() + 1);
                if next > quota {
                    clip_exhausted.set(true);
                    return false;
                }
                clip_work.set(next);
                clip.contains(x, y) && inside_clips(clips, x, y)
            },
        );
        self.charge_pixels(report.work.saturating_add(clip_work.get()));
        self.limited |= clip_exhausted.get();
        self.limited |= report.truncated || report.invalid_surface || scene.diagnostics.truncated;
    }
    pub(super) fn charge_pixels(&mut self, work: usize) -> bool {
        if work > self.work_left {
            self.limited = true;
            self.work_left = 0;
            false
        } else {
            self.work_left -= work;
            true
        }
    }
    pub(super) fn push_state(&mut self, transform: Affine2D, clip: Option<Rect>) {
        self.stack.push(PaintState {
            transform: self.transform,
            clip: self.clip,
            clips: self.clips.len(),
        });
        self.transform = self.transform.multiply(transform);
        let Some(inverse) = self.transform.inverse() else {
            self.clip = Rect::default();
            return;
        };
        if let Some(rect) = clip {
            if !valid_rect(rect) {
                self.clip = Rect::default();
                return;
            }
            let mapped = self.transform.rect(rect);
            if !valid_rect(mapped) {
                self.clip = Rect::default();
                return;
            }
            self.clip = intersection(self.clip, mapped);
            // Rectilinear clips need only the intersected device bounding box.
            // Rotated/skewed clips retain their inverse local-space test.
            if self.transform.b != 0. || self.transform.c != 0. {
                self.clips.push(PaintClip { inverse, rect });
            }
        }
    }
    pub(super) fn pop_state(&mut self) {
        if let Some(prior) = self.stack.pop() {
            self.transform = prior.transform;
            self.clip = prior.clip;
            self.clips.truncate(prior.clips);
        }
    }
    pub(super) fn raster_rect<F: Fn(f32, f32) -> Option<Color>>(&mut self, rect: Rect, sample: F) {
        if !valid_rect(rect) || !valid_rect(self.clip) {
            return;
        }
        let Some(inverse) = self.transform.inverse() else {
            return;
        };
        let bounds = self.transform.rect(rect);
        if !valid_rect(bounds) {
            return;
        }
        let bounds = intersection(
            intersection(bounds, self.clip),
            Rect {
                x: 0.,
                y: 0.,
                width: self.width as f32,
                height: self.height as f32,
            },
        );
        if !valid_rect(bounds) {
            return;
        }
        let left = bounds.x.floor().max(0.) as u32;
        let top = bounds.y.floor().max(0.) as u32;
        let right = (bounds.x + bounds.width).ceil().min(self.width as f32) as u32;
        let bottom = (bounds.y + bounds.height).ceil().min(self.height as f32) as u32;
        let work = (right - left) as usize * (bottom - top) as usize;
        if !self.charge_pixels(work.saturating_mul(self.clips.len() + 1)) {
            return;
        }
        for y in top..bottom {
            for x in left..right {
                let dx = x as f32 + 0.5;
                let dy = y as f32 + 0.5;
                if !self.clip.contains(dx, dy) || !inside_clips(&self.clips, dx, dy) {
                    continue;
                }
                let (lx, ly) = inverse.point(dx, dy);
                if !rect.contains(lx, ly) {
                    continue;
                }
                if let Some(color) = sample(lx, ly)
                    && color.3 != 0
                {
                    let index = (y * self.width + x) as usize;
                    self.pixels[index] = blend(self.pixels[index], color);
                }
            }
        }
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Text primitive keeps explicit coordinates and sampling scale"
    )]
    pub(super) fn transformed_text(
        &mut self,
        x: f32,
        baseline: f32,
        run: &Run,
        color: Color,
        fonts: &mut Fonts,
        scale: f32,
    ) {
        if run.size <= 0. || !scale.is_finite() || scale <= 0. {
            return;
        }
        let requested = self
            .transform
            .a
            .hypot(self.transform.b)
            .max(self.transform.c.hypot(self.transform.d))
            .clamp(0.0625, 64.);
        let effective = (run.size * scale * requested * 16.)
            .round()
            .clamp(1., 8192.)
            / 16.;
        let sampling = effective / (run.size * scale);
        for glyph in &run.glyphs {
            let face = run.face_for(glyph);
            let x = x + glyph.x * scale;
            let y = baseline - glyph.y * scale;
            if let Some(bounds) = face.ink_bounds(glyph.id, effective)
                && !self.visible(Rect {
                    x: x + bounds[0] / sampling,
                    y: y + bounds[1] / sampling,
                    width: (bounds[2] - bounds[0]) / sampling,
                    height: (bounds[3] - bounds[1]) / sampling,
                })
            {
                continue;
            }
            let (m, bitmap) =
                match fonts.raster_glyph(face, glyph.id, effective, &mut self.work_left) {
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
            let rect = Rect {
                x: x + m.xmin as f32 / sampling,
                y: y - (m.ymin as f32 + m.height as f32) / sampling,
                width: m.width as f32 / sampling,
                height: m.height as f32 / sampling,
            };
            if !self.visible(rect) {
                continue;
            }
            self.raster_rect(rect, |lx, ly| {
                let col = ((lx - rect.x) * sampling) as usize;
                let row = ((ly - rect.y) * sampling) as usize;
                let alpha = bitmap.get(row * m.width + col).copied().unwrap_or(0) as u32
                    * color.3 as u32
                    / 255;
                Some(Color(color.0, color.1, color.2, alpha as u8))
            });
            if self.work_left == 0 {
                break;
            }
        }
    }
    pub(super) fn transformed_disc(&mut self, cx: f32, cy: f32, radius: f32, color: Color) {
        self.raster_rect(
            Rect {
                x: cx - radius,
                y: cy - radius,
                width: radius * 2.,
                height: radius * 2.,
            },
            |x, y| ((x - cx).powi(2) + (y - cy).powi(2) <= radius * radius).then_some(color),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transformed_clip_tests_local_shape_and_restores_canvas_state() {
        let mut c = Canvas::new(16, 16, Color::WHITE);
        let matrix = Affine2D {
            a: 1.,
            b: 1.,
            c: -1.,
            d: 1.,
            e: 8.,
            f: 0.,
        };
        c.push_state(
            matrix,
            Some(Rect {
                x: 0.,
                y: 0.,
                width: 4.,
                height: 4.,
            }),
        );
        c.fill_rect(
            Rect {
                x: -100.,
                y: -100.,
                width: 200.,
                height: 200.,
            },
            Color(255, 0, 0, 255),
        );
        assert_eq!(c.pixels[3 * 16 + 8], 0xff0000);
        assert_eq!(c.pixels[16 + 4], 0xffffff); // Inside bounding box, outside rotated clip.
        c.pop_state();
        c.fill_rect(
            Rect {
                x: 0.,
                y: 0.,
                width: 1.,
                height: 1.,
            },
            Color::BLACK,
        );
        assert_eq!(c.pixels[0], 0);
        assert_eq!(c.transform, Affine2D::IDENTITY);
    }
    #[test]
    fn mirrored_image_and_singular_transform() {
        let mut c = Canvas::new(4, 2, Color::WHITE);
        let mut image = image::RgbaImage::new(2, 1);
        image.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        image.put_pixel(1, 0, image::Rgba([0, 0, 255, 255]));
        c.push_state(
            Affine2D {
                a: -1.,
                e: 2.,
                ..Affine2D::IDENTITY
            },
            None,
        );
        c.image(
            Rect {
                x: 0.,
                y: 0.,
                width: 2.,
                height: 1.,
            },
            &image,
        );
        c.pop_state();
        assert_eq!(&c.pixels[..2], &[0xff, 0xff0000]);
        c.push_state(
            Affine2D {
                a: 0.,
                ..Affine2D::IDENTITY
            },
            None,
        );
        c.fill_rect(
            Rect {
                x: 0.,
                y: 0.,
                width: 4.,
                height: 2.,
            },
            Color::BLACK,
        );
        c.pop_state();
        assert_eq!(c.pixels[2], 0xffffff);
    }
    #[test]
    fn raster_work_limit_is_explicit() {
        let mut c = Canvas::new(4, 4, Color::WHITE);
        c.work_left = 1;
        c.fill_rect(
            Rect {
                x: 0.,
                y: 0.,
                width: 4.,
                height: 4.,
            },
            Color::BLACK,
        );
        assert!(c.limited);
        assert!(c.pixels.iter().all(|p| *p == 0xffffff));
    }
    #[test]
    fn offscreen_and_over_budget_text_does_not_rasterize_glyphs() {
        let mut fonts = Fonts::new().unwrap();
        let run = fonts.shape("ABCDEFGHIJKLMNOPQRSTUVWXYZ", 512., false, false, false);
        let mut canvas = Canvas::new(400, 300, Color::WHITE);
        canvas.text(0., 100000., &run, Color::BLACK, &mut fonts);
        assert_eq!(fonts.cache_bytes(), 0);
        canvas.push_state(
            Affine2D {
                a: 0.8,
                b: 0.6,
                c: -0.6,
                d: 0.8,
                e: 100000.,
                f: 100000.,
            },
            None,
        );
        canvas.text(0., 0., &run, Color::BLACK, &mut fonts);
        canvas.pop_state();
        assert_eq!(fonts.cache_bytes(), 0);
        canvas.work_left = 0;
        canvas.text(0., 200., &run, Color::BLACK, &mut fonts);
        assert_eq!(fonts.cache_bytes(), 0);
        canvas.work_left = 64 * 1024 * 1024;
        canvas.text(1e30, 200., &run, Color::BLACK, &mut fonts);
        assert_eq!(fonts.cache_bytes(), 0);
    }
    #[test]
    fn content_budget_exhaustion_does_not_starve_ui_paint() {
        let page = crate::LoadedPage::from_html(
            "https://example.test/",
            "<!doctype html><style>body{margin:0}div{width:10px;height:10px;background:green}</style><div></div>",
        );
        let mut fonts = Fonts::new().unwrap();
        let layout = Layout::build(&page, &fonts, 16.);
        let mut canvas = Canvas::new(16, 16, Color::WHITE);
        canvas.work_left = 1;
        canvas.document(&layout, &mut fonts, 0., 0.);
        assert!(canvas.limited);
        canvas.fill_rect(
            Rect {
                x: 15.,
                y: 15.,
                width: 1.,
                height: 1.,
            },
            Color::BLACK,
        );
        assert_eq!(canvas.pixels[255], 0);
    }
}
