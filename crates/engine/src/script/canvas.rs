//! Canvas element and context identity, bitmap ownership and paint publication.
mod bitmap;
mod drawing;
mod image_bitmap;
mod pixels;
mod text;

use super::{blobs::BlobFactory, layout_api::LayoutHost, *};
use crate::canvas::{self as element_canvas, Frame, Frames};
use crate::loader::ImageAvailability;
use std::{cell::Cell, collections::HashSet};

use bitmap::Bitmap;
use drawing::Drawing;
pub(super) use image_bitmap::{
    ImageBitmapFactory, install as install_image_bitmap, run_decode_tasks as run_image_bitmap_tasks,
};
pub(super) use pixels::ImageDataFactory;
use pixels::{ImageStatus, Source};

const CONTEXT_LIMIT: usize = 4096;

#[derive(Clone, Copy)]
struct Exceptions(RootId);

impl Exceptions {
    fn error(self, vm: &mut Vm, name: &str, message: &str) -> JsError {
        let constructor = vm.root_value(self.0).expect("rooted DOMException");
        match abort::dom_exception_error(vm, constructor, name, message) {
            Ok(error) | Err(error) => error,
        }
    }

    fn security(self, vm: &mut Vm) -> JsError {
        self.error(vm, "SecurityError", "The canvas is not origin-clean")
    }
}

#[derive(Default)]
pub(super) struct CanvasStore {
    entries: HashMap<NodeId, CanvasEntry>,
    contexts: HashMap<ObjectId, NodeId>,
    bitmap_bytes: Rc<Cell<usize>>,
    drawing_quota: Rc<drawing::Quota>,
    pub(super) frames: Frames,
    changed_frames: HashSet<NodeId>,
    revision: u64,
    presented_revision: u64,
    sources: CanvasSources,
    image_decodes: image_bitmap::DecodeQueue,
}

#[derive(Default)]
struct CanvasSources {
    images: HashMap<String, Arc<image::RgbaImage>>,
    svg_sizes: HashMap<String, [u32; 2]>,
    status: Arc<HashMap<String, ImageAvailability>>,
    references: Arc<HashMap<NodeId, (String, String)>>,
}

struct CanvasEntry {
    context: Value,
    bitmap: Bitmap,
    drawing: Drawing,
    origin_clean: bool,
    will_read_frequently: bool,
}

impl CanvasStore {
    fn changed(&mut self, node: NodeId) {
        self.changed_frames.insert(node);
        self.revision = self.revision.wrapping_add(1);
    }

    pub(super) fn update_sources(&mut self, page: &LoadedPage) {
        self.sources.images.clone_from(&page.images);
        self.sources.svg_sizes.clear();
        self.sources.svg_sizes.extend(page.svg_images.iter().map(|(key, svg)| {
            let (width, height) = svg.intrinsic_size();
            (key.clone(), [width as u32, height as u32])
        }));
        self.sources.status = page.image_availability.clone();
        self.sources.references = page.image_references.clone();
    }

    pub(super) fn bitmap_deadline(&self) -> Option<std::time::Instant> {
        self.image_decodes.deadline()
    }

    pub(super) fn publish(&mut self) -> bool {
        if self.revision == self.presented_revision {
            return false;
        }
        let entries = &self.entries;
        self.frames.update(self.changed_frames.drain().map(|node| {
            let bitmap = &entries[&node].bitmap;
            let frame = bitmap.snapshot().map(Frame::Bitmap).or_else(|| {
                (!bitmap.alpha()
                    && bitmap.supported()
                    && bitmap.width() != 0
                    && bitmap.height() != 0)
                    .then_some(Frame::OpaqueBlack)
            });
            (node, frame)
        }));
        self.presented_revision = self.revision;
        true
    }
}

fn bitmap_size(state: &State, node: NodeId) -> [u32; 2] {
    element_canvas::size(&state.document, node)
}

fn image_url(state: &State, node: NodeId) -> Option<std::borrow::Cow<'_, str>> {
    use std::borrow::Cow;

    let document = &state.document;
    let source = document.attr(node, "src")?;
    let src = source.trim();
    if src.is_empty() {
        return None;
    }
    if let Some((observed, url)) = state.canvases.sources.references.get(&node)
        && observed == source
    {
        return Some(Cow::Borrowed(url));
    }
    url::Url::parse(src)
        .or_else(|_| document_base(state).join(src))
        .ok()
        .map(|url| Cow::Owned(url.into()))
}

fn image_key(state: &State, node: NodeId, url: &str) -> String {
    let cors = crate::network::crossorigin_credentials(state.document.attr(node, "crossorigin"));
    crate::loader::image_asset_key(url, cors)
}

pub(super) fn image_request_key(state: &State, node: NodeId) -> Option<String> {
    image_url(state, node).map(|url| image_key(state, node, &url))
}

#[derive(Default)]
pub(super) struct ImageMetadata {
    pub(super) size: [u32; 2],
    pub(super) settled: bool,
    pub(super) url: Option<String>,
}

