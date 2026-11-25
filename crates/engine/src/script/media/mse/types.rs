//! Query the Media Foundation source extension that consumes appendBuffer bytes.
//! Documents share one COM apartment while any of them still need capabilities.
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, Weak,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::E_FAIL,
        Media::MediaFoundation::{
            CLSID_MFMediaEngineClassFactory, IMFMediaEngineClassFactoryEx, IMFMediaSourceExtension,
            IMFMediaSourceExtensionNotify, IMFMediaSourceExtensionNotify_Impl, MF_MSE_CALLBACK,
            MF_VERSION, MFCreateAttributes, MFShutdown, MFStartup,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
    },
    core::{BSTR, Error, Result, implement},
};

type Query = (String, SyncSender<bool>);

pub(super) struct Probe(Arc<Worker>);

struct Worker {
    queries: SyncSender<Query>,
    retry_at: Mutex<Option<Instant>>,
}

impl Probe {
    pub(super) fn acquire() -> Option<Self> {
        static SHARED: Mutex<Weak<Worker>> = Mutex::new(Weak::new());
        let mut shared = SHARED.lock().ok()?;
        if let Some(worker) = shared.upgrade() {
            return Some(Self(worker));
        }
        let (queries, receiver) = sync_channel(16);
        thread::Builder::new()
            .name("satin-media-types".into())
            .spawn(move || run(receiver))
            .ok()?;
        let worker = Arc::new(Worker {
            queries,
            retry_at: Mutex::new(None),
        });
        *shared = Arc::downgrade(&worker);
        Some(Self(worker))
    }

    pub(super) fn supported(&self, mime: &str) -> bool {
        if self
            .0
            .retry_at
            .lock()
            .unwrap()
            .is_some_and(|at| at > Instant::now())
        {
            return false;
        }
        let (answer, result) = sync_channel(1);
        if self.0.queries.try_send((mime.to_owned(), answer)).is_err() {
            return false;
        }
        match result.recv_timeout(Duration::from_secs(2)) {
            Ok(supported) => supported,
            Err(_) => {
                // A stalled codec provider must not block every probe on the page.
                *self.0.retry_at.lock().unwrap() = Some(Instant::now() + Duration::from_secs(5));
                false
            }
        }
    }
}

fn run(queries: Receiver<Query>) {
    with_source(|source| {
        let mut cache = VecDeque::<(String, bool)>::new();
        for (mime, answer) in queries {
            let supported = match cache.iter().find(|(kind, _)| kind == &mime) {
                Some((_, result)) => *result,
                None => {
                    let result =
                        unsafe { source.IsTypeSupported(&BSTR::from(mime.as_str())) }.as_bool();
                    if cache.len() == 64 {
                        cache.pop_front();
                    }
                    cache.push_back((mime, result));
                    result
                }
            };
            let _ = answer.send(supported);
        }
    });
}

fn with_source(callback: impl FnOnce(&IMFMediaSourceExtension)) {
    unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return;
        }
        if MFStartup(MF_VERSION, 0).is_ok() {
            if let Ok(source) = create_source() {
                callback(&source);
            }
            let _ = MFShutdown();
        }
        CoUninitialize();
    }
}

fn create_source() -> Result<IMFMediaSourceExtension> {
    let notify: IMFMediaSourceExtensionNotify = Notify.into();
    unsafe {
        let factory: IMFMediaEngineClassFactoryEx =
            CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes = attributes.ok_or_else(|| Error::from_hresult(E_FAIL))?;
        attributes.SetUnknown(&MF_MSE_CALLBACK, &notify)?;
        factory.CreateMediaSourceExtension(0, &attributes)
    }
}

// A capability-only source is never attached to a media element.
#[implement(IMFMediaSourceExtensionNotify)]
struct Notify;

impl IMFMediaSourceExtensionNotify_Impl for Notify_Impl {
    fn OnSourceOpen(&self) {}
    fn OnSourceEnded(&self) {}
    fn OnSourceClose(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_media_queries_match_the_installed_source_extension() {
        let mut results = Vec::new();
        with_source(|source| {
            for mime in [
                r#"video/mp4; codecs="avc1.42C01E""#,
                r#"audio/mp4; codecs="mp4a.40.2""#,
                r#"video/mp4; codecs="hvc1.1.40000000.L60.80.0.0.0.0.0""#,
                r#"video/mp4; codecs="av01.0.05M.08""#,
                r#"video/mp2t; codecs="avc1.4d001f,mp4a.40.2""#,
                r#"video/webm; codecs="vp09.00.10.08""#,
            ] {
                let supported = unsafe { source.IsTypeSupported(&BSTR::from(mime)) }.as_bool();
                results.push((mime, supported));
            }
        });
        assert_eq!(results.len(), 6, "Media Foundation must initialize");
        let store = super::super::Store::default();
        for (mime, expected) in results {
            assert_eq!(store.is_supported(mime), expected, "{mime}");
        }
        assert!(!store.is_supported("audio/aac"));
        assert!(!store.is_supported("application/vnd.apple.mpegurl"));
    }
}
