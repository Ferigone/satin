//! Files come from the native chooser or an explicitly configured smoke response.
use rust_browser_engine::script::{
    FilePickerRequest, MAX_PICKER_BYTES, MAX_PICKER_FILE_BYTES, MAX_PICKER_FILES, ScriptRuntime,
    SelectedFile,
};
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const RESPONSE_BYTES: u64 = 1024 * 1024;
const RESPONSE_COUNT: usize = 256;

pub(crate) struct Responses(VecDeque<Option<Vec<PathBuf>>>);

impl Responses {
    pub(crate) fn read(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| format!("File picker responses: {error}"))?;
        let mut raw = Vec::new();
        file.take(RESPONSE_BYTES + 1)
            .read_to_end(&mut raw)
            .map_err(|error| format!("File picker responses: {error}"))?;
        if raw.len() as u64 > RESPONSE_BYTES {
            return Err("File picker responses exceed 1 MiB".into());
        }
        let responses: VecDeque<Option<Vec<PathBuf>>> = serde_json::from_slice(&raw)
            .map_err(|error| format!("File picker responses: {error}"))?;
        if responses.len() > RESPONSE_COUNT {
            return Err("Too many file picker responses".into());
        }
        if responses
            .iter()
            .flatten()
            .any(|paths| paths.len() > MAX_PICKER_FILES)
        {
            return Err("Too many paths in file picker response".into());
        }
        Ok(Self(responses))
    }

    pub(crate) fn select(
        &mut self,
        request: &FilePickerRequest,
    ) -> Result<Option<Vec<SelectedFile>>, String> {
        let paths = self
            .0
            .pop_front()
            .ok_or("File picker responses exhausted")?;
        paths
            .map(|paths| read_selected(paths, request.multiple))
            .transpose()
    }
}

pub(crate) fn answer_headless(
    runtime: &mut ScriptRuntime,
    responses: &mut Option<Responses>,
) -> Result<bool, String> {
    if let Some(request) = runtime.take_file_picker_request() {
        let files = match responses
            .as_mut()
            .map_or(Ok(None), |answers| answers.select(&request))
        {
            Ok(files) => files,
            Err(error) => {
                runtime
                    .complete_file_picker(&request, None)
                    .map_err(|error| error.to_string())?;
                return Err(error);
            }
        };
        return runtime
            .complete_file_picker(&request, files)
            .map_err(|error| error.to_string());
    }
    Ok(false)
}

fn milliseconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis().min(i64::MAX as u128) as i64,
        Err(error) => -(error.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

fn read_selected(paths: Vec<PathBuf>, multiple: bool) -> Result<Vec<SelectedFile>, String> {
    if paths.len() > if multiple { MAX_PICKER_FILES } else { 1 } {
        return Err("Too many selected files".into());
    }
    let mut total = 0usize;
    let mut out = Vec::with_capacity(paths.len());
    for selected in paths {
        let name = selected
            .file_name()
            .ok_or("Selected path has no file name")?
            .to_string_lossy()
            .into_owned();
        let path = fs::canonicalize(&selected).map_err(|error| format!("{name}: {error}"))?;
        if !fs::metadata(&path)
            .map_err(|error| format!("{name}: {error}"))?
            .is_file()
        {
            return Err(format!("{name} is not a regular file"));
        }
        let mut handle = File::open(&path).map_err(|error| format!("{name}: {error}"))?;
        let meta = handle
            .metadata()
            .map_err(|error| format!("{name}: {error}"))?;
        let limit = MAX_PICKER_FILE_BYTES.min(MAX_PICKER_BYTES - total);
        if !meta.is_file() || meta.len() > limit as u64 {
            return Err(format!("{name} exceeds the selected-file size limit"));
        }
        let modified = meta.modified().ok();
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(meta.len() as usize)
            .map_err(|error| error.to_string())?;
        (&mut handle)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("{name}: {error}"))?;
        if bytes.len() > limit {
            return Err(format!("{name} exceeds the selected-file size limit"));
        }
        let after = handle
            .metadata()
            .map_err(|error| format!("{name}: {error}"))?;
        if after.len() != meta.len()
            || bytes.len() as u64 != meta.len()
            || after.modified().ok() != modified
        {
            return Err(format!("{name} changed while being read"));
        }
        total += bytes.len();
        let extension = selected
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let mime = EXTENSIONS
            .iter()
            .find(|(suffix, _)| suffix.eq_ignore_ascii_case(extension))
            .map_or("", |(_, mime)| *mime)
            .to_owned();
        out.push(SelectedFile {
            path,
            name,
            mime,
            last_modified: milliseconds(modified.unwrap_or_else(SystemTime::now)),
            bytes,
        });
    }
    Ok(out)
}

