//! Query the Media Foundation source extension that consumes appendBuffer bytes.
//! Documents share one COM apartment while any of them still need capabilities.
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, Weak,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread,
    time::{Duration, Instant},
};
use rust_browser_media_foundation::with_source_types;

type Query = (String, SyncSender<bool>);

pub(super) struct Probe(Arc<Worker>);

struct Worker {
    queries: SyncSender<Query>,
    retry_at: Mutex<Option<Instant>>,
}

impl Probe {
    pub(super) fn acquire() -> Option<Self> {
        static SHARED: Mutex<Weak<Worker>> = Mutex::new(Weak::new());
        let mut shared = SHARED.lock().ok()?;
        if let Some(worker) = shared.upgrade() {
            return Some(Self(worker));
        }
        let (queries, receiver) = sync_channel(16);
        thread::Builder::new()
            .name("satin-media-types".into())
            .spawn(move || run(receiver))
            .ok()?;
        let worker = Arc::new(Worker {
            queries,
            retry_at: Mutex::new(None),
        });
        *shared = Arc::downgrade(&worker);
        Some(Self(worker))
    }

    pub(super) fn supported(&self, mime: &str) -> bool {
        if self
            .0
            .retry_at
            .lock()
            .unwrap()
            .is_some_and(|at| at > Instant::now())
        {
            return false;
        }
        let (answer, result) = sync_channel(1);
        if self.0.queries.try_send((mime.to_owned(), answer)).is_err() {
            return false;
        }
        match result.recv_timeout(Duration::from_secs(2)) {
            Ok(supported) => supported,
            Err(_) => {
                // A stalled codec provider must not block every probe on the page.
                *self.0.retry_at.lock().unwrap() = Some(Instant::now() + Duration::from_secs(5));
                false
            }
        }
    }
}

fn run(queries: Receiver<Query>) {
    let _ = with_source_types(|is_supported| {
        let mut cache = VecDeque::<(String, bool)>::new();
        for (mime, answer) in queries {
            let supported = match cache.iter().find(|(kind, _)| kind == &mime) {
                Some((_, result)) => *result,
                None => {
                    let result = is_supported(&mime);
                    if cache.len() == 64 {
                        cache.pop_front();
                    }
                    cache.push_back((mime, result));
                    result
                }
            };
            let _ = answer.send(supported);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_media_queries_match_the_installed_source_extension() {
        let mut results = Vec::new();
        with_source_types(|is_supported| {
            for mime in [
                r#"video/mp4; codecs="avc1.42C01E""#,
                r#"audio/mp4; codecs="mp4a.40.2""#,
                r#"video/mp4; codecs="hvc1.1.40000000.L60.80.0.0.0.0.0""#,
                r#"video/mp4; codecs="av01.0.05M.08""#,
                r#"video/mp2t; codecs="avc1.4d001f,mp4a.40.2""#,
                r#"video/webm; codecs="vp09.00.10.08""#,
            ] {
                let supported = is_supported(mime);
                results.push((mime, supported));
            }
        }).expect("Media Foundation must initialize");
        assert_eq!(results.len(), 6, "Media Foundation must initialize");
        let store = super::super::Store::default();
        for (mime, expected) in results {
            assert_eq!(store.is_supported(mime), expected, "{mime}");
        }
        assert!(!store.is_supported("audio/aac"));
        assert!(!store.is_supported("application/vnd.apple.mpegurl"));
    }
}
