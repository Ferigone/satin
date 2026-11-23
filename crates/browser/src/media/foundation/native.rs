use super::{Command, EventKind, Key, Shared, Signal};
use std::{ptr, sync::Arc};
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
            IMFMediaEngineClassFactory, IMFMediaEngineEx, IMFMediaEngineNotify,
            IMFMediaEngineNotify_Impl, MF_E_INVALID_FILE_FORMAT, MF_MEDIA_ENGINE_CALLBACK,
            MF_MEDIA_ENGINE_EVENT, MF_MEDIA_ENGINE_EVENT_CANPLAY, MF_MEDIA_ENGINE_EVENT_ENDED,
            MF_MEDIA_ENGINE_EVENT_ERROR, MF_MEDIA_ENGINE_EVENT_FIRSTFRAMEREADY,
            MF_MEDIA_ENGINE_EVENT_LOADEDDATA, MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA,
            MF_MEDIA_ENGINE_EVENT_PAUSE, MF_MEDIA_ENGINE_EVENT_PLAYING,
            MF_MEDIA_ENGINE_EVENT_SEEKED, MF_MEDIA_ENGINE_EVENT_SEEKING,
            MF_MEDIA_ENGINE_EVENT_TIMEUPDATE, MF_MEDIA_ENGINE_EVENT_WAITING,
            MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MF_VERSION, MFARGB, MFCreateAttributes,
            MFCreateMFByteStreamOnStream, MFShutdown, MFStartup,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
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
        let notify: IMFMediaEngineNotify = Notify { key, shared }.into();
        let mut attributes = None;
        unsafe { MFCreateAttributes(&mut attributes, 2)? };
        let attributes = attributes.ok_or_else(|| Error::from_hresult(E_FAIL))?;
        let (engine, stream) = unsafe {
            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
            attributes.SetUINT32(
                &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
            )?;
            // SHCreateMemStream owns a copy. Media Foundation sees only this byte stream.
            let memory =
                SHCreateMemStream(Some(bytes)).ok_or_else(|| Error::from_hresult(E_OUTOFMEMORY))?;
            let stream = MFCreateMFByteStreamOnStream(&memory)?;
            let engine = self.engine.CreateInstance(0, &attributes)?;
            let source = engine.cast::<IMFMediaEngineEx>()?;
            // Select the MP4 byte-stream handler without exposing the document's URL.
            if let Err(error) =
                source.SetSourceFromByteStream(&stream, &BSTR::from("satin-media.mp4"))
            {
                let _ = engine.Shutdown();
                return Err(error);
            }
            (engine, stream)
        };
        Ok(Player {
            engine,
            imaging: self.imaging.clone(),
            _stream: stream,
            surface: None,
            native_size: None,
            pending_seek: None,
            ready: false,
            playing: false,
            seeking: false,
        })
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
    _stream: IMFByteStream,
    surface: Option<WicSurface>,
    native_size: Option<(u32, u32)>,
    pending_seek: Option<f64>,
    ready: bool,
    playing: bool,
    seeking: bool,
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = unsafe { self.engine.Shutdown() };
    }
}

impl Player {
    pub(super) fn control(&mut self, command: Command) -> Result<()> {
        // Each operation runs on the same apartment that owns the engine.
        unsafe {
            match command {
                Command::Play(_) => self.engine.Play(),
                Command::Pause(_) => self.engine.Pause(),
                Command::Seek { time, .. } if !time.is_finite() => {
                    Err(Error::from_hresult(E_INVALIDARG))
                }
                Command::Seek { time, .. } if self.native_size.is_none() => {
                    self.pending_seek = Some(time.max(0.));
                    Ok(())
                }
                Command::Seek { time, .. } => self.engine.SetCurrentTime(time.max(0.)),
                Command::Volume { value, .. } if (0. ..=1.).contains(&value) => {
                    self.engine.SetVolume(value)
                }
                Command::Volume { .. } => Err(Error::from_hresult(E_INVALIDARG)),
                Command::Muted { muted, .. } => self.engine.SetMuted(muted),
                _ => unreachable!("only player controls are passed to a native player"),
            }
        }
    }

    fn metadata(&mut self, output: &Shared, key: Key) -> Result<()> {
        if self.native_size.is_some() {
            return Ok(());
        }
        let (mut width, mut height) = (0, 0);
        let duration = unsafe {
            if self.engine.HasVideo().as_bool() {
                self.engine
                    .GetNativeVideoSize(Some(&mut width), Some(&mut height))?;
            }
            self.engine.GetDuration()
        };
        self.native_size = Some((width, height));
        output.publish(
            key,
            EventKind::Metadata {
                width,
                height,
                duration,
                #[cfg(test)]
                has_audio: unsafe { self.engine.HasAudio().as_bool() },
            },
        );
        if let Some(time) = self.pending_seek.take() {
            unsafe { self.engine.SetCurrentTime(time)? };
        }
        Ok(())
    }

    pub(super) fn notification(
        &mut self,
        signal: Signal,
        output: &Shared,
        key: Key,
    ) -> std::result::Result<(), String> {
        let mut capture = false;
        let kind = match signal {
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
            Signal::Loaded => return Ok(()),
            Signal::FirstFrame => {
                if let Some(frame) = self.frame(true).map_err(|error| error.to_string())? {
                    output.publish(key, EventKind::Frame(frame));
                }
                return Ok(());
            }
            Signal::Playing => {
                self.playing = true;
                EventKind::Playing
            }
            Signal::Paused => {
                self.playing = false;
                output.publish(key, EventKind::Time(self.time()));
                EventKind::Paused
            }
            Signal::Waiting => {
                self.playing = false;
                EventKind::Waiting
            }
            Signal::Seeking => {
                self.seeking = true;
                EventKind::Seeking
            }
            Signal::Seeked => {
                self.seeking = false;
                capture = self.ready;
                EventKind::Seeked(self.time())
            }
            Signal::Time => EventKind::Time(self.time()),
            Signal::Ended => {
                self.playing = false;
                output.publish(key, EventKind::Time(self.time()));
                EventKind::Ended
            }
            Signal::Error { code, detail } => {
                return Err(format!("Windows media error {code} (0x{detail:08X})"));
            }
        };
        if capture && let Some(frame) = self.frame(true).map_err(|error| error.to_string())? {
            output.publish(key, EventKind::Frame(frame));
        }
        output.publish(key, kind);
        Ok(())
    }

    pub(super) fn needs_frame(&self) -> bool {
        (self.playing || self.seeking)
            && self
                .native_size
                .is_some_and(|(width, height)| width > 0 && height > 0)
    }

    fn time(&self) -> f64 {
        unsafe { self.engine.GetCurrentTime() }
    }

    pub(super) fn frame(&mut self, force: bool) -> Result<Option<Arc<image::RgbaImage>>> {
        let Some((width, height)) = self
            .native_size
            .filter(|(width, height)| *width > 0 && *height > 0)
        else {
            return Ok(None);
        };
        let mut pts = 0;
        // The generated Result wrapper treats S_FALSE as success and loses the no-new-frame signal.
        let status = unsafe {
            (Interface::vtable(&self.engine).OnVideoStreamTick)(
                Interface::as_raw(&self.engine),
                &mut pts,
            )
        };
        status.ok()?;
        if status == S_FALSE && !force {
            return Ok(None);
        }
        if self.surface.is_none() {
            self.surface = Some(WicSurface::new(&self.imaging, width, height)?);
        }
        self.surface
            .as_ref()
            .unwrap()
            .copy_frame(&self.engine)
            .map(Some)
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
