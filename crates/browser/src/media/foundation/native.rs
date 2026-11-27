use super::{Command, EventKind, Key, Shared, Signal};
use std::{cell::Cell, ptr, rc::Rc, sync::Arc};
mod stream;

use stream::{SOURCE_URL, Stream};
use windows::{
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG, E_OUTOFMEMORY, RECT, S_FALSE},
        Graphics::{
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Imaging::{
                CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICBitmap,
                IWICImagingFactory, WICBitmapCacheOnLoad,
            },
        },
        Media::MediaFoundation::{
            CLSID_MFMediaEngineClassFactory, IMFByteStream, IMFMediaEngine,
            IMFMediaEngineClassFactory, IMFMediaEngineEx, IMFMediaEngineExtension,
            IMFMediaEngineNotify, IMFMediaEngineNotify_Impl, MF_E_INVALID_FILE_FORMAT,
            MF_MEDIA_ENGINE_CALLBACK, MF_MEDIA_ENGINE_EVENT, MF_MEDIA_ENGINE_EVENT_CANPLAY,
            MF_MEDIA_ENGINE_EVENT_ENDED, MF_MEDIA_ENGINE_EVENT_ERROR,
            MF_MEDIA_ENGINE_EVENT_FIRSTFRAMEREADY, MF_MEDIA_ENGINE_EVENT_LOADEDDATA,
            MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA, MF_MEDIA_ENGINE_EVENT_PAUSE,
            MF_MEDIA_ENGINE_EVENT_PLAYING, MF_MEDIA_ENGINE_EVENT_SEEKED,
            MF_MEDIA_ENGINE_EVENT_SEEKING, MF_MEDIA_ENGINE_EVENT_TIMEUPDATE,
            MF_MEDIA_ENGINE_EVENT_WAITING, MF_MEDIA_ENGINE_EXTENSION,
            MF_MEDIA_ENGINE_READY_HAVE_FUTURE_DATA, MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
            MF_MEDIA_ENGINE_STATISTIC_FRAMES_DROPPED, MF_MEDIA_ENGINE_STATISTIC_FRAMES_CORRUPTED,
            MF_VERSION, MFARGB, MFCreateAttributes, MFCreateMFByteStreamOnStream, MFShutdown,
            MFStartup,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        System::Com::StructuredStorage::PropVariantToUInt32,
        UI::Shell::SHCreateMemStream,
    },
    core::{BSTR, Error, Interface, Result, implement},
};

struct Com;

impl Drop for Com {
    fn drop(&mut self) {
        // The worker initializes and uninitializes its own COM apartment.
        unsafe { CoUninitialize() };
    }
}

struct MediaFoundation;

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        let _ = unsafe { MFShutdown() };
    }
}

pub(super) struct Interop {
    engine: IMFMediaEngineClassFactory,
    imaging: IWICImagingFactory,
    outstanding_stream_bytes: Rc<Cell<usize>>,
    // Drop COM objects before shutting down their platform and apartment.
    _foundation: MediaFoundation,
    _com: Com,
}

