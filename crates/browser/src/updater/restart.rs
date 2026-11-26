//! Trial-launch a verified build while the legacy launcher still sees the prior manifest.
use super::{
    Installation, Manifest, Publication, hex, install_root, installed_target, valid_manifest,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::{Pipes::PeekNamedPipe, Threading::WaitForSingleObject},
};

const TICKET_ENV: &str = "SATIN_UPDATE_HANDOFF_V2";
const START_TIMEOUT: Duration = Duration::from_secs(25);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);
const TICKET_LIMIT: usize = 2048;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ticket {
    nonce: String,
    manifest: Manifest,
}

impl Ticket {
    fn receipt(&self) -> String {
        format!("Satin first frame v2 {}\n", self.nonce)
    }
}

pub(crate) struct StartupSignal(String);

impl StartupSignal {
    pub(crate) fn from_parent() -> Result<Option<Self>, String> {
        let Some(ticket) = std::env::var_os(TICKET_ENV) else {
            return Ok(None);
        };
        let json = ticket
            .to_str()
            .filter(|text| text.len() <= TICKET_LIMIT)
            .ok_or("Invalid update handoff size or encoding")?;
        let root = install_root().ok_or("Cannot locate the installation for the update handoff")?;
        let running = std::env::current_exe()
            .and_then(|exe| exe.canonicalize())
            .map_err(|error| error.to_string())?;
        let receipt = validate_ticket(json, &root, &running, env!("CARGO_PKG_VERSION"))?;
        // On Windows, the environment may be changed safely even with other
        // threads. Do not pass the one-shot handoff to any later subprocess.
        unsafe { std::env::remove_var(TICKET_ENV) };
        Ok(Some(Self(receipt)))
    }

    pub(crate) fn ready(self) {
        let mut pipe = io::stdout().lock();
        if let Err(error) = pipe
            .write_all(self.0.as_bytes())
            .and_then(|()| pipe.flush())
        {
            eprintln!("Satin updater: Could not acknowledge the first browser frame: {error}");
        }
    }
}

pub(super) fn validate_ticket(
    json: &str,
    root: &Path,
    running: &Path,
    version: &str,
) -> Result<String, String> {
    if json.len() > TICKET_LIMIT {
        return Err("Update handoff exceeded its size limit".into());
    }
    let ticket = serde_json::from_str::<Ticket>(json).map_err(|_| "Invalid update handoff")?;
    // The trial is intentionally not in current.json. Match the bounded ticket
    // against the compiled version and exact canonical versioned executable.
    // The parent pins that file and owns the only read end of the pipe, so an
    // environment ticket alone cannot confirm startup.
    if ticket.nonce.len() != 32
        || !ticket.nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !valid_manifest(&ticket.manifest)
        || ticket.manifest.version != version
        || installed_target(root, &ticket.manifest).as_deref() != Some(running)
    {
        return Err("This browser does not match the update handoff".into());
    }
    Ok(ticket.receipt())
}

pub(super) struct VerifiedBuild {
    target: PathBuf,
    manifest: Manifest,
    // Deny writes/delete from hash verification through the supervised launch.
    _file: File,
}

impl VerifiedBuild {
    pub(super) fn new(target: PathBuf, manifest: Manifest, file: File) -> Self {
        Self {
            target,
            manifest,
            _file: file,
        }
    }
}

struct Trial {
    child: Child,
    receipt: String,
}

impl Trial {
    fn spawn(install: &Installation, build: &VerifiedBuild) -> Result<Self, String> {
        if installed_target(&install.root, &build.manifest).as_ref() != Some(&build.target) {
            return Err(
                "The verified executable is no longer at its expected installation path".into(),
            );
        }
        let mut nonce = [0_u8; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "Could not create startup nonce")?;
        let ticket = Ticket {
            nonce: hex(&nonce),
            manifest: build.manifest.clone(),
        };
        let json = serde_json::to_string(&ticket).map_err(|error| error.to_string())?;
        if json.len() > TICKET_LIMIT {
            return Err("Update handoff exceeded its size limit".into());
        }
        // Legacy launchers detach; start the exact verified child with the
        // same profile/cwd convention. Only its process gets the write end of
        // this fresh pipe. No named, same-user-signallable event is used.
        let child = Command::new(&build.target)
            .current_dir(&install.root)
            .arg("--profile")
            .arg(&install.profile)
            .env(TICKET_ENV, json)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not start Satin: {error}"))?;
        Ok(Self {
            child,
            receipt: ticket.receipt(),
        })
    }

    fn still_running(&mut self) -> Result<(), String> {
        match self.child.try_wait().map_err(|error| error.to_string())? {
            Some(status) => Err(format!("Satin exited during its update trial ({status})")),
            None => Ok(()),
        }
    }

