//! Opt-in metadata tap for HTTP(S) jobs. It observes loader calls and their
//! visible outcomes, not individual wire exchanges.
//!
//! Jobs skipped before entering the loader, redirect hops, CORS preflights,
//! and traffic that does not use these workers are not observed. Dropped
//! events or worker shutdown can leave a lifecycle incomplete.
use rust_browser_engine::{
    loader::{HttpContentMetadata, ResourceKind},
    network::WebResponse,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use url::{Position, Url};

pub(crate) const CAPACITY: usize = 256;
const MAX_URL_BYTES: usize = 2048;
const MAX_METHOD_BYTES: usize = 64;

/// IDs are local to their task kind and document, not global request IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub tab: u64,
    pub navigation: u64,
    pub request: Request,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Request {
    Navigate,
    Download(u64),
    Resource(u64),
    /// A script Fetch job. There is no distinct XHR marker at this boundary.
    Fetch(u64),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Event {
    pub key: Key,
    /// Wall time since entering the loader path: not wire or socket timing.
    pub elapsed: Duration,
    pub started: Instant,
    pub phase: Phase,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Initial request, not proof a socket was used. `None` is an invalid or
    /// overlong display method. The URL is already redacted and length capped.
    Started {
        method: Option<String>,
        url: String,
        resource_type: Option<&'static str>,
    },
    /// Only the final (or Fetch-exposed) response. Header arrival may be
    /// unavailable for buffered policy-checked resources.
    Response {
        status: Option<u16>,
        headers_at: Option<Instant>,
        details: ResponseDetails,
    },
    Finished {
        body_bytes: Option<u64>,
    },
    /// Loader/worker errors are deliberately not included as raw strings.
    Failed,
    Cancelled,
}

/// Only allowlisted, bounded display fields cross the observer queue.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ResponseDetails {
    pub final_url: Option<String>,
    pub mime: Option<String>,
    pub declared_bytes: Option<u64>,
    pub body_bytes: Option<u64>,
    pub response_type: Option<&'static str>,
}

impl ResponseDetails {
    pub(super) fn from_url(url: &str) -> Self {
        let final_url = Url::parse(url)
            .ok()
            .filter(is_http)
            .map(|url| display_url(&url));
        Self {
            final_url,
            ..Self::default()
        }
    }

    pub(super) fn from_fetch(response: &WebResponse) -> Self {
        let mut details = Self::from_url(&response.url);
        details.response_type = Some(response.response_type.as_str());
        let content = HttpContentMetadata::from_exposed_headers(&response.headers);
        details.mime = content.mime;
        details.declared_bytes = content.declared_bytes;
        details
    }
}

pub(super) fn resource_label(kind: &ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Stylesheet => "Stylesheet",
        ResourceKind::ClassicScript => "Script",
        ResourceKind::ModuleScript => "Module",
        ResourceKind::Image => "Image",
        ResourceKind::Media => "Media",
        ResourceKind::Font | ResourceKind::LocalFont(_) => "Font",
    }
}

/// Consumers must filter by tab and navigation before storing or displaying.
/// Jobs use the existing browser wake to make these observations visible.
pub(crate) struct Receiver {
    rx: mpsc::Receiver<Event>,
    dropped: Arc<AtomicUsize>,
}

impl Receiver {
    pub(crate) fn try_recv(&mut self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// Number of observations lost to a full queue since the last read, across
    /// all tabs/documents. Do not attribute this to a particular document.
    pub(crate) fn take_dropped(&self) -> usize {
        self.dropped.swap(0, Ordering::Relaxed)
    }
}

#[derive(Clone)]
pub(super) struct Observer {
    tx: mpsc::Sender<Event>,
    dropped: Arc<AtomicUsize>,
}

pub(super) fn channel() -> (Observer, Receiver) {
    let (tx, rx) = mpsc::channel(CAPACITY);
    let dropped = Arc::new(AtomicUsize::new(0));
    (
        Observer {
            tx,
            dropped: dropped.clone(),
        },
        Receiver { rx, dropped },
    )
}

impl Observer {
    fn record(&self, key: Key, started: Instant, elapsed: Duration, phase: impl FnOnce() -> Phase) {
        // Reserve without waiting, even if no one drains the observer. Format
        // strings only after reserving space; never enqueue raw request data.
        match self.tx.try_reserve() {
            Ok(slot) => slot.send(Event {
                key,
                elapsed,
                started,
                phase: phase(),
            }),
            Err(mpsc::error::TrySendError::Full(())) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Closed(())) => {}
        }
    }

    pub(super) fn begin(
        &self,
        key: Key,
        method: &str,
        url: &Url,
        resource_type: Option<&'static str>,
    ) -> Option<Trace> {
        if !is_http(url) || self.tx.is_closed() {
            return None;
        }
        let trace = Trace {
            observer: self.clone(),
            key,
            started: Instant::now(),
        };
        self.record(key, trace.started, Duration::ZERO, || Phase::Started {
            method: display_method(method),
            url: display_url(url),
            resource_type,
        });
        Some(trace)
    }
}

