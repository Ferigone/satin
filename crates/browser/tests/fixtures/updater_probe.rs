//! Standalone offline smoke helper; compile directly, never package with Satin.
#![cfg_attr(windows, windows_subsystem = "windows")]
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let executable = std::env::current_exe().unwrap();
    let build = executable.parent().unwrap();
    if args.iter().any(|argument| argument == "--version") {
        // Identify the executable the unmodified legacy launcher selected.
        println!("{}", build.file_name().unwrap().to_str().unwrap());
        return;
    }
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
    fs::write(build.join("probe-started"), b"has profile").unwrap();
    let mode = fs::read_to_string(build.join("probe-mode")).ok();
    if matches!(
        mode.as_deref(),
        Some("exit" | "hang" | "wrong-ack" | "publish-fail")
    ) {
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
        Some("ack" | "wrong-ack" | "publish-fail") => {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !build.join("probe-release").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
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
    let handoff = std::env::var("SATIN_UPDATE_HANDOFF_V2").unwrap();
    let nonce = handoff
        .split('"')
        .nth(3)
        .expect("nonce is the first ticket field");
    assert_eq!(nonce.len(), 32);
    let nonce = if mode.as_deref() == Some("wrong-ack") {
        "00000000000000000000000000000000"
    } else {
        nonce
    };
    {
        let mut pipe = io::stdout().lock();
        writeln!(pipe, "Satin first frame v2 {nonce}").unwrap();
        pipe.flush().unwrap();
    }
    // A real browser remains running after its first presentation. Keep the
    // test process alive until the parent has checked that it got the ACK.
    let end = Instant::now() + Duration::from_secs(8);
    let stop = profile.join("updater-probe-exit");
    while !stop.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
}
