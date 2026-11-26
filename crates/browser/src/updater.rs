//! Interactive updates for the per-user Windows installation.
#![cfg(all(windows, target_arch = "x86_64"))]

use reqwest::{Client, RequestBuilder, Response, StatusCode, Url, header};
use ring::digest::{Context, SHA256};
use rustls_platform_verifier::BuilderVerifierExt;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_READ};

const RELEASES: &str = "https://api.github.com/repos/Ferigone/satin/releases";
const ASSET_NAME: &str = "satin-windows-x64.exe";
const BINARY_NAME: &str = "rust-browser.exe";
const LAUNCHER_NAME: &str = "RustBrowser.exe";
const INSTALL_PRODUCT: &str = "RustBrowser";
const RELEASE_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(8)];
const RELEASES_PER_PAGE: usize = 50;
const RELEASE_PAGE_LIMIT: usize = 10;
const METADATA_LIMIT: usize = 4 * 1024 * 1024;
const MANIFEST_LIMIT: u64 = 16 * 1024;
const EXECUTABLE_LIMIT: u64 = 128 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    product: String,
    version: String,
    build_id: String,
    executable: String,
    sha256: String,
    profile_schema: u32,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
    browser_download_url: String,
}

struct Candidate {
    version: Version,
    size: u64,
    hash: [u8; 32],
    url: Url,
}

