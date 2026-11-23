//! Canvas bitmap reads, writes, image sampling and serialization entry points.
mod data;
mod encode;
pub(in crate::script) use data::Factory as ImageDataFactory;

use super::{
    super::{Shared, arg, blobs::BlobFactory, node, webidl},
    Exceptions, bitmap_size, canvas_node, context_node, image_source,
};
use crate::dom::{Namespace, NodeId};
use image::RgbaImage;
use rust_browser_javascript::{JsError, Value, Vm};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct Source {
    pub size: [u32; 2],
    pub bitmap: Option<Arc<RgbaImage>>,
    pub origin_clean: bool,
    pub alpha: bool,
    pub premultiplied: bool,
}

pub(super) enum ImageStatus {
    Pending,
    Broken,
    Ready(Source),
}

impl Source {
    fn empty(size: [u32; 2]) -> Self {
        Self {
            size,
            bitmap: None,
            origin_clean: true,
            alpha: true,
            premultiplied: false,
        }
    }

    pub(super) fn pixel(&self, x: u32, y: u32, premultiplied: bool) -> [u8; 4] {
        let raw = self
            .bitmap
            .as_ref()
            .map_or_else(|| empty_pixel(self.alpha), |image| image.get_pixel(x, y).0);
        alpha_format(raw, self.premultiplied, premultiplied)
    }

    pub(super) fn sample(&self, x: f64, y: f64, smooth: bool) -> [u8; 4] {
        let sample = |x: i64, y: i64| {
            self.pixel(
                (x.max(0) as u32).min(self.size[0] - 1),
                (y.max(0) as u32).min(self.size[1] - 1),
                false,
            )
        };
        if !smooth {
            return sample(x.floor() as i64, y.floor() as i64);
        }
        bilinear([x - 0.5, y - 0.5], sample)
    }
}

pub(super) fn alpha_format(mut pixel: [u8; 4], from: bool, to: bool) -> [u8; 4] {
    let alpha = u16::from(pixel[3]);
    if from == to || alpha == 255 {
        return pixel;
    }
    for color in &mut pixel[..3] {
        let value = u16::from(*color);
        *color = if to {
            ((value * alpha + 127) / 255) as u8
        } else {
            (value * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or(0)
                .min(255) as u8
        };
    }
    pixel
}

pub(super) fn bilinear([x, y]: [f64; 2], sample: impl Fn(i64, i64) -> [u8; 4]) -> [u8; 4] {
    let [left, top] = [x.floor() as i64, y.floor() as i64];
    let [dx, dy] = [x - left as f64, y - top as f64];
    if dx == 0. && dy == 0. {
        return sample(left, top);
    }
    // Interpolate premultiplied channels so fully transparent colors do not bleed.
    let mut rgb = [0.; 3];
    let mut alpha = 0.;
    for (x, y, weight) in [
        (left, top, (1. - dx) * (1. - dy)),
        (left + 1, top, dx * (1. - dy)),
        (left, top + 1, (1. - dx) * dy),
        (left + 1, top + 1, dx * dy),
    ] {
        let pixel = sample(x, y);
        let weight = weight * f64::from(pixel[3]);
        alpha += weight;
        for (sum, channel) in rgb.iter_mut().zip(pixel) {
            *sum += weight * f64::from(channel);
        }
    }
    if alpha == 0. {
        return [0; 4];
    }
    let [r, g, b] = rgb.map(|sum| (sum / alpha).round() as u8);
    [r, g, b, alpha.round() as u8]
}

pub(super) fn rgba_storage(length: usize) -> Result<Vec<u8>, JsError> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(|_| {
        JsError::new(
            rust_browser_javascript::ErrorKind::HeapLimit,
            "RGBA allocation failed",
        )
    })?;
    Ok(bytes)
}