impl Interop {
    pub(super) fn new() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        let com = Com;
        unsafe { MFStartup(MF_VERSION, 0)? };
        let foundation = MediaFoundation;
        let engine = unsafe {
            CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?
        };
        let imaging =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
        Ok(Self {
            engine,
            imaging,
            outstanding_stream_bytes: Rc::default(),
            _foundation: foundation,
            _com: com,
        })
    }

    pub(super) fn open(&self, key: Key, bytes: &[u8], shared: Arc<Shared>) -> Result<Player> {
        if bytes.len() > u32::MAX as usize {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        let Some(header) = bytes.get(..16).filter(|header| &header[4..8] == b"ftyp") else {
            return Err(Error::from_hresult(MF_E_INVALID_FILE_FORMAT));
        };
        let size = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        if !(16..=bytes.len()).contains(&size) || !size.is_multiple_of(4) {
            return Err(Error::from_hresult(MF_E_INVALID_FILE_FORMAT));
        }
        let stream = unsafe {
            // SHCreateMemStream owns a copy. Media Foundation sees only this byte stream.
            let memory =
                SHCreateMemStream(Some(bytes)).ok_or_else(|| Error::from_hresult(E_OUTOFMEMORY))?;
            MFCreateMFByteStreamOnStream(&memory)?
        };
        let engine = self.create_engine(key, shared, None)?;
        let player = Player::new(
            engine,
            self.imaging.clone(),
            Input::File {
                _bytes: stream.clone(),
            },
        );
        let source = player.engine.cast::<IMFMediaEngineEx>()?;
        // Select the MP4 byte-stream handler without exposing the document's URL.
        unsafe { source.SetSourceFromByteStream(&stream, &BSTR::from("satin-media.mp4"))? };
        Ok(player)
    }

    pub(super) fn open_stream(&self, key: Key, shared: Arc<Shared>) -> Result<Player> {
        let stream = Stream::new(
            &self.engine,
            key,
            shared.clone(),
            self.outstanding_stream_bytes.clone(),
        )?;
        let resolver = stream.resolver()?;
        let engine = self.create_engine(key, shared, Some(&resolver))?;
        let player = Player::new(engine, self.imaging.clone(), Input::Stream(stream));
        unsafe { player.engine.SetSource(&BSTR::from(SOURCE_URL))? };
        Ok(player)
    }

    fn create_engine(
        &self,
        key: Key,
        shared: Arc<Shared>,
        resolver: Option<&IMFMediaEngineExtension>,
    ) -> Result<IMFMediaEngine> {
        let notify: IMFMediaEngineNotify = Notify { key, shared }.into();
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 2 + u32::from(resolver.is_some()))?;
            let attributes = attributes.ok_or_else(|| Error::from_hresult(E_FAIL))?;
            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
            attributes.SetUINT32(
                &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
            )?;
            if let Some(resolver) = resolver {
                attributes.SetUnknown(&MF_MEDIA_ENGINE_EXTENSION, resolver)?;
            }
            self.engine.CreateInstance(0, &attributes)
        }
    }
}

enum Input {
    File { _bytes: IMFByteStream },
    Stream(Stream),
}

impl Player {
    fn new(engine: IMFMediaEngine, imaging: IWICImagingFactory, input: Input) -> Self {
        Self {
            engine,
            imaging,
            input,
            surface: None,
            frame_sequence: 0,
            frame_stats: (0, 0),
            native_size: None,
            pending_seek: None,
            submitted_seek: None,
            paused_for_seek: false,
            play_requested: false,
            pending_rate: None,
            ready: false,
            playing: false,
            waiting_through: None,
            seeking: false,
            ended: false,
            play_after_stream_end: false,
        }
    }
}

#[implement(IMFMediaEngineNotify)]
struct Notify {
    key: Key,
    shared: Arc<Shared>,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, param2: u32) -> Result<()> {
        let signal = match MF_MEDIA_ENGINE_EVENT(event as i32) {
            MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA => Signal::Metadata,
            MF_MEDIA_ENGINE_EVENT_LOADEDDATA | MF_MEDIA_ENGINE_EVENT_CANPLAY => Signal::Loaded,
            MF_MEDIA_ENGINE_EVENT_FIRSTFRAMEREADY => Signal::FirstFrame,
            MF_MEDIA_ENGINE_EVENT_PLAYING => Signal::Playing,
            MF_MEDIA_ENGINE_EVENT_PAUSE => Signal::Paused,
            MF_MEDIA_ENGINE_EVENT_WAITING => Signal::Waiting,
            MF_MEDIA_ENGINE_EVENT_SEEKING => Signal::Seeking,
            MF_MEDIA_ENGINE_EVENT_SEEKED => Signal::Seeked,
            MF_MEDIA_ENGINE_EVENT_TIMEUPDATE => Signal::Time,
            MF_MEDIA_ENGINE_EVENT_ENDED => Signal::Ended,
            MF_MEDIA_ENGINE_EVENT_ERROR => Signal::Error {
                code: param1,
                detail: param2,
            },
            _ => return Ok(()),
        };
        self.shared.signal(self.key, signal);
        Ok(())
    }
}

pub(super) struct Player {
    engine: IMFMediaEngine,
    imaging: IWICImagingFactory,
    input: Input,
    surface: Option<WicSurface>,
    frame_sequence: u64,
    frame_stats: (u32, u32),
    native_size: Option<(u32, u32)>,
    pending_seek: Option<f64>,
    submitted_seek: Option<f64>,
    paused_for_seek: bool,
    play_requested: bool,
    pending_rate: Option<(u64, f64)>,
    ready: bool,
    playing: bool,
    waiting_through: Option<f64>,
    seeking: bool,
    ended: bool,
    play_after_stream_end: bool,
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = unsafe { self.engine.Shutdown() };
    }
}

