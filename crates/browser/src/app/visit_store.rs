//! Bounded, local history for committed top-level navigations, not tab history.
use crate::profile::atomic_write;
use rust_browser_engine::LoadedPage;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fmt::Write as _,
    fs::File,
    io::{ErrorKind, Read},
    path::PathBuf,
};
use url::Url;

pub(super) const FILE_NAME: &str = "Visits.v1.json";
pub(super) const PAGE_URL: &str = "about:history";
const CONFIRM: &str = "about:history-confirm";
const CANCEL: &str = "about:history-cancel";
const CLEAR: &str = "about:history-clear";
const MAX_ENTRIES: usize = 200;
const MAX_URL_BYTES: usize = 2048;
const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Visit {
    pub(super) url: String,
    pub(super) title: String,
    pub(super) visited_at: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Disk {
    schema: u32,
    entries: Vec<Visit>,
}

pub(super) struct Visits {
    file: PathBuf,
    entries: Vec<Visit>,
}

fn address(raw: &str) -> Option<String> {
    if raw.len() > MAX_URL_BYTES || raw.chars().any(char::is_control) {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    // Do not persist parameters, credentials or fragments; stripping them
    // would make the History link point to a different address.
    (matches!(url.scheme(), "http" | "https")
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.as_str().len() <= MAX_URL_BYTES)
        .then(|| url.into())
}

impl Visits {
    pub(super) fn open(file: PathBuf) -> Result<Self, String> {
        let entries = match File::open(&file) {
            Ok(input) => {
                let mut bytes = Vec::new();
                input
                    .take(MAX_FILE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())?;
                if bytes.len() as u64 > MAX_FILE_BYTES {
                    return Err("Plik historii przekracza limit; zachowano oryginał.".into());
                }
                let disk: Disk =
                    serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                let mut urls = HashSet::new();
                if disk.schema != 1
                    || disk.entries.len() > MAX_ENTRIES
                    || !disk.entries.iter().all(|visit| {
                        address(&visit.url).as_deref() == Some(visit.url.as_str())
                            && super::bookmarks::page_title(&visit.title) == visit.title
                            && urls.insert(visit.url.as_str())
                    })
                {
                    return Err("Nieobsługiwany plik historii; zachowano oryginał.".into());
                }
                disk.entries
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.to_string()),
        };
        Ok(Self { file, entries })
    }

    pub(super) fn entries(&self) -> &[Visit] {
        &self.entries
    }

    pub(super) fn record(&mut self, url: &str, title: &str, visited_at: u64) -> Result<(), String> {
        let Some(url) = address(url) else {
            return Ok(());
        };
        let mut entries = self.entries.clone();
        entries.retain(|visit| visit.url != url);
        entries.insert(
            0,
            Visit {
                url,
                title: super::bookmarks::page_title(title),
                visited_at,
            },
        );
        entries.truncate(MAX_ENTRIES);
        self.replace(entries)
    }

    pub(super) fn remove(&mut self, url: &str) -> Result<(), String> {
        let mut entries = self.entries.clone();
        entries.retain(|visit| visit.url != url);
        self.replace(entries)
    }

    pub(super) fn clear(&mut self) -> Result<(), String> {
        self.replace(Vec::new())
    }

    fn replace(&mut self, entries: Vec<Visit>) -> Result<(), String> {
        if self.entries == entries {
            return Ok(());
        }
        let disk = Disk { schema: 1, entries };
        let bytes = serde_json::to_vec(&disk).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("Historia przekracza limit zapisu.".into());
        }
        atomic_write(&self.file, &bytes)?;
        self.entries = disk.entries;
        Ok(())
    }
}

pub(super) enum HistoryClick {
    Confirm,
    Cancel,
    Clear,
    Remove(String),
}

fn remove_link(url: &str) -> String {
    let mut encoded = url::form_urlencoded::Serializer::new(String::new());
    encoded.append_pair("url", url);
    format!("about:history-remove?{}", encoded.finish())
}

impl HistoryClick {
    pub(super) fn from_link(link: &str) -> Option<Self> {
        match link {
            CONFIRM => Some(Self::Confirm),
            CANCEL => Some(Self::Cancel),
            CLEAR => Some(Self::Clear),
            _ => {
                let url = Url::parse(link).ok()?;
                if url.scheme() != "about" || url.path() != "history-remove" {
                    return None;
                }
                let (_, target) = url.query_pairs().next()?;
                let target = address(&target)?;
                (remove_link(&target) == link).then_some(Self::Remove(target))
            }
        }
    }
}

fn escaped(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&#39;"),
            _ => result.push(c),
        }
    }
    result
}

