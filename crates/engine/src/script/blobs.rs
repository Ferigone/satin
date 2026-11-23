//! Immutable File API data. Slices and renamed files share their backing;
//! constructors snapshot BufferSources and reads vend independent buffers.
use super::{Shared, State, arg, fatal, getter, schedule_timer, streams, webidl};
use rust_browser_javascript::{ErrorKind, JsError, RootId, Value, Vm};
use std::{
    cell::{Cell, OnceCell, RefCell},
    ops::Range,
    rc::{Rc, Weak},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) const BLOB_BYTES: usize = 16 * 1024 * 1024;
pub(super) const LIVE_BYTES: usize = 32 * 1024 * 1024;
const PARTS: usize = 4096;
const TYPE_BYTES: usize = 64 * 1024;
pub(super) const NAME_BYTES: usize = 64 * 1024;

struct Reserved {
    resident: Rc<Cell<usize>>,
    cost: usize,
}

impl Drop for Reserved {
    fn drop(&mut self) {
        self.resident.set(self.resident.get() - self.cost);
    }
}

struct Storage {
    bytes: Arc<Vec<u8>>,
    _reservation: Reserved,
}

#[derive(Clone)]
struct Bytes {
    storage: Rc<Storage>,
    range: Range<usize>,
}

#[derive(Clone)]
pub(super) struct FileInfo {
    pub(super) name: String,
    pub(super) last_modified: i64,
}

pub(super) struct Blob {
    bytes: Bytes,
    pub(super) mime: Rc<str>,
    pub(super) file: Option<FileInfo>,
    _reservation: Reserved,
}

impl Blob {
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes.storage.bytes[self.bytes.range.clone()]
    }

    pub(super) fn shared_bytes(&self) -> (Arc<Vec<u8>>, Range<usize>) {
        (self.bytes.storage.bytes.clone(), self.bytes.range.clone())
    }
}

#[derive(Clone)]
pub(super) struct BlobFactory {
    blob_prototype: RootId,
    file_prototype: RootId,
    resident: Rc<Cell<usize>>,
    scheduler: Rc<OnceCell<Weak<RefCell<State>>>>,
}

impl BlobFactory {
    pub(super) fn set_scheduler(&self, state: &Shared) {
        let _ = self.scheduler.set(Rc::downgrade(state));
    }

    pub(super) fn clone_blob(&self, vm: &mut Vm, source: &Value) -> Result<Option<Value>, JsError> {
        let Some(blob) = vm.host_data::<Blob>(source) else {
            return Ok(None);
        };
        vm.charge_work(blob.file.as_ref().map_or(0, |file| file.name.len()))?;
        // Blob and File backing bytes are immutable, including sliced ranges.
        self.new_blob(
            vm,
            blob.bytes.clone(),
            blob.mime.clone(),
            blob.file.clone(),
            None,
        )
        .map(Some)
    }