impl Player {
    pub(super) fn control(&mut self, command: Command, output: &Shared) -> Result<()> {
        // Each operation runs on the same apartment that owns the engine.
        unsafe {
            match command {
                Command::Play(_) => {
                    self.play_requested = true;
                    if self.paused_for_seek {
                        return Ok(());
                    }
                    if matches!(&self.input, Input::Stream(stream) if stream.ending()) {
                        self.play_after_stream_end = true;
                        return Ok(());
                    }
                    if self.ended && matches!(self.input, Input::Stream(_)) {
                        self.engine.SetCurrentTime(0.)?;
                    }
                    self.ended = false;
                    self.engine.Play()
                }
                Command::Pause(_) => {
                    self.play_requested = false;
                    self.play_after_stream_end = false;
                    self.waiting_through = None;
                    self.engine.Pause()
                }
                Command::Seek { time, .. } if !time.is_finite() => {
                    Err(Error::from_hresult(E_INVALIDARG))
                }
                Command::Seek { key, time } => {
                    self.pending_seek = Some(time.max(0.));
                    self.submitted_seek = None;
                    self.waiting_through = None;
                    self.ended = false;
                    self.try_pending_seek(output, key)
                }
                Command::PlaybackRate {
                    revision, value, ..
                } if self.native_size.is_none() => {
                    self.pending_rate = Some((revision, value));
                    Ok(())
                }
                Command::PlaybackRate {
                    key,
                    revision,
                    value,
                } => {
                    self.apply_rate(key, output, revision, value);
                    Ok(())
                }
                Command::Volume { value, .. } if (0. ..=1.).contains(&value) => {
                    self.engine.SetVolume(value)
                }
                Command::Volume { .. } => Err(Error::from_hresult(E_INVALIDARG)),
                Command::Muted { muted, .. } => self.engine.SetMuted(muted),
                streaming @ (Command::AddBuffer { .. }
                | Command::Append { .. }
                | Command::RemoveRange { .. }
                | Command::RemoveBuffer { .. }
                | Command::AbortBuffer { .. }
                | Command::SetDuration { .. }
                | Command::EndStream { .. }) => {
                    if let Input::Stream(stream) = &mut self.input {
                        stream.command(streaming)
                    } else {
                        Err(Error::from_hresult(E_INVALIDARG))
                    }
                }
                _ => unreachable!("only player controls are passed to a native player"),
            }
        }
    }

    fn try_pending_seek(&mut self, output: &Shared, key: Key) -> Result<()> {
        let Some(time) = self.pending_seek else {
            return Ok(());
        };
        if self.native_size.is_none() {
            return Ok(());
        }
        let wait_for_data = matches!(&self.input, Input::Stream(stream)
            if stream.end_time().is_none() && !stream.ending() && stream.buffered_until(time).is_none());
        if wait_for_data {
            if !self.paused_for_seek {
                // Media Foundation can strand a seek submitted before MSE has
                // the target range. Let the page request and append it first.
                if !unsafe { self.engine.IsPaused().as_bool() } {
                    unsafe { self.engine.Pause()? };
                }
                self.paused_for_seek = true;
                self.seeking = true;
                self.playing = false;
                output.publish(key, EventKind::Seeking);
                if self.play_requested {
                    output.publish(key, EventKind::Waiting);
                }
            }
            return Ok(());
        }
        self.pending_seek = None;
        unsafe {
            self.engine.SetCurrentTime(time)?;
            self.submitted_seek = Some(time);
            if self.paused_for_seek && self.play_requested {
                // Running the engine lets Media Foundation consume the newly
                // appended samples while completing the deferred seek.
                self.engine.Play()?;
            }
        }
        self.trace_seek("submitted");
        Ok(())
    }

    #[cfg(any(test, feature = "media-startup-diagnostics"))]
    fn trace_seek(&self, phase: &str) {
        use std::sync::{OnceLock, atomic::{AtomicUsize, Ordering}};
        static ENABLED: OnceLock<bool> = OnceLock::new();
        static TRACES: AtomicUsize = AtomicUsize::new(0);
        if *ENABLED.get_or_init(|| std::env::var_os("SATIN_QA_NATIVE_SEEK").is_some())
            && TRACES.fetch_add(1, Ordering::Relaxed) < 48
        {
            let (paused, seeking, ready) = unsafe {
                (self.engine.IsPaused().as_bool(), self.engine.IsSeeking().as_bool(), self.engine.GetReadyState())
            };
            eprintln!("SATIN_QA_NATIVE_SEEK {phase} target={:?} time={:.5} paused={paused} seeking={seeking} ready={ready}",
                self.submitted_seek.or(self.pending_seek), self.time());
        }
    }

