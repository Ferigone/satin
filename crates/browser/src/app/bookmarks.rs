use crate::profile::atomic_write;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::File,
    io::{ErrorKind, Read},
    path::PathBuf,
};
use url::Url;

pub(super) const FILE_NAME: &str = "bookmarks.json";
const SCHEMA: u32 = 1;
const MAX_ENTRIES: usize = 128;
const MAX_URL_BYTES: usize = 8192;
pub(super) const MAX_TITLE_CHARS: usize = 120;
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct Bookmark {
    pub url: Url,
    pub title: String,
}

// Serialize URL values as strings without requiring the url crate's serde feature.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskBookmark {
    url: String,
    title: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BookmarkFile {
    schema: u32,
    entries: Vec<DiskBookmark>,
}

pub(super) struct Bookmarks {
    path: PathBuf,
    entries: Vec<Bookmark>,
}

pub(super) fn page_url(input: &str) -> Option<Url> {
    if input.len() > MAX_URL_BYTES
        || input.contains('\\')
        || input.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    let (_, rest) = input.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let url = Url::parse(input).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.has_host()
        && url.as_str().len() <= MAX_URL_BYTES)
        .then_some(url)
}

fn page_title(input: &str) -> String {
    // A document title is untrusted text shown in native chrome.
    let mut title = String::new();
    let mut count = 0;
    let mut space = false;
    for c in input.chars() {
        if count >= MAX_TITLE_CHARS {
            break;
        }
        if c.is_whitespace() {
            space = count > 0;
        } else if !c.is_control()
            && !matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            if space {
                if count + 1 >= MAX_TITLE_CHARS {
                    break;
                }
                title.push(' ');
                count += 1;
                space = false;
            }
            title.push(c);
            count += 1;
        }
    }
    title
}