    fn reserve(&self, cost: usize) -> Result<Reserved, JsError> {
        let total = self.resident.get().saturating_add(cost);
        if total > LIVE_BYTES {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Live Blob and File data exceeds 32 MiB",
            ));
        }
        self.resident.set(total);
        Ok(Reserved {
            resident: self.resident.clone(),
            cost,
        })
    }

    fn backing(&self, bytes: Vec<u8>) -> Result<Bytes, JsError> {
        check_size(bytes.len())?;
        let reservation = self.reserve(bytes.capacity() + 128)?;
        let end = bytes.len();
        Ok(Bytes {
            storage: Rc::new(Storage {
                bytes: Arc::new(bytes),
                _reservation: reservation,
            }),
            range: 0..end,
        })
    }

    fn new_blob(
        &self,
        vm: &mut Vm,
        bytes: Bytes,
        mime: Rc<str>,
        file: Option<FileInfo>,
        target: Option<Value>,
    ) -> Result<Value, JsError> {
        let reservation =
            self.reserve(192 + mime.len() + file.as_ref().map_or(0, |file| file.name.capacity()))?;
        let intrinsic = if file.is_some() {
            self.file_prototype
        } else {
            self.blob_prototype
        };
        let prototype = vm.root_value(intrinsic).expect("rooted Blob prototype");
        let object = if let Some(target) = target {
            webidl::instance(vm, target, &prototype)?
        } else {
            vm.new_object_with_prototype(prototype)?
        };
        vm.set_host_data(
            object.clone(),
            Rc::new(Blob {
                bytes,
                mime,
                file,
                _reservation: reservation,
            }),
        )?;
        Ok(object)
    }

    #[allow(
        clippy::wrong_self_convention,
        reason = "the factory uses realm intrinsics and budget"
    )]
    pub(super) fn from_bytes(
        &self,
        vm: &mut Vm,
        bytes: Vec<u8>,
        mime: &str,
    ) -> Result<Value, JsError> {
        let mime = mime_type(mime)?;
        self.new_blob(vm, self.backing(bytes)?, mime, None, None)
    }

    pub(super) fn file_from_bytes(
        &self,
        vm: &mut Vm,
        bytes: Vec<u8>,
        mime: &str,
        name: String,
    ) -> Result<Value, JsError> {
        self.file_from_host(vm, bytes, mime, name, modified_now())
    }

    pub(super) fn file_from_host(
        &self,
        vm: &mut Vm,
        bytes: Vec<u8>,
        mime: &str,
        name: String,
        last_modified: i64,
    ) -> Result<Value, JsError> {
        if name.len() > NAME_BYTES {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "File name exceeds 64 KiB",
            ));
        }
        self.new_blob(
            vm,
            self.backing(bytes)?,
            mime_type(mime)?,
            Some(FileInfo {
                name,
                last_modified,
            }),
            None,
        )
    }

    pub(super) fn as_file(
        &self,
        vm: &mut Vm,
        value: Value,
        blob: &Blob,
        name: Option<String>,
    ) -> Result<Value, JsError> {
        if name.is_none() && blob.file.is_some() {
            return Ok(value);
        }
        self.new_blob(
            vm,
            blob.bytes.clone(),
            blob.mime.clone(),
            Some(FileInfo {
                name: name.unwrap_or_else(|| "blob".into()),
                last_modified: blob
                    .file
                    .as_ref()
                    .map_or_else(modified_now, |file| file.last_modified),
            }),
            None,
        )
    }

    fn construct(
        &self,
        vm: &mut Vm,
        args: &[Value],
        target: Option<Value>,
        file: bool,
    ) -> Result<Value, JsError> {
        let name = if file { "File" } else { "Blob" };
        let target = target.ok_or_else(|| JsError::type_error(format!("{name} requires new")))?;
        if file {
            webidl::required(args, 2, name)?;
        }
        let input = arg(args, 0);
        let mut parts = if !file && input.is_undefined() {
            Vec::new()
        } else {
            if input.as_object().is_none() {
                return Err(JsError::type_error("Blob parts must be an iterable object"));
            }
            let iterator = webidl::iterator_method(vm, &input)?;
            let mut text_bytes = 0usize;
            webidl::sequence(vm, input, iterator, PARTS, |vm, value| {
                if let Some(blob) = get(vm, &value) {
                    Ok(Part::Blob(blob))
                } else if vm.is_buffer_source(&value) {
                    // Web IDL rejects resizable backing during conversion.
                    // Read the bytes only after the options getters have run.
                    vm.fixed_buffer_source_bytes(&value)?;
                    Ok(Part::Buffer(value))
                } else {
                    let text = webidl::usv_string(vm, &value, BLOB_BYTES - text_bytes)?;
                    text_bytes += text.len();
                    Ok(Part::Text(text))
                }
            })?
        };
        let filename = if file {
            Some(webidl::usv_string(vm, &args[1], NAME_BYTES)?)
        } else {
            None
        };
        let options = arg(args, if file { 2 } else { 1 });
        if !options.is_nullish() && options.as_object().is_none() {
            return Err(JsError::type_error("Blob options must be a dictionary"));
        }
        // Inherited dictionary fields precede the fields of the derived bag.
        let endings = field(vm, &options, "endings")?;
        let native = if endings.is_undefined() {
            false
        } else {
            match webidl::usv_string(vm, &endings, TYPE_BYTES)?.as_str() {
                "transparent" => false,
                "native" => true,
                _ => return Err(JsError::type_error("Invalid Blob ending mode")),
            }
        };
        let mime = field(vm, &options, "type")?;
        let mime = type_value(vm, &mime)?;
        let file = if let Some(name) = filename {
            let last_modified = field(vm, &options, "lastModified")?;
            Some(FileInfo {
                name,
                last_modified: if last_modified.is_undefined() {
                    modified_now()
                } else {
                    long_long(vm, &last_modified)?
                },
            })
        } else {
            None
        };
        let bytes = if let [Part::Blob(blob)] = parts.as_slice() {
            blob.bytes.clone()
        } else {
            self.collect_parts(vm, &mut parts, native)?
        };
        self.new_blob(vm, bytes, mime, file, Some(target))
    }

    fn collect_parts(
        &self,
        vm: &mut Vm,
        parts: &mut [Part],
        native: bool,
    ) -> Result<Bytes, JsError> {
        let mut total = 0usize;
        for part in parts.iter_mut() {
            let len = match part {
                Part::Text(text) => {
                    if native {
                        vm.charge_work(text.len().div_ceil(8))?;
                        *text = line_endings(text, if cfg!(windows) { "\r\n" } else { "\n" });
                    }
                    text.len()
                }
                Part::Blob(blob) => blob.bytes().len(),
                Part::Buffer(buffer) => vm.fixed_buffer_source_bytes(buffer)?.len(),
            };
            total = total.saturating_add(len);
            check_size(total)?;
        }
        vm.charge_work(total.div_ceil(32) + parts.len())?;
        let reservation = self.reserve(total + 128)?;
        let mut bytes = Vec::with_capacity(total);
        for part in parts {
            match part {
                Part::Text(text) => bytes.extend_from_slice(text.as_bytes()),
                Part::Blob(blob) => bytes.extend_from_slice(blob.bytes()),
                Part::Buffer(buffer) => {
                    bytes.extend_from_slice(vm.fixed_buffer_source_bytes(buffer)?);
                }
            }
        }
        debug_assert_eq!(bytes.len(), total);
        Ok(Bytes {
            storage: Rc::new(Storage {
                bytes: Arc::new(bytes),
                _reservation: reservation,
            }),
            range: 0..total,
        })
    }
}