pub(super) fn image_metadata(state: &State, node: NodeId) -> ImageMetadata {
    if state.image_updates.contains(&node) {
        return ImageMetadata::default();
    }
    if state.invalid_images.contains(&node) {
        return ImageMetadata { settled: true, ..Default::default() };
    }
    let Some(url) = image_url(state, node) else {
        return ImageMetadata { settled: true, ..Default::default() };
    };
    let key = image_key(state, node, &url);
    let sources = &state.canvases.sources;
    let size = sources.images.get(&key)
        .map(|bitmap| [bitmap.width(), bitmap.height()])
        .or_else(|| sources.svg_sizes.get(&key).copied());
    ImageMetadata {
        size: size.unwrap_or_default(),
        settled: size.is_some() || sources.status.contains_key(&key),
        url: Some(url.into_owned()),
    }
}

fn image_source(state: &State, node: NodeId) -> ImageStatus {
    let document = &state.document;
    if !super::image_element::is_image(document, node) || state.invalid_images.contains(&node) {
        return ImageStatus::Broken;
    }
    if document.attr(node, "src").is_none() {
        return ImageStatus::Pending;
    }
    let Some(url) = image_url(state, node) else {
        return ImageStatus::Broken;
    };
    let key = image_key(state, node, &url);
    let sources = &state.canvases.sources;
    let availability = sources.status.get(&key);
    if let Some(image) = sources.images.get(&key) {
        return ImageStatus::Ready(Source {
            size: [image.width(), image.height()],
            bitmap: Some(image.clone()),
            origin_clean: matches!(availability, Some(ImageAvailability::Clean))
                || (availability.is_none() && url.starts_with("data:")),
            alpha: true,
            premultiplied: false,
        });
    }
    match availability {
        Some(ImageAvailability::Failed) => ImageStatus::Broken,
        _ => ImageStatus::Pending,
    }
}

fn canvas_node(state: &Shared, this: &Value) -> Result<NodeId, JsError> {
    let id = element(state, this)?;
    if element_canvas::is_canvas(&state.borrow().document, id) {
        Ok(id)
    } else {
        Err(JsError::type_error("Illegal HTMLCanvasElement receiver"))
    }
}

fn context_node(state: &Shared, this: &Value) -> Result<NodeId, JsError> {
    this.as_object()
        .and_then(|id| state.borrow().canvases.contexts.get(&id).copied())
        .ok_or_else(|| JsError::type_error("Illegal CanvasRenderingContext2D receiver"))
}

fn reset(vm: &mut Vm, state: &mut State, node: NodeId, resize: bool) -> Result<(), JsError> {
    let Some(entry) = state.canvases.entries.get(&node) else {
        return Ok(());
    };
    let size = if resize {
        bitmap_size(state, node)
    } else {
        [entry.bitmap.width(), entry.bitmap.height()]
    };
    let entry = state.canvases.entries.get_mut(&node).unwrap();
    drawing::clear_hosts(vm, entry.context.clone())?;
    entry.bitmap.reset(size);
    entry.drawing.reset();
    if resize {
        entry.origin_clean = true;
    }
    state.canvases.changed(node);
    Ok(())
}

pub(super) fn attribute_changed(
    vm: &mut Vm,
    state: &mut State,
    node: NodeId,
    name: &str,
) -> Result<(), JsError> {
    if matches!(name, "width" | "height")
        && state.document.attribute_namespace(node, name).is_none()
    {
        reset(vm, state, node, true)?;
    }
    Ok(())
}

struct Settings {
    alpha: bool,
    will_read_frequently: bool,
    supported: bool,
}

fn settings(vm: &mut Vm, options: Value) -> Result<Settings, JsError> {
    let mut settings = Settings {
        alpha: true,
        will_read_frequently: false,
        supported: true,
    };
    if matches!(options, Value::Null | Value::Undefined) {
        return Ok(settings);
    }
    if options.as_object().is_none() {
        return Err(JsError::type_error(
            "Canvas context options must be an object",
        ));
    }
    let alpha = vm.get_property(options.clone(), "alpha")?;
    settings.alpha = alpha == Value::Undefined || alpha.is_truthy();
    for (name, initial, values) in [
        ("colorSpace", "srgb", ["srgb", "display-p3"]),
        ("colorType", "unorm8", ["unorm8", "float16"]),
    ] {
        let value = vm.get_property(options.clone(), name)?;
        if value != Value::Undefined {
            let value = super::text(vm, value)?;
            if !values.contains(&value.as_str()) {
                return Err(JsError::type_error(format!("Invalid Canvas {name}")));
            }
            settings.supported &= value == initial;
        }
    }
    // The renderer presents completed frames on its normal redraw schedule.
    vm.get_property(options.clone(), "desynchronized")?;
    settings.will_read_frequently = vm.get_property(options, "willReadFrequently")?.is_truthy();
    Ok(settings)
}