    fn wait_for_frame(&mut self, timeout: Duration) -> Result<(), String> {
        let mut pipe = self
            .child
            .stdout
            .take()
            .ok_or("Missing private startup pipe")?;
        let deadline = Instant::now() + timeout;
        loop {
            self.still_running()?;
            let mut available = 0_u32;
            // No other thread reads this pipe. Peeking with no output buffer
            // cannot consume data; read only when a whole frame is buffered.
            let peeked = unsafe {
                PeekNamedPipe(
                    pipe.as_raw_handle(),
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    &mut available,
                    ptr::null_mut(),
                )
            };
            if peeked == 0 {
                return Err(format!(
                    "The startup pipe closed before acknowledgment: {}",
                    io::Error::last_os_error()
                ));
            }
            let available = available as usize;
            if available >= self.receipt.len() {
                if available != self.receipt.len() {
                    return Err("Invalid update startup acknowledgment length".into());
                }
                let mut received = vec![0; self.receipt.len()];
                pipe.read_exact(&mut received)
                    .map_err(|error| error.to_string())?;
                if received != self.receipt.as_bytes() {
                    return Err("Invalid update startup acknowledgment".into());
                }
                return self.still_running();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Satin did not acknowledge its first window in time".into());
            }
            std::thread::sleep(POLL.min(remaining));
        }
    }

    fn stop(&mut self) -> bool {
        let _ = self.child.kill();
        // Borrow the exact child's still-owned process handle; never search
        // for or terminate other Satin processes by binary name or PID.
        unsafe {
            WaitForSingleObject(self.child.as_raw_handle(), STOP_TIMEOUT.as_millis() as u32)
                == WAIT_OBJECT_0
        }
    }
}

pub(crate) struct Restart {
    _lock: File,
    install: Installation,
    next: VerifiedBuild,
    prior: VerifiedBuild,
    publication: Publication,
}

impl Restart {
    pub(super) fn new(
        lock: File,
        install: Installation,
        next: VerifiedBuild,
        prior: VerifiedBuild,
        publication: Publication,
    ) -> Self {
        Self {
            _lock: lock,
            install,
            next,
            prior,
            publication,
        }
    }

    /// The caller must have dropped the App (including its Profile) first.
    pub(crate) fn launch(self) -> Result<(), String> {
        self.launch_with_timeout(START_TIMEOUT)
    }

    pub(super) fn launch_with_timeout(self, timeout: Duration) -> Result<(), String> {
        if !self.publication.prior_is_current(&self.install.root)? {
            return Err(
                "The active installation changed before the trial; it was left untouched".into(),
            );
        }
        let saved_session = match crate::profile::snapshot_update_session(&self.install.profile) {
            Ok(session) => session,
            Err(error) => {
                return self.recover(&format!("Could not preserve the saved session: {error}"));
            }
        };
        let mut trial = match Trial::spawn(&self.install, &self.next) {
            Ok(trial) => trial,
            Err(error) => return self.recover(&error),
        };
        if let Err(reason) = trial
            .wait_for_frame(timeout)
            .and_then(|()| self.publication.commit(&self.install.root))
        {
            if !trial.stop() {
                return Err(format!(
                    "{reason}; the trial could not be confirmed stopped. The updater did not reopen another browser."
                ));
            }
            return self.recover_with_session(&reason, Some(&saved_session));
        }
        Ok(())
    }

    /// Use only after the old App and any attempted trial have released the profile.
    pub(crate) fn recover(self, reason: &str) -> Result<(), String> {
        self.recover_with_session(reason, None)
    }

    fn recover_with_session(self, reason: &str, session: Option<&[u8]>) -> Result<(), String> {
        let reopen = (|| {
            if !self.publication.prior_is_current(&self.install.root)? {
                return Err("The active installation changed; it was left untouched".into());
            }
            if let Some(session) = session {
                crate::profile::restore_update_session(&self.install.profile, session)
                    .map_err(|error| format!("Could not restore the saved tab session: {error}"))?;
            }
            let mut prior = Trial::spawn(&self.install, &self.prior)?;
            if let Err(error) = prior.wait_for_frame(START_TIMEOUT) {
                return Err(if prior.stop() {
                    error
                } else {
                    format!("{error}; could not confirm the recovery process stopped")
                });
            }
            Ok(())
        })();
        match reopen {
            Ok(()) => Err(format!(
                "The update was not applied: {reason}. Satin reopened the prior build."
            )),
            Err(error) => Err(format!(
                "The update was not applied: {reason}. Could not reopen the prior build: {error}"
            )),
        }
    }
}