pub(super) fn image_data_source(
    vm: &mut Vm,
    value: &Value,
) -> Result<Option<ImageStatus>, JsError> {
    let Some(record) = vm.host_data::<data::Record>(value) else {
        return Ok(None);
    };
    let view = data::view(vm, value);
    let Ok(Some(length)) = vm.buffer_source_len(&view) else {
        return Ok(Some(ImageStatus::Broken));
    };
    let direct = record.settings.direct();
    vm.charge_work(length.div_ceil(if direct { 32 } else { 4 }))?;
    let input = vm.fixed_buffer_source_bytes(&view)?;
    let size = [record.size.width, record.size.height];
    let mut bytes = rgba_storage(size[0] as usize * size[1] as usize * 4)?;
    if direct {
        bytes.extend_from_slice(input);
    } else {
        for pixel in input.chunks_exact(record.settings.format.bytes_per_pixel()) {
            bytes.extend_from_slice(&record.settings.decode_srgb(pixel));
        }
    }
    let bitmap = RgbaImage::from_raw(size[0], size[1], bytes)
        .expect("ImageData dimensions match its RGBA storage");
    Ok(Some(ImageStatus::Ready(Source {
        size,
        bitmap: Some(Arc::new(bitmap)),
        origin_clean: true,
        alpha: true,
        premultiplied: false,
    })))
}

pub(super) fn is_image_data(vm: &Vm, value: &Value) -> bool {
    vm.host_data::<data::Record>(value).is_some()
}

fn empty_pixel(alpha: bool) -> [u8; 4] {
    [0, 0, 0, if alpha { 0 } else { 255 }]
}

pub(super) fn source_node(state: &Shared, value: &Value) -> Result<NodeId, JsError> {
    let id = node(state, value)?;
    let state = state.borrow();
    if state.document.namespace(id) == Namespace::Html
        && matches!(state.document.tag(id), "img" | "canvas")
    {
        return Ok(id);
    }
    Err(JsError::type_error("Expected a CanvasImageSource"))
}

pub(super) fn source(vm: &mut Vm, state: &Shared, id: NodeId) -> Result<ImageStatus, JsError> {
    let state = state.borrow();
    if state.document.tag(id) != "canvas" {
        charge_image_lookup(vm, &state, id)?;
        return Ok(image_source(&state, id));
    }
    let snapshot = canvas_snapshot(vm, &state, id)?;
    if snapshot.size.contains(&0) {
        Ok(ImageStatus::Broken)
    } else {
        Ok(ImageStatus::Ready(snapshot))
    }
}

fn charge_image_lookup(
    vm: &mut Vm,
    state: &super::super::State,
    id: NodeId,
) -> Result<(), JsError> {
    let document = &state.document;
    let Some(src) = document.attr(id, "src") else {
        return Ok(());
    };
    // Resolving a data URL can scan it several times and copy its entire key.
    let cors = document.attr(id, "crossorigin").map_or(0, str::len);
    vm.charge_work(src.len().saturating_add(cors).div_ceil(8))?;
    if let Some((observed, url)) = state.canvases.sources.references.get(&id)
        && observed == src
    {
        return vm.charge_work(url.len().div_ceil(8));
    }
    let cache = state.base_cache.borrow();
    let base_len = if cache.revision == Some(state.revision) {
        cache
            .selected
            .as_ref()
            .and_then(|item| item.url.as_ref())
            .map_or(0, |url| url.as_str().len())
    } else {
        vm.charge_work(document.nodes.len().div_ceil(16))?;
        super::super::base_element(document).map_or(0, |(_, href)| href.len())
    };
    vm.charge_work(
        base_len
            .saturating_add(state.url.as_str().len())
            .div_ceil(8),
    )
}

fn canvas_snapshot(
    vm: &mut Vm,
    state: &super::super::State,
    id: NodeId,
) -> Result<Source, JsError> {
    if let Some(entry) = state.canvases.entries.get(&id) {
        return Ok(Source {
            size: [entry.bitmap.width(), entry.bitmap.height()],
            bitmap: entry.bitmap.snapshot(),
            origin_clean: entry.origin_clean,
            alpha: entry.bitmap.alpha(),
            premultiplied: false,
        });
    }
    let attribute_bytes = ["width", "height"]
        .iter()
        .map(|key| state.document.attr(id, key).map_or(0, str::len))
        .sum::<usize>();
    vm.charge_work(attribute_bytes.div_ceil(8))?;
    Ok(Source::empty(bitmap_size(state, id)))
}

