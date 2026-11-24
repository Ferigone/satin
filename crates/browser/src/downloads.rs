//! A transfer owns its original response and writes only to a new, local destination.
use rust_browser_engine::{loader::DownloadResponse, network::CancelSignal};
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{fs, io::AsyncWriteExt};

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Partial(PathBuf);
impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(crate) async fn save(
    response: DownloadResponse,
    path: &Path,
    cancel: &CancelSignal,
    progress: impl FnMut(u64),
) -> Result<u64, String> {
    let parent = path.parent().ok_or("Missing download directory")?;
    let (partial, mut file) = loop {
        if cancel.is_cancelled() {
            return Err("Download cancelled".into());
        }
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let part = parent.join(format!(".satin-{}-{id}.part", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&part).await {
            Ok(file) => break (Partial(part), file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    };
    let count = response.write_to(&mut file, cancel, progress).await;
    let flushed = file.shutdown().await;
    drop(file);
    let count = count?;
    flushed.map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        // Carry Internet Zone information into the final file before it is visible.
        let mut zone = partial.0.clone().into_os_string();
        zone.push(":Zone.Identifier");
        fs::write(zone, b"[ZoneTransfer]\r\nZoneId=3\r\n")
            .await
            .map_err(|e| e.to_string())?;
    }
    if cancel.is_cancelled() {
        return Err("Download cancelled".into());
    }
    publish(&partial.0, path).map_err(|e| format!("Could not publish the downloaded file: {e}"))?;
    Ok(count)
}

#[cfg(target_os = "linux")]
fn publish(part: &Path, dest: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let part = CString::new(part.as_os_str().as_bytes())?;
    let dest = CString::new(dest.as_os_str().as_bytes())?;
    // SAFETY: the two NUL-terminated paths live for the entire call.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            part.as_ptr(),
            libc::AT_FDCWD,
            dest.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn publish(part: &Path, dest: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |p: &Path| {
        p.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let part = wide(part);
    let dest = wide(dest);
    // SAFETY: both paths are NUL terminated. No replace flag means an existing file wins.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(part.as_ptr(), dest.as_ptr(), 0)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn publish(part: &Path, dest: &Path) -> io::Result<()> {
    std::fs::hard_link(part, dest)
}

#[cfg(windows)]
pub(crate) fn destination(
    window: &winit::window::Window,
    hint: &str,
) -> Result<Option<PathBuf>, String> {
    use std::{
        ffi::OsString,
        os::windows::ffi::{OsStrExt, OsStringExt},
    };
    use windows_sys::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, GetSaveFileNameW, OFN_DONTADDTORECENT, OFN_EXPLORER, OFN_NOCHANGEDIR,
        OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::Win32(owner) = window.window_handle().map_err(|e| e.to_string())?.as_raw()
    else {
        return Err("Missing native window".into());
    };
    let mut buffer = vec![0u16; 32768];
    for (out, ch) in buffer
        .iter_mut()
        .zip(std::ffi::OsStr::new(hint).encode_wide())
    {
        *out = ch;
    }
    let mut title: Vec<_> = "Save download".encode_utf16().chain(Some(0)).collect();
    loop {
        // SAFETY: all pointers borrow live local buffers for the modal call.
        let mut d: OPENFILENAMEW = unsafe { std::mem::zeroed() };
        d.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        d.hwndOwner = owner.hwnd.get() as _;
        d.lpstrTitle = title.as_ptr();
        d.lpstrFile = buffer.as_mut_ptr();
        d.nMaxFile = buffer.len() as u32;
        d.Flags = OFN_EXPLORER | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST | OFN_DONTADDTORECENT;
        if unsafe { GetSaveFileNameW(&mut d) } == 0 {
            let error = unsafe { CommDlgExtendedError() };
            return if error == 0 {
                Ok(None)
            } else {
                Err(format!("Save dialog failed ({error:#x})"))
            };
        }
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        let path = PathBuf::from(OsString::from_wide(&buffer[..end]));
        if !path.try_exists().map_err(|e| e.to_string())? {
            return Ok(Some(path));
        }
        title = "File already exists. Choose a new download name"
            .encode_utf16()
            .chain(Some(0))
            .collect();
    }
}

#[cfg(not(windows))]
pub(crate) fn destination(
    _: &winit::window::Window,
    hint: &str,
) -> Result<Option<PathBuf>, String> {
    let dir = std::env::var_os("XDG_DOWNLOAD_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads")))
        .ok_or("No downloads directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let root = dir.join(hint);
    let mut candidate = root.clone();
    for n in 1u64.. {
        if !candidate.try_exists().map_err(|e| e.to_string())? {
            return Ok(Some(candidate));
        }
        let stem = root
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("download");
        let ext = root.extension().and_then(|s| s.to_str());
        candidate = dir.join(match ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        });
    }
    unreachable!()
}
#[cfg(test)]
mod tests {
    use super::*;
    use rust_browser_engine::{Loader, loader::NavigationRequest};
    use std::{
        fs as disk,
        io::{BufRead, BufReader, Read, Write},
        net::{TcpListener, TcpStream},
        thread,
        time::{Duration, Instant},
    };
    use url::Url;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            loop {
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir()
                    .join(format!("satin-download-test-{}-{id}", std::process::id()));
                match disk::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("scratch directory: {e}"),
                }
            }
        }
        fn files(&self) -> Vec<String> {
            let mut names: Vec<_> = disk::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = disk::remove_dir_all(&self.0);
        }
    }

    fn fixture(
        handler: impl FnOnce(&mut TcpStream) + Send + 'static,
    ) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!(
            "http://{}/payload",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(20)))
                .unwrap();
            let mut head = String::new();
            let mut reader = BufReader::new(&mut stream);
            while !head.ends_with("\r\n\r\n") {
                assert_ne!(reader.read_line(&mut head).unwrap(), 0);
            }
            assert!(head.starts_with("GET /payload "));
            handler(&mut stream);
        });
        (url, server)
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[test]
    fn saves_complete_file_larger_than_api_limit_without_publishing_partial_contents() {
        const SIZE: usize = 65 * 1024 * 1024 + 17;
        const BLOCK: usize = 32 * 1024;
        let (url, server) = fixture(|s| {
            write!(s,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {SIZE}\r\nConnection: close\r\n\r\n").unwrap();
            let mut block = [0u8; BLOCK];
            for (n, start) in (0..SIZE).step_by(BLOCK).enumerate() {
                block.fill((n % 251) as u8);
                s.write_all(&block[..(SIZE - start).min(BLOCK)]).unwrap();
            }
        });
        let dir = Directory::new();
        let loader = Loader::new().unwrap();
        let request = NavigationRequest::from_document(url.clone(), url);
        let signal = CancelSignal::new();
        loader.transport_handle().block_on(async {
            let response = loader
                .open_download_link(&request, "../CON.txt", &signal, deadline())
                .await
                .unwrap();
            assert_eq!(response.suggested_name, "_CON.txt");
            let dest = dir.0.join(&response.suggested_name);
            let mut last = 0;
            let bytes = save(response, &dest, &signal, |now| {
                assert!(now >= last);
                assert!(!dest.exists());
                last = now;
            })
            .await
            .unwrap();
            assert_eq!(last, SIZE as u64);
            assert_eq!(bytes, last);
            assert_eq!(dir.files(), ["_CON.txt"]);
            let mut file = disk::File::open(&dest).unwrap();
            assert_eq!(file.metadata().unwrap().len(), bytes);
            let mut block = [0u8; BLOCK];
            for (n, start) in (0..SIZE).step_by(BLOCK).enumerate() {
                let data = &mut block[..(SIZE - start).min(BLOCK)];
                file.read_exact(data).unwrap();
                assert!(data.iter().all(|byte| *byte == (n % 251) as u8));
            }
        });
        server.join().unwrap();
    }

    #[test]
    fn cancellation_and_interrupted_response_remove_partial_files() {
        for cancel in [true, false] {
            let (url, server) = fixture(|s| {
                s.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\nhalf",
                )
                .unwrap();
                s.flush().unwrap();
                thread::sleep(Duration::from_millis(80));
            });
            let dir = Directory::new();
            let loader = Loader::new().unwrap();
            let request = NavigationRequest::from_document(url.clone(), url);
            let signal = CancelSignal::new();
            loader.transport_handle().block_on(async {
                let response = loader
                    .open_download_link(&request, "out", &signal, deadline())
                    .await
                    .unwrap();
                let result = save(response, &dir.0.join("out"), &signal, |_| {
                    if cancel {
                        signal.cancel();
                    }
                })
                .await;
                let error = result.unwrap_err();
                if cancel {
                    assert!(error.contains("cancelled"), "{error}");
                }
                assert!(dir.files().is_empty());
            });
            server.join().unwrap();
        }
    }

    #[test]
    fn existing_destination_wins_a_race_with_the_finishing_download() {
        let (url, server) = fixture(|s| {
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nfile")
                .unwrap();
        });
        let dir = Directory::new();
        let dest = dir.0.join("saved");
        let loader = Loader::new().unwrap();
        let request = NavigationRequest::from_document(url.clone(), url);
        let signal = CancelSignal::new();
        loader.transport_handle().block_on(async {
            let response = loader
                .open_download_link(&request, "saved", &signal, deadline())
                .await
                .unwrap();
            let mut competitor = None;
            let result = save(response, &dest, &signal, |_| {
                if competitor.is_none() {
                    let mut file = disk::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(&dest)
                        .unwrap();
                    file.write_all(b"existing").unwrap();
                    competitor = Some(file);
                }
            })
            .await;
            drop(competitor);
            assert!(result.is_err());
            assert_eq!(disk::read(&dest).unwrap(), b"existing");
            assert_eq!(dir.files(), ["saved"]);
        });
        server.join().unwrap();
    }

    #[test]
    fn redirect_to_another_origin_requires_an_attachment_and_uses_its_name() {
        for attachment in [false, true] {
            let (target, target_server) = fixture(move |s| {
                let header = if attachment {
                    "Content-Disposition: attachment; filename*=UTF-8''logs%2Freport%20final.txt\r\n"
                } else {
                    ""
                };
                write!(
                    s,
                    "HTTP/1.1 200 OK\r\n{header}Content-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            });
            let (first, redirect_server) = fixture(move |s| {
                write!(s,"HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            });
            let loader = Loader::new().unwrap();
            let request = NavigationRequest::from_document(first.clone(), first);
            let signal = CancelSignal::new();
            let outcome = loader
                .transport_handle()
                .block_on(loader.open_download_link(&request, "local.txt", &signal, deadline()));
            if attachment {
                assert_eq!(outcome.unwrap().suggested_name, "report final.txt");
            } else {
                assert!(outcome.err().unwrap().contains("cross-origin"));
            }
            redirect_server.join().unwrap();
            target_server.join().unwrap();
        }
    }
}