enum Part {
    Blob(Rc<Blob>),
    Buffer(Value),
    Text(String),
}

pub(super) fn get(vm: &Vm, value: &Value) -> Option<Rc<Blob>> {
    vm.host_data::<Blob>(value)
}

fn receiver(vm: &Vm, value: &Value) -> Result<Rc<Blob>, JsError> {
    get(vm, value).ok_or_else(|| JsError::type_error("Illegal Blob receiver"))
}

fn check_size(bytes: usize) -> Result<(), JsError> {
    if bytes > BLOB_BYTES {
        Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Blob data exceeds 16 MiB",
        ))
    } else {
        Ok(())
    }
}

fn field(vm: &mut Vm, options: &Value, name: &str) -> Result<Value, JsError> {
    if options.is_nullish() {
        Ok(Value::Undefined)
    } else {
        vm.get_property(options.clone(), name)
    }
}

fn type_value(vm: &mut Vm, value: &Value) -> Result<Rc<str>, JsError> {
    if value.is_undefined() {
        return Ok(Rc::from(""));
    }
    mime_type(&webidl::usv_string(vm, value, TYPE_BYTES)?)
}

fn mime_type(value: &str) -> Result<Rc<str>, JsError> {
    if value.len() > TYPE_BYTES {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Blob type exceeds 64 KiB",
        ));
    }
    if value.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        Ok(Rc::from(value.to_ascii_lowercase()))
    } else {
        Ok(Rc::from(""))
    }
}

