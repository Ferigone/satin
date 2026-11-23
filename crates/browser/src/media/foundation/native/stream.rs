//! Media Foundation's source extension parses and owns the coded frame buffers.
use super::{Command, EventKind, Key, Shared, Signal};
use std::{
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicI32, AtomicU64, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::{E_ABORT, E_FAIL, E_INVALIDARG},
        Media::MediaFoundation::{
            IMFAsyncCallback, IMFAsyncResult, IMFByteStream, IMFMediaEngineClassFactory,
            IMFMediaEngineClassFactoryEx, IMFMediaEngineExtension, IMFMediaEngineExtension_Impl,
            IMFMediaSourceExtension, IMFMediaSourceExtensionNotify,
            IMFMediaSourceExtensionNotify_Impl, IMFSourceBuffer, IMFSourceBufferNotify,
            IMFSourceBufferNotify_Impl, MF_E_UNEXPECTED, MF_MEDIA_ENGINE_CANPLAY,
            MF_MEDIA_ENGINE_CANPLAY_NOT_SUPPORTED, MF_MSE_CALLBACK, MF_MSE_ERROR_DECODE,
            MF_MSE_ERROR_NETWORK, MF_MSE_ERROR_NOERROR, MF_OBJECT_MEDIASOURCE, MF_OBJECT_TYPE,
            MFCreateAsyncResult, MFCreateAttributes,
        },
    },
    core::{BOOL, BSTR, Error, HRESULT, IUnknown, Interface, OutRef, Ref, Result, implement},
};

pub(super) const SOURCE_URL: &str = "satin:media-source";
const MAX_BUFFERS: usize = 4;
const MAX_APPEND_BYTES: usize = 8 * 1024 * 1024;
const MAX_OUTSTANDING_BYTES: usize = 32 * 1024 * 1024;
const MAX_RANGES: u32 = 128;

pub(super) struct Stream {
    native: IMFMediaSourceExtension,
    buffers: HashMap<u64, Buffer>,
    key: Key,
    shared: Arc<Shared>,
    end_time: Option<f64>,
    ending: bool,
    outstanding_bytes: Rc<Cell<usize>>,
}

struct Buffer {
    native: IMFSourceBuffer,
    callbacks: Arc<PendingCallbacks>,
    pending: Option<Pending>,
    deferred: Option<Operation>,
}

#[derive(Default)]
struct PendingCallbacks {
    submitted: AtomicU64,
    active: AtomicU64,
    status: AtomicI32,
}

struct Pending {
    operation: Operation,
    cancelled: bool,
}

struct Operation {
    id: u64,
    data: BufferData,
}

enum BufferData {
    Append {
        segment: Segment,
        timestamp_offset: f64,
        append_window: (f64, f64),
    },
    Remove {
        start: f64,
        end: f64,
    },
}

// Pending and deferred appends retain their memory until completion or cancellation.
struct Segment {
    bytes: Arc<[u8]>,
    outstanding_bytes: Rc<Cell<usize>>,
}

impl Drop for Segment {
    fn drop(&mut self) {
        self.outstanding_bytes
            .set(self.outstanding_bytes.get() - self.bytes.len());
    }
}