const EXTENSIONS: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("apng", "image/apng"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("bmp", "image/bmp"),
    ("svg", "image/svg+xml"),
    ("ico", "image/x-icon"),
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("csv", "text/csv"),
    ("html", "text/html"),
    ("json", "application/json"),
    ("pdf", "application/pdf"),
    ("zip", "application/zip"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("oga", "audio/ogg"),
    ("flac", "audio/flac"),
    ("aac", "audio/aac"),
    ("m4a", "audio/mp4"),
    ("mp4", "video/mp4"),
    ("webm", "video/webm"),
    ("ogv", "video/ogg"),
    ("mov", "video/quicktime"),
    ("mkv", "video/x-matroska"),
];

pub(crate) fn choose(
    window: &winit::window::Window,
    request: &FilePickerRequest,
) -> Result<Option<Vec<SelectedFile>>, String> {
    native_paths(window, request)?
        .map(|paths| read_selected(paths, request.multiple))
        .transpose()
}

#[cfg(windows)]
fn native_paths(
    window: &winit::window::Window,
    request: &FilePickerRequest,
) -> Result<Option<Vec<PathBuf>>, String> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    use windows_sys::Win32::{
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        },
        UI::Controls::Dialogs::{
            CommDlgExtendedError, GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_DONTADDTORECENT,
            OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
        },
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            // The matching initialization happened on this UI thread.
            unsafe { CoUninitialize() };
        }
    }

    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("File picker has no Win32 owner".into());
    };
    // The apartment and dialog stay on the winit thread. The only pointers
    // passed to the dialog borrow the owner and local buffers for the call.
    let initialized = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    if initialized < 0 {
        return Err(format!(
            "File dialog apartment initialization failed ({initialized:#x})"
        ));
    }
    let _apartment = Apartment;
    let filter = windows_filter(&request.accept);
    let title: Vec<u16> = "Wybierz plik\0".encode_utf16().collect();
    let mut buffer = vec![0u16; 64 * 1024];
    // OPENFILENAMEW has integer fields and nullable native pointers.
    let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = handle.hwnd.get() as _;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.nFilterIndex = 1;
    dialog.lpstrTitle = title.as_ptr();
    dialog.lpstrFile = buffer.as_mut_ptr();
    dialog.nMaxFile = buffer.len() as u32;
    dialog.Flags = OFN_EXPLORER
        | OFN_FILEMUSTEXIST
        | OFN_PATHMUSTEXIST
        | OFN_NOCHANGEDIR
        | OFN_DONTADDTORECENT;
    if request.multiple {
        dialog.Flags |= OFN_ALLOWMULTISELECT;
    }
    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 {
        let error = unsafe { CommDlgExtendedError() };
        return if error == 0 {
            Ok(None)
        } else {
            Err(format!("File dialog failed ({error:#x})"))
        };
    }
    let mut entries = buffer
        .split(|&unit| unit == 0)
        .take_while(|entry| !entry.is_empty())
        .map(|entry| PathBuf::from(OsString::from_wide(entry)));
    let first = entries.next().ok_or("File dialog returned an empty path")?;
    let mut children = entries.peekable();
    if children.peek().is_none() {
        return Ok(Some(vec![first]));
    }
    let mut paths = Vec::new();
    for child in children {
        if child.file_name() != Some(child.as_os_str()) || paths.len() >= MAX_PICKER_FILES {
            return Err("File dialog returned an invalid multi-file selection".into());
        }
        paths.push(first.join(child));
    }
    Ok(Some(paths))
}

#[cfg(windows)]
fn windows_filter(accept: &str) -> Vec<u16> {
    use std::collections::BTreeSet;
    let mut patterns = BTreeSet::new();
    for token in accept
        .split(',')
        .map(|word| word.trim().to_ascii_lowercase())
    {
        if let Some(suffix) = token.strip_prefix('.') {
            if !suffix.is_empty()
                && suffix.len() <= 32
                && suffix
                    .bytes()
                    .all(|unit| unit.is_ascii_alphanumeric() || b".+-_".contains(&unit))
            {
                patterns.insert(format!("*{token}"));
            }
        } else {
            let group = match token.as_str() {
                "image/*" => Some("image/"),
                "audio/*" => Some("audio/"),
                "video/*" => Some("video/"),
                _ => None,
            };
            for &(suffix, mime) in EXTENSIONS {
                if token == mime || group.is_some_and(|prefix| mime.starts_with(prefix)) {
                    patterns.insert(format!("*.{suffix}"));
                }
            }
        }
    }
    let mut filter = String::new();
    if !patterns.is_empty() {
        filter.push_str("Zalecane typy plików\0");
        filter.push_str(&patterns.into_iter().collect::<Vec<_>>().join(";"));
        filter.push('\0');
    }
    filter.push_str("Wszystkie pliki\0*.*\0\0");
    filter.encode_utf16().collect()
}

#[cfg(not(windows))]
fn native_paths(
    _: &winit::window::Window,
    _: &FilePickerRequest,
) -> Result<Option<Vec<PathBuf>>, String> {
    Err("The native file picker is available on Windows".into())
}