fn numbers<T, const N: usize>(
    vm: &mut Vm,
    args: &[Value],
    convert: impl Fn(&mut Vm, &Value) -> Result<T, JsError>,
) -> Result<[T; N], JsError>
where
    T: Default + Copy,
{
    let mut converted = [T::default(); N];
    for (target, input) in converted.iter_mut().zip(args) {
        *target = convert(vm, input)?;
    }
    Ok(converted)
}

fn interval(offset: i64, extent: i64) -> std::ops::Range<i64> {
    let end = offset + extent;
    offset.min(end)..offset.max(end)
}

fn intersect(a: std::ops::Range<i64>, b: std::ops::Range<i64>) -> std::ops::Range<i64> {
    a.start.max(b.start)..a.end.min(b.end)
}

fn long(vm: &mut Vm, input: &Value) -> Result<i64, JsError> {
    let number = vm.to_number(input)?.trunc();
    if (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&number) {
        Ok(number as i64)
    } else {
        Err(JsError::type_error(
            "Canvas coordinate is outside the long range",
        ))
    }
}

fn get_image_data(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    args: &[Value],
    factory: data::Factory,
) -> Result<Value, JsError> {
    webidl::required(args, 4, "getImageData")?;
    let [x, y, width, height] = numbers(vm, args, long)?;
    let settings = data::Settings::parse(vm, arg(args, 4))?;
    let size = factory.size(
        vm,
        width.unsigned_abs() as u32,
        height.unsigned_abs() as u32,
    )?;
    let state = state.borrow();
    let entry = state
        .canvases
        .entries
        .get(&id)
        .expect("context has a canvas");
    if !entry.origin_clean {
        drop(state);
        return Err(factory.errors.security(vm));
    }
    let alpha = entry.bitmap.alpha();
    let source = entry.bitmap.image();
    let pixels_x = interval(x, width);
    let pixels_y = interval(y, height);
    let cols = intersect(pixels_x.clone(), 0..i64::from(entry.bitmap.width()));
    let rows = intersect(pixels_y.clone(), 0..i64::from(entry.bitmap.height()));
    if cols.is_empty() || rows.is_empty() || (source.is_none() && alpha) {
        return factory.blank_with(vm, size, settings, |_| {});
    }
    let bytes_per_pixel = settings.format.bytes_per_pixel();
    if !settings.direct() {
        size.elements(settings.format)?;
        let columns = (cols.end - cols.start) as usize;
        let count = columns.saturating_mul((rows.end - rows.start) as usize);
        vm.charge_work(count.div_ceil(4))?;
    }
    factory.blank_with(vm, size, settings, |bytes| {
        let stride = size.width as usize * bytes_per_pixel;
        for y in rows {
            let row = (y - pixels_y.start) as usize * stride;
            let col = (cols.start - pixels_x.start) as usize * bytes_per_pixel;
            if settings.direct()
                && let Some(image) = source
            {
                let count = (cols.end - cols.start) as usize * 4;
                let offset = (y as usize * image.width() as usize + cols.start as usize) * 4;
                bytes[row + col..row + col + count]
                    .copy_from_slice(&image.as_raw()[offset..offset + count]);
                continue;
            }
            for (n, x) in cols.clone().enumerate() {
                let rgba = source.map_or_else(
                    || empty_pixel(alpha),
                    |image| image.get_pixel(x as u32, y as u32).0,
                );
                let offset = row + col + n * bytes_per_pixel;
                settings.encode_srgb(rgba, &mut bytes[offset..offset + bytes_per_pixel]);
            }
        }
    })
}