impl Bookmarks {
    // Call while holding the owning profile's lock.
    pub(super) fn open(path: PathBuf) -> Result<Self, String> {
        let entries = match File::open(&path) {
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.to_string()),
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take((MAX_FILE_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())?;
                if bytes.len() > MAX_FILE_BYTES {
                    return Err("Bookmarks file exceeds its size budget".into());
                }
                let disk: BookmarkFile =
                    serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                if disk.schema != SCHEMA || disk.entries.len() > MAX_ENTRIES {
                    return Err("Unsupported bookmarks file".into());
                }
                let mut seen = HashSet::new();
                let mut entries = Vec::new();
                for entry in disk.entries {
                    let url = page_url(&entry.url).ok_or("Invalid bookmark URL")?;
                    if !seen.insert(url.clone()) {
                        return Err("Duplicate bookmark URL in file".into());
                    }
                    entries.push(Bookmark {
                        url,
                        title: page_title(&entry.title),
                    });
                }
                entries
            }
        };
        Ok(Self { path, entries })
    }

    pub(super) fn entries(&self) -> &[Bookmark] {
        &self.entries
    }

    pub(super) fn contains(&self, url: &Url) -> bool {
        self.entries.iter().any(|entry| &entry.url == url)
    }

    pub(super) fn add(&mut self, address: &str, title: &str) -> Result<bool, String> {
        let url = page_url(address)
            .ok_or("Only HTTP(S) pages without URL credentials can be bookmarked.")?;
        if self.contains(&url) {
            return Ok(false);
        }
        if self.entries.len() >= MAX_ENTRIES {
            return Err("Bookmarks are full. Remove a bookmark to save another.".into());
        }
        let mut entries = self.entries.clone();
        entries.insert(
            0,
            Bookmark {
                url,
                title: page_title(title),
            },
        );
        self.commit(entries)?;
        Ok(true)
    }

    pub(super) fn remove(&mut self, url: &Url) -> Result<bool, String> {
        let Some(index) = self.entries.iter().position(|entry| &entry.url == url) else {
            return Ok(false);
        };
        let mut entries = self.entries.clone();
        entries.remove(index);
        self.commit(entries)?;
        Ok(true)
    }

    pub(super) fn rename(&mut self, url: &Url, input: &str) -> Result<(), String> {
        let index = self
            .entries
            .iter()
            .position(|entry| &entry.url == url)
            .ok_or("Bookmark no longer exists.")?;
        let title = page_title(input);
        if title.is_empty() {
            return Err("Enter a bookmark name.".into());
        }
        if title == self.entries[index].title {
            return Ok(());
        }
        let mut entries = self.entries.clone();
        entries[index].title = title;
        self.commit(entries)
    }

    fn commit(&mut self, entries: Vec<Bookmark>) -> Result<(), String> {
        let disk = BookmarkFile {
            schema: SCHEMA,
            entries: entries
                .iter()
                .map(|entry| DiskBookmark {
                    url: entry.url.as_str().into(),
                    title: entry.title.clone(),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&disk).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err("Bookmarks file exceeds its size budget.".into());
        }
        atomic_write(&self.path, &bytes)
            .map_err(|_| "Could not write bookmarks. No changes were made.".to_owned())?;
        self.entries = entries;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
    };

    const ALPHA: &str = "http://127.0.0.1:8099/alpha.html";
    const BETA: &str = "http://127.0.0.1:8099/beta.html?edition=2#intro";

    struct DisposableProfile(PathBuf);

    impl DisposableProfile {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bookmark-tests");
            fs::create_dir_all(&base).unwrap();
            loop {
                let path = base.join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("{error}"),
                }
            }
        }

        fn bookmarks(&self) -> PathBuf {
            self.0.join(FILE_NAME)
        }
    }

    impl Drop for DisposableProfile {
        fn drop(&mut self) {
            for file in [self.bookmarks(), self.bookmarks().with_extension("pending")] {
                let _ = fs::remove_file(file);
            }
            let _ = fs::remove_dir(&self.0);
        }
    }

    #[test]
    fn two_pages_survive_reopen_and_removal_does_not_touch_other_profile_data() {
        let profile = DisposableProfile::new();
        let path = profile.bookmarks();
        let session = profile.0.join("session.json");
        fs::write(&session, "untouched session").unwrap();
        let mut bookmarks = Bookmarks::open(path.clone()).unwrap();
        assert!(bookmarks.entries().is_empty());
        assert!(!path.exists());
        assert!(bookmarks.add(ALPHA, "Authored page Alpha").unwrap());
        assert!(
            bookmarks
                .add(BETA, " Authored \u{202e}page  Beta ")
                .unwrap()
        );
        assert!(
            !bookmarks
                .add("HTTP://127.0.0.1:8099/alpha.html", "Not a rename")
                .unwrap()
        );
        let mut bookmarks = Bookmarks::open(path.clone()).unwrap();
        assert_eq!(
            bookmarks
                .entries()
                .iter()
                .map(|entry| (entry.url.as_str(), entry.title.as_str()))
                .collect::<Vec<_>>(),
            [(BETA, "Authored page Beta"), (ALPHA, "Authored page Alpha")]
        );
        assert!(bookmarks.remove(&page_url(ALPHA).unwrap()).unwrap());
        assert!(!bookmarks.remove(&page_url(ALPHA).unwrap()).unwrap());
        let bookmarks = Bookmarks::open(path).unwrap();
        assert_eq!(bookmarks.entries().len(), 1);
        assert!(bookmarks.contains(&page_url(BETA).unwrap()));
        assert_eq!(fs::read_to_string(&session).unwrap(), "untouched session");
        fs::remove_file(session).unwrap();
    }

    #[test]
    fn only_absolute_credential_free_web_urls_are_eligible() {
        for url in [
            "about:home",
            "file:///alpha.html",
            "javascript:void(0)",
            "data:text/html,a",
            "https://user:secret@example.test/",
            "https://@example.test/",
            "https:///alpha.html",
            "http:example.test",
            "//example.test/",
            "https://example.test\\alpha",
            "https://ex ample.test/",
            "https://exa\nmple.test/",
        ] {
            assert!(page_url(url).is_none(), "unexpectedly accepted {url}");
        }
        assert_eq!(
            page_url("HTTPS://EXAMPLE.test:443/section?edition=2#intro")
                .unwrap()
                .as_str(),
            "https://example.test/section?edition=2#intro"
        );
    }

    #[test]
    fn invalid_or_newer_files_are_not_rewritten() {
        let profile = DisposableProfile::new();
        for contents in [
            r#"{"schema":2,"entries":[]}"#,
            r#"{"schema":1,"entries":[{"url":"javascript:void(0)","title":"Bad"}]}"#,
            r#"{"schema":1,"entries":[{"url":"http://example.test:80","title":"A"},{"url":"http://EXAMPLE.test/","title":"B"}]}"#,
            "{broken",
        ] {
            let path = profile.bookmarks();
            fs::write(&path, contents).unwrap();
            assert!(Bookmarks::open(path.clone()).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), contents);
        }
    }

    #[test]
    fn failed_save_does_not_publish_in_memory_changes() {
        let profile = DisposableProfile::new();
        let path = profile.bookmarks();
        let mut bookmarks = Bookmarks::open(path.clone()).unwrap();
        bookmarks.add(ALPHA, "Authored page Alpha").unwrap();
        let pending = path.with_extension("pending");
        fs::create_dir(&pending).unwrap();
        assert!(bookmarks.add(BETA, "Authored page Beta").is_err());
        assert!(!bookmarks.contains(&page_url(BETA).unwrap()));
        assert!(bookmarks.remove(&page_url(ALPHA).unwrap()).is_err());
        assert!(bookmarks.contains(&page_url(ALPHA).unwrap()));
        assert!(
            bookmarks
                .rename(&page_url(ALPHA).unwrap(), "Not published")
                .is_err()
        );
        assert_eq!(bookmarks.entries()[0].title, "Authored page Alpha");
        assert!(
            Bookmarks::open(path)
                .unwrap()
                .contains(&page_url(ALPHA).unwrap())
        );
        fs::remove_dir(pending).unwrap();
    }

    #[test]
    fn rename_reuses_title_validation_and_keeps_url_and_order() {
        let profile = DisposableProfile::new();
        let path = profile.bookmarks();
        let mut bookmarks = Bookmarks::open(path.clone()).unwrap();
        bookmarks.add(ALPHA, "Alpha").unwrap();
        bookmarks.add(BETA, "Beta").unwrap();
        let url = page_url(ALPHA).unwrap();
        assert!(bookmarks.rename(&url, "  \u{202e}  ").is_err());
        bookmarks.rename(&url, "  Nowy \u{202e} tytuł  ").unwrap();
        let bookmarks = Bookmarks::open(path).unwrap();
        assert_eq!(bookmarks.entries()[0].url.as_str(), BETA);
        assert_eq!(bookmarks.entries()[1].url, url);
        assert_eq!(bookmarks.entries()[1].title, "Nowy tytuł");
    }
}
