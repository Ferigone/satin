//! Snapshot, resize, and move decoded pictures without exposing their storage.
mod decode;
mod format;

use super::{
    Exceptions,
    bitmap::REALM_BYTES,
    pixels::{self, ImageStatus, Source},
};
use crate::{
    dom::NodeId,
    script::{
        Shared, arg, blobs, diagnostic, drain_microtask_queue, fatal, getter, schedule_timer,
        webidl,
    },
};
pub(super) use decode::DecodeQueue;
use format::Options;
use rust_browser_javascript::{
    ErrorKind, JsError, PlatformClone, PlatformTransfer, PromiseCapability, RootId,
    StructuredCloneError, Value, Vm,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

struct Pixels {
    source: Source,
    resident: Rc<Cell<usize>>,
    cost: usize,
}

impl Pixels {
    fn keep(state: &Shared, source: Source) -> Result<Rc<Self>, JsError> {
        let resident = state.borrow().canvases.bitmap_bytes.clone();
        let cost = source
            .bitmap
            .as_ref()
            .map_or(0, |bitmap| bitmap.as_raw().capacity())
            + 128;
        let total = resident.get().saturating_add(cost);
        if total > REALM_BYTES {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Canvas and ImageBitmap pixel budget exceeded",
            ));
        }
        resident.set(total);
        Ok(Rc::new(Self {
            source,
            resident,
            cost,
        }))
    }
}

impl Drop for Pixels {
    fn drop(&mut self) {
        self.resident.set(self.resident.get() - self.cost);
    }
}

struct Record(RefCell<Option<Rc<Pixels>>>);

fn receiver(vm: &Vm, object: &Value) -> Result<Rc<Record>, JsError> {
    vm.host_data(object)
        .ok_or_else(|| JsError::type_error("Illegal ImageBitmap receiver"))
}

fn transferable(record: &Record) -> Result<Rc<Pixels>, StructuredCloneError> {
    let pixels = record
        .0
        .borrow()
        .clone()
        .ok_or(StructuredCloneError::DataClone("ImageBitmap is detached"))?;
    if !pixels.source.origin_clean {
        return Err(StructuredCloneError::DataClone(
            "ImageBitmap is not origin-clean",
        ));
    }
    Ok(pixels)
}

pub(super) fn is_image_bitmap(vm: &Vm, object: &Value) -> bool {
    vm.host_data::<Record>(object).is_some()
}

pub(super) fn source(vm: &Vm, object: &Value) -> Option<ImageStatus> {
    let record = vm.host_data::<Record>(object)?;
    let pixels = record.0.borrow();
    Some(pixels.as_ref().map_or(ImageStatus::Broken, |pixels| {
        ImageStatus::Ready(pixels.source.clone())
    }))
}

#[derive(Clone, Copy)]
pub(in crate::script) struct ImageBitmapFactory {
    prototype: RootId,
    errors: Exceptions,
}

impl ImageBitmapFactory {
    fn wrap(self, vm: &mut Vm, pixels: Option<Rc<Pixels>>) -> Result<Value, JsError> {
        let prototype = vm
            .root_value(self.prototype)
            .expect("rooted ImageBitmap prototype");
        let object = vm.new_object_with_prototype(prototype)?;
        vm.set_host_data(object.clone(), Rc::new(Record(RefCell::new(pixels))))?;
        Ok(object)
    }

    pub(in crate::script) fn clone_bitmap(
        self,
        vm: &mut Vm,
        source: &Value,
    ) -> Result<Option<PlatformClone>, StructuredCloneError> {
        let Some(record) = vm.host_data::<Record>(source) else {
            return Ok(None);
        };
        let object = self.wrap(vm, Some(transferable(&record)?))?;
        Ok(Some(PlatformClone {
            object,
            slots: vec![],
        }))
    }

    pub(in crate::script) fn prepare_transfer(
        self,
        vm: &mut Vm,
        source: &Value,
    ) -> Result<Option<PlatformTransfer>, StructuredCloneError> {
        let Some(record) = vm.host_data::<Record>(source) else {
            return Ok(None);
        };
        let object = self.wrap(vm, None)?;
        let destination = receiver(vm, &object)?;
        let validate = record.clone();
        Ok(Some(PlatformTransfer {
            object,
            validate: Box::new(move |_| transferable(&validate).map(|_| ())),
            commit: Box::new(move |_| {
                destination.0.replace(record.0.take());
            }),
        }))
    }

    fn invalid(self, vm: &mut Vm, message: &str) -> JsError {
        self.errors.error(vm, "InvalidStateError", message)
    }

    fn create(
        self,
        vm: &mut Vm,
        state: &Shared,
        this: Value,
        args: &[Value],
    ) -> Result<Value, JsError> {
        let binding = bind(vm, state, this, args);
        let promise = vm.new_promise_capability()?;
        if let Err(error) =
            binding.and_then(|(input, settings)| self.request(vm, state, input, settings, &promise))
        {
            reject(vm, promise.reject, error)?;
        }
        Ok(promise.promise)
    }