#[derive(Clone)]
pub(super) struct Trace {
    observer: Observer,
    key: Key,
    pub(super) started: Instant,
}

impl Trace {
    pub(super) fn record(&self, phase: Phase) {
        self.observer
            .record(self.key, self.started, self.started.elapsed(), || phase);
    }
}

fn is_http(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

fn display_url(url: &Url) -> String {
    // This slice omits userinfo, query and fragment without ever cloning them.
    // Callers restrict the scheme so this cannot expose, e.g., a data: body.
    let prefix = &url[..Position::BeforeUsername];
    let rest = &url[Position::BeforeHost..Position::AfterPath];
    let end = rest.floor_char_boundary(MAX_URL_BYTES - prefix.len());
    let mut display = String::with_capacity(prefix.len() + end);
    display.push_str(prefix);
    display.push_str(&rest[..end]);
    display
}

fn display_method(method: &str) -> Option<String> {
    if method.is_empty()
        || method.len() > MAX_METHOD_BYTES
        || !method
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    {
        return None;
    }
    Some(method.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(request: Request) -> Key {
        Key {
            tab: 2,
            navigation: 3,
            request,
        }
    }

    #[test]
    fn display_metadata_is_redacted_and_bounded() {
        let url = Url::parse("https://user:password@[::1]:8080/a%3Fb?q=secret#private").unwrap();
        assert_eq!(display_url(&url), "https://[::1]:8080/a%3Fb");
        let url = Url::parse("https://user@example.test/?secret#private").unwrap();
        assert_eq!(display_url(&url), "https://example.test/");
        let url = Url::parse(&format!("http://example.test/{}", "x".repeat(4096))).unwrap();
        assert_eq!(display_url(&url).len(), MAX_URL_BYTES);
        assert_eq!(display_method("PATCH").as_deref(), Some("PATCH"));
        assert!(display_method("bad\nmethod").is_none());
        assert!(display_method(&"X".repeat(MAX_METHOD_BYTES + 1)).is_none());
    }

    #[test]
    fn response_metadata_keeps_only_bounded_display_fields() {
        use rust_browser_engine::network::ResponseType;
        let response = WebResponse {
            url: "https://user:private@example.test/route?token=private#private".into(),
            status: 200,
            status_text: "private".into(),
            headers: vec![
                ("content-type".into(), "Text/HTML; boundary=private".into()),
                ("content-length".into(), "23".into()),
                ("set-cookie".into(), "private".into()),
                ("authorization".into(), "private".into()),
                ("x-not-displayed".into(), "private".into()),
            ],
            body: Some(Arc::new(b"private".to_vec())),
            response_type: ResponseType::Basic,
            redirected: true,
        };
        let details = ResponseDetails::from_fetch(&response);
        assert_eq!(details.mime.as_deref(), Some("text/html"));
        assert_eq!(details.declared_bytes, Some(23));
        assert_eq!(
            details.final_url.as_deref(),
            Some("https://example.test/route")
        );
        assert!(!format!("{details:?}").contains("private"));
        for invalid in ["+12", "-2", "1, 2", "18446744073709551616"] {
            assert!(
                HttpContentMetadata::from_fields(None, Some(invalid))
                    .declared_bytes
                    .is_none()
            );
        }
    }

    #[test]
    fn overflow_and_disconnect_never_require_the_consumer() {
        let (observer, mut receiver) = channel();
        let key = key(Request::Fetch(1));
        let url = Url::parse("https://example.test/").unwrap();
        let trace = observer.begin(key, "GET", &url, None).unwrap();
        for _ in 1..CAPACITY {
            trace.record(Phase::Finished {
                body_bytes: Some(7),
            });
        }
        // The phase closure must not run when the queue is full or closed.
        observer.record(key, Instant::now(), Duration::ZERO, || {
            panic!("full queue formatted a phase")
        });
        assert_eq!(receiver.take_dropped(), 1);
        assert_eq!(receiver.take_dropped(), 0);
        assert!(matches!(
            receiver.try_recv().unwrap().phase,
            Phase::Started { .. }
        ));
        trace.record(Phase::Cancelled);
        for _ in 1..CAPACITY {
            assert_eq!(
                receiver.try_recv().unwrap().phase,
                Phase::Finished {
                    body_bytes: Some(7)
                }
            );
        }
        assert_eq!(receiver.try_recv().unwrap().phase, Phase::Cancelled);
        assert!(receiver.try_recv().is_none());
        drop(receiver);
        observer.record(key, Instant::now(), Duration::ZERO, || {
            panic!("closed queue formatted a phase")
        });
    }

    #[test]
    fn local_or_inline_work_is_not_published() {
        let (observer, mut receiver) = channel();
        for address in ["data:text/plain,secret", "file:///private", "about:home"] {
            let url = Url::parse(address).unwrap();
            assert!(
                observer
                    .begin(key(Request::Navigate), "GET", &url, None)
                    .is_none()
            );
            assert!(
                observer
                    .begin(key(Request::Resource(1)), "GET", &url, Some("Image"))
                    .is_none()
            );
        }
        assert!(receiver.try_recv().is_none());
        assert_eq!(receiver.take_dropped(), 0);
    }
}
