//! Standalone offline smoke helper; compile directly, never package with Satin.
use std::{fs, path::PathBuf};

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
    let pending = profile.join("updater-probe-receipt.pending");
    fs::copy(profile.join("session.json"), &pending).unwrap();
    fs::rename(pending, profile.join("updater-probe-receipt.json")).unwrap();
}
