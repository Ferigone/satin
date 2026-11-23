//! Background updates for the per-user Windows installation.
#![cfg(all(windows, target_arch = "x86_64"))]

use reqwest::{Client, Response, Url, header};
use ring::digest::{Context, SHA256};
use rustls_platform_verifier::BuilderVerifierExt;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const RELEASES: &str = "https://api.github.com/repos/Ferigone/satin/releases";
const ASSET_NAME: &str = "satin-windows-x64.exe";
const BINARY_NAME: &str = "rust-browser.exe";
const INSTALL_PRODUCT: &str = "RustBrowser";
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
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

pub(crate) fn start_for_installed_launch() {
    let Some(root) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let root = PathBuf::from(root).join(INSTALL_PRODUCT);
    let Ok(running) = std::env::current_exe().and_then(|path| path.canonicalize()) else {
        return;
    };
    if active_manifest(&root, &running).is_none() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("satin-updater".into())
        .spawn(move || {
            if let Err(error) = run_worker(&root, &running) {
                eprintln!("Satin updater: {error}");
            }
        });
}

fn run_worker(root: &Path, running: &Path) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let client = client()?;
        while let Some((current, _)) = active_manifest(root, running) {
            match check(&client, root, running, &current.version).await {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => eprintln!("Satin updater: {error}"),
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
        Ok(())
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
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MANIFEST_LIMIT + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return None;
    }
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

fn installed_target(root: &Path, manifest: &Manifest) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let versions = root.join("versions").canonicalize().ok()?;
    let target = root.join(&manifest.executable).canonicalize().ok()?;
    (target.is_file() && target.starts_with(&versions) && versions.starts_with(&root))
        .then_some(target)
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

async fn check(
    client: &Client,
    root: &Path,
    running: &Path,
    current: &str,
) -> Result<bool, String> {
    let current = Version::parse(current).map_err(|error| error.to_string())?;
    let floor = update_floor(root, current.clone());
    let Some(candidate) = newest(client, &floor).await? else {
        return Ok(false);
    };
    let build_id = format!("{}-{}", candidate.version, hex(&candidate.hash[..8]));
    if !valid_build_id(&build_id) {
        return Err("Invalid update build identifier".into());
    }
    let executable = executable_path(&build_id);
    let path = root.join(&executable);
    let directory = path.parent().expect("version directory");
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let allowed_parent = root
        .join("versions")
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !directory
        .canonicalize()
        .map_err(|error| error.to_string())?
        .starts_with(allowed_parent)
    {
        return Err("Update path is outside the versions directory".into());
    }
    let pending = if path.exists() {
        None
    } else {
        Some(download(client, &candidate, &path).await?)
    };
    let _lock = publication_lock(root).await?;
    let Some((_, prior)) = active_manifest(root, running) else {
        return Ok(false);
    };
    if candidate
        .version
        .cmp_precedence(&update_floor(root, current))
        != Ordering::Greater
    {
        return Ok(false);
    }
    if path.exists() {
        verify_existing(&path, &candidate)?;
    } else if let Some(pending) = &pending {
        fs::rename(&pending.0, &path).map_err(|error| error.to_string())?;
    } else {
        return Err("A staged update was removed before publication".into());
    }
    let manifest = Manifest {
        product: INSTALL_PRODUCT.into(),
        version: candidate.version.to_string(),
        build_id,
        executable,
        sha256: hex(&candidate.hash),
        profile_schema: 1,
    };
    publish(root, &prior, &manifest)?;
    Ok(true)
}

async fn publication_lock(root: &Path) -> Result<File, String> {
    let path = root.join("update.lock");
    let mut options = OpenOptions::new();
    // Use the same Windows sharing lock as the PowerShell installer and rollback.
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0);
    for _ in 0..20 {
        match options.open(&path) {
            Ok(lock) => return Ok(lock),
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
    crate::profile::atomic_write(&root.join("previous.json"), prior)?;
    crate::profile::atomic_write(&root.join("current.json"), &update)
}

async fn newest(client: &Client, current: &Version) -> Result<Option<Candidate>, String> {
    let mut best: Option<Candidate> = None;
    for page in 1..=RELEASE_PAGE_LIMIT {
        let response = client
            .get(RELEASES)
            .query(&[("per_page", RELEASES_PER_PAGE), ("page", page)])
            .header(header::ACCEPT, "application/vnd.github+json")
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(http_error)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
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

impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

async fn download(
    client: &Client,
    candidate: &Candidate,
    path: &Path,
) -> Result<PendingFile, String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let pending = path.with_extension(format!("pending-{}-{stamp}", std::process::id()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|error| error.to_string())?;
    let pending = PendingFile(pending);
    let mut file = file;
    let mut response = asset_response(client, candidate).await?;
    let mut hash = Context::new(&SHA256);
    let mut remaining = candidate.size;
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        remaining = remaining
            .checked_sub(chunk.len() as u64)
            .ok_or("Release download exceeded the GitHub asset size")?;
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|error| error.to_string())?;
    }
    if remaining != 0 || hash.finish().as_ref() != candidate.hash {
        return Err("Release download did not match the GitHub SHA-256 digest and size".into());
    }
    check_x64(&mut file)?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    Ok(pending)
}

fn verify_existing(path: &Path, candidate: &Candidate) -> Result<(), String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
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
    check_x64(&mut file)
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