    #[cfg(not(any(test, feature = "media-startup-diagnostics")))]
    fn trace_seek(&self, _: &str) {}

    fn finish_seek(&mut self) -> Result<f64> {
        self.trace_seek("complete");
        self.submitted_seek = None;
        self.seeking = false;
        let time = self.time();
        if std::mem::take(&mut self.paused_for_seek) && self.play_requested {
            self.waiting_through = Some(time);
            if unsafe { self.engine.IsPaused().as_bool() } {
                unsafe { self.engine.Play()? };
            }
        }
        Ok(time)
    }

    fn apply_rate(&self, key: Key, output: &Shared, revision: u64, requested: f64) {
        // Rate support can also depend on the current source. A rejected
        // optional speed change must leave the current media session usable.
        if unsafe { self.engine.SetPlaybackRate(requested) }.is_err() {
            let actual = unsafe { self.engine.GetPlaybackRate() };
            output.publish(key, EventKind::RateRejected { revision, actual });
        }
    }

    fn metadata(&mut self, output: &Shared, key: Key) -> Result<()> {
        if self.native_size.is_some() {
            return Ok(());
        }
        let size = self.video_size()?;
        self.publish_metadata(size, output, key);
        self.try_pending_seek(output, key)
    }

    fn video_size(&self) -> Result<(u32, u32)> {
        let (mut width, mut height) = (0, 0);
        unsafe {
            if self.engine.HasVideo().as_bool() {
                self.engine
                    .GetNativeVideoSize(Some(&mut width), Some(&mut height))?;
            }
        }
        Ok((width, height))
    }

    fn publish_metadata(&mut self, (width, height): (u32, u32), output: &Shared, key: Key) {
        self.native_size = Some((width, height));
        self.surface = None;
        if let Some((revision, rate)) = self.pending_rate.take() {
            self.apply_rate(key, output, revision, rate);
        }
        output.publish(
            key,
            EventKind::Metadata {
                width,
                height,
                duration: unsafe { self.engine.GetDuration() },
                has_audio: unsafe { self.engine.HasAudio().as_bool() },
            },
        );
    }