fn modified_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn long_long(vm: &mut Vm, value: &Value) -> Result<i64, JsError> {
    const HALF: f64 = 9_223_372_036_854_775_808.0;
    let integer = vm.to_number(value)?.trunc() % (HALF * 2.0);
    Ok(if integer >= HALF {
        (integer - HALF * 2.0) as i64
    } else if integer < -HALF {
        (integer + HALF * 2.0) as i64
    } else {
        integer as i64
    })
}

fn relative(offset: i64, size: usize) -> usize {
    let size = size as i64;
    if offset < 0 {
        (size + offset).max(0) as usize
    } else {
        offset.min(size) as usize
    }
}

pub(super) fn line_endings(value: &str, ending: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find(['\r', '\n']) {
        out.push_str(&rest[..index]);
        out.push_str(ending);
        let tail = &rest[index..];
        rest = tail.strip_prefix("\r\n").unwrap_or(&tail[1..]);
    }
    out.push_str(rest);
    out
}

#[derive(Clone, Copy)]
enum Read {
    Text,
    ArrayBuffer,
    Uint8Array,
}

fn read_value(vm: &mut Vm, blob: &Blob, kind: Read) -> Result<Value, JsError> {
    let bytes = blob.bytes();
    vm.charge_work(bytes.len().div_ceil(32))?;
    match kind {
        Read::Text => {
            let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
            vm.ensure_string_allocation(bytes.len())?;
            Ok(String::from_utf8_lossy(bytes).as_ref().into())
        }
        Read::ArrayBuffer => vm.new_array_buffer(bytes.to_vec()),
        Read::Uint8Array => vm.new_uint8_array(bytes.to_vec()),
    }
}

fn settle(
    vm: &mut Vm,
    resolve: Value,
    reject: Value,
    result: Result<Value, JsError>,
    queue: bool,
) -> Result<(), JsError> {
    match result {
        Ok(value) if queue => vm.enqueue_call(resolve, Value::Undefined, &[value])?,
        Ok(value) => {
            vm.call(resolve, Value::Undefined, &[value])?;
        }
        Err(error) if fatal(&error) => return Err(error),
        Err(error) => {
            let reason = vm.error_value(&error)?;
            vm.call(reject, Value::Undefined, &[reason])?;
        }
    }
    Ok(())
}

fn read(
    vm: &mut Vm,
    this: &Value,
    kind: Read,
    scheduler: Option<Shared>,
) -> Result<Value, JsError> {
    let promise = vm.new_promise_capability()?;
    match (receiver(vm, this), scheduler) {
        (Ok(blob), Some(scheduler)) => {
            // Immutable data remains alive until its file-reading task runs.
            // The existing host queue runs due tasks after the microtask
            // checkpoint and the timer ID cannot be cleared by page code.
            let task = vm.new_native_function("read Blob", move |vm, _, args| {
                let result = read_value(vm, &blob, kind);
                settle(vm, arg(args, 0), arg(args, 1), result, false)?;
                Ok(Value::Undefined)
            });
            if let Err(error) = schedule_timer(
                vm,
                &scheduler,
                task,
                vec![promise.resolve.clone(), promise.reject.clone()],
                Duration::ZERO,
            ) {
                settle(vm, promise.resolve, promise.reject, Err(error), false)?;
            }
        }
        // A standalone VM can install the byte bindings without a browser
        // event loop. It still receives an asynchronous fulfilment.
        (Ok(blob), None) => {
            let result = read_value(vm, &blob, kind);
            settle(vm, promise.resolve, promise.reject, result, true)?;
        }
        (Err(error), _) => settle(vm, promise.resolve, promise.reject, Err(error), false)?,
    }
    Ok(promise.promise)
}

