//! Bitmap export with bounded encoding and deferred Blob callbacks.
use super::super::Bitmap;
use super::{Exceptions, Source, canvas_snapshot, empty_pixel, flatten};
use crate::{
    dom::NodeId,
    script::{Shared, arg, blobs::BlobFactory, schedule_timer, webidl},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{
    ExtendedColorType, ImageEncoder, RgbaImage,
    codecs::{
        jpeg::{JpegEncoder, PixelDensity},
        png::{CompressionType, FilterType, PngEncoder},
        webp::WebPEncoder,
    },
};
use rust_browser_javascript::{JsError, Value, Vm};
use std::{
    cell::{Cell, RefCell},
    io::{self, Write},
    rc::Rc,
    time::Duration,
};

const MAX_EXPORT_BYTES: usize = 16 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
// PNG sRGB with perceptual intent and pHYs declaring 3780 px/metre (96 dpi).
const PNG_METADATA: [u8; 34] = *b"\x00\x00\x00\x01sRGB\x00\xae\xce\x1c\xe9\x00\x00\x00\x09pHYs\x00\x00\x0e\xc4\x00\x00\x0e\xc4\x01\x95\x2b\x0e\x1b";

#[derive(Clone, Copy)]
enum Encoding {
    Png,
    Jpeg(u8),
    WebP,
}

impl Encoding {
    fn from_args(vm: &mut Vm, media_type: Value, quality: Value) -> Result<Self, JsError> {
        let jpeg_quality = match quality {
            Value::Number(quality) if (0.0..=1.0).contains(&quality) => {
                (quality * 100.).round() as u8
            }
            _ => 92,
        };
        if media_type.is_undefined() {
            return Ok(Self::Png);
        }
        let media_type = vm.to_string(&media_type)?;
        if media_type.len() > "image/jpeg".len() {
            return Ok(Self::Png);
        }
        Ok(
            match media_type.to_string_lossy().to_ascii_lowercase().as_str() {
                "image/jpeg" => Self::Jpeg(jpeg_quality),
                "image/webp" => Self::WebP,
                _ => Self::Png,
            },
        )
    }

    fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg(_) => "image/jpeg",
            Self::WebP => "image/webp",
        }
    }
}

#[derive(Default)]
struct Output(Vec<u8>);

impl Write for Output {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() > MAX_EXPORT_BYTES - self.0.len() {
            return Err(io::Error::other("Canvas encoding exceeds its byte budget"));
        }
        self.0
            .try_reserve(data.len())
            .map_err(|_| io::Error::other("Canvas encoding allocation failed"))?;
        self.0.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn bitmap_bytes(source: &Source) -> Option<usize> {
    if source.size.contains(&0) || !Bitmap::supports_size(source.size) {
        return None;
    }
    let [width, height] = source.size;
    Some(width as usize * height as usize * 4)
}

fn serialize(vm: &mut Vm, source: &Source, encoding: Encoding) -> Result<Option<Vec<u8>>, JsError> {
    let Some(size) = bitmap_bytes(source) else {
        return Ok(None);
    };
    // Charge before allocating pixels for canvases that have yet to be drawn on.
    vm.charge_work(size.div_ceil(64))?;
    let [width, height] = source.size;
    let blank;
    let image = if let Some(image) = &source.bitmap {
        image
    } else {
        let mut pixels = Vec::new();
        if pixels.try_reserve_exact(size).is_err() {
            return Ok(None);
        }
        pixels.resize(size, 0);
        if !source.alpha {
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel.copy_from_slice(&empty_pixel(false));
            }
        }
        blank = RgbaImage::from_raw(width, height, pixels).expect("checked bitmap size");
        &blank
    };
    if image.dimensions() != (width, height) {
        return Ok(None);
    }