    pub(super) fn notification(
        &mut self,
        signal: Signal,
        output: &Shared,
        key: Key,
    ) -> std::result::Result<(), String> {
        if matches!(signal, Signal::Time | Signal::Paused | Signal::Ended) {
            self.update_frame_stats(output, key);
        }
        let mut capture = false;
        let kind = match signal {
            Signal::StreamOpened => {
                if let Input::Stream(stream) = &mut self.input {
                    stream.reopened();
                }
                self.ended = false;
                EventKind::StreamOpened
            }
            Signal::StreamEnded => {
                if let Input::Stream(stream) = &mut self.input {
                    stream.ended();
                }
                self.try_pending_seek(output, key).map_err(|error| error.to_string())?;
                if std::mem::take(&mut self.play_after_stream_end) {
                    self.control(Command::Play(key), output)
                        .map_err(|error| error.to_string())?;
                }
                return Ok(());
            }
            Signal::BufferComplete {
                buffer,
                operation,
                status,
            } => {
                if let Input::Stream(stream) = &mut self.input {
                    stream.complete(buffer, operation, status);
                }
                self.try_pending_seek(output, key).map_err(|error| error.to_string())?;
                return Ok(());
            }
            Signal::Metadata => {
                self.metadata(output, key)
                    .map_err(|error| error.to_string())?;
                return Ok(());
            }
            Signal::Loaded if !self.ready => {
                self.metadata(output, key)
                    .map_err(|error| error.to_string())?;
                self.ready = true;
                capture = true;
                EventKind::Ready
            }
            // Windows can report CANPLAY again after an open stream refills.
            Signal::Loaded => EventKind::Ready,
            Signal::FirstFrame => {
                if let Some(frame) = self
                    .frame(true, output, key)
                    .map_err(|error| error.to_string())?
                {
                    output.publish(key, frame);
                }
                return Ok(());
            }
            Signal::Playing => {
                if self.paused_for_seek {
                    return Ok(());
                }
                self.playing = true;
                self.waiting_through = None;
                EventKind::Playing
            }
            Signal::Paused => {
                if self.paused_for_seek {
                    return Ok(());
                }
                self.playing = false;
                self.waiting_through = None;
                if self.ended {
                    return Ok(());
                }
                output.publish(key, EventKind::Time(self.time()));
                EventKind::Paused
            }
            Signal::Waiting => {
                if self.playing
                    && !self.seeking
                    && let Input::Stream(stream) = &self.input
                {
                    self.waiting_through = stream.buffered_until(self.time());
                }
                self.playing = false;
                EventKind::Waiting
            }
            Signal::Seeking => {
                if self.paused_for_seek {
                    return Ok(());
                }
                self.seeking = true;
                EventKind::Seeking
            }
            Signal::Seeked => {
                if self.pending_seek.is_some() || (!self.seeking && self.submitted_seek.is_none()) {
                    return Ok(());
                }
                capture = self.ready;
                EventKind::Seeked(self.finish_seek().map_err(|error| error.to_string())?)
            }
            Signal::Time => {
                if self.ended || self.pending_seek.is_some() {
                    return Ok(());
                }
                let time = self.time();
                if self.paused_for_seek {
                    self.trace_seek("clock");
                    let reached_position = self.submitted_seek.is_some_and(|target| {
                        time >= target - 0.02 && !unsafe { self.engine.IsSeeking().as_bool() }
                    });
                    if !reached_position {
                        return Ok(());
                    }
                    let seeked = self.finish_seek().map_err(|error| error.to_string())?;
                    output.publish(key, EventKind::Seeked(seeked));
                }
                if self.waiting_through.is_some_and(|edge| time > edge)
                    && !self.seeking
                    && unsafe {
                        !self.engine.IsPaused().as_bool()
                            && i32::from(self.engine.GetReadyState())
                                >= MF_MEDIA_ENGINE_READY_HAVE_FUTURE_DATA.0
                    }
                {
                    // Media Foundation can resume after refill without CANPLAY or
                    // PLAYING. Its ready state alone can stay high during starvation,
                    // so require the clock to pass the previously buffered range.
                    self.waiting_through = None;
                    self.playing = true;
                    output.publish(key, EventKind::Playing);
                }
                if self.playing && !self.seeking && self.stream_end_time() == Some(time) {
                    // Media Foundation can keep running its clock after an MSE seek
                    // has consumed all the data in an explicitly completed stream.
                    unsafe { self.engine.Pause() }.map_err(|error| error.to_string())?;
                    self.finish(output, key);
                    return Ok(());
                }
                EventKind::Time(time)
            }
            Signal::Ended if self.paused_for_seek => return Ok(()),
            Signal::Ended => {
                self.finish(output, key);
                return Ok(());
            }
            Signal::Error { code, detail } => {
                return Err(format!("Windows media error {code} (0x{detail:08X})"));
            }
        };
        if capture
            && let Some(frame) = self
                .frame(true, output, key)
                .map_err(|error| error.to_string())?
        {
            output.publish(key, frame);
        }
        output.publish(key, kind);
        Ok(())
    }

    fn update_frame_stats(&mut self, output: &Shared, key: Key) {
        if !unsafe { self.engine.HasVideo().as_bool() } {
            return;
        }
        let Ok(engine) = self.engine.cast::<IMFMediaEngineEx>() else {
            return;
        };
        let statistic = |kind| unsafe {
            let value = engine.GetStatistics(kind).ok()?;
            PropVariantToUInt32(&value).ok()
        };
        let dropped = statistic(MF_MEDIA_ENGINE_STATISTIC_FRAMES_DROPPED).unwrap_or(self.frame_stats.0);
        let corrupted = statistic(MF_MEDIA_ENGINE_STATISTIC_FRAMES_CORRUPTED).unwrap_or(self.frame_stats.1);
        if (dropped, corrupted) != self.frame_stats {
            self.frame_stats = (dropped, corrupted);
            output.publish(key, EventKind::DecoderFrameStats { dropped, corrupted });
        }
    }

    pub(super) fn needs_frame(&self) -> bool {
        (self.playing || self.seeking)
            && match self.input {
                Input::Stream(_) => unsafe { self.engine.HasVideo().as_bool() },
                Input::File { .. } => self
                    .native_size
                    .is_some_and(|(width, height)| width > 0 && height > 0),
            }
    }

