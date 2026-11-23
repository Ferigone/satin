use crate::script::{arg, text};
use image::RgbaImage;
use rust_browser_javascript::{JsError, Value, Vm};
use std::sync::Arc;

use super::super::{
    bitmap::Bitmap,
    pixels::{Source, alpha_format, bilinear, rgba_storage},
};

#[derive(Clone, Copy, Default)]
enum Quality {
    #[default]
    Low,
    Medium,
    High,
    Pixelated,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Orientation {
    #[default]
    FromImage,
    FlipY,
    None,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Options {
    crop: Option<[i64; 4]>,
    resize: [Option<u32>; 2],
    orientation: Orientation,
    premultiplied: Option<bool>,
    quality: Quality,
}

impl Options {
    pub(super) fn from_call(vm: &mut Vm, args: &[Value]) -> Result<Self, JsError> {
        let (crop, dictionary) = match args.len() {
            1 | 2 => (None, arg(args, 1)),
            5.. => {
                let mut crop = [0; 4];
                for (target, value) in crop.iter_mut().zip(&args[1..5]) {
                    let number = vm.to_number(value)?.trunc();
                    *target = number.rem_euclid(4_294_967_296.) as u32 as i32 as i64;
                }
                (Some(crop), arg(args, 5))
            }
            _ => {
                return Err(JsError::type_error(
                    "Invalid createImageBitmap argument count",
                ));
            }
        };
        if dictionary.is_nullish() {
            return Ok(Self {
                crop,
                ..Self::default()
            });
        }
        if dictionary.as_object().is_none() {
            return Err(JsError::type_error("Expected ImageBitmapOptions"));
        }
        // Dictionary members are converted in lexicographic order, including
        // getters whose results do not affect this raster backend's color space.
        enum_member(
            vm,
            &dictionary,
            "colorSpaceConversion",
            &["default", "none"],
        )?;
        let orientation = match enum_member(
            vm,
            &dictionary,
            "imageOrientation",
            &["from-image", "flipY", "none"],
        )? {
            1 => Orientation::FlipY,
            2 => Orientation::None,
            _ => Orientation::FromImage,
        };
        let premultiplied = match enum_member(
            vm,
            &dictionary,
            "premultiplyAlpha",
            &["default", "premultiply", "none"],
        )? {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        let height = resize_member(vm, &dictionary, "resizeHeight")?;
        let quality = match enum_member(
            vm,
            &dictionary,
            "resizeQuality",
            &["low", "medium", "high", "pixelated"],
        )? {
            1 => Quality::Medium,
            2 => Quality::High,
            3 => Quality::Pixelated,
            _ => Quality::Low,
        };
        let width = resize_member(vm, &dictionary, "resizeWidth")?;
        Ok(Self {
            crop,
            resize: [width, height],
            orientation,
            premultiplied,
            quality,
        })
    }

    pub(super) fn zero_crop(self) -> bool {
        self.crop
            .is_some_and(|[_, _, width, height]| width == 0 || height == 0)
    }

    pub(super) fn zero_resize(self) -> bool {
        self.resize.contains(&Some(0))
    }

    pub(super) fn uses_metadata_orientation(self) -> bool {
        self.orientation == Orientation::FromImage
    }

    pub(super) fn raster(self, source: &Source) -> Option<Raster> {
        let [x, y, width, height] =
            self.crop
                .unwrap_or([0, 0, i64::from(source.size[0]), i64::from(source.size[1])]);
        let origin = [x + width.min(0), y + height.min(0)];
        let crop = [width.abs(), height.abs()];
        if crop.contains(&0) {
            return None;
        }
        let [width, height] = crop.map(|extent| extent as u64);
        let scaled = |explicit: Option<u32>, other: Option<u32>, span: u64, other_span: u64| {
            explicit.map_or_else(
                || other.map_or(span, |other| (span * u64::from(other)).div_ceil(other_span)),
                u64::from,
            )
        };
        let [w, h] = self.resize;
        let size = [
            u32::try_from(scaled(w, h, width, height)).ok()?,
            u32::try_from(scaled(h, w, height, width)).ok()?,
        ];
        if !Bitmap::supports_size(size) || size.contains(&0) {
            return None;
        }
        Some(Raster {
            origin,
            crop,
            size,
            flip_y: self.orientation == Orientation::FlipY,
            premultiplied: self.premultiplied.unwrap_or(source.premultiplied),
            quality: self.quality,
        })
    }
}

fn enum_member(
    vm: &mut Vm,
    options: &Value,
    key: &str,
    choices: &[&str],
) -> Result<usize, JsError> {
    let value = vm.get_property(options.clone(), key)?;
    if value.is_undefined() {
        return Ok(0);
    }
    let value = text(vm, value)?;
    choices
        .iter()
        .position(|choice| *choice == value)
        .ok_or_else(|| JsError::type_error(format!("Invalid ImageBitmapOptions.{key}")))
}

fn resize_member(vm: &mut Vm, options: &Value, key: &str) -> Result<Option<u32>, JsError> {
    let value = vm.get_property(options.clone(), key)?;
    if value.is_undefined() {
        return Ok(None);
    }
    let number = vm.to_number(&value)?.trunc();
    if !(0.0..=f64::from(u32::MAX)).contains(&number) {
        return Err(JsError::type_error(format!(
            "ImageBitmapOptions.{key} is outside its range"
        )));
    }
    Ok(Some(number as u32))
}

pub(super) struct Raster {
    origin: [i64; 2],
    crop: [i64; 2],
    size: [u32; 2],
    flip_y: bool,
    premultiplied: bool,
    quality: Quality,
}

impl Raster {
    pub(super) fn reuses_storage(&self, source: &Source) -> bool {
        self.origin == [0, 0]
            && self.crop == source.size.map(i64::from)
            && self.size == source.size
            && !self.flip_y
            && self.premultiplied == source.premultiplied
    }

    pub(super) fn work(&self, source: &Source) -> usize {
        if self.reuses_storage(source) {
            return 0;
        }
        let pixels = self.size[0] as usize * self.size[1] as usize;
        let taps = if matches!(self.quality, Quality::High) {
            16
        } else {
            4
        };
        (pixels * taps).div_ceil(64)
    }

    pub(super) fn apply(&self, source: Source) -> Result<Source, JsError> {
        if self.reuses_storage(&source) {
            return Ok(source);
        }
        let [iw, ih] = source.size.map(i64::from);
        let [ox, oy] = self.origin;
        let [cw, ch] = self.crop;
        let mut output = Source {
            size: self.size,
            origin_clean: source.origin_clean,
            bitmap: None,
            alpha: true,
            premultiplied: self.premultiplied,
        };
        if ox >= iw || oy >= ih || ox + cw <= 0 || oy + ch <= 0 {
            return Ok(output);
        }
        if source.bitmap.is_none() {
            if source.alpha {
                return Ok(output);
            }
            if ox >= 0 && oy >= 0 && ox + cw <= iw && oy + ch <= ih {
                output.alpha = false;
                return Ok(output);
            }
        }

        let [width, height] = self.size;
        let mut bytes = rgba_storage(width as usize * height as usize * 4)?;
        let sampled = |x: i64, y: i64, premultiplied| {
            let x = ox + x.clamp(0, cw - 1);
            let y = oy + y.clamp(0, ch - 1);
            if (0..iw).contains(&x) && (0..ih).contains(&y) {
                source.pixel(x as u32, y as u32, premultiplied)
            } else {
                [0; 4]
            }
        };
        let exact = self.size.map(i64::from) == self.crop;
        let repeat = if matches!(self.quality, Quality::Pixelated) {
            [
                ((i64::from(width) + cw / 2) / cw).max(1),
                ((i64::from(height) + ch / 2) / ch).max(1),
            ]
        } else {
            [1, 1]
        };
        let sample =
            |x: i64, y: i64| sampled(x.div_euclid(repeat[0]), y.div_euclid(repeat[1]), false);
        let x_scale = (cw * repeat[0]) as f64 / f64::from(width);
        let y_scale = (ch * repeat[1]) as f64 / f64::from(height);
        for row in 0..height {
            let y = if self.flip_y { height - 1 - row } else { row };
            let sample_y = (f64::from(y) + 0.5) * y_scale - 0.5;
            for x in 0..width {
                let pixel = if exact {
                    sampled(i64::from(x), i64::from(y), self.premultiplied)
                } else {
                    let point = [(f64::from(x) + 0.5) * x_scale - 0.5, sample_y];
                    let rgba = if matches!(self.quality, Quality::High) {
                        bicubic(point, sample)
                    } else {
                        bilinear(point, sample)
                    };
                    alpha_format(rgba, false, self.premultiplied)
                };
                bytes.extend_from_slice(&pixel);
            }
        }
        output.bitmap = Some(Arc::new(
            RgbaImage::from_raw(width, height, bytes).expect("Raster output dimensions"),
        ));
        Ok(output)
    }
}

fn bicubic([x, y]: [f64; 2], sample: impl Fn(i64, i64) -> [u8; 4]) -> [u8; 4] {
    let weight = |distance: f64| {
        let t = distance.abs();
        if t <= 1.0 {
            1.0 + t * t * (1.5 * t - 2.5)
        } else if t < 2.0 {
            2.0 + t * (-4.0 + t * (2.5 - 0.5 * t))
        } else {
            0.0
        }
    };
    let [left, top] = [x.floor() as i64, y.floor() as i64];
    let xs = [-1, 0, 1, 2].map(|dx| weight(x - (left + dx) as f64));
    let mut rgb = [0.0; 3];
    let mut alpha = 0.0;
    for row in top - 1..=top + 2 {
        let yw = weight(y - row as f64);
        for (column, xw) in (left - 1..=left + 2).zip(xs) {
            let pixel = sample(column, row);
            let contribution = xw * yw * f64::from(pixel[3]);
            alpha += contribution;
            for (total, channel) in rgb.iter_mut().zip(pixel) {
                *total += contribution * f64::from(channel);
            }
        }
    }
    if alpha <= 0.0 {
        return [0; 4];
    }
    let [r, g, b] = rgb.map(|channel| (channel / alpha).round() as u8);
    [r, g, b, alpha.round() as u8]
}