impl Stream {
    pub(super) fn new(
        factory: &IMFMediaEngineClassFactory,
        key: Key,
        shared: Arc<Shared>,
        outstanding_bytes: Rc<Cell<usize>>,
    ) -> Result<Self> {
        let notify: IMFMediaSourceExtensionNotify = SourceNotify {
            key,
            shared: shared.clone(),
        }
        .into();
        let native = unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1)?;
            let attributes = attributes.ok_or_else(|| Error::from_hresult(E_FAIL))?;
            attributes.SetUnknown(&MF_MSE_CALLBACK, &notify)?;
            factory
                .cast::<IMFMediaEngineClassFactoryEx>()?
                .CreateMediaSourceExtension(0, &attributes)?
        };
        Ok(Self {
            native,
            buffers: HashMap::new(),
            key,
            shared,
            end_time: None,
            ending: false,
            outstanding_bytes,
        })
    }

    pub(super) fn reopened(&mut self) {
        self.end_time = None;
        self.ending = false;
    }

    pub(super) fn end_time(&self) -> Option<f64> {
        self.end_time
    }

    pub(super) fn ending(&self) -> bool {
        self.ending
    }

    pub(super) fn ended(&mut self) {
        self.ending = false;
    }

    pub(super) fn resolver(&self) -> Result<IMFMediaEngineExtension> {
        Ok(Resolver {
            source: self.native.cast()?,
        }
        .into())
    }

    pub(super) fn command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::AddBuffer { buffer, mime, .. } => {
                if let Err(error) = self.add(buffer, &mime) {
                    self.update(buffer, 0, Err(error.to_string()));
                }
            }
            Command::Append {
                buffer,
                operation,
                bytes,
                timestamp_offset,
                append_window,
                ..
            } => {
                let segment = match self.reserve(bytes) {
                    Ok(segment) => segment,
                    Err(error) => {
                        self.update(buffer, operation, Err(error));
                        return Ok(());
                    }
                };
                self.begin(
                    buffer,
                    Operation {
                        id: operation,
                        data: BufferData::Append {
                            segment,
                            timestamp_offset,
                            append_window,
                        },
                    },
                );
            }
            Command::RemoveRange {
                buffer,
                operation,
                start,
                end,
                ..
            } => {
                self.begin(
                    buffer,
                    Operation {
                        id: operation,
                        data: BufferData::Remove { start, end },
                    },
                );
            }
            Command::RemoveBuffer { buffer, .. } => {
                if let Some(active) = self.buffers.remove(&buffer) {
                    unsafe { self.native.RemoveSourceBuffer(&active.native)? };
                }
            }
            Command::AbortBuffer { buffer, .. } => {
                if let Some(active) = self.buffers.get_mut(&buffer) {
                    unsafe { active.native.Abort()? };
                    active.deferred = None;
                    if let Some(pending) = &mut active.pending {
                        pending.cancelled = true;
                    }
                }
            }
            Command::SetDuration { duration, .. } => unsafe { self.native.SetDuration(duration)? },
            Command::EndStream { error, .. } => {
                let error = match error {
                    None => MF_MSE_ERROR_NOERROR,
                    Some("network") => MF_MSE_ERROR_NETWORK,
                    Some("decode") => MF_MSE_ERROR_DECODE,
                    Some(_) => return Err(Error::from_hresult(E_INVALIDARG)),
                };
                unsafe { self.native.SetEndOfStream(error)? };
                self.ending = error == MF_MSE_ERROR_NOERROR;
                self.end_time = None;
                if error == MF_MSE_ERROR_NOERROR {
                    let duration = unsafe { self.native.GetDuration() };
                    self.end_time = if duration.is_finite() {
                        Some(duration.max(0.))
                    } else {
                        self.buffers
                            .values()
                            .filter_map(|buffer| buffer.ranges().ok())
                            .flatten()
                            .map(|(_, end)| end)
                            .filter(|end| end.is_finite())
                            .max_by(f64::total_cmp)
                    };
                }
            }
            _ => unreachable!("only stream controls are passed to an extension"),
        }
        Ok(())
    }

    fn reserve(&self, bytes: Arc<[u8]>) -> std::result::Result<Segment, String> {
        if bytes.len() > MAX_APPEND_BYTES {
            return Err("The media segment exceeds the native append limit".into());
        }
        let total = self.outstanding_bytes.get() + bytes.len();
        if total > MAX_OUTSTANDING_BYTES {
            return Err("Too much media data is awaiting native append".into());
        }
        self.outstanding_bytes.set(total);
        Ok(Segment {
            bytes,
            outstanding_bytes: self.outstanding_bytes.clone(),
        })
    }

    fn add(&mut self, id: u64, mime: &str) -> Result<()> {
        if self.buffers.len() >= MAX_BUFFERS || self.buffers.contains_key(&id) {
            return Err(Error::new(
                E_INVALIDARG,
                "The native media source buffer limit was reached",
            ));
        }
        let mime = BSTR::from(mime);
        let callbacks = Arc::new(PendingCallbacks::default());
        let notify: IMFSourceBufferNotify = BufferNotify {
            key: self.key,
            buffer: id,
            callbacks: callbacks.clone(),
            shared: self.shared.clone(),
        }
        .into();
        let native = unsafe { self.native.AddSourceBuffer(&mime, &notify)? };
        self.buffers.insert(
            id,
            Buffer {
                native,
                callbacks,
                pending: None,
                deferred: None,
            },
        );
        Ok(())
    }

    fn begin(&mut self, id: u64, operation: Operation) {
        let token = operation.id;
        let result = self.buffers.get_mut(&id).map_or_else(
            || Err("The native source buffer is unavailable".into()),
            |buffer| buffer.begin(operation),
        );
        if let Err(error) = result {
            self.update(id, token, Err(error));
        }
    }

    pub(super) fn complete(&mut self, id: u64, operation: u64, status: i32) {
        let Some(buffer) = self.buffers.get_mut(&id) else {
            return;
        };
        let result = if buffer
            .pending
            .as_ref()
            .is_some_and(|pending| pending.operation.id == operation)
        {
            let pending = buffer.pending.take().unwrap();
            (!pending.cancelled).then(|| {
                HRESULT(status)
                    .ok()
                    .and_then(|()| buffer.ranges())
                    .map_err(|error| error.to_string())
            })
        } else {
            None
        };
        let deferred =
            if buffer.pending.is_none() && !unsafe { buffer.native.GetUpdating().as_bool() } {
                buffer.deferred.take()
            } else {
                None
            };
        if let Some(result) = result {
            self.update(id, operation, result);
        }
        if let Some(next) = deferred {
            self.begin(id, next);
        }
    }

    fn update(
        &self,
        buffer: u64,
        operation: u64,
        result: std::result::Result<Vec<(f64, f64)>, String>,
    ) {
        self.shared.publish(
            self.key,
            EventKind::BufferUpdated {
                buffer,
                operation,
                result,
            },
        );
    }
}