#[derive(Clone)]
struct Installation {
    root: PathBuf,
    running: PathBuf,
    profile: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateAction {
    Check,
    Download,
    Restart,
}

enum Phase {
    Current,
    Checking,
    Available(Arc<Candidate>),
    Downloading(Arc<Candidate>),
    Ready(Arc<Candidate>),
    Applying(Arc<Candidate>),
    Failed {
        retry: UpdateAction,
        candidate: Option<Arc<Candidate>>,
    },
    Restarting,
}

enum Outcome {
    Discovered(Option<Candidate>),
    Staged,
    Applied(Restart),
}

struct Job {
    result: Receiver<Result<Outcome, String>>,
    bytes: Arc<AtomicU64>,
    shown: u64,
    cancel: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for Job {
    fn drop(&mut self) {
        // On ordinary window close, unwind a waiting transfer so its pending
        // file is removed before the process exits. Completed jobs join quickly.
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Reply {
    send: Option<Sender<Result<Outcome, String>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Drop for Reply {
    fn drop(&mut self) {
        // Disconnect first, then wake (also on worker panic).
        drop(self.send.take());
        (self.wake)();
    }
}

pub(crate) struct Updates {
    install: Installation,
    wake: Arc<dyn Fn() + Send + Sync>,
    phase: Phase,
    job: Option<Job>,
}

pub(crate) struct View {
    pub(crate) title: &'static str,
    pub(crate) detail: String,
    pub(crate) button: Option<(UpdateAction, &'static str)>,
    pub(crate) progress: Option<f32>,
}

pub(crate) struct Restart {
    _lock: File,
    _verified: File,
    launcher: PathBuf,
    install: Installation,
}

impl Restart {
    /// The caller must have dropped the App (including its Profile) before this call.
    pub(crate) fn launch(self) -> Result<(), String> {
        // An existing installed launcher works unchanged. Wait for it to spawn the
        // browser before releasing update.lock so installer/rollback cannot change
        // current.json between publication and the launcher's manifest read.
        let status = Command::new(&self.launcher)
            .current_dir(&self.install.root)
            .arg("--profile")
            .arg(&self.install.profile)
            .status()
            .map_err(|error| format!("Could not reopen Satin: {error}"))?;
        if !status.success() {
            return Err(format!("Could not reopen Satin: {status}"));
        }
        Ok(())
    }
}

impl Updates {
    pub(crate) fn start(profile: &Path, wake: Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        let root = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join(INSTALL_PRODUCT);
        let running = std::env::current_exe().ok()?.canonicalize().ok()?;
        let root = plain_directory(&root)?;
        active_manifest(&root, &running)?;
        launcher(&root)?;
        let profile = profile.canonicalize().ok()?;
        let mut updates = Self {
            install: Installation {
                root,
                running,
                profile,
            },
            wake,
            phase: Phase::Current,
            job: None,
        };
        updates.start_job(UpdateAction::Check);
        Some(updates)
    }

    pub(crate) fn view(&self, panel_open: bool) -> Option<View> {
        let mut view = View {
            title: "",
            detail: String::new(),
            button: None,
            progress: None,
        };
        match &self.phase {
            Phase::Current if !panel_open => return None,
            Phase::Current => view.title = "Satin is up to date",
            Phase::Checking if !panel_open => return None,
            Phase::Checking => view.title = "Checking for updates…",
            Phase::Available(candidate) => {
                view.title = "Satin update available";
                view.detail = format!("Version {}", candidate.version);
                view.button = Some((UpdateAction::Download, "Download update"));
            }
            Phase::Downloading(candidate) => {
                let bytes = self.job.as_ref().map_or(0, |job| job.bytes.load(Relaxed));
                let percent = bytes * 100 / candidate.size;
                if bytes == candidate.size {
                    view.title = "Verifying update…";
                } else {
                    view.title = "Downloading update…";
                }
                view.detail = format!("{} / {} MiB · {percent}%", mib(bytes), mib(candidate.size));
                view.progress = Some(percent as f32 / 100.);
            }
            Phase::Ready(candidate) => {
                view.title = "Update ready to install";
                view.detail = format!("Restart Satin to use version {}.", candidate.version);
                view.button = Some((UpdateAction::Restart, "Restart and update"));
            }
            Phase::Applying(_) | Phase::Restarting => {
                view.title = "Preparing to restart…";
                view.detail = "Keep this window open.".into();
            }
            Phase::Failed { retry, .. } => {
                (view.title, view.detail, view.button) = match retry {
                    UpdateAction::Check => (
                        "Could not check for updates",
                        "Satin can still be used.".into(),
                        Some((UpdateAction::Check, "Retry check")),
                    ),
                    UpdateAction::Download => (
                        "Could not prepare the update",
                        "The running version was not changed.".into(),
                        Some((UpdateAction::Download, "Retry download")),
                    ),
                    UpdateAction::Restart => (
                        "Could not install the update",
                        "You can keep browsing and try again.".into(),
                        Some((UpdateAction::Restart, "Retry restart")),
                    ),
                };
            }
        }
        Some(view)
    }

    pub(crate) fn applying(&self) -> bool {
        matches!(self.phase, Phase::Applying(_) | Phase::Restarting)
    }

    pub(crate) fn can_start(&self, action: UpdateAction) -> bool {
        self.job.is_none()
            && match &self.phase {
                Phase::Current => action == UpdateAction::Check,
                Phase::Available(_) => action == UpdateAction::Download,
                Phase::Ready(_) => action == UpdateAction::Restart,
                Phase::Failed { retry, .. } => action == *retry,
                _ => false,
            }
    }

    fn offered(&self) -> Option<Arc<Candidate>> {
        match &self.phase {
            Phase::Available(candidate)
            | Phase::Downloading(candidate)
            | Phase::Ready(candidate)
            | Phase::Applying(candidate) => Some(candidate.clone()),
            Phase::Failed { candidate, .. } => candidate.clone(),
            _ => None,
        }
    }

    pub(crate) fn report_failure(&mut self, retry: UpdateAction, error: &str) {
        eprintln!("Satin updater: {error}");
        self.phase = Phase::Failed {
            retry,
            candidate: self.offered(),
        };
    }

    pub(crate) fn start_job(&mut self, action: UpdateAction) {
        if !self.can_start(action) {
            return;
        }
        let candidate = self.offered();
        let install = self.install.clone();
        let wake = self.wake.clone();
        let bytes = Arc::new(AtomicU64::new(0));
        let worker_bytes = bytes.clone();
        let (send, result) = mpsc::channel();
        let (cancel, cancelled) = oneshot::channel();
        let worker = std::thread::Builder::new()
            .name("satin-updater".into())
            .spawn(move || {
                let reply = Reply {
                    send: Some(send),
                    wake: wake.clone(),
                };
                let result = run_worker(
                    install,
                    action,
                    candidate.as_deref(),
                    &wake,
                    &worker_bytes,
                    cancelled,
                );
                let _ = reply
                    .send
                    .as_ref()
                    .expect("worker reply sender")
                    .send(result);
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                self.report_failure(action, &error.to_string());
                return;
            }
        };
        // Each operation owns a distinct result channel. Progress and completion
        // from a previous operation cannot change any later operation's state.
        self.job = Some(Job {
            result,
            bytes,
            shown: 0,
            cancel: Some(cancel),
            worker: Some(worker),
        });
        self.phase = match action {
            UpdateAction::Check => Phase::Checking,
            UpdateAction::Download => Phase::Downloading(self.offered().expect("offered update")),
            UpdateAction::Restart => Phase::Applying(self.offered().expect("staged update")),
        };
    }

    pub(crate) fn poll(&mut self) -> (bool, Option<Restart>) {
        let Some(job) = self.job.as_mut() else {
            return (false, None);
        };
        let result = match job.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                if let Phase::Downloading(candidate) = &self.phase {
                    let percent = job.bytes.load(Relaxed) * 100 / candidate.size;
                    if percent != job.shown {
                        job.shown = percent;
                        return (true, None);
                    }
                }
                return (false, None);
            }
            Err(TryRecvError::Disconnected) => Err("Update worker closed unexpectedly".into()),
        };
        self.job = None;
        match (&self.phase, result) {
            (Phase::Checking, Ok(Outcome::Discovered(candidate))) => {
                self.phase = candidate.map_or(Phase::Current, |candidate| {
                    Phase::Available(Arc::new(candidate))
                });
            }
            (Phase::Downloading(candidate), Ok(Outcome::Staged)) => {
                self.phase = Phase::Ready(candidate.clone());
            }
            (Phase::Applying(_), Ok(Outcome::Applied(restart))) => {
                self.phase = Phase::Restarting;
                return (true, Some(restart));
            }
            (phase, result) => {
                let retry = match phase {
                    Phase::Checking => UpdateAction::Check,
                    // Recheck/redownload the staged file if it disappeared or changed.
                    Phase::Downloading(_) | Phase::Applying(_) => UpdateAction::Download,
                    _ => unreachable!("only busy states have a worker"),
                };
                let error = result
                    .err()
                    .unwrap_or_else(|| "Unexpected updater result".into());
                self.report_failure(retry, &error);
            }
        }
        (true, None)
    }
}

fn mib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_048_576.)
}

fn run_worker(
    install: Installation,
    action: UpdateAction,
    candidate: Option<&Candidate>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    bytes: &AtomicU64,
    cancelled: oneshot::Receiver<()>,
) -> Result<Outcome, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let operation = async {
        match action {
            UpdateAction::Check => {
                let (current, _) = active_manifest(&install.root, &install.running)
                    .ok_or("The active installation changed")?;
                discover(&client()?, &install.root, &current.version)
                    .await
                    .map(Outcome::Discovered)
            }
            UpdateAction::Download => {
                let candidate = candidate.expect("offered update");
                let mut shown = 0;
                stage(
                    &client()?,
                    &install.root,
                    &install.running,
                    candidate,
                    |received| {
                        bytes.store(received, Relaxed);
                        let percent = received * 100 / candidate.size;
                        if percent > shown {
                            shown = percent;
                            wake();
                        }
                    },
                )
                .await?;
                Ok(Outcome::Staged)
            }
            UpdateAction::Restart => apply(install, candidate.expect("staged update")).await,
        }
    };
    runtime.block_on(async {
        tokio::select! {
            biased;
            _ = cancelled => Err("Updater cancelled".into()),
            result = operation => result,
        }
    })
}

fn client() -> Result<Client, String> {
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| error.to_string())?
    .with_platform_verifier()
    .map_err(|error| error.to_string())?
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Client::builder()
        .use_preconfigured_tls(tls)
        .user_agent(concat!("Satin-updater/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(70))
        .build()
        .map_err(http_error)
}

fn http_error(error: reqwest::Error) -> String {
    error.without_url().to_string()
}

fn active_manifest(root: &Path, running: &Path) -> Option<(Manifest, Vec<u8>)> {
    let (manifest, bytes) = read_manifest(&root.join("current.json"))?;
    (manifest.version == env!("CARGO_PKG_VERSION") && installed_target(root, &manifest)? == running)
        .then_some((manifest, bytes))
}

fn read_manifest(path: &Path) -> Option<(Manifest, Vec<u8>)> {
    let bytes = read_manifest_bytes(path).ok()??;
    let manifest: Manifest = serde_json::from_slice(&bytes).ok()?;
    if manifest.product != INSTALL_PRODUCT
        || manifest.profile_schema != 1
        || parse_hash(&manifest.sha256).is_none()
        || !valid_build_id(&manifest.build_id)
        || manifest.executable != executable_path(&manifest.build_id)
    {
        return None;
    }
    Some((manifest, bytes))
}

fn plain_directory(path: &Path) -> Option<PathBuf> {
    let meta = path.symlink_metadata().ok()?;
    (meta.is_dir() && meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0)
        .then(|| path.canonicalize().ok())
        .flatten()
}

fn read_manifest_bytes(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match path.symlink_metadata() {
        Ok(meta)
            if meta.is_file() && meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 => {}
        Ok(_) => return Err("The installation manifest is not a regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(MANIFEST_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err("The installation manifest exceeds its size limit".into());
    }
    Ok(Some(bytes))
}

fn plain_file(path: &Path) -> Option<PathBuf> {
    let meta = path.symlink_metadata().ok()?;
    (meta.is_file() && meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0)
        .then(|| path.canonicalize().ok())
        .flatten()
}

fn child_of(path: PathBuf, parent: &Path, name: &str) -> Option<PathBuf> {
    (path.parent() == Some(parent)
        && path
            .file_name()
            .is_some_and(|file| file.eq_ignore_ascii_case(std::ffi::OsStr::new(name))))
    .then_some(path)
}

fn versions(root: &Path) -> Option<PathBuf> {
    let root = plain_directory(root)?;
    child_of(plain_directory(&root.join("versions"))?, &root, "versions")
}

fn version_directory(versions: &Path, build_id: &str) -> Option<PathBuf> {
    if !valid_build_id(build_id) {
        return None;
    }
    child_of(
        plain_directory(&versions.join(build_id))?,
        versions,
        build_id,
    )
}

fn installed_target(root: &Path, manifest: &Manifest) -> Option<PathBuf> {
    let directory = version_directory(&versions(root)?, &manifest.build_id)?;
    child_of(
        plain_file(&directory.join(BINARY_NAME))?,
        &directory,
        BINARY_NAME,
    )
}

fn launcher(root: &Path) -> Option<PathBuf> {
    let root = plain_directory(root)?;
    child_of(plain_file(&root.join(LAUNCHER_NAME))?, &root, LAUNCHER_NAME)
}

fn update_floor(root: &Path, current: Version) -> Version {
    let Some((previous, _)) = read_manifest(&root.join("previous.json")) else {
        return current;
    };
    let Ok(version) = Version::parse(&previous.version) else {
        return current;
    };
    // Rollback swaps the manifests. A higher previous version was deliberately
    // replaced, so wait for a new release before offering another update.
    if version.cmp_precedence(&current) == Ordering::Greater
        && installed_target(root, &previous).is_some()
    {
        version
    } else {
        current
    }
}

fn valid_build_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

fn executable_path(build_id: &str) -> String {
    format!("versions/{build_id}/{BINARY_NAME}")
}

async fn discover(
    client: &Client,
    root: &Path,
    current: &str,
) -> Result<Option<Candidate>, String> {
    let current = Version::parse(current).map_err(|error| error.to_string())?;
    newest(client, &update_floor(root, current)).await
}

fn stage_manifest(candidate: &Candidate) -> Result<Manifest, String> {
    let build_id = format!("{}-{}", candidate.version, hex(&candidate.hash[..8]));
    if !valid_build_id(&build_id) {
        return Err("Invalid update build identifier".into());
    }
    Ok(Manifest {
        product: INSTALL_PRODUCT.into(),
        version: candidate.version.to_string(),
        executable: executable_path(&build_id),
        build_id,
        sha256: hex(&candidate.hash),
        profile_schema: 1,
    })
}

async fn stage(
    client: &Client,
    root: &Path,
    running: &Path,
    candidate: &Candidate,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    stage_with(root, running, candidate, async move |path| {
        let response = asset_response(client, candidate).await?;
        download(response, candidate, path, &mut progress).await
    })
    .await
}

async fn stage_with(
    root: &Path,
    running: &Path,
    candidate: &Candidate,
    transfer: impl AsyncFnOnce(PathBuf) -> Result<PendingFile, String>,
) -> Result<(), String> {
    eligible_update(root, running, candidate)?;
    let manifest = stage_manifest(candidate)?;
    let allowed_parent = versions(root).ok_or("Invalid installed versions directory")?;
    let directory = allowed_parent.join(&manifest.build_id);
    match fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let directory = version_directory(&allowed_parent, &manifest.build_id)
        .ok_or("Update path is outside the versions directory")?;
    let path = directory.join(BINARY_NAME);
    let pending = match staged_file(root, &manifest, &path)? {
        Some(target) if verify_existing(&target, candidate).is_ok() => None,
        _ => Some(transfer(path.clone()).await?),
    };
    let _lock = publication_lock(root).await?;
    eligible_update(root, running, candidate)?;
    if version_directory(&allowed_parent, &manifest.build_id).as_ref() != Some(&directory) {
        return Err("Staging directory changed during download".into());
    }
    if let Some(target) = staged_file(root, &manifest, &path)?
        && verify_existing(&target, candidate).is_ok()
    {
        return Ok(());
    }
    let pending = pending.ok_or("A staged update was changed during verification")?;
    if read_manifest(&root.join("previous.json"))
        .and_then(|(previous, _)| installed_target(root, &previous))
        .as_ref()
        == Some(&path)
    {
        return Err("Cannot replace the installed rollback build".into());
    }
    fs::rename(&pending.0, &path).map_err(|error| error.to_string())
}

fn staged_file(root: &Path, manifest: &Manifest, path: &Path) -> Result<Option<PathBuf>, String> {
    match path.symlink_metadata() {
        Ok(_) => installed_target(root, manifest)
            .map(Some)
            .ok_or_else(|| "Staged update is outside the installation".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn eligible_update(root: &Path, running: &Path, candidate: &Candidate) -> Result<Vec<u8>, String> {
    let (current, prior) =
        active_manifest(root, running).ok_or("The active installation changed")?;
    let current = Version::parse(&current.version).map_err(|error| error.to_string())?;
    if candidate
        .version
        .cmp_precedence(&update_floor(root, current))
        != Ordering::Greater
    {
        return Err("This version is no longer newer than the installed update floor".into());
    }
    Ok(prior)
}

async fn apply(install: Installation, candidate: &Candidate) -> Result<Outcome, String> {
    let root = &install.root;
    let lock = publication_lock(root).await?;
    let prior = eligible_update(root, &install.running, candidate)?;
    let launcher = launcher(root).ok_or("The installed launcher is missing")?;
    check_x64(&mut File::open(&launcher).map_err(|error| error.to_string())?)
        .map_err(|error| format!("Cannot use the installed launcher: {error}"))?;
    let manifest = stage_manifest(candidate)?;
    let target = installed_target(root, &manifest).ok_or("Staged update is missing or invalid")?;
    let verified = verify_existing(&target, candidate)?;
    publish(root, &prior, &manifest)?;
    Ok(Outcome::Applied(Restart {
        _lock: lock,
        _verified: verified,
        launcher,
        install,
    }))
}

async fn publication_lock(root: &Path) -> Result<File, String> {
    let path = plain_directory(root)
        .ok_or("Invalid installation directory")?
        .join("update.lock");
    let reparse_flag = windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    let mut options = OpenOptions::new();
    // Use the same Windows sharing lock as the PowerShell installer and rollback.
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(reparse_flag)
        .share_mode(0);
    for _ in 0..20 {
        match options.open(&path) {
            Ok(lock)
                if lock.metadata().is_ok_and(|meta| {
                    meta.is_file() && meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
                }) =>
            {
                return Ok(lock);
            }
            Ok(_) => return Err("Invalid installation publication lock".into()),
            Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("The installation is being updated by another process".into())
}

fn publish(root: &Path, prior: &[u8], manifest: &Manifest) -> Result<(), String> {
    let update = serde_json::to_vec_pretty(manifest).map_err(|error| error.to_string())?;
    let current = root.join("current.json");
    let previous = root.join("previous.json");
    let pending_current = prepared_manifest(&current, &update)?;
    let pending_previous = prepared_manifest(&previous, prior)?;
    let old_previous = read_manifest_bytes(&previous)?
        .map(|bytes| prepared_manifest(&previous, &bytes))
        .transpose()?;
    // All manifest writes are prepared first. The last rename is the only
    // activation point; failing it leaves the existing current.json in place.
    fs::rename(&pending_previous.0, &previous).map_err(|error| error.to_string())?;
    if let Err(error) = fs::rename(&pending_current.0, &current) {
        // Do not lose a rollback floor after an ordinary publication error.
        let restore = match &old_previous {
            Some(saved) => fs::rename(&saved.0, &previous),
            None => fs::remove_file(&previous),
        };
        if let Err(restore_error) = restore {
            return Err(format!(
                "Cannot activate update: {error}; cannot restore rollback manifest: {restore_error}"
            ));
        }
        return Err(format!("Cannot activate update: {error}"));
    }
    Ok(())
}

fn prepared_manifest(path: &Path, bytes: &[u8]) -> Result<PendingFile, String> {
    let (pending, mut file) = PendingFile::create(path)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(pending)
}

async fn newest(client: &Client, current: &Version) -> Result<Option<Candidate>, String> {
    let mut best: Option<Candidate> = None;
    for page in 1..=RELEASE_PAGE_LIMIT {
        let response = request_release_page(
            || {
                client
                    .get(RELEASES)
                    .query(&[("per_page", RELEASES_PER_PAGE), ("page", page)])
                    .header(header::ACCEPT, "application/vnd.github+json")
                    .timeout(Duration::from_secs(20))
            },
            RELEASE_RETRY_DELAYS,
        )
        .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let releases: Vec<Release> = serde_json::from_slice(&metadata(response).await?)
            .map_err(|error| error.to_string())?;
        let done = releases.len() < RELEASES_PER_PAGE;
        for release in releases {
            if let Some(next) = candidate(release, current)
                && best.as_ref().is_none_or(|prior| {
                    next.version.cmp_precedence(&prior.version) == Ordering::Greater
                })
            {
                best = Some(next);
            }
        }
        if done {
            return Ok(best);
        }
    }
    Err("GitHub release list exceeded its pagination budget".into())
}

async fn request_release_page(
    request: impl Fn() -> RequestBuilder,
    retry_delays: [Duration; 2],
) -> Result<Response, String> {
    let mut result = request().send().await;
    for delay in retry_delays {
        let retry = match &result {
            Ok(response) => {
                matches!(
                    response.status(),
                    StatusCode::INTERNAL_SERVER_ERROR
                        | StatusCode::BAD_GATEWAY
                        | StatusCode::SERVICE_UNAVAILABLE
                        | StatusCode::GATEWAY_TIMEOUT
                ) && !response.headers().contains_key(header::RETRY_AFTER)
            }
            Err(error) => error.is_connect() || error.is_timeout(),
        };
        if !retry {
            break;
        }
        drop(result);
        tokio::time::sleep(delay).await;
        result = request().send().await;
    }
    result.map_err(http_error)
}

async fn metadata(mut response: Response) -> Result<Vec<u8>, String> {
    if !response.status().is_success() {
        return Err(format!(
            "GitHub Releases returned HTTP {}",
            response.status()
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        if chunk.len() > METADATA_LIMIT - bytes.len() {
            return Err("GitHub release metadata exceeded its size budget".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn candidate(release: Release, current: &Version) -> Option<Candidate> {
    if release.draft || release.tag_name.len() > 120 {
        return None;
    }
    let tag = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let version = Version::parse(tag).ok()?;
    if version.cmp_precedence(current) != Ordering::Greater {
        return None;
    }
    let (url, size, hash) = release.assets.into_iter().find_map(|asset| {
        if asset.name != ASSET_NAME
            || asset.state != "uploaded"
            || !(64..=EXECUTABLE_LIMIT).contains(&asset.size)
        {
            return None;
        }
        let hash = parse_hash(asset.digest?.strip_prefix("sha256:")?)?;
        let url = Url::parse(&asset.browser_download_url).ok()?;
        if !official_asset(&url) {
            return None;
        }
        let path_tag = url.path_segments()?.nth(4)?;
        let path_tag = path_tag.replace("%2B", "+").replace("%2b", "+");
        (path_tag == release.tag_name).then_some((url, asset.size, hash))
    })?;
    Some(Candidate {
        version,
        size,
        hash,
        url,
    })
}

fn https(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn official_asset(url: &Url) -> bool {
    if !https(url) || url.host_str() != Some("github.com") || url.query().is_some() {
        return false;
    }
    let mut parts = url.path().split('/').skip(1);
    parts
        .next()
        .is_some_and(|part| part.eq_ignore_ascii_case("Ferigone"))
        && parts
            .next()
            .is_some_and(|part| part.eq_ignore_ascii_case("satin"))
        && parts.next() == Some("releases")
        && parts.next() == Some("download")
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next() == Some(ASSET_NAME)
        && parts.next().is_none()
}

fn release_redirect(url: &Url) -> bool {
    official_asset(url)
        || (https(url)
            && matches!(
                url.host_str(),
                Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
            ))
}

async fn asset_response(client: &Client, candidate: &Candidate) -> Result<Response, String> {
    let mut url = candidate.url.clone();
    for _ in 0..=4 {
        let response = client
            .get(url.clone())
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(http_error)?;
        if !response.status().is_redirection() {
            if response.status() != reqwest::StatusCode::OK {
                return Err(format!(
                    "Release download returned HTTP {}",
                    response.status()
                ));
            }
            if response
                .content_length()
                .is_some_and(|length| length != candidate.size)
            {
                return Err("Release download length differs from the GitHub asset size".into());
            }
            return Ok(response);
        }
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or("Release redirect has no valid destination")?;
        let next = url
            .join(location)
            .map_err(|_| "Release redirect is malformed")?;
        if !release_redirect(&next) {
            return Err("Release redirect left the HTTPS GitHub asset hosts".into());
        }
        url = next;
    }
    Err("Release download exceeded its redirect budget".into())
}

struct PendingFile(PathBuf);

impl PendingFile {
    fn create(path: &Path) -> Result<(Self, File), String> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let serial = SERIAL.fetch_add(1, Relaxed);
        let path = path.with_extension(format!("pending-{}-{stamp}-{serial}", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        Ok((Self(path), file))
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

async fn download(
    mut response: Response,
    candidate: &Candidate,
    path: PathBuf,
    mut progress: impl FnMut(u64),
) -> Result<PendingFile, String> {
    let (pending, mut file) = PendingFile::create(&path)?;
    let mut hash = Context::new(&SHA256);
    let mut remaining = candidate.size;
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        remaining = remaining
            .checked_sub(chunk.len() as u64)
            .ok_or("Release download exceeded the GitHub asset size")?;
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|error| error.to_string())?;
        progress(candidate.size - remaining);
    }
    if remaining != 0 || hash.finish().as_ref() != candidate.hash {
        return Err("Release download did not match the GitHub SHA-256 digest and size".into());
    }
    check_x64(&mut file)?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    Ok(pending)
}

fn verify_existing(path: &Path, candidate: &Candidate) -> Result<File, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() != candidate.size {
        return Err("An existing update file has the wrong size".into());
    }
    let mut hash = Context::new(&SHA256);
    let mut bytes = [0_u8; 32 * 1024];
    let mut reader = (&mut file).take(candidate.size + 1);
    while reader.limit() != 0 {
        match reader.read(&mut bytes).map_err(|error| error.to_string())? {
            0 => break,
            count => hash.update(&bytes[..count]),
        }
    }
    if reader.limit() != 1 || hash.finish().as_ref() != candidate.hash {
        return Err("An existing update file has the wrong SHA-256 digest".into());
    }
    check_x64(&mut file)?;
    Ok(file)
}

fn check_x64(file: &mut File) -> Result<(), String> {
    let mut dos = [0_u8; 64];
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut dos))
        .map_err(|error| error.to_string())?;
    if dos[..2] != *b"MZ" {
        return Err("Release asset is not a Windows executable".into());
    }
    let offset = u32::from_le_bytes(dos[60..64].try_into().expect("four-byte PE offset"));
    let mut pe = [0_u8; 6];
    file.seek(SeekFrom::Start(u64::from(offset)))
        .and_then(|_| file.read_exact(&mut pe))
        .map_err(|error| error.to_string())?;
    if pe != *b"PE\0\0\x64\x86" {
        return Err("Release asset is not a Windows x64 executable".into());
    }
    Ok(())
}

fn parse_hash(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut hash = [0_u8; 32];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(hash)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut value, "{byte:02X}").expect("write hex to String");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "satin-interactive-update-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }

        fn setup(&self) -> (Installation, Vec<u8>) {
            let build_id = "fixture-installed";
            let executable = executable_path(build_id);
            let running = self.0.join(&executable);
            fs::create_dir_all(running.parent().unwrap()).unwrap();
            fs::write(&running, b"running fixture").unwrap();
            fs::write(self.0.join(LAUNCHER_NAME), sample_payload()).unwrap();
            let profile = self.0.join("Separate test profile");
            fs::create_dir(&profile).unwrap();
            let current = Manifest {
                product: INSTALL_PRODUCT.into(),
                version: env!("CARGO_PKG_VERSION").into(),
                build_id: build_id.into(),
                executable,
                sha256: "A".repeat(64),
                profile_schema: 1,
            };
            let prior = serde_json::to_vec(&current).unwrap();
            fs::write(self.0.join("current.json"), &prior).unwrap();
            (
                Installation {
                    root: self.0.clone(),
                    profile,
                    running: running.canonicalize().unwrap(),
                },
                prior,
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample_payload() -> Vec<u8> {
        let mut payload = vec![0_u8; 512];
        payload[..2].copy_from_slice(b"MZ");
        payload[60] = 64;
        payload[64..70].copy_from_slice(b"PE\0\0\x64\x86");
        payload
    }

    fn sample_candidate(payload: &[u8]) -> Candidate {
        let current = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        Candidate {
            version: Version::new(current.major + 1, 0, 0),
            size: payload.len() as u64,
            hash: ring::digest::digest(&SHA256, payload)
                .as_ref()
                .try_into()
                .unwrap(),
            url: Url::parse(RELEASES).unwrap(),
        }
    }

    fn attach_test_job(updates: &mut Updates) -> Sender<Result<Outcome, String>> {
        let (send, result) = mpsc::channel();
        updates.job = Some(Job {
            result,
            bytes: Arc::new(AtomicU64::new(0)),
            shown: 0,
            cancel: None,
            worker: None,
        });
        send
    }

    #[test]
    fn discovery_only_offers_download_and_late_commands_cannot_restart() {
        let fixture = Fixture::new();
        let (install, prior) = fixture.setup();
        let mut updates = Updates {
            install,
            wake: Arc::new(|| {}),
            phase: Phase::Checking,
            job: None,
        };
        let check = attach_test_job(&mut updates);
        assert!(updates.view(false).is_none());
        assert!(!updates.can_start(UpdateAction::Download));
        assert!(!updates.can_start(UpdateAction::Restart));
        check
            .send(Ok(Outcome::Discovered(Some(sample_candidate(
                &sample_payload(),
            )))))
            .unwrap();
        assert!(updates.poll().0);
        assert!(
            updates.job.is_none(),
            "discovery must not enqueue a download"
        );
        assert_eq!(fs::read(fixture.0.join("current.json")).unwrap(), prior);
        assert_eq!(fs::read_dir(fixture.0.join("versions")).unwrap().count(), 1);
        assert!(updates.can_start(UpdateAction::Download));
        assert!(!updates.can_start(UpdateAction::Restart));
        // Model the accepted Download action without starting the production network.
        updates.phase = Phase::Downloading(updates.offered().unwrap());
        let download = attach_test_job(&mut updates);
        assert!(!updates.can_start(UpdateAction::Download));
        assert!(!updates.can_start(UpdateAction::Restart));
        updates.job.as_ref().unwrap().bytes.store(256, Relaxed);
        assert!(updates.poll().0, "intermediate progress needs a redraw");
        assert_eq!(updates.view(true).unwrap().progress, Some(0.5));
        download.send(Ok(Outcome::Staged)).unwrap();
        assert!(updates.poll().0);
        assert!(updates.can_start(UpdateAction::Restart));
        assert!(
            !updates.can_start(UpdateAction::Download),
            "stale download click"
        );
        assert!(check.send(Ok(Outcome::Discovered(None))).is_err());
        assert!(!updates.poll().0);
        assert_eq!(fs::read(fixture.0.join("current.json")).unwrap(), prior);
    }

    #[test]
    fn startup_or_download_failure_allows_retry_without_a_restart() {
        let fixture = Fixture::new();
        let (install, prior) = fixture.setup();
        let mut updates = Updates {
            install,
            wake: Arc::new(|| {}),
            phase: Phase::Checking,
            job: None,
        };
        drop(attach_test_job(&mut updates));
        assert!(updates.poll().0);
        assert!(updates.can_start(UpdateAction::Check));
        assert!(
            updates.view(false).is_some(),
            "retry is visible but opens no panel"
        );
        updates.phase = Phase::Downloading(Arc::new(sample_candidate(&sample_payload())));
        attach_test_job(&mut updates)
            .send(Err("offline fixture".into()))
            .unwrap();
        assert!(updates.poll().0);
        assert!(updates.can_start(UpdateAction::Download));
        assert!(!updates.can_start(UpdateAction::Restart));
        assert_eq!(fs::read(fixture.0.join("current.json")).unwrap(), prior);
    }

    // Use a real streamed response, but never change the production endpoint.
    // The server releases the second half only after the first progress report.
    async fn stream_fixture(
        path: PathBuf,
        candidate: &Candidate,
        payload: &[u8],
        reports: &mut Vec<u64>,
    ) -> Result<PendingFile, String> {
        use std::{io::BufRead, net::TcpListener};

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/asset", listener.local_addr().unwrap());
        let payload = payload.to_owned();
        let (observed, release) = mpsc::channel();
        let first = payload.len() / 2;
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            for line in std::io::BufReader::new(&mut connection).lines() {
                if line.unwrap().is_empty() {
                    break;
                }
            }
            write!(
                connection,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            )
            .unwrap();
            connection.write_all(&payload[..first]).unwrap();
            release
                .recv_timeout(Duration::from_secs(5))
                .expect("no progress before EOF");
            connection.write_all(&payload[first..]).unwrap();
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(6))
            .build()
            .unwrap();
        let response = client.get(url).send().await.unwrap();
        let mut observed = Some(observed);
        let result = download(response, candidate, path, |bytes| {
            reports.push(bytes);
            if bytes == first as u64 {
                observed.take().unwrap().send(()).unwrap();
            }
        })
        .await;
        server.join().unwrap();
        result
    }

    #[tokio::test]
    async fn corrupted_stream_does_not_stage_and_good_stream_waits_for_apply() {
        let fixture = Fixture::new();
        let (install, prior) = fixture.setup();
        let payload = sample_payload();
        let candidate = sample_candidate(&payload);
        let manifest = stage_manifest(&candidate).unwrap();
        let target = fixture.0.join(&manifest.executable);
        let mut corrupted = payload.clone();
        corrupted[71] ^= 1;
        let mut reports = Vec::new();
        let error = stage_with(&install.root, &install.running, &candidate, async |path| {
            stream_fixture(path, &candidate, &corrupted, &mut reports).await
        })
        .await
        .unwrap_err();
        assert!(error.contains("SHA-256"));
        assert!(!target.exists());
        assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 0);
        assert_eq!(fs::read(fixture.0.join("current.json")).unwrap(), prior);
        assert!(!fixture.0.join("previous.json").exists());

        reports.clear();
        stage_with(&install.root, &install.running, &candidate, async |path| {
            stream_fixture(path, &candidate, &payload, &mut reports).await
        })
        .await
        .unwrap();
        assert!(reports.contains(&256));
        assert_eq!(reports.last(), Some(&candidate.size));
        assert_eq!(fs::read(&target).unwrap(), payload);
        assert_eq!(fs::read(fixture.0.join("current.json")).unwrap(), prior);
        assert!(!fixture.0.join("previous.json").exists());
        assert!(active_manifest(&install.root, &install.running).is_some());

        fs::write(&target, &corrupted).unwrap();
        assert!(
            apply(install.clone(), &candidate)
                .await
                .err()
                .unwrap()
                .contains("SHA-256")
        );
        assert_eq!(fs::read(fixture.0.join("current.json")).unwrap(), prior);
        assert!(!fixture.0.join("previous.json").exists());
        stage_with(&install.root, &install.running, &candidate, async |path| {
            stream_fixture(path, &candidate, &payload, &mut reports).await
        })
        .await
        .unwrap();
        let Outcome::Applied(restart) = apply(install, &candidate).await.unwrap() else {
            panic!("successful explicit apply should request relaunch");
        };
        assert_eq!(fs::read(fixture.0.join("previous.json")).unwrap(), prior);
        let (current, _) = read_manifest(&fixture.0.join("current.json")).unwrap();
        assert_eq!(current.version, candidate.version.to_string());
        assert_eq!(current.executable, manifest.executable);
        // The PE fixture isn't a runnable executable. A separate real-launcher
        // smoke covers the handoff without pointing production at a fixture.
        drop(restart);
    }

    #[tokio::test]
    async fn failed_publication_keeps_current_and_restores_existing_rollback() {
        let fixture = Fixture::new();
        let (install, prior) = fixture.setup();
        let payload = sample_payload();
        let candidate = sample_candidate(&payload);
        let target = fixture
            .0
            .join(stage_manifest(&candidate).unwrap().executable);
        fs::create_dir(target.parent().unwrap()).unwrap();
        fs::write(target, payload).unwrap();
        let previous = b"previous manifest bytes";
        fs::write(fixture.0.join("previous.json"), previous).unwrap();
        let block_replace = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(fixture.0.join("current.json"))
            .unwrap();
        let error = apply(install.clone(), &candidate).await.err().unwrap();
        assert!(error.contains("Cannot activate update"), "{error}");
        assert_eq!(fs::read(fixture.0.join("current.json")).unwrap(), prior);
        assert_eq!(fs::read(fixture.0.join("previous.json")).unwrap(), previous);
        drop(block_replace);
        assert!(matches!(
            apply(install, &candidate).await,
            Ok(Outcome::Applied(_))
        ));
    }

    #[tokio::test]
    async fn apply_rechecks_the_active_build_and_rollback_floor() {
        let fixture = Fixture::new();
        let (install, prior) = fixture.setup();
        let payload = sample_payload();
        let candidate = sample_candidate(&payload);
        let manifest = stage_manifest(&candidate).unwrap();
        let target = fixture.0.join(&manifest.executable);
        fs::create_dir(target.parent().unwrap()).unwrap();
        fs::write(&target, payload).unwrap();
        let newer = serde_json::to_vec(&manifest).unwrap();
        fs::write(fixture.0.join("previous.json"), &newer).unwrap();
        let error = apply(install.clone(), &candidate).await.err().unwrap();
        assert!(error.contains("update floor"), "{error}");
        assert_eq!(fs::read(fixture.0.join("current.json")).unwrap(), prior);
        assert_eq!(fs::read(fixture.0.join("previous.json")).unwrap(), newer);

        fs::write(fixture.0.join("current.json"), &newer).unwrap();
        let error = apply(install, &candidate).await.err().unwrap();
        assert!(error.contains("active installation changed"), "{error}");
        assert_eq!(fs::read(fixture.0.join("current.json")).unwrap(), newer);
    }

    #[tokio::test]
    #[ignore = "run with the workspace launcher and compiled offline updater probe"]
    async fn restart_through_installed_launcher_reopens_exact_profile() {
        use crate::profile::{Profile, Session};

        let fixture = Fixture::new();
        let (install, prior) = fixture.setup();
        let launcher_source = std::env::var_os("SATIN_TEST_LAUNCHER").expect("workspace launcher");
        let probe_source = std::env::var_os("SATIN_TEST_UPDATER_PROBE").expect("updater probe");
        fs::copy(launcher_source, fixture.0.join(LAUNCHER_NAME)).unwrap();
        let payload = fs::read(probe_source).unwrap();
        let candidate = sample_candidate(&payload);
        let target = fixture
            .0
            .join(stage_manifest(&candidate).unwrap().executable);
        fs::create_dir(target.parent().unwrap()).unwrap();
        fs::write(&target, payload).unwrap();
        let (profile, _) = Profile::open(install.profile.clone()).unwrap();
        profile
            .save(&Session {
                schema: 1,
                tabs: vec!["about:home".into(), "about:demo".into()],
                active: 1,
            })
            .unwrap();
        assert_eq!(fs::read(fixture.0.join("current.json")).unwrap(), prior);
        let opened = install.profile.join("updater-probe-receipt.json");
        assert!(!opened.exists());
        let Outcome::Applied(restart) = apply(install.clone(), &candidate).await.unwrap() else {
            panic!("explicit apply did not request a relaunch");
        };
        assert!(
            !opened.exists(),
            "publishing must not launch while the profile is locked"
        );
        assert!(Profile::open(install.profile.clone()).is_err());
        drop(profile);
        restart.launch().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !opened.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let restored: Session = serde_json::from_slice(&fs::read(&opened).unwrap()).unwrap();
        assert_eq!(restored.tabs, ["about:home", "about:demo"]);
        assert_eq!(restored.active, 1);
        assert_eq!(fs::read(fixture.0.join("previous.json")).unwrap(), prior);
        assert!(!fixture.0.join("User Data").exists());
    }

    fn release(current_tag: &str, asset_tag: &str, digest: Option<String>) -> Release {
        Release {
            tag_name: current_tag.into(),
            draft: false,
            assets: vec![Asset {
                name: ASSET_NAME.into(),
                state: "uploaded".into(),
                size: 512,
                digest,
                browser_download_url: format!(
                    "https://github.com/Ferigone/satin/releases/download/{asset_tag}/{ASSET_NAME}"
                ),
            }],
        }
    }

    #[tokio::test]
    async fn a_gateway_timeout_does_not_end_the_release_check() {
        use std::{io::BufRead, net::TcpListener};

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/releases", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for reply in [
                "HTTP/1.1 504 Gateway Timeout\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]",
            ] {
                let (mut connection, _) = listener.accept().unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                for line in std::io::BufReader::new(&mut connection).lines() {
                    if line.unwrap().is_empty() {
                        break;
                    }
                }
                connection.write_all(reply.as_bytes()).unwrap();
            }
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let response = request_release_page(|| client.get(&url), [Duration::ZERO; 2])
            .await
            .unwrap();
        assert_eq!(metadata(response).await.unwrap(), b"[]");
        server.join().unwrap();
    }

    #[test]
    fn semver_and_the_release_digest_control_update_selection() {
        let current = Version::parse("0.1.0-alpha.9").unwrap();
        let valid_digest = Some(format!("sha256:{}", "ab".repeat(32)));
        assert!(
            candidate(
                release("v0.1.0-alpha.10", "v0.1.0-alpha.10", valid_digest.clone()),
                &current
            )
            .is_some()
        );
        assert!(
            candidate(
                release("v0.1.0-alpha.2", "v0.1.0-alpha.2", valid_digest.clone()),
                &current
            )
            .is_none()
        );
        assert!(
            candidate(
                release("v0.1.0-alpha.09", "v0.1.0-alpha.09", valid_digest.clone()),
                &current
            )
            .is_none()
        );
        assert!(
            candidate(
                release(
                    "v0.1.0-alpha.9+rebuild",
                    "v0.1.0-alpha.9+rebuild",
                    valid_digest.clone()
                ),
                &current
            )
            .is_none()
        );
        assert!(
            candidate(
                release("v0.1.0-alpha.10", "v0.1.0-alpha.9", valid_digest.clone()),
                &current
            )
            .is_none()
        );
        assert!(
            candidate(
                release("v0.1.0-alpha.10", "v0.1.0-alpha.10", None),
                &current
            )
            .is_none()
        );
        let mut drafted = release("v0.2.0", "v0.2.0", valid_digest);
        drafted.draft = true;
        assert!(candidate(drafted, &current).is_none());
    }

    #[test]
    fn redirects_are_limited_to_https_github_release_hosts() {
        let allowed = "https://release-assets.githubusercontent.com/github-production-release-asset/a?public-test=value";
        assert!(release_redirect(&Url::parse(allowed).unwrap()));
        for address in [
            "http://release-assets.githubusercontent.com/a",
            "https://release-assets.githubusercontent.com:444/a",
            "https://release-assets.githubusercontent.com.example.org/a",
            "https://example.org@release-assets.githubusercontent.com/a",
            "https://github.com/Ferigone/other/releases/download/v0.2.0/satin-windows-x64.exe",
        ] {
            assert!(
                !release_redirect(&Url::parse(address).unwrap()),
                "{address}"
            );
        }
    }

    #[test]
    fn manifest_swap_preserves_the_active_build_as_rollback() {
        let root = std::env::temp_dir().join(format!("satin-update-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let prior = br#"{"version":"0.1.0-alpha.14"}"#;
        fs::write(root.join("current.json"), prior).unwrap();
        let update = Manifest {
            product: INSTALL_PRODUCT.into(),
            version: "0.1.0-alpha.15".into(),
            build_id: "0.1.0-alpha.15-AABB".into(),
            executable: executable_path("0.1.0-alpha.15-AABB"),
            sha256: "A".repeat(64),
            profile_schema: 1,
        };
        publish(&root, prior, &update).unwrap();
        let actual: Manifest =
            serde_json::from_slice(&fs::read(root.join("current.json")).unwrap()).unwrap();
        assert_eq!(actual.executable, update.executable);
        assert_eq!(fs::read(root.join("previous.json")).unwrap(), prior);
        fs::remove_file(root.join("current.json")).unwrap();
        fs::remove_file(root.join("previous.json")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn restoring_a_previous_version_waits_for_a_newer_release() {
        let root = std::env::temp_dir().join(format!("satin-rollback-test-{}", std::process::id()));
        let build = "0.1.0-alpha.16-ABCD";
        let directory = root.join("versions").join(build);
        fs::create_dir_all(&directory).unwrap();
        let exe = directory.join(BINARY_NAME);
        fs::write(&exe, b"test executable").unwrap();

        let mut prior = Manifest {
            product: INSTALL_PRODUCT.into(),
            version: "0.1.0-alpha.16".into(),
            build_id: build.into(),
            executable: executable_path(build),
            sha256: "A".repeat(64),
            profile_schema: 1,
        };
        let previous = root.join("previous.json");
        fs::write(&previous, serde_json::to_vec(&prior).unwrap()).unwrap();
        let current = Version::parse("0.1.0-alpha.15").unwrap();
        let floor = update_floor(&root, current.clone());
        assert_eq!(floor, Version::parse("0.1.0-alpha.16").unwrap());
        let digest = format!("sha256:{}", "b".repeat(64));
        assert!(
            candidate(
                release("v0.1.0-alpha.16", "v0.1.0-alpha.16", Some(digest.clone())),
                &floor
            )
            .is_none()
        );
        assert!(
            candidate(
                release("v0.1.0-alpha.17", "v0.1.0-alpha.17", Some(digest)),
                &floor
            )
            .is_some()
        );

        prior.executable = "../elsewhere.exe".into();
        fs::write(&previous, serde_json::to_vec(&prior).unwrap()).unwrap();
        assert_eq!(update_floor(&root, current.clone()), current);
        fs::remove_file(previous).unwrap();
        fs::remove_file(exe).unwrap();
        fs::remove_dir(directory).unwrap();
        fs::remove_dir(root.join("versions")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[tokio::test]
    async fn publication_lock_excludes_other_windows_openers_until_released() {
        let root =
            std::env::temp_dir().join(format!("satin-update-lock-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let lock = publication_lock(&root).await.unwrap();
        let path = root.join("update.lock");
        let competing = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&path);
        assert_eq!(competing.unwrap_err().raw_os_error(), Some(32));
        drop(lock);
        drop(publication_lock(&root).await.unwrap());
        fs::remove_file(path).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn staged_payload_requires_the_github_digest_and_x64_pe_header() {
        let path =
            std::env::temp_dir().join(format!("satin-update-payload-test-{}", std::process::id()));
        let mut payload = [0_u8; 70];
        payload[..2].copy_from_slice(b"MZ");
        payload[60] = 64;
        payload[64..].copy_from_slice(b"PE\0\0\x64\x86");
        let mut candidate = Candidate {
            version: Version::parse("0.1.0-alpha.15").unwrap(),
            size: payload.len() as u64,
            hash: ring::digest::digest(&SHA256, &payload)
                .as_ref()
                .try_into()
                .unwrap(),
            url: Url::parse(RELEASES).unwrap(),
        };
        fs::write(&path, payload).unwrap();
        assert!(verify_existing(&path, &candidate).is_ok());
        payload[68] ^= 1;
        fs::write(&path, payload).unwrap();
        assert!(
            verify_existing(&path, &candidate)
                .unwrap_err()
                .contains("SHA-256")
        );
        candidate.hash = ring::digest::digest(&SHA256, &payload)
            .as_ref()
            .try_into()
            .unwrap();
        assert!(
            verify_existing(&path, &candidate)
                .unwrap_err()
                .contains("Windows x64")
        );
        fs::remove_file(path).unwrap();
    }
}