    let mut output = Output::default();
    let result = match encoding {
        Encoding::Png => {
            PngEncoder::new_with_quality(&mut output, CompressionType::Fast, FilterType::Adaptive)
                .write_image(image.as_raw(), width, height, ExtendedColorType::Rgba8)
        }
        Encoding::Jpeg(quality) => {
            let mut rgb = Vec::new();
            if rgb.try_reserve_exact(size / 4 * 3).is_err() {
                return Ok(None);
            }
            for pixel in image.pixels() {
                let mut rgba = pixel.0;
                flatten(&mut rgba);
                rgb.extend_from_slice(&rgba[..3]);
            }
            let mut encoder = JpegEncoder::new_with_quality(&mut output, quality);
            encoder.set_pixel_density(PixelDensity::dpi(96));
            encoder.write_image(&rgb, width, height, ExtendedColorType::Rgb8)
        }
        Encoding::WebP => WebPEncoder::new_lossless(&mut output).write_image(
            image.as_raw(),
            width,
            height,
            ExtendedColorType::Rgba8,
        ),
    };
    if result.is_err() {
        return Ok(None);
    }
    if matches!(encoding, Encoding::Png) {
        const AFTER_IHDR: usize = 33;
        if output.0.len() > MAX_EXPORT_BYTES - PNG_METADATA.len() {
            return Ok(None);
        }
        drop(output.0.splice(AFTER_IHDR..AFTER_IHDR, PNG_METADATA));
    }
    Ok(Some(output.0))
}

pub(super) fn data_url(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    args: &[Value],
    errors: Exceptions,
) -> Result<Value, JsError> {
    let encoding = Encoding::from_args(vm, arg(args, 0), arg(args, 1))?;
    let snapshot = canvas_snapshot(vm, &state.borrow(), id)?;
    if !snapshot.origin_clean {
        return Err(errors.security(vm));
    }
    let Some(bytes) = serialize(vm, &snapshot, encoding)? else {
        return Ok("data:,".into());
    };
    vm.charge_work(bytes.len().div_ceil(64))?;
    let mut url = format!("data:{};base64,", encoding.mime());
    let encoded_len = base64::encoded_len(bytes.len(), true).expect("bounded encoding size");
    if url.len() + encoded_len > vm.limits().max_string_units
        || url.try_reserve_exact(encoded_len).is_err()
    {
        return Ok("data:,".into());
    }
    STANDARD.encode_string(bytes, &mut url);
    Ok(url.into())
}

/// Reserve memory for snapshots while callbacks are queued. New writes to an
/// adopted canvas can otherwise leave any number of old COW bitmaps resident.
#[derive(Clone, Default)]
pub(super) struct Queue(Rc<Cell<usize>>);

struct Reservation {
    source: Source,
    bytes: usize,
    resident: Rc<Cell<usize>>,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.resident.set(self.resident.get() - self.bytes);
    }
}

impl Queue {
    fn reserve(&self, source: Source) -> Option<Reservation> {
        let bytes = bitmap_bytes(&source)?;
        let total = self.0.get() + bytes;
        if total > MAX_SNAPSHOT_BYTES {
            return None;
        }
        self.0.set(total);
        Some(Reservation {
            source,
            bytes,
            resident: self.0.clone(),
        })
    }
}

pub(super) fn blob(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    args: &[Value],
    errors: Exceptions,
    blobs: &BlobFactory,
    queue: &Queue,
) -> Result<Value, JsError> {
    webidl::required(args, 1, "toBlob")?;
    let callback = &args[0];
    if !vm.is_callable(callback) {
        return Err(JsError::type_error("toBlob requires a callable callback"));
    }
    let encoding = Encoding::from_args(vm, arg(args, 1), arg(args, 2))?;
    let snapshot = canvas_snapshot(vm, &state.borrow(), id)?;
    if !snapshot.origin_clean {
        return Err(errors.security(vm));
    }

    let pending = Rc::new(RefCell::new(queue.reserve(snapshot)));
    let task_pending = pending.clone();
    let blobs = blobs.clone();
    let task = vm.new_native_function("serialize canvas to Blob", move |vm, _, args| {
        let result = if let Some(reservation) = task_pending.borrow_mut().take() {
            match serialize(vm, &reservation.source, encoding)? {
                None => Value::Null,
                Some(bytes) => blobs
                    .from_bytes(vm, bytes, encoding.mime())
                    .unwrap_or(Value::Null),
            }
        } else {
            Value::Null
        };
        vm.call(arg(args, 0), Value::Undefined, &[result])?;
        Ok(Value::Undefined)
    });
    if let Err(error) = schedule_timer(vm, state, task, vec![callback.clone()], Duration::ZERO) {
        pending.borrow_mut().take();
        return Err(error);
    }
    Ok(Value::Undefined)
}