fn put_image_data(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    args: &[Value],
    factory: data::Factory,
) -> Result<(), JsError> {
    match args.len() {
        3 | 7.. => {}
        _ => {
            return Err(JsError::type_error(
                "putImageData requires 3 or 7 arguments",
            ));
        }
    }
    let data = data::receiver(vm, &args[0])?;
    let [dx, dy] = numbers(vm, &args[1..], long)?;
    let [width, height] = [i64::from(data.size.width), i64::from(data.size.height)];
    let [x, y, dirty_width, dirty_height] = if args.len() >= 7 {
        numbers(vm, &args[3..], long)?
    } else {
        [0, 0, width, height]
    };
    let view = data::view(vm, &args[0]);
    if vm.buffer_source_len(&view).is_err() {
        return Err(factory.errors.error(
            vm,
            "InvalidStateError",
            "The ImageData buffer is detached",
        ));
    }

    let mut state = state.borrow_mut();
    let entry = state
        .canvases
        .entries
        .get_mut(&id)
        .expect("context has a canvas");
    let [canvas_width, canvas_height] = [
        i64::from(entry.bitmap.width()),
        i64::from(entry.bitmap.height()),
    ];
    let cols = intersect(
        intersect(interval(x, dirty_width), 0..width),
        -dx..canvas_width - dx,
    );
    let rows = intersect(
        intersect(interval(y, dirty_height), 0..height),
        -dy..canvas_height - dy,
    );
    if cols.is_empty() || rows.is_empty() {
        return Ok(());
    }
    let count = (cols.end - cols.start) as usize;
    let height_to_copy = (rows.end - rows.start) as usize;
    let alpha = entry.bitmap.alpha();
    let bytes_per_pixel = data.settings.format.bytes_per_pixel();
    let direct = data.settings.direct();
    let work_per_pixel = if direct { 1 } else { 8 };
    vm.charge_work(
        (count * height_to_copy)
            .saturating_mul(work_per_pixel)
            .div_ceil(16),
    )?;
    let bytes = vm.fixed_buffer_source_bytes(&view)?;
    let Some(image) = entry.bitmap.image_mut()? else {
        return Ok(());
    };
    let target_stride = image.width() as usize * 4;
    let target = image.as_mut();
    for y in rows {
        let from = (y * width + cols.start) as usize * bytes_per_pixel;
        let to = (dy + y) as usize * target_stride + (dx + cols.start) as usize * 4;
        let input = &bytes[from..from + count * bytes_per_pixel];
        let output = &mut target[to..to + count * 4];
        if direct {
            output.copy_from_slice(input);
            for pixel in output.as_chunks_mut::<4>().0 {
                fix_alpha(pixel, alpha);
            }
        } else {
            for (input, output) in input
                .chunks_exact(bytes_per_pixel)
                .zip(output.as_chunks_mut::<4>().0)
            {
                let mut pixel = data.settings.decode_srgb(input);
                fix_alpha(&mut pixel, alpha);
                output.copy_from_slice(&pixel);
            }
        }
    }
    state.canvases.changed(id);
    Ok(())
}

fn fix_alpha(pixel: &mut [u8], alpha: bool) {
    if !alpha {
        // putImageData replaces RGB verbatim in an opaque bitmap.
        pixel[3] = 255;
    } else if pixel[3] == 0 {
        pixel[..3].fill(0);
    }
}

fn flatten(pixel: &mut [u8; 4]) {
    let alpha = u16::from(pixel[3]);
    for channel in &mut pixel[..3] {
        *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
    }
    pixel[3] = 255;
}

struct ImageAxis {
    src: f64,
    dst: f64,
    span: f64,
    scale: f64,
}

impl ImageAxis {
    fn clipped(src: [f64; 2], dst: [f64; 2], limit: u32) -> Option<Self> {
        let [src, src_extent] = src;
        let [dst, dst_extent] = dst;
        let src = src.min(src + src_extent);
        let dst = dst.min(dst + dst_extent);
        let scale = dst_extent.abs() / src_extent.abs();
        let start = src.max(0.);
        let end = (src + src_extent.abs()).min(f64::from(limit));
        let span = (end - start) * scale;
        let dst = dst + (start - src) * scale;
        (span > 0. && span.is_finite() && dst.is_finite()).then_some(Self {
            src: start,
            dst,
            span,
            scale,
        })
    }

