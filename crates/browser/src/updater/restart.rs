//! Supervise the verified version through its first presented browser window.
use super::{Activation, Installation, hex, installed_running, installed_target, read_manifest};
use ring::rand::{SecureRandom, SystemRandom};
use std::{
    fs::File,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    process::{Child, Command},
    ptr,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, SetLastError, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{
        CreateEventW, EVENT_MODIFY_STATE, OpenEventW, SetEvent, WaitForMultipleObjects,
        WaitForSingleObject,
    },
};

const READY_ENV: &str = "SATIN_UPDATE_READY_V1";
const EVENT_PREFIX: &str = "Local\\Satin.UpdateReady.";
const START_TIMEOUT: Duration = Duration::from_secs(25);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

fn event_name(nonce: &str) -> Vec<u16> {
    EVENT_PREFIX
        .encode_utf16()
        .chain(nonce.encode_utf16())
        .chain([0])
        .collect()
}

pub(crate) struct StartupSignal(OwnedHandle);

impl StartupSignal {
    pub(crate) fn from_parent() -> Option<Self> {
        let nonce = std::env::var(READY_ENV).ok()?;
        if nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        installed_running()?;
        let name = event_name(&nonce);
        // The terminated name stays live for the call; this process only needs
        // permission to signal, and does not pass the handle to other children.
        let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
        if handle.is_null() {
            return None;
        }
        // Successful OpenEventW gives us an owned kernel handle.
        Some(Self(unsafe { OwnedHandle::from_raw_handle(handle) }))
    }

    pub(crate) fn ready(self) {
        // OwnedHandle keeps the event live while it is signalled.
        if unsafe { SetEvent(self.0.as_raw_handle()) } == 0 {
            eprintln!(
                "Satin updater: Could not signal startup: {}",
                io::Error::last_os_error()
            );
        }
    }
}

pub(super) struct ParentSignal {
    nonce: String,
    handle: OwnedHandle,
}

impl ParentSignal {
    pub(super) fn new() -> Result<Self, String> {
        let mut nonce = [0; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "Could not create startup nonce")?;
        let nonce = hex(&nonce);
        let name = event_name(&nonce);
        // Null security attributes use the caller's default security descriptor
        // and make the event non-inheritable. The name is NUL-terminated.
        let handle = unsafe {
            SetLastError(0);
            CreateEventW(ptr::null(), 1, 0, name.as_ptr())
        };
        let last_error = io::Error::last_os_error();
        if handle.is_null() {
            return Err(format!(
                "Could not create update startup event: {last_error}"
            ));
        }
        // Successful CreateEventW gives us an owned kernel handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        if last_error.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
            return Err("Update startup nonce already in use".into());
        }
        Ok(Self { nonce, handle })
    }

    fn wait(&self, child: &mut Child, timeout: Duration) -> Result<(), String> {
        // If both are signaled, prefer the child exit over the event.
        let handles = [child.as_raw_handle(), self.handle.as_raw_handle()];
        // Both live handles are borrowed for the duration of this bounded wait.
        let result = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, millis(timeout)) };
        if result == WAIT_OBJECT_0 + 1
            && child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_none()
        {
            return Ok(());
        }
        match result {
            WAIT_TIMEOUT => Err("Satin did not acknowledge its first window in time".into()),
            WAIT_FAILED => Err(format!(
                "Could not verify Satin startup: {}",
                io::Error::last_os_error()
            )),
            _ => {
                let status = child.try_wait().map_err(|error| error.to_string())?;
                Err(
                    status.map_or("Satin did not confirm startup".into(), |status| {
                        format!("Satin exited before confirming startup ({status})")
                    }),
                )
            }
        }
    }
}

fn millis(timeout: Duration) -> u32 {
    // INFINITE (u32::MAX) is not a valid timeout for a restart.
    timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32
}

struct StartFailure {
    reason: String,
    stopped: bool,
}