fn get_context(
    vm: &mut Vm,
    state: &Shared,
    node: NodeId,
    context_prototype: &Value,
    options: Value,
) -> Result<Value, JsError> {
    if let Some(entry) = state.borrow().canvases.entries.get(&node) {
        return Ok(entry.context.clone());
    }
    let settings = settings(vm, options)?;
    // Dictionary getters can reenter getContext on the same element.
    let mut state = state.borrow_mut();
    if let Some(entry) = state.canvases.entries.get(&node) {
        return Ok(entry.context.clone());
    }
    if !settings.supported || state.canvases.entries.len() >= CONTEXT_LIMIT {
        return Ok(Value::Null);
    }
    let bitmap = Bitmap::new(
        bitmap_size(&state, node),
        settings.alpha,
        state.canvases.bitmap_bytes.clone(),
    );
    let context = vm.new_object_with_prototype(context_prototype.clone())?;
    vm.root(context.clone());
    state
        .canvases
        .contexts
        .insert(context.as_object().unwrap(), node);
    let drawing = Drawing::new(state.canvases.drawing_quota.clone());
    state.canvases.entries.insert(
        node,
        CanvasEntry {
            context: context.clone(),
            bitmap,
            drawing,
            origin_clean: true,
            will_read_frequently: settings.will_read_frequently,
        },
    );
    if !settings.alpha {
        state.canvases.changed(node);
    }
    Ok(context)
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    canvas_prototype: Value,
    layout: &LayoutHost,
    blobs: &BlobFactory,
) -> Result<ImageDataFactory, JsError> {
    for (name, default) in element_canvas::DIMENSIONS {
        let dom = state.clone();
        let get = vm.new_native_function(name, move |vm, this, _| {
            let node = canvas_node(&dom, &this)?;
            let state = dom.borrow();
            vm.charge_work(state.document.attr(node, name).map_or(0, str::len))?;
            Ok(element_canvas::dimension(&state.document, node, name, default).into())
        });
        let dom = state.clone();
        let set = vm.new_native_function(name, move |vm, this, args| {
            let node = canvas_node(&dom, &this)?;
            let value = vm.to_number(&arg(args, 0))?;
            let value = value.trunc().rem_euclid(4_294_967_296.) as u32;
            let dimension = if value > i32::MAX as u32 {
                default
            } else {
                value
            };
            set_attribute(vm, &dom, node, name, Some(dimension.to_string()))?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(canvas_prototype.clone(), name, Some(get), Some(set))?;
    }
    let context_prototype = vm.new_object();
    for (name, prototype) in [
        ("HTMLCanvasElement", canvas_prototype.clone()),
        ("CanvasRenderingContext2D", context_prototype.clone()),
    ] {
        webidl::interface(vm, prototype, name, 0, move |_, _, _, _| {
            Err(JsError::type_error(format!("{name} is not constructible")))
        })?;
    }
    let dom = state.clone();
    let prototype = context_prototype.clone();
    webidl::operation(
        vm,
        canvas_prototype.clone(),
        "getContext",
        1,
        move |vm, this, args| {
            let node = canvas_node(&dom, &this)?;
            webidl::required(args, 1, "getContext")?;
            if super::text(vm, arg(args, 0))? == "2d" {
                get_context(vm, &dom, node, &prototype, arg(args, 1))
            } else {
                Ok(Value::Null)
            }
        },
    )?;
    let dom = state.clone();
    getter(
        vm,
        context_prototype.clone(),
        "canvas",
        move |vm, this, _| {
            let node = context_node(&dom, &this)?;
            wrap(vm, &dom, Some(node))
        },
    )?;
    let dom = state.clone();
    webidl::operation(
        vm,
        context_prototype.clone(),
        "reset",
        0,
        move |vm, this, _| {
            let node = context_node(&dom, &this)?;
            reset(vm, &mut dom.borrow_mut(), node, false)?;
            Ok(Value::Undefined)
        },
    )?;
    let dom = state.clone();
    webidl::operation(
        vm,
        context_prototype.clone(),
        "isContextLost",
        0,
        move |_, this, _| {
            let node = context_node(&dom, &this)?;
            Ok((!dom.borrow().canvases.entries[&node].bitmap.supported()).into())
        },
    )?;
    let dom = state.clone();
    webidl::operation(
        vm,
        context_prototype.clone(),
        "getContextAttributes",
        0,
        move |vm, this, _| {
            let node = context_node(&dom, &this)?;
            let attributes = {
                let dom = dom.borrow();
                let entry = &dom.canvases.entries[&node];
                [
                    ("alpha", entry.bitmap.alpha().into()),
                    ("colorSpace", "srgb".into()),
                    ("colorType", "unorm8".into()),
                    ("desynchronized", false.into()),
                    ("willReadFrequently", entry.will_read_frequently.into()),
                ]
            };
            let object = vm.new_object();
            for (name, value) in attributes {
                vm.define_data_property(object.clone(), name, value, true, true, true)?;
            }
            Ok(object)
        },
    )?;
    let exception = vm.get_global("DOMException")?;
    let errors = Exceptions(vm.root(exception));
    drawing::install(vm, state, context_prototype.clone(), layout, errors)?;
    text::install(vm, state, context_prototype.clone(), layout)?;
    pixels::install(
        vm,
        state,
        context_prototype,
        canvas_prototype,
        blobs,
        errors,
    )
}
