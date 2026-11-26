use fs2::FileExt;
use rust_browser_engine::{
    cookies::{CookieStore, MAX_COOKIE_FILE_BYTES},
    web_storage::{MAX_STORAGE_FILE_BYTES, StoragePersistence, StorageProfile},
};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub schema: u32,
    pub tabs: Vec<String>,
    pub active: usize,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            schema: 1,
            tabs: vec!["about:home".into()],
            active: 0,
        }
    }
}
pub fn default_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("RustBrowser")
        .join("User Data")
}

fn profile_lock(path: &Path) -> Result<File, String> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.join("profile.lock"))
        .map_err(|e| e.to_string())?;
    lock.try_lock_exclusive().map_err(|_|"Ten profil jest już otwarty w Satin. Zamknij jego okno przed uruchomieniem nowej wersji.".to_owned())?;
    Ok(lock)
}

/// Keep only the saved tab session across an unacknowledged update. Take the
/// same exclusive lock as the browser so this never reads a live session.
#[cfg(all(windows, target_arch = "x86_64"))]
pub(crate) fn snapshot_update_session(path: &Path) -> Result<Vec<u8>, String> {
    const LIMIT: u64 = 4 * 1024 * 1024;
    let _lock = profile_lock(path)?;
    let mut session = Vec::new();
    File::open(path.join("session.json"))
        .and_then(|file| file.take(LIMIT + 1).read_to_end(&mut session))
        .map_err(|error| error.to_string())?;
    if session.len() as u64 > LIMIT {
        return Err("The saved session is too large for an update restart".into());
    }
    Ok(session)
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub(crate) fn restore_update_session(path: &Path, session: &[u8]) -> Result<(), String> {
    let _lock = profile_lock(path)?;
    atomic_write(&path.join("session.json"), session)
}

pub struct Profile {
    path: PathBuf,
    _lock: File,
    cookies: CookieStore,
    cookie_revision: Cell<u64>,
    storage: StorageProfile,
}
impl Profile {
    pub fn open(path: PathBuf) -> Result<(Self, Session), String> {
        fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        let lock = profile_lock(&path)?;
        let file = path.join("session.json");
        let session = if file.exists() {
            let text = fs::read_to_string(&file).map_err(|e| e.to_string())?;
            let s: Session = serde_json::from_str(&text)
                .map_err(|e| format!("Nie można odczytać profilu; dane zachowano: {e}"))?;
            if s.schema != 1 {
                return Err("Profil pochodzi z nieobsługiwanej wersji; dane zachowano.".into());
            }
            s
        } else {
            Session::default()
        };
        let cookies = CookieStore::new();
        let cookie_file = path.join("Cookies.v1.dpapi");
        if cookie_file.exists() {
            if fs::metadata(&cookie_file).map_err(|e| e.to_string())?.len()
                > (MAX_COOKIE_FILE_BYTES + 65536) as u64
            {
                return Err("Plik cookies przekracza limit; zachowano oryginalne dane.".into());
            }
            let protected = fs::read(&cookie_file).map_err(|e| e.to_string())?;
            let bytes = unprotect_cookies(&protected)?;
            cookies.restore_persistent(&bytes)?;
        }
        let revision = cookies.revision();
        let storage =
            StorageProfile::with_persistence(Rc::new(StorageFiles(path.join("Local Storage"))));
        Ok((
            Self {
                path,
                _lock: lock,
                cookies,
                cookie_revision: Cell::new(revision),
                storage,
            },
            session,
        ))
    }
    pub fn cookies(&self) -> CookieStore {
        self.cookies.clone()
    }
    pub fn storage(&self) -> StorageProfile {
        self.storage.clone()
    }
    pub fn save_storage(&self) -> Result<(), String> {
        self.storage.flush()
    }
    pub fn save_cookies(&self) -> Result<(), String> {
        let revision = self.cookies.revision();
        if revision == self.cookie_revision.get() {
            return Ok(());
        }
        let bytes = self.cookies.export_persistent()?;
        let protected = protect_cookies(&bytes)?;
        atomic_write(&self.path.join("Cookies.v1.dpapi"), &protected)?;
        self.cookie_revision.set(revision);
        Ok(())
    }
    pub fn save(&self, session: &Session) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(session).map_err(|e| e.to_string())?;
        atomic_write(&self.path.join("session.json"), &bytes)?;
        self.save_cookies()
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        // The profile lock remains held through the final atomic cookie write.
        let _ = self.save_cookies();
        let _ = self.save_storage();
    }
}

struct StorageFiles(PathBuf);

impl StorageFiles {
    fn path(&self, id: &str) -> PathBuf {
        self.0.join(format!("{id}.bin"))
    }
}