    fn request(
        self,
        vm: &mut Vm,
        state: &Shared,
        input: Input,
        settings: Options,
        promise: &PromiseCapability,
    ) -> Result<(), JsError> {
        if settings.zero_crop() {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "ImageBitmap crop has a zero side",
            ));
        }
        if settings.zero_resize() {
            return Err(self.invalid(vm, "ImageBitmap resize has a zero side"));
        }
        let (status, reuse) = match input {
            Input::Blob(blob) => {
                vm.charge_work(blob.bytes().len().div_ceil(32))?;
                let result = state
                    .borrow_mut()
                    .canvases
                    .image_decodes
                    .submit(vm, promise, blob, settings);
                return result
                    .map_err(|message| self.errors.error(vm, "QuotaExceededError", message));
            }
            Input::Bitmap(record) => {
                let pixels = record
                    .0
                    .borrow()
                    .clone()
                    .ok_or_else(|| self.invalid(vm, "ImageBitmap is detached"))?;
                (ImageStatus::Ready(pixels.source.clone()), Some(pixels))
            }
            Input::ImageData(value) => (
                pixels::image_data_source(vm, &value)?.expect("branded ImageData"),
                None,
            ),
            Input::Element(node) => (pixels::source(vm, state, node)?, None),
        };
        let ImageStatus::Ready(source) = status else {
            return Err(self.invalid(vm, "The source image has no usable data"));
        };
        let raster = settings
            .raster(&source)
            .ok_or_else(|| self.invalid(vm, "ImageBitmap dimensions exceed the bitmap limit"))?;
        let pixels = if let Some(pixels) = reuse.filter(|_| raster.reuses_storage(&source)) {
            pixels
        } else {
            vm.charge_work(raster.work(&source))?;
            Pixels::keep(state, raster.apply(source)?)?
        };
        let bitmap = self.wrap(vm, Some(pixels))?;
        schedule_timer(
            vm,
            state,
            promise.resolve.clone(),
            vec![bitmap],
            Duration::ZERO,
        )?;
        Ok(())
    }
}

enum Input {
    Blob(Rc<blobs::Blob>),
    Bitmap(Rc<Record>),
    ImageData(Value),
    Element(NodeId),
}

fn bind(
    vm: &mut Vm,
    state: &Shared,
    this: Value,
    args: &[Value],
) -> Result<(Input, Options), JsError> {
    if !this.is_nullish() && this != vm.global() {
        return Err(JsError::type_error("Illegal createImageBitmap receiver"));
    }
    webidl::required(args, 1, "createImageBitmap")?;
    let image = arg(args, 0);
    let input = if let Some(blob) = blobs::get(vm, &image) {
        Input::Blob(blob)
    } else if let Some(bitmap) = vm.host_data::<Record>(&image) {
        Input::Bitmap(bitmap)
    } else if pixels::is_image_data(vm, &image) {
        Input::ImageData(image)
    } else {
        Input::Element(
            pixels::source_node(state, &image)
                .map_err(|_| JsError::type_error("Expected an ImageBitmapSource"))?,
        )
    };
    Ok((input, Options::from_call(vm, args)?))
}

fn reject(vm: &mut Vm, callback: Value, error: JsError) -> Result<(), JsError> {
    if fatal(&error) {
        return Err(error);
    }
    let value = match error.thrown {
        Some(value) => value,
        None => vm.error_value(&error)?,
    };
    vm.call(callback, Value::Undefined, &[value])?;
    Ok(())
}

pub(in crate::script) fn install(
    vm: &mut Vm,
    state: &Shared,
    window: Value,
) -> Result<ImageBitmapFactory, JsError> {
    let constructor = vm.get_global("DOMException")?;
    let prototype = vm.new_object();
    let factory = ImageBitmapFactory {
        prototype: vm.root(prototype.clone()),
        errors: Exceptions(vm.root(constructor)),
    };
    webidl::interface(vm, prototype.clone(), "ImageBitmap", 0, |_, _, _, _| {
        Err(JsError::type_error("ImageBitmap is not constructible"))
    })?;
    for (axis, name) in ["width", "height"].into_iter().enumerate() {
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            let bitmap = receiver(vm, &this)?;
            let pixels = bitmap.0.borrow();
            Ok(pixels
                .as_ref()
                .map_or(0, |pixels| pixels.source.size[axis])
                .into())
        })?;
    }
    webidl::operation(vm, prototype, "close", 0, |vm, this, _| {
        receiver(vm, &this)?.0.take();
        Ok(Value::Undefined)
    })?;
    let state = state.clone();
    webidl::operation(vm, window, "createImageBitmap", 1, move |vm, this, args| {
        factory.create(vm, &state, this, args)
    })?;
    Ok(factory)
}

pub(in crate::script) fn run_decode_tasks(
    vm: &mut Vm,
    state: &Shared,
    factory: ImageBitmapFactory,
) {
    let completed = state.borrow_mut().canvases.image_decodes.take_completed();
    for task in completed {
        let resolve = vm
            .root_value(task.resolve)
            .expect("rooted ImageBitmap resolver");
        let reject_callback = vm
            .root_value(task.reject)
            .expect("rooted ImageBitmap rejecter");
        let outcome = vm.with_task(|vm| {
            let result = task
                .result
                .map_err(|message| factory.invalid(vm, message))
                .and_then(|source| Pixels::keep(state, source))
                .and_then(|pixels| factory.wrap(vm, Some(pixels)));
            match result {
                Ok(bitmap) => {
                    vm.call(resolve, Value::Undefined, &[bitmap])?;
                }
                Err(error) => reject(vm, reject_callback, error)?,
            }
            drain_microtask_queue(vm, state)
        });
        vm.unroot(task.resolve);
        vm.unroot(task.reject);
        if let Err(error) = outcome {
            diagnostic(state, error.to_string());
        }
    }
}
