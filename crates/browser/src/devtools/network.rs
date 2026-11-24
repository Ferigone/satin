//! In-memory Network rows for one tab/document. Only the observer can report
//! an actual request or lifecycle milestone; the log does no transport work.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use url::{Position, Url};

const ROW_LIMIT: usize = 256;
const URL_BYTES: usize = 2048;
const METHOD_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NetworkScope {
    pub tab: u64,
    pub navigation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RequestKind {
    Navigation,
    Resource,
    // Both enter the same transport path without an API source tag.
    FetchOrXhr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RequestKey {
    pub scope: NetworkScope,
    pub kind: RequestKind,
    // Unique within (scope, kind) for the life of the document.
    pub id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DisplayUrl(String);

impl DisplayUrl {
    pub(crate) fn from_url(url: &Url) -> Self {
        let prefix = match url.scheme() {
            "http" => "http://",
            "https" => "https://",
            // Never retain local file paths or opaque payloads (including data URLs).
            "file" => return Self("file:[redacted]".into()),
            "data" => return Self("data:[redacted]".into()),
            "blob" => return Self("blob:[redacted]".into()),
            "about" => return Self("about:[redacted]".into()),
            _ => return Self("[redacted URL]".into()),
        };
        // Slice the parsed serialization, not the input string. This copies
        // neither userinfo nor anything after the path, even for encoded URLs.
        let target = &url[Position::BeforeHost..Position::AfterPath];
        let mut display = String::from(prefix);
        if prefix.len() + target.len() <= URL_BYTES {
            display.push_str(target);
        } else {
            let mut end = URL_BYTES - prefix.len() - "...".len();
            while !target.is_char_boundary(end) {
                end -= 1;
            }
            display.push_str(&target[..end]);
            display.push_str("...");
        }
        Self(display)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DisplayMethod(String);

impl DisplayMethod {
    pub(crate) fn from_method(method: &str) -> Self {
        let valid = !method.is_empty()
            && method.len() <= METHOD_BYTES
            && method
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
        Self(if valid { method } else { "—" }.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalOutcome {
    // The observer confirmed the request ended successfully, not just that
    // a response head arrived, progress was made, or the channel closed.
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NetworkPhase {
    Started {
        method: DisplayMethod,
        url: DisplayUrl,
    },
    // The observed response head; None if its HTTP status isn't exposed.
    ResponseHeaders {
        status: Option<u16>,
    },
    Terminal(TerminalOutcome),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NetworkEvent {
    pub key: RequestKey,
    // Observation time, not the time at which the UI drains its queue.
    pub at: Option<Instant>,
    pub phase: NetworkPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NetworkOutcome {
    Started,
    HeadersReceived,
    Terminal(TerminalOutcome),
}

#[derive(Debug)]
struct Observation<T> {
    value: T,
    at: Option<Instant>,
}

#[derive(Debug)]
pub(crate) struct NetworkRow {
    pub key: RequestKey,
    pub method: DisplayMethod,
    pub url: DisplayUrl,
    started_at: Option<Instant>,
    headers: Option<Observation<Option<u16>>>,
    terminal: Option<Observation<TerminalOutcome>>,
}

impl NetworkRow {
    pub(crate) fn status(&self) -> Option<u16> {
        self.headers.as_ref()?.value
    }

    pub(crate) fn outcome(&self) -> NetworkOutcome {
        if let Some(terminal) = &self.terminal {
            NetworkOutcome::Terminal(terminal.value)
        } else if self.headers.is_some() {
            NetworkOutcome::HeadersReceived
        } else {
            NetworkOutcome::Started
        }
    }

    /// Time to the row's displayed phase, never wall time since start.
    pub(crate) fn elapsed(&self) -> Option<Duration> {
        let at = match &self.terminal {
            Some(terminal) => terminal.at,
            None => self.headers.as_ref()?.at,
        }?;
        at.checked_duration_since(self.started_at?)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct NetworkLoss {
    pub evicted_rows: u64,
    pub dropped_events: u64,
    pub unmatched_events: u64,
}

pub(crate) struct NetworkLog {
    scope: NetworkScope,
    rows: VecDeque<NetworkRow>,
    loss: NetworkLoss,
}

impl NetworkLog {
    pub(crate) fn new(scope: NetworkScope) -> Self {
        Self {
            scope,
            rows: VecDeque::new(),
            loss: NetworkLoss::default(),
        }
    }

    pub(crate) fn set_scope(&mut self, scope: NetworkScope) {
        if self.scope != scope {
            self.scope = scope;
            self.clear();
        }
    }

    pub(crate) fn clear(&mut self) {
        self.rows.clear();
        self.loss = NetworkLoss::default();
    }

    /// Oldest start first. Updates don't move rows.
    pub(crate) fn rows(&self) -> impl ExactSizeIterator<Item = &NetworkRow> + DoubleEndedIterator {
        self.rows.iter()
    }

    pub(crate) fn loss(&self) -> NetworkLoss {
        self.loss
    }

    /// Report a delta of lost observer events for this scope. Queue loss,
    /// row eviction, and unmatched updates count different things.
    pub(crate) fn note_dropped_events(&mut self, scope: NetworkScope, count: u64) {
        if self.scope == scope {
            self.loss.dropped_events = self.loss.dropped_events.saturating_add(count);
        }
    }

    pub(crate) fn observe(&mut self, event: NetworkEvent) {
        let NetworkEvent { key, at, phase } = event;
        if key.scope != self.scope {
            return;
        }

        // A producer emits at most one start per key. Start alone admits a
        // row, so a missing start or a late update cannot resurrect a row
        // after eviction/clear. No unbounded orphan or tombstone storage.
        let row = self.rows.iter_mut().find(|row| row.key == key);
        match (phase, row) {
            (NetworkPhase::Started { method, url }, None) => {
                if self.rows.len() == ROW_LIMIT {
                    self.rows.pop_front();
                    self.loss.evicted_rows = self.loss.evicted_rows.saturating_add(1);
                }
                self.rows.push_back(NetworkRow {
                    key,
                    method,
                    url,
                    started_at: at,
                    headers: None,
                    terminal: None,
                });
            }
            (NetworkPhase::Started { .. }, Some(_)) => {}
            (NetworkPhase::ResponseHeaders { status }, Some(row)) => {
                // A delayed head can fill in status without undoing a terminal
                // observation. Retain the first head if replayed.
                row.headers.get_or_insert(Observation {
                    value: status.filter(|status| (100..=999).contains(status)),
                    at,
                });
            }
            (NetworkPhase::Terminal(value), Some(row)) => {
                row.terminal.get_or_insert(Observation { value, at });
            }
            (_, None) => {
                self.loss.unmatched_events = self.loss.unmatched_events.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCOPE: NetworkScope = NetworkScope {
        tab: 7,
        navigation: 11,
    };

    fn key(kind: RequestKind, id: u64) -> RequestKey {
        RequestKey {
            scope: SCOPE,
            kind,
            id,
        }
    }

    fn send(
        log: &mut NetworkLog,
        key: RequestKey,
        at: impl Into<Option<Instant>>,
        phase: NetworkPhase,
    ) {
        log.observe(NetworkEvent {
            key,
            at: at.into(),
            phase,
        });
    }

    fn start(log: &mut NetworkLog, key: RequestKey, at: impl Into<Option<Instant>>) {
        let url = Url::parse("https://name:password@example.test:8443/path?secret#token").unwrap();
        send(
            log,
            key,
            at,
            NetworkPhase::Started {
                method: DisplayMethod::from_method("GET"),
                url: DisplayUrl::from_url(&url),
            },
        );
    }

    #[test]
    fn same_numeric_id_does_not_merge_kinds_tabs_or_documents() {
        let mut log = NetworkLog::new(SCOPE);
        let navigation = key(RequestKind::Navigation, 1);
        let resource = key(RequestKind::Resource, 1);
        let fetch = key(RequestKind::FetchOrXhr, 1);
        for key in [navigation, resource, fetch] {
            start(&mut log, key, None);
        }
        for scope in [
            NetworkScope { tab: 8, ..SCOPE },
            NetworkScope {
                navigation: 12,
                ..SCOPE
            },
        ] {
            let foreign = RequestKey { scope, ..fetch };
            start(&mut log, foreign, None);
            send(
                &mut log,
                foreign,
                None,
                NetworkPhase::Terminal(TerminalOutcome::Failed),
            );
            log.note_dropped_events(scope, 99);
        }
        send(
            &mut log,
            fetch,
            None,
            NetworkPhase::ResponseHeaders { status: Some(404) },
        );
        assert_eq!(
            log.rows()
                .map(|row| (row.key, row.status()))
                .collect::<Vec<_>>(),
            [(navigation, None), (resource, None), (fetch, Some(404))]
        );
        assert_eq!(log.loss(), NetworkLoss::default());
    }

    #[test]
    fn headers_and_http_error_status_do_not_claim_transport_completion_or_failure() {
        let mut log = NetworkLog::new(SCOPE);
        let key = key(RequestKind::FetchOrXhr, 1);
        let at = Instant::now();
        start(&mut log, key, at);
        let row = log.rows().next().unwrap();
        assert_eq!(row.outcome(), NetworkOutcome::Started);
        assert_eq!(row.elapsed(), None);
        assert_eq!(row.method.as_str(), "GET");
        assert_eq!(row.url.as_str(), "https://example.test:8443/path");

        let to_headers = Duration::from_millis(5);
        send(
            &mut log,
            key,
            at + to_headers,
            NetworkPhase::ResponseHeaders { status: Some(404) },
        );
        let row = log.rows().next().unwrap();
        assert_eq!(row.outcome(), NetworkOutcome::HeadersReceived);
        assert_eq!(row.status(), Some(404));
        assert_eq!(row.elapsed(), Some(to_headers));
        log.note_dropped_events(SCOPE, 1);
        assert_eq!(
            log.rows().next().unwrap().outcome(),
            NetworkOutcome::HeadersReceived
        );

        let to_end = Duration::from_millis(20);
        send(
            &mut log,
            key,
            at + to_end,
            NetworkPhase::Terminal(TerminalOutcome::Completed),
        );
        let row = log.rows().next().unwrap();
        assert_eq!(
            row.outcome(),
            NetworkOutcome::Terminal(TerminalOutcome::Completed)
        );
        assert_eq!(row.status(), Some(404));
        assert_eq!(row.elapsed(), Some(to_end));
    }

    #[test]
    fn late_headers_cannot_undo_a_failure_or_a_cancellation() {
        for outcome in [TerminalOutcome::Failed, TerminalOutcome::Cancelled] {
            let mut log = NetworkLog::new(SCOPE);
            let key = key(RequestKind::FetchOrXhr, 2);
            let at = Instant::now();
            start(&mut log, key, at);
            send(&mut log, key, None, NetworkPhase::Terminal(outcome));
            send(
                &mut log,
                key,
                at,
                NetworkPhase::ResponseHeaders { status: Some(200) },
            );
            send(
                &mut log,
                key,
                at,
                NetworkPhase::ResponseHeaders { status: Some(202) },
            );
            start(&mut log, key, at);
            send(
                &mut log,
                key,
                at,
                NetworkPhase::Terminal(TerminalOutcome::Completed),
            );
            let row = log.rows().next().unwrap();
            assert_eq!(row.outcome(), NetworkOutcome::Terminal(outcome));
            assert_eq!(row.status(), Some(200));
            assert_eq!(row.elapsed(), None); // Never use header time as terminal time.
            assert_eq!(log.rows().len(), 1);
        }
    }

    #[test]
    fn unavailable_status_and_invalid_elapsed_are_not_invented() {
        let mut log = NetworkLog::new(SCOPE);
        let key = key(RequestKind::Resource, 1);
        let at = Instant::now();
        start(&mut log, key, at + Duration::from_millis(1));
        send(
            &mut log,
            key,
            at,
            NetworkPhase::ResponseHeaders { status: Some(0) },
        );
        let row = log.rows().next().unwrap();
        assert_eq!(row.status(), None);
        assert_eq!(row.elapsed(), None);
        assert_eq!(row.outcome(), NetworkOutcome::HeadersReceived);
    }

    #[test]
    fn updates_without_a_start_are_counted_without_inventing_rows() {
        let mut log = NetworkLog::new(SCOPE);
        let key = key(RequestKind::Navigation, 1);
        send(
            &mut log,
            key,
            None,
            NetworkPhase::ResponseHeaders { status: Some(200) },
        );
        send(
            &mut log,
            key,
            None,
            NetworkPhase::Terminal(TerminalOutcome::Completed),
        );
        assert_eq!(log.rows().len(), 0);
        assert_eq!(log.loss().unmatched_events, 2);
        start(&mut log, key, None);
        let row = log.rows().next().unwrap();
        assert_eq!(row.outcome(), NetworkOutcome::Started);
        assert_eq!(row.status(), None);
    }

    #[test]
    fn rows_are_bounded_and_evicted_rows_are_not_recreated_by_late_updates() {
        let mut log = NetworkLog::new(SCOPE);
        let oldest = key(RequestKind::Resource, 0);
        for id in 0..ROW_LIMIT as u64 {
            start(&mut log, key(RequestKind::Resource, id), None);
        }
        send(
            &mut log,
            oldest,
            None,
            NetworkPhase::ResponseHeaders { status: Some(200) },
        );
        let newest = key(RequestKind::Resource, ROW_LIMIT as u64);
        start(&mut log, newest, None);
        send(
            &mut log,
            oldest,
            None,
            NetworkPhase::Terminal(TerminalOutcome::Failed),
        );
        assert_eq!(log.rows().len(), ROW_LIMIT);
        assert_eq!(log.rows().next().unwrap().key.id, 1);
        assert_eq!(log.rows().next_back().unwrap().key, newest);
        assert_eq!(
            log.loss(),
            NetworkLoss {
                evicted_rows: 1,
                unmatched_events: 1,
                dropped_events: 0
            }
        );
    }

    #[test]
    fn clear_and_scope_change_reset_rows_and_loss() {
        let mut log = NetworkLog::new(SCOPE);
        let old_key = key(RequestKind::Navigation, 1);
        start(&mut log, old_key, None);
        log.note_dropped_events(SCOPE, u64::MAX);
        log.note_dropped_events(SCOPE, 1);
        log.set_scope(SCOPE);
        assert_eq!(log.loss().dropped_events, u64::MAX);
        assert_eq!(log.rows().len(), 1);
        log.clear();
        assert_eq!(log.loss(), NetworkLoss::default());
        send(
            &mut log,
            old_key,
            None,
            NetworkPhase::Terminal(TerminalOutcome::Cancelled),
        );
        assert_eq!(log.loss().unmatched_events, 1);
        assert_eq!(log.rows().len(), 0);

        start(&mut log, key(RequestKind::FetchOrXhr, 1), None);
        let scope = NetworkScope {
            navigation: 12,
            ..SCOPE
        };
        log.set_scope(scope);
        let new_key = RequestKey { scope, ..old_key };
        start(&mut log, new_key, None);
        send(
            &mut log,
            old_key,
            None,
            NetworkPhase::Terminal(TerminalOutcome::Cancelled),
        );
        let row = log.rows().next().unwrap();
        assert_eq!(row.key, new_key);
        assert_eq!(row.outcome(), NetworkOutcome::Started);
        assert_eq!(log.loss(), NetworkLoss::default());
    }

    #[test]
    fn url_display_drops_userinfo_query_fragment_and_non_http_payloads() {
        for (input, expected) in [
            (
                "https://user%40x:p%3F%23@example.test:8443/api%3Fpart?token=secret#private",
                "https://example.test:8443/api%3Fpart",
            ),
            ("http://name@[::1]:8080/path?#", "http://[::1]:8080/path"),
            ("https://example.test/p@th", "https://example.test/p@th"),
            ("data:text/html,private-request-content", "data:[redacted]"),
            (
                "file:///C:/Users/private-account/file.txt",
                "file:[redacted]",
            ),
            ("blob:https://example.test/private-id", "blob:[redacted]"),
            ("about:home", "about:[redacted]"),
            ("custom:private-opaque-payload", "[redacted URL]"),
        ] {
            assert_eq!(
                DisplayUrl::from_url(&Url::parse(input).unwrap()).as_str(),
                expected
            );
        }
    }

    #[test]
    fn display_fields_are_bounded() {
        let url = Url::parse(&format!(
            "https://user:secret@example.test/{}?private=1",
            "é".repeat(URL_BYTES)
        ))
        .unwrap();
        let display = DisplayUrl::from_url(&url);
        assert_eq!(display.as_str().len(), URL_BYTES);
        assert!(display.as_str().starts_with("https://example.test/"));
        assert!(display.as_str().ends_with("..."));
        assert!(!display.as_str().contains("secret"));
        assert!(!display.as_str().contains("private"));
        assert_eq!(DisplayMethod::from_method("PROPFIND").as_str(), "PROPFIND");
        for invalid in ["", "GET\nunsafe", &"A".repeat(METHOD_BYTES + 1)] {
            assert_eq!(DisplayMethod::from_method(invalid).as_str(), "—");
        }
    }
}
