//! Safe, apartment-scoped access to installed Windows Media Foundation source types.
#![cfg(windows)]

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

struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        // Balanced against the successful initialization on this thread.
        unsafe { CoUninitialize() };
    }
}

struct Foundation;

impl Drop for Foundation {
    fn drop(&mut self) {
        // The extension is released before the process-wide startup reference.
        let _ = unsafe { MFShutdown() };
    }
}

/// Query exact container and codec strings on the calling thread. All COM
/// interfaces are dropped in their original apartment when the callback returns.
pub fn with_source_types<T>(query: impl FnOnce(&dyn Fn(&str) -> bool) -> T) -> Option<T> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().ok()?;
    let _apartment = Apartment;
    unsafe { MFStartup(MF_VERSION, 0) }.ok()?;
    let _foundation = Foundation;
    let source = create_source().ok()?;
    Some(query(&|mime| {
        // The owned BSTR remains valid throughout the synchronous native call.
        unsafe { source.IsTypeSupported(&BSTR::from(mime)) }.as_bool()
    }))
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
