//! Opt-in metadata tap for HTTP(S) jobs. It observes loader calls and their
//! visible outcomes, not individual wire exchanges.
//!
//! Jobs skipped before entering the loader, redirect hops, CORS preflights,
//! and traffic that does not use these workers are not observed. Dropped
//! events or worker shutdown can leave a lifecycle incomplete.
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
    Resource(u64),
    /// A script Fetch job. There is no distinct XHR marker at this boundary.
    Fetch(u64),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Event {
    pub key: Key,
    /// Wall time since entering the loader path: not wire or socket timing.
    pub elapsed: Duration,
    pub phase: Phase,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Initial request, not proof a socket was used. `None` is an invalid or
    /// overlong display method. The URL is already redacted and length capped.
    Started { method: Option<String>, url: String },
    /// The loader emitted Open, not necessarily at header arrival. Neither
    /// final URL nor HTTP status is exposed on OpenedPage at this boundary.
    NavigationOpened,
    /// The navigation loader returned successfully, not DOM/load completion.
    NavigationFinished,
    /// The exposed Fetch response, not body completion. Opaque status is None.
    FetchResponse { status: Option<u16> },
    /// One-shot event from a successful GET resource load. Only the sanitized
    /// final URL is visible; the request URL, subtype and HTTP status are not.
    ResourceLoaded { url: String },
    /// Loader/worker errors are deliberately not included as raw strings.
    Failed,
    Cancelled,
}

/// Consumers must filter by tab and navigation before storing or displaying.
/// The tap does not wake the UI; drain opportunistically and poll if pending
/// jobs or events without a corresponding page wake must be visible promptly.
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
    fn record(&self, key: Key, elapsed: Duration, phase: impl FnOnce() -> Phase) {
        // Reserve without waiting, even if no one drains the observer. Format
        // strings only after reserving space; never enqueue raw request data.
        match self.tx.try_reserve() {
            Ok(slot) => slot.send(Event {
                key,
                elapsed,
                phase: phase(),
            }),
            Err(mpsc::error::TrySendError::Full(())) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Closed(())) => {}
        }
    }

    pub(super) fn begin(&self, key: Key, method: &str, url: &Url) -> Option<Trace<'_>> {
        if !is_http(url) || self.tx.is_closed() {
            return None;
        }
        let trace = Trace {
            observer: self,
            key,
            started: Instant::now(),
        };
        self.record(key, Duration::ZERO, || Phase::Started {
            method: display_method(method),
            url: display_url(url),
        });
        Some(trace)
    }

    pub(super) fn resource_loaded(&self, key: Key, started: Instant, url: &Url) {
        if is_http(url) {
            self.record(key, started.elapsed(), || Phase::ResourceLoaded {
                url: display_url(url),
            });
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Trace<'a> {
    observer: &'a Observer,
    key: Key,
    started: Instant,
}

impl Trace<'_> {
    pub(super) fn record(self, phase: Phase) {
        self.observer
            .record(self.key, self.started.elapsed(), || phase);
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
    fn overflow_and_disconnect_never_require_the_consumer() {
        let (observer, mut receiver) = channel();
        let key = key(Request::Fetch(1));
        let url = Url::parse("https://example.test/").unwrap();
        let trace = observer.begin(key, "GET", &url).unwrap();
        for _ in 1..CAPACITY {
            trace.record(Phase::FetchResponse { status: Some(200) });
        }
        // The phase closure must not run when the queue is full or closed.
        observer.record(key, Duration::ZERO, || panic!("full queue formatted a phase"));
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
                Phase::FetchResponse { status: Some(200) }
            );
        }
        assert_eq!(receiver.try_recv().unwrap().phase, Phase::Cancelled);
        assert!(receiver.try_recv().is_none());
        drop(receiver);
        observer.record(key, Duration::ZERO, || {
            panic!("closed queue formatted a phase")
        });
    }

    #[test]
    fn local_or_inline_work_is_not_published() {
        let (observer, mut receiver) = channel();
        for address in ["data:text/plain,secret", "file:///private", "about:home"] {
            let url = Url::parse(address).unwrap();
            assert!(observer.begin(key(Request::Navigate), "GET", &url).is_none());
            observer.resource_loaded(key(Request::Resource(1)), Instant::now(), &url);
        }
        assert!(receiver.try_recv().is_none());
        assert_eq!(receiver.take_dropped(), 0);
    }
}
