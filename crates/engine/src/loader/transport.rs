//! One reactor serves document navigation, parser resources, and Fetch.
use std::{
    future::{Future, pending},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub use tokio_util::sync::CancellationToken as CancelSignal;

pub(crate) fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("browser-transport")
                .max_blocking_threads(8)
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

#[derive(Debug)]
pub(crate) enum Interrupted {
    Aborted,
    Deadline,
}

pub(crate) enum BodyError {
    Interrupted(Interrupted),
    Http(reqwest::Error),
    Limit,
}

pub(crate) struct RequestControl<'a> {
    deadline: Instant,
    generation: Option<&'a CancelSignal>,
    request: Option<&'a CancelSignal>,
    legacy: Option<&'a AtomicBool>,
}

impl<'a> RequestControl<'a> {
    pub(crate) fn new(generation: &'a CancelSignal, deadline: Instant) -> Self {
        Self {
            deadline,
            generation: Some(generation),
            request: None,
            legacy: None,
        }
    }

    pub(crate) fn legacy(cancel: &'a AtomicBool, deadline: Instant) -> Self {
        Self {
            deadline,
            generation: None,
            request: None,
            legacy: Some(cancel),
        }
    }

    pub(crate) fn with_request(mut self, request: Option<&'a CancelSignal>) -> Self {
        self.request = request;
        self
    }

    pub(crate) fn check(&self) -> Result<(), Interrupted> {
        if self.generation.is_some_and(CancelSignal::is_cancelled)
            || self.request.is_some_and(CancelSignal::is_cancelled)
            || self
                .legacy
                .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            Err(Interrupted::Aborted)
        } else if Instant::now() >= self.deadline {
            Err(Interrupted::Deadline)
        } else {
            Ok(())
        }
    }

    pub(crate) async fn wait<F: Future>(&self, future: F) -> Result<F::Output, Interrupted> {
        self.check()?;
        tokio::select! {
            biased;
            () = signal(self.generation) => Err(Interrupted::Aborted),
            () = signal(self.request) => Err(Interrupted::Aborted),
            () = legacy_signal(self.legacy) => Err(Interrupted::Aborted),
            () = tokio::time::sleep_until(self.deadline.into()) => Err(Interrupted::Deadline),
            value = future => Ok(value),
        }
    }

    pub(crate) async fn read_body(
        &self,
        mut response: reqwest::Response,
        limit: usize,
    ) -> Result<Vec<u8>, BodyError> {
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(BodyError::Limit);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = self
            .wait(response.chunk())
            .await
            .map_err(BodyError::Interrupted)?
            .map_err(BodyError::Http)?
        {
            if bytes.len().saturating_add(chunk.len()) > limit {
                return Err(BodyError::Limit);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

async fn signal(signal: Option<&CancelSignal>) {
    if let Some(signal) = signal {
        signal.cancelled().await;
    } else {
        pending().await
    }
}

async fn legacy_signal(cancel: Option<&AtomicBool>) {
    // The former synchronous engine API takes a borrowed AtomicBool. Only
    // that compatibility path polls. Browser cancellation wakes immediately.
    if let Some(cancel) = cancel {
        while !cancel.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    } else {
        pending().await
    }
}