    fn time(&self) -> f64 {
        let current = unsafe { self.engine.GetCurrentTime() };
        self.stream_end_time()
            .map_or(current, |end| current.min(end))
    }

    fn stream_end_time(&self) -> Option<f64> {
        match &self.input {
            Input::Stream(stream) => stream.end_time(),
            Input::File { .. } => None,
        }
    }

    fn finish(&mut self, output: &Shared, key: Key) {
        self.playing = false;
        self.play_requested = false;
        self.waiting_through = None;
        if !std::mem::replace(&mut self.ended, true) {
            output.publish(key, EventKind::Time(self.time()));
            output.publish(key, EventKind::Ended);
        }
    }

    pub(super) fn frame(
        &mut self,
        force: bool,
        output: &Shared,
        key: Key,
    ) -> Result<Option<EventKind>> {
        if self.pending_seek.is_some() {
            return Ok(None);
        }
        let stream = matches!(self.input, Input::Stream(_));
        if (stream && !unsafe { self.engine.HasVideo().as_bool() })
            || (!stream
                && self
                    .native_size
                    .is_none_or(|(width, height)| width == 0 || height == 0))
        {
            return Ok(None);
        }
        let mut pts = 0;
        // The generated Result wrapper treats S_FALSE as success and loses the no-new-frame signal.
        let status = unsafe {
            (Interface::vtable(&self.engine).OnVideoStreamTick)(
                Interface::as_raw(&self.engine),
                &mut pts,
            )
        };
        status.ok()?;
        if status == S_FALSE && (!force || (stream && self.surface.is_none())) {
            return Ok(None);
        }
        if stream {
            // The media engine can publish provisional dimensions before decoding an
            // MSE frame. It can also change dimensions during a bitrate switch.
            let size = self.video_size()?;
            if Some(size) != self.native_size {
                self.publish_metadata(size, output, key);
            }
        }
        let Some((width, height)) = self
            .native_size
            .filter(|(width, height)| *width > 0 && *height > 0)
        else {
            return Ok(None);
        };
        if self.surface.is_none() {
            self.surface = Some(WicSurface::new(&self.imaging, width, height)?);
        }
        let pixels = self.surface.as_ref().unwrap().copy_frame(&self.engine)?;
        if status != S_FALSE || self.frame_sequence == 0 {
            self.frame_sequence = self.frame_sequence.saturating_add(1);
        }
        Ok(Some(EventKind::Frame { pixels, sequence: self.frame_sequence }))
    }
}

struct WicSurface {
    bitmap: IWICBitmap,
    width: u32,
    height: u32,
}

impl WicSurface {
    fn new(factory: &IWICImagingFactory, width: u32, height: u32) -> Result<Self> {
        // Bound CPU readback and retain the source aspect ratio for large video.
        let scale = (1920. / f64::from(width))
            .min(1080. / f64::from(height))
            .min(1.);
        let width = (f64::from(width) * scale).round().max(1.) as u32;
        let height = (f64::from(height) * scale).round().max(1.) as u32;
        let bitmap = unsafe {
            factory.CreateBitmap(
                width,
                height,
                &GUID_WICPixelFormat32bppBGRA,
                WICBitmapCacheOnLoad,
            )?
        };
        Ok(Self {
            bitmap,
            width,
            height,
        })
    }

    fn copy_frame(&self, engine: &IMFMediaEngine) -> Result<Arc<image::RgbaImage>> {
        let dest = RECT {
            left: 0,
            top: 0,
            right: self.width as i32,
            bottom: self.height as i32,
        };
        let border = MFARGB {
            rgbBlue: 0,
            rgbGreen: 0,
            rgbRed: 0,
            rgbAlpha: 255,
        };
        let stride = self.width * 4;
        let mut pixels = vec![0; (stride * self.height) as usize];
        unsafe {
            engine.TransferVideoFrame(&self.bitmap, None, &dest, Some(&border))?;
            self.bitmap.CopyPixels(ptr::null(), stride, &mut pixels)?;
        }
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        let frame = image::RgbaImage::from_raw(self.width, self.height, pixels)
            .ok_or_else(|| Error::from_hresult(E_FAIL))?;
        Ok(Arc::new(frame))
    }
}