impl Buffer {
    fn begin(&mut self, operation: Operation) -> std::result::Result<(), String> {
        if let Some(pending) = &self.pending {
            if pending.cancelled && self.deferred.is_none() {
                // A subsequent append may already be queued while Windows still posts the
                // abort operation's update-end callback. Keep the two operations distinct.
                self.deferred = Some(operation);
                return Ok(());
            }
            return Err("The native source buffer is updating".into());
        }
        if unsafe { self.native.GetUpdating().as_bool() } && self.deferred.is_none() {
            // Changing MediaSource.duration can start internal updates unrelated to appendBuffer.
            self.deferred = Some(operation);
            return Ok(());
        }
        let result = unsafe {
            match &operation.data {
                BufferData::Append {
                    segment,
                    timestamp_offset,
                    append_window,
                } => self.append(
                    operation.id,
                    &segment.bytes,
                    *timestamp_offset,
                    *append_window,
                ),
                BufferData::Remove { start, end } => {
                    self.callbacks
                        .submitted
                        .store(operation.id, Ordering::Release);
                    self.native.Remove(*start, *end)
                }
            }
        };
        if let Err(error) = result {
            self.callbacks.submitted.store(0, Ordering::Release);
            return Err(error.to_string());
        }
        // Keep the bytes alive until the last callback from the asynchronous append.
        self.pending = Some(Pending {
            operation,
            cancelled: false,
        });
        Ok(())
    }

    unsafe fn append(
        &self,
        operation: u64,
        bytes: &[u8],
        offset: f64,
        (start, end): (f64, f64),
    ) -> Result<()> {
        // Intermediates must also satisfy appendWindowStart < appendWindowEnd.
        unsafe {
            let previous_end = self.native.GetAppendWindowEnd();
            if start >= previous_end {
                self.native.SetAppendWindowEnd(end)?;
            }
            if self.native.GetAppendWindowStart() != start {
                self.native.SetAppendWindowStart(start)?;
            }
            if start < previous_end && previous_end != end {
                self.native.SetAppendWindowEnd(end)?;
            }
            if self.native.GetTimeStampOffset() != offset {
                self.native.SetTimeStampOffset(offset)?;
            }
            self.callbacks.submitted.store(operation, Ordering::Release);
            self.native.Append(bytes)
        }
    }