fn verified_start(
    install: &Installation,
    target: &Path,
    signal: &ParentSignal,
    timeout: Duration,
) -> Result<(), StartFailure> {
    // Legacy installed launchers detach immediately and don't return the browser
    // process handle. Use their same working directory/profile convention, but
    // supervise the verified installation target ourselves on an update handoff.
    let mut child = Command::new(target)
        .current_dir(&install.root)
        .arg("--profile")
        .arg(&install.profile)
        .env(READY_ENV, &signal.nonce)
        .spawn()
        .map_err(|error| StartFailure {
            reason: format!("Could not start Satin: {error}"),
            stopped: true,
        })?;
    if let Err(reason) = signal.wait(&mut child, timeout) {
        let _ = child.kill();
        // The process handle is owned by `child`. Do not try to open the profile
        // in a replacement process until Windows confirms this process is gone.
        let stopped = unsafe { WaitForSingleObject(child.as_raw_handle(), millis(STOP_TIMEOUT)) }
            == WAIT_OBJECT_0;
        return Err(StartFailure { reason, stopped });
    }
    Ok(())
}

pub(crate) struct Restart {
    _lock: File,
    _verified: File,
    target: PathBuf,
    install: Installation,
    activation: Option<Activation>,
    signal: ParentSignal,
}

impl Restart {
    pub(super) fn new(
        lock: File,
        verified: File,
        target: PathBuf,
        install: Installation,
        activation: Activation,
        signal: ParentSignal,
    ) -> Self {
        Self {
            _lock: lock,
            _verified: verified,
            target,
            install,
            activation: Some(activation),
            signal,
        }
    }

    /// The caller must have dropped the App (including its Profile) before this call.
    pub(crate) fn launch(self) -> Result<(), String> {
        self.launch_with_timeout(START_TIMEOUT)
    }

    pub(super) fn launch_with_timeout(mut self, timeout: Duration) -> Result<(), String> {
        let saved_session = match crate::profile::snapshot_update_session(&self.install.profile) {
            Ok(session) => session,
            Err(error) => {
                return self.recover(&format!("Could not preserve the saved session: {error}"));
            }
        };
        match verified_start(&self.install, &self.target, &self.signal, timeout) {
            Ok(()) => {
                self.activation.take();
                Ok(())
            }
            Err(failure) if failure.stopped => {
                self.recover_with_session(&failure.reason, Some(&saved_session))
            }
            Err(failure) => {
                // If the candidate may still be running, restoration/reopen
                // could contend for its profile. Report and leave it alone.
                self.activation.take();
                Err(format!(
                    "{}; the new process could not be confirmed stopped. Close it before recovering the installation.",
                    failure.reason
                ))
            }
        }
    }

    /// Recover only after the old App and any attempted child no longer hold the profile.
    pub(crate) fn recover(self, reason: &str) -> Result<(), String> {
        self.recover_with_session(reason, None)
    }

    fn recover_with_session(mut self, reason: &str, session: Option<&[u8]>) -> Result<(), String> {
        let activation = self.activation.take().expect("pending startup");
        activation.restore(&self.install.root)
            .map_err(|error| format!("The update did not start: {reason}. Could not restore the prior installation: {error}"))?;
        let reopen = (|| {
            if let Some(session) = session {
                crate::profile::restore_update_session(&self.install.profile, session)
                    .map_err(|error| format!("Could not restore the saved tab session: {error}"))?;
            }
            let (manifest, _) = read_manifest(&self.install.root.join("current.json"))
                .ok_or("The restored manifest could not be read")?;
            let target = installed_target(&self.install.root, &manifest)
                .filter(|path| path == &self.install.running)
                .ok_or("The restored executable no longer matches the previous build")?;
            let signal = ParentSignal::new()?;
            verified_start(&self.install, &target, &signal, START_TIMEOUT)
                .map_err(|error| error.reason)
        })();
        match reopen {
            Ok(()) => Err(format!(
                "The update did not start: {reason}. Satin restored and reopened the prior build."
            )),
            Err(error) => Err(format!(
                "The update did not start: {reason}. Satin restored the prior build but could not reopen it: {error}"
            )),
        }
    }
}

impl Drop for Restart {
    fn drop(&mut self) {
        // The worker may publish immediately before a window/event-loop error
        // tears down its receiver. No child was spawned, so undo that activation
        // under the lock; never try to reopen while the old App may still exist.
        if let Some(activation) = self.activation.take()
            && let Err(error) = activation.restore(&self.install.root)
        {
            eprintln!("Satin updater: Could not undo abandoned activation: {error}");
        }
    }
}