pub(super) fn install(vm: &mut Vm, streams: streams::StreamHost) -> Result<BlobFactory, JsError> {
    let blob_prototype = vm.new_object();
    let file_prototype = vm.new_object_with_prototype(blob_prototype.clone())?;
    let factory = BlobFactory {
        blob_prototype: vm.root(blob_prototype.clone()),
        file_prototype: vm.root(file_prototype.clone()),
        resident: Rc::new(Cell::new(0)),
        scheduler: Rc::new(OnceCell::new()),
    };
    let host = factory.clone();
    let blob = webidl::interface(
        vm,
        blob_prototype.clone(),
        "Blob",
        0,
        move |vm, _, args, new| host.construct(vm, args, new, false),
    )?;
    let host = factory.clone();
    let file = webidl::interface(
        vm,
        file_prototype.clone(),
        "File",
        2,
        move |vm, _, args, new| host.construct(vm, args, new, true),
    )?;
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    vm.call(set_prototype, Value::Undefined, &[file, blob])?;

    getter(vm, blob_prototype.clone(), "size", |vm, this, _| {
        Ok(Value::Number(receiver(vm, &this)?.bytes().len() as f64))
    })?;
    getter(vm, blob_prototype.clone(), "type", |vm, this, _| {
        let blob = receiver(vm, &this)?;
        vm.charge_work(blob.mime.len().div_ceil(8))?;
        vm.ensure_string_allocation(blob.mime.len())?;
        Ok(blob.mime.as_ref().into())
    })?;
    for name in ["name", "lastModified"] {
        getter(vm, file_prototype.clone(), name, move |vm, this, _| {
            let blob = receiver(vm, &this)?;
            let file = blob
                .file
                .as_ref()
                .ok_or_else(|| JsError::type_error("Illegal File receiver"))?;
            Ok(if name == "name" {
                vm.charge_work(file.name.len().div_ceil(8))?;
                vm.ensure_string_allocation(file.name.len())?;
                file.name.as_str().into()
            } else {
                Value::Number(file.last_modified as f64)
            })
        })?;
    }
    let host = factory.clone();
    webidl::operation(
        vm,
        blob_prototype.clone(),
        "slice",
        0,
        move |vm, this, args| {
            let blob = receiver(vm, &this)?;
            let size = blob.bytes().len();
            let start = relative(long_long(vm, &arg(args, 0))?, size);
            let end = arg(args, 1);
            let end = if end.is_undefined() {
                size
            } else {
                relative(long_long(vm, &end)?, size)
            };
            let mime = type_value(vm, &arg(args, 2))?;
            let offset = blob.bytes.range.start;
            host.new_blob(
                vm,
                Bytes {
                    storage: blob.bytes.storage.clone(),
                    range: offset + start..offset + end.max(start),
                },
                mime,
                None,
                None,
            )
        },
    )?;
    webidl::operation(
        vm,
        blob_prototype.clone(),
        "stream",
        0,
        move |vm, this, _| {
            let blob = receiver(vm, &this)?;
            streams.memory_stream(
                vm,
                blob.bytes.storage.bytes.clone(),
                blob.bytes.range.clone(),
            )
        },
    )?;
    for (name, kind) in [
        ("text", Read::Text),
        ("arrayBuffer", Read::ArrayBuffer),
        ("bytes", Read::Uint8Array),
    ] {
        let scheduler = factory.scheduler.clone();
        webidl::operation(vm, blob_prototype.clone(), name, 0, move |vm, this, _| {
            read(vm, &this, kind, scheduler.get().and_then(Weak::upgrade))
        })?;
    }
    Ok(factory)
}