    fn ranges(&self) -> Result<Vec<(f64, f64)>> {
        unsafe {
            let ranges = self.native.GetBuffered()?;
            (0..ranges.GetLength().min(MAX_RANGES))
                .map(|index| Ok((ranges.GetStart(index)?, ranges.GetEnd(index)?)))
                .collect()
        }
    }
}

#[implement(IMFMediaEngineExtension)]
struct Resolver {
    source: IUnknown,
}

impl IMFMediaEngineExtension_Impl for Resolver_Impl {
    fn CanPlayType(&self, _: BOOL, _: &BSTR) -> Result<MF_MEDIA_ENGINE_CANPLAY> {
        Ok(MF_MEDIA_ENGINE_CANPLAY_NOT_SUPPORTED)
    }

    fn BeginCreateObject(
        &self,
        url: &BSTR,
        _: Ref<IMFByteStream>,
        kind: MF_OBJECT_TYPE,
        cancel: OutRef<IUnknown>,
        callback: Ref<IMFAsyncCallback>,
        state: Ref<IUnknown>,
    ) -> Result<()> {
        if !cancel.is_null() {
            cancel.write(None)?;
        }
        // The engine asks for a byte stream first, then resolves the source object.
        if kind != MF_OBJECT_MEDIASOURCE || url.to_string() != SOURCE_URL {
            return Err(Error::from_hresult(MF_E_UNEXPECTED));
        }
        let callback = callback.ok()?;
        unsafe {
            let result = MFCreateAsyncResult(&self.source, callback, state.as_ref())?;
            callback.Invoke(&result)
        }
    }

    fn CancelObjectCreation(&self, _: Ref<IUnknown>) -> Result<()> {
        Err(Error::from_hresult(MF_E_UNEXPECTED))
    }

    fn EndCreateObject(&self, result: Ref<IMFAsyncResult>) -> Result<IUnknown> {
        unsafe { result.ok()?.GetObject() }
    }
}

#[implement(IMFMediaSourceExtensionNotify)]
struct SourceNotify {
    key: Key,
    shared: Arc<Shared>,
}

impl IMFMediaSourceExtensionNotify_Impl for SourceNotify_Impl {
    fn OnSourceOpen(&self) {
        self.shared.signal(self.key, Signal::StreamOpened);
    }
    fn OnSourceEnded(&self) {
        self.shared.signal(self.key, Signal::StreamEnded);
    }
    fn OnSourceClose(&self) {}
}

#[implement(IMFSourceBufferNotify)]
struct BufferNotify {
    key: Key,
    buffer: u64,
    callbacks: Arc<PendingCallbacks>,
    shared: Arc<Shared>,
}

impl IMFSourceBufferNotify_Impl for BufferNotify_Impl {
    fn OnUpdateStart(&self) {
        let operation = self.callbacks.submitted.swap(0, Ordering::AcqRel);
        self.callbacks.active.store(operation, Ordering::Release);
        self.callbacks.status.store(0, Ordering::Release);
    }
    fn OnAbort(&self) {
        self.callbacks.status.store(E_ABORT.0, Ordering::Release);
    }
    fn OnError(&self, error: HRESULT) {
        self.callbacks.status.store(error.0, Ordering::Release);
    }
    fn OnUpdate(&self) {}
    fn OnUpdateEnd(&self) {
        let status = self.callbacks.status.swap(0, Ordering::AcqRel);
        let operation = self.callbacks.active.swap(0, Ordering::AcqRel);
        self.shared.signal(
            self.key,
            Signal::BufferComplete {
                buffer: self.buffer,
                operation,
                status,
            },
        );
    }
}