impl StoragePersistence for StorageFiles {
    fn load(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
        let file = match File::open(self.path(id)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let mut bytes = Vec::new();
        file.take((MAX_STORAGE_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_STORAGE_FILE_BYTES {
            return Err("Origin storage file exceeds its size budget".into());
        }
        Ok(Some(bytes))
    }

    fn save(&self, id: &str, data: &[u8]) -> Result<(), String> {
        fs::create_dir_all(&self.0).map_err(|error| error.to_string())?;
        atomic_write(&self.path(id), data)
    }
}

const COOKIE_MAGIC: &[u8] = b"RBCOOK1\0";
#[cfg(windows)]
fn crypt_cookie_bytes(bytes: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    if bytes.len() > MAX_COOKIE_FILE_BYTES + 65536 {
        return Err("Cookie data exceeds its size budget".into());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // DPAPI scopes the store to this Windows account; UI is explicitly forbidden.
    // The API borrows the input only for this call and allocates output via LocalAlloc.
    let result = unsafe {
        if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if result == 0 {
        return Err(
            "Nie można zabezpieczyć/odczytać cookies tego konta Windows; dane zachowano.".into(),
        );
    }
    // A successful DPAPI call owns exactly cbData bytes; release on every path.
    let value = if output.cbData as usize > MAX_COOKIE_FILE_BYTES + 65536 {
        Err("Protected cookie data exceeds its size budget".into())
    } else if output.cbData == 0 {
        Ok(vec![])
    } else if output.pbData.is_null() {
        Err("DPAPI returned invalid cookie data".into())
    } else {
        Ok(unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec())
    };
    if !output.pbData.is_null() {
        unsafe {
            LocalFree(output.pbData.cast());
        }
    }
    value
}
fn protect_cookies(bytes: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        let encrypted = crypt_cookie_bytes(bytes, true)?;
        let mut result = Vec::with_capacity(COOKIE_MAGIC.len() + encrypted.len());
        result.extend_from_slice(COOKIE_MAGIC);
        result.extend_from_slice(&encrypted);
        Ok(result)
    }
    #[cfg(not(windows))]
    {
        let _ = bytes;
        Err("Cookie persistence needs a platform credential protector".into())
    }
}
fn unprotect_cookies(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let bytes = bytes
        .strip_prefix(COOKIE_MAGIC)
        .ok_or("Unrecognized cookie store format")?;
    #[cfg(windows)]
    {
        crypt_cookie_bytes(bytes, false)
    }
    #[cfg(not(windows))]
    {
        let _ = bytes;
        Err("Cookie persistence needs a platform credential protector".into())
    }
}
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = path.with_extension("pending");
    let mut file = File::create(&temp).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(temp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temporary() -> PathBuf {
        let token = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/profile-tests")
            .join(format!("{}-{token}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn clean(path: &Path) {
        for name in [
            "session.json",
            "profile.lock",
            "session.pending",
            "Cookies.v1.dpapi",
            "Cookies.v1.pending",
        ] {
            let file = path.join(name);
            if file.exists() {
                fs::remove_file(file).unwrap();
            }
        }
        fs::remove_dir(path).unwrap();
    }
    #[test]
    fn session_survives_replacement_and_reopen() {
        let path = temporary();
        let (profile, _) = Profile::open(path.clone()).unwrap();
        profile
            .save(&Session {
                schema: 1,
                tabs: vec!["about:demo".into()],
                active: 0,
            })
            .unwrap();
        profile
            .save(&Session {
                schema: 1,
                tabs: vec!["https://example.com/".into(), "about:home".into()],
                active: 1,
            })
            .unwrap();
        drop(profile);
        let (profile, restored) = Profile::open(path.clone()).unwrap();
        assert_eq!(restored.tabs, vec!["https://example.com/", "about:home"]);
        assert_eq!(restored.active, 1);
        drop(profile);
        clean(&path);
    }
    #[test]
    fn concurrent_profile_is_rejected() {
        let path = temporary();
        let (profile, _) = Profile::open(path.clone()).unwrap();
        assert!(Profile::open(path.clone()).is_err());
        drop(profile);
        clean(&path);
    }
    #[test]
    fn unknown_schema_is_preserved() {
        let path = temporary();
        let original = br#"{"schema":999,"tabs":["https://example.com/"],"active":0}"#;
        fs::write(path.join("session.json"), original).unwrap();
        assert!(Profile::open(path.clone()).is_err());
        assert_eq!(fs::read(path.join("session.json")).unwrap(), original);
        clean(&path);
    }
    #[cfg(windows)]
    #[test]
    fn persistent_cookies_are_account_protected_and_session_cookies_end() {
        let path = temporary();
        let address = url::Url::parse("https://example.test/").unwrap();
        let context = rust_browser_engine::cookies::CookieContext::subresource(&address);
        let (profile, _) = Profile::open(path.clone()).unwrap();
        assert!(
            profile
                .cookies()
                .receive(&address, b"session=temporary", &context)
        );
        assert!(profile.cookies().receive(
            &address,
            b"auth=qa-private-cookie; Secure; HttpOnly; Max-Age=3600",
            &context
        ));
        profile.save_cookies().unwrap();
        let protected = fs::read(path.join("Cookies.v1.dpapi")).unwrap();
        assert!(protected.starts_with(COOKIE_MAGIC));
        assert!(
            !protected
                .windows(b"qa-private-cookie".len())
                .any(|bytes| bytes == b"qa-private-cookie")
        );
        drop(profile);
        let (profile, _) = Profile::open(path.clone()).unwrap();
        assert_eq!(
            profile.cookies().header(&address, &context),
            b"auth=qa-private-cookie"
        );
        assert!(profile.cookies().document_cookie(&address).is_empty());
        drop(profile);
        clean(&path);
    }
    #[cfg(windows)]
    #[test]
    fn damaged_protected_store_fails_without_rewriting_original_bytes() {
        let path = temporary();
        let mut protected = protect_cookies(br#"{"schema":1,"cookies":[]}"#).unwrap();
        let last = protected.len() - 1;
        protected[last] ^= 64;
        fs::write(path.join("Cookies.v1.dpapi"), &protected).unwrap();
        assert!(Profile::open(path.clone()).is_err());
        assert_eq!(fs::read(path.join("Cookies.v1.dpapi")).unwrap(), protected);
        clean(&path);
    }
}
