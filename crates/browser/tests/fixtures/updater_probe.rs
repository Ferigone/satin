//! Standalone offline smoke helper; compile directly, never package with Satin.
use std::{
    fs,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    path::PathBuf,
    time::{Duration, Instant},
};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> RawHandle;
    fn SetEvent(event: RawHandle) -> i32;
}

fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    assert_eq!(
        args.len(),
        2,
        "do not reissue startup URLs or smoke arguments"
    );
    assert_eq!(args[0], "--profile");
    let profile = PathBuf::from(&args[1]);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(profile.join("profile.lock"))
        .expect("launch used the same profile directory");
    lock.try_lock()
        .expect("the prior profile released its exclusive lock");
    let executable = std::env::current_exe().unwrap();
    let build = executable.parent().unwrap();
    fs::write(build.join("probe-started"), b"has profile").unwrap();
    let mode = fs::read_to_string(build.join("probe-mode")).ok();
    if matches!(mode.as_deref(), Some("exit" | "hang")) {
        // Model an unsupported/failed startup that already wrote a new session.
        fs::write(
            profile.join("session.json"),
            br#"{"schema":1,"tabs":["about:blank"],"active":0}"#,
        )
        .unwrap();
    }
    match mode.as_deref() {
        Some("exit") => return,
        Some("hang") => {
            std::thread::sleep(Duration::from_secs(10));
            return;
        }
        _ => {}
    }
    let pending = profile.join("updater-probe-receipt.pending");
    fs::copy(profile.join("session.json"), &pending).unwrap();
    fs::rename(pending, profile.join("updater-probe-receipt.json")).unwrap();
    fs::write(
        profile.join("updater-probe-build"),
        build.file_name().unwrap().to_str().unwrap(),
    )
    .unwrap();
    let nonce = std::env::var("SATIN_UPDATE_READY_V1").unwrap();
    let event = format!("Local\\Satin.UpdateReady.{nonce}\0")
        .encode_utf16()
        .collect::<Vec<_>>();
    // Borrow a terminated event name and take ownership of the returned handle.
    let handle = unsafe { OpenEventW(2, 0, event.as_ptr()) };
    assert!(!handle.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    assert_ne!(unsafe { SetEvent(handle.as_raw_handle()) }, 0);
    // A real browser remains running after its first presentation. Keep the
    // test process alive until the parent has checked that it got the ACK.
    let end = Instant::now() + Duration::from_secs(8);
    let stop = profile.join("updater-probe-exit");
    while !stop.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
}