    fn source_at(&self, position: f64) -> f64 {
        self.src + (position - self.dst) / self.scale
    }
}

fn draw_image(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    args: &[Value],
    errors: Exceptions,
) -> Result<(), JsError> {
    let count = args.len().min(9);
    if !matches!(count, 3 | 5 | 9) {
        return Err(JsError::type_error(
            "drawImage requires 3, 5, or 9 arguments",
        ));
    }
    let image = &args[0];
    let source_id = (!super::image_bitmap::is_image_bitmap(vm, image))
        .then(|| source_node(state, image))
        .transpose()?;
    let args: [f64; 8] = numbers(vm, &args[1..count], Vm::to_number)?;
    if args.iter().any(|value| !value.is_finite()) {
        return Ok(());
    }
    let status = if let Some(source_id) = source_id {
        source(vm, state, source_id)?
    } else {
        super::image_bitmap::source(vm, image).unwrap_or(ImageStatus::Broken)
    };
    let source = match status {
        ImageStatus::Pending => return Ok(()),
        ImageStatus::Broken => {
            return Err(errors.error(
                vm,
                "InvalidStateError",
                "The source image has no usable data",
            ));
        }
        ImageStatus::Ready(source) => source,
    };
    let [width, height] = source.size.map(f64::from);
    let [sx, sy, sw, sh, dx, dy, dw, dh] = match count {
        3 => [0., 0., width, height, args[0], args[1], width, height],
        5 => [0., 0., width, height, args[0], args[1], args[2], args[3]],
        _ => args,
    };
    let Some(x) = ImageAxis::clipped([sx, sw], [dx, dw], source.size[0]) else {
        return Ok(());
    };
    let Some(y) = ImageAxis::clipped([sy, sh], [dy, dh], source.size[1]) else {
        return Ok(());
    };
    let mut state = state.borrow_mut();
    let entry = state
        .canvases
        .entries
        .get_mut(&id)
        .expect("context has a canvas");
    let smooth = entry.drawing.image_smoothing_enabled();
    // Retain the taint even if a pixel budget interrupts the drawing.
    entry.origin_clean &= source.origin_clean;
    let changed = entry.drawing.paint_sampled(
        vm,
        &mut entry.bitmap,
        [x.dst, y.dst, x.span, y.span],
        |dx, dy| source.sample(x.source_at(dx), y.source_at(dy), smooth),
    )?;
    if changed {
        state.canvases.changed(id);
    }
    Ok(())
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    ctx_proto: Value,
    canvas_proto: Value,
    blobs: &BlobFactory,
    errors: Exceptions,
) -> Result<ImageDataFactory, JsError> {
    let factory = data::Factory::install(vm, errors)?;
    for (name, arity) in [
        ("createImageData", 1),
        ("getImageData", 4),
        ("putImageData", 3),
        ("drawImage", 3),
    ] {
        let state = state.clone();
        webidl::operation(vm, ctx_proto.clone(), name, arity, move |vm, this, args| {
            let id = context_node(&state, &this)?;
            match name {
                "createImageData" => factory.create(vm, args),
                "getImageData" => get_image_data(vm, &state, id, args, factory),
                "putImageData" => {
                    put_image_data(vm, &state, id, args, factory)?;
                    Ok(Value::Undefined)
                }
                _ => {
                    draw_image(vm, &state, id, args, errors)?;
                    Ok(Value::Undefined)
                }
            }
        })?;
    }
    let state_for_url = state.clone();
    webidl::operation(
        vm,
        canvas_proto.clone(),
        "toDataURL",
        0,
        move |vm, this, args| {
            let id = canvas_node(&state_for_url, &this)?;
            encode::data_url(vm, &state_for_url, id, args, errors)
        },
    )?;
    let state = state.clone();
    let blobs = blobs.clone();
    let queue = encode::Queue::default();
    webidl::operation(vm, canvas_proto, "toBlob", 1, move |vm, this, args| {
        let id = canvas_node(&state, &this)?;
        encode::blob(vm, &state, id, args, errors, &blobs, &queue)
    })?;
    Ok(factory)
}