pub(super) fn page(visits: Option<&Visits>, confirm: bool) -> LoadedPage {
    let mut html = String::from(
        r#"<!doctype html><html lang="pl"><meta charset="utf-8">
<title>Historia</title><style>
body{margin:0;background:#19191c;color:#f0edf0;font:14px 'Segoe UI Variable Text','Segoe UI',sans-serif}
main{width:calc(100% - 48px);max-width:896px;margin:0 auto;padding:clamp(18px,4vw,50px) 0}
header{display:flex;align-items:center;justify-content:space-between;gap:14px;margin-bottom:23px}
h1{font-size:clamp(27px,4vw,45px);letter-spacing:-.046em;font-weight:600;margin:0;line-height:1.07}
header p{margin:8px 0 0;color:#b8afb5}
ul{background:#222125;border:1px solid #3e3a42;border-radius:12px;padding:0;margin:0;list-style:none}
li{display:flex;align-items:center;gap:10px;border-bottom:1px solid #3e3a42;padding:8px 10px;min-height:36px}
li:last-child{border-bottom:0}
a{color:inherit;text-decoration:none}
a:hover,a:focus{color:#c4a3f4}
.entry{display:block;flex:1;min-width:0}
.name,.url{display:block;overflow:hidden;white-space:nowrap;text-overflow:ellipsis}
.url,time,.empty{font-size:11px;color:#b8afb5}
.button{padding:7px 11px;border:1px solid #3e3a42;border-radius:7px;white-space:nowrap}
.empty{padding:10px;margin:0}.confirm{border:1px solid #3e3a42;border-radius:12px;padding:14px;margin-bottom:12px}
@media(max-width:620px){main{width:calc(100% - 28px)}time{display:none}}
</style><main><header><div><h1>Historia</h1><p>Ostatnio odwiedzone strony</p></div>"#,
    );
    if visits.is_some_and(|data| !data.entries().is_empty()) && !confirm {
        let _ = write!(
            html,
            r#"<a class="button" id="history-clear" href="{CONFIRM}">Wyczyść</a>"#
        );
    }
    html.push_str("</header>");
    if confirm {
        let _ = write!(
            html,
            r#"<section class="confirm"><p>Wyczyścić całą historię odwiedzin?</p>
<a class="button" id="history-cancel" href="{CANCEL}">Anuluj</a>
<a class="button" id="history-confirm" href="{CLEAR}">Wyczyść historię</a></section>"#
        );
    }
    if let Some(visits) = visits {
        html.push_str("<ul>");
        if visits.entries().is_empty() {
            html.push_str(r#"<li class="empty">Historia jest pusta.</li>"#);
        }
        for (index, visit) in visits.entries().iter().enumerate() {
            let url = escaped(&visit.url);
            let title = if visit.title.is_empty() {
                url.clone()
            } else {
                escaped(&visit.title)
            };
            let remove = remove_link(&visit.url);
            let hour = (visit.visited_at / 3600) % 24;
            let minute = (visit.visited_at / 60) % 60;
            let _ = write!(
                html,
                r#"<li><a class="entry" href="{url}"><span class="name">{title}</span>
<span class="url">{url}</span></a><time>{hour:02}:{minute:02} UTC</time>
<a class="button" id="history-remove-{n}" title="Usuń wpis" aria-label="Usuń wpis {n}" href="{remove}">×</a></li>"#,
                n = index + 1
            );
        }
        html.push_str("</ul>");
    } else {
        html.push_str(r#"<p class="empty">Nie można odczytać historii. Zapisane dane pozostają bez zmian.</p>"#);
    }
    html.push_str("</main>");
    // No script realm for this internal page. Only its trusted document may
    // dispatch commands through the native click path.
    LoadedPage::from_html(PAGE_URL, &html)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir =
                std::env::temp_dir().join(format!("satin-visit-{}-{time}", std::process::id()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join(FILE_NAME)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.file());
            let _ = fs::remove_dir(&self.0);
        }
    }

    #[test]
    fn admits_only_reopenable_http_pages_without_private_parameters() {
        assert_eq!(
            address("https://example.com").as_deref(),
            Some("https://example.com/")
        );
        for invalid in [
            "https://example.com/a?key=secret",
            "https://example.com/a#private",
            "https://login:abc@example.com/a",
            "file:///C:/report",
            "about:home",
        ] {
            assert_eq!(address(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn saves_latest_title_orders_by_visit_and_persists_removal_and_clear() {
        let dir = Scratch::new();
        let mut store = Visits::open(dir.file()).unwrap();
        let a = "https://example.com/a";
        let b = "https://example.com/b";
        store.record(a, "Pierwsza", 1).unwrap();
        store.record(b, "Druga", 2).unwrap();
        store.record(a, "Nowa  nazwa\n", 3).unwrap();
        let mut store = Visits::open(dir.file()).unwrap();
        assert_eq!(
            store
                .entries()
                .iter()
                .map(|row| row.url.as_str())
                .collect::<Vec<_>>(),
            [a, b]
        );
        assert_eq!(store.entries()[0].title, "Nowa nazwa");
        assert_eq!(store.entries()[0].visited_at, 3);
        store.remove(a).unwrap();
        assert_eq!(Visits::open(dir.file()).unwrap().entries()[0].url, b);
        store.clear().unwrap();
        assert!(Visits::open(dir.file()).unwrap().entries().is_empty());
    }

    #[test]
    fn rejects_unknown_or_corrupt_files_without_rewriting_them() {
        let dir = Scratch::new();
        let path = dir.file();
        for original in [b"invalid".as_slice(), br#"{"schema":2,"entries":[]}"#] {
            fs::write(&path, original).unwrap();
            assert!(Visits::open(path.clone()).is_err());
            assert_eq!(fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn matches_only_canonical_actions_and_escapes_visited_text() {
        assert!(matches!(
            HistoryClick::from_link(CONFIRM),
            Some(HistoryClick::Confirm)
        ));
        let url = "https://example.com/a%22";
        assert!(
            matches!(HistoryClick::from_link(&remove_link(url)), Some(HistoryClick::Remove(v)) if v == url)
        );
        assert!(HistoryClick::from_link("about:history-remove?url=file%3A%2F%2F%2Fa").is_none());
        assert!(HistoryClick::from_link(&format!("{}&other=1", remove_link(url))).is_none());
        assert_eq!(escaped("<&\"'"), "&lt;&amp;&quot;&#39;");
    }

    #[test]
    fn document_only_exposes_clear_after_confirmation() {
        let dir = Scratch::new();
        let mut store = Visits::open(dir.file()).unwrap();
        store
            .record("https://example.com/ok", "<script>danger</script>", 0)
            .unwrap();
        for (confirmation, link) in [(false, CONFIRM), (true, CLEAR)] {
            let history = page(Some(&store), confirmation);
            let hrefs = history
                .document
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(index, _)| {
                    history
                        .document
                        .attr(rust_browser_engine::dom::NodeId(index), "href")
                })
                .collect::<Vec<_>>();
            assert!(hrefs.contains(&link));
            assert_eq!(hrefs.contains(&CLEAR), confirmation);
            assert!(hrefs.contains(&"https://example.com/ok"));
            assert_eq!(history.scripts.len(), 0);
        }
    }
}
