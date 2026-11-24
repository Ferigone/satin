//! Bounded transport queues share a reactor. A stalled Fetch has no effect on
//! the slots reserved for navigation or parser resources.
pub(crate) mod devtools;

use rust_browser_engine::{
    Loader,
    cookies::CookieStore,
    loader::{DocumentResource, NAVIGATION_TIMEOUT, NavigationPart, NavigationRequest},
    network::{
        CancelSignal, FETCH_TIMEOUT, FetchError, FetchErrorKind, FetchJob, StreamingResponse,
    },
    script::ResourceLoad,
};
use std::{
    sync::{Arc, mpsc::TrySendError},
    time::Instant,
};
use tokio::{
    runtime::Handle,
    sync::{Mutex, Semaphore, mpsc},
};

use self::devtools::{Key, Observer, Phase, Request};

const FETCH_TRANSFERS: usize = 64;

pub(crate) enum Task {
    Navigate(Box<NavigationRequest>),
    Fetch(Box<FetchJob>),
    Resource(Box<ResourceLoad>),
}
pub(crate) struct Job {
    pub tab: u64,
    pub navigation: u64,
    pub task: Task,
    pub cancel: CancelSignal,
}
pub(crate) enum Payload {
    MediaWake,
    WorkerWake,
    Navigation(NavigationPart),
    NavigationError(String),
    Fetch {
        id: u64,
        result: Result<StreamingResponse, FetchError>,
    },
    FetchBodyProgress(u64),
    FetchUploadDemand(u64),
    Resource {
        id: u64,
        result: Result<DocumentResource, String>,
    },
}
pub(crate) struct Message {
    pub tab: u64,
    pub navigation: u64,
    pub payload: Payload,
}
pub(crate) struct Workers {
    pages: mpsc::Sender<QueuedJob>,
    resources: mpsc::Sender<QueuedJob>,
    fetches: mpsc::Sender<QueuedJob>,
    uploads: mpsc::Sender<QueuedJob>,
    handle: Handle,
    shutdown: CancelSignal,
}

struct QueuedJob {
    job: Job,
    queued: Instant,
}

impl Workers {
    pub fn new(
        cookies: CookieStore,
        results: mpsc::Sender<Message>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Self::with_observer(cookies, results, wake, None)
    }

    pub fn new_observed(
        cookies: CookieStore,
        results: mpsc::Sender<Message>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<(Self, devtools::Receiver), String> {
        let (observer, receiver) = devtools::channel();
        Self::with_observer(cookies, results, wake, Some(observer))
            .map(|workers| (workers, receiver))
    }

    fn with_observer(
        cookies: CookieStore,
        results: mpsc::Sender<Message>,
        wake: impl Fn() + Send + Sync + 'static,
        observer: Option<Observer>,
    ) -> Result<Self, String> {
        let loader = Arc::new(Loader::with_cookies(cookies)?);
        let handle = loader.transport_handle().clone();
        let wake = Arc::new(wake);
        let shutdown = CancelSignal::new();
        let transfers = Arc::new(Semaphore::new(FETCH_TRANSFERS));
        let pool = || {
            let (tx, rx) = mpsc::channel::<QueuedJob>(8);
            let rx = Arc::new(Mutex::new(rx));
            for _ in 0..2 {
                let rx = rx.clone();
                let loader = loader.clone();
                let results = results.clone();
                let wake = wake.clone();
                let shutdown = shutdown.clone();
                let transfers = transfers.clone();
                let observer = observer.clone();
                handle.spawn(async move {
                    loop {
                        let next = tokio::select! {
                            biased;
                            () = shutdown.cancelled() => break,
                            job = async { rx.lock().await.recv().await } => job,
                        };
                        let Some(job) = next else {
                            break;
                        };
                        tokio::select! {
                            biased;
                            () = shutdown.cancelled() => break,
                            () = run_job(
                                job, &loader, &results, &wake, &transfers, &shutdown,
                                observer.as_ref(),
                            ) => {}
                        }
                    }
                });
            }
            tx
        };
        Ok(Self {
            pages: pool(),
            resources: pool(),
            fetches: pool(),
            uploads: pool(),
            handle,
            shutdown,
        })
    }

    pub fn submit(&self, job: Job) -> Result<(), String> {
        self.try_submit(job)
            .map_err(|_| "The bounded network queue is full or disconnected".into())
    }

    pub fn try_submit(&self, job: Job) -> Result<(), TrySendError<Job>> {
        let sender = match &job.task {
            Task::Navigate(_) => &self.pages,
            Task::Resource(_) => &self.resources,
            Task::Fetch(job) if job.upload.is_some() => &self.uploads,
            Task::Fetch(_) => &self.fetches,
        };
        sender
            .try_send(QueuedJob {
                job,
                queued: Instant::now(),
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(queued) => TrySendError::Full(queued.job),
                mpsc::error::TrySendError::Closed(queued) => TrySendError::Disconnected(queued.job),
            })
    }

    pub fn receive_until(
        &self,
        rx: &mut mpsc::Receiver<Message>,
        deadline: Instant,
    ) -> Option<Message> {
        if let Ok(message) = rx.try_recv() {
            return Some(message);
        }
        self.handle.block_on(async {
            tokio::time::timeout_at(deadline.into(), rx.recv())
                .await
                .ok()
                .flatten()
        })
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn run_job(
    queued: QueuedJob,
    loader: &Loader,
    results: &mpsc::Sender<Message>,
    wake: &Arc<impl Fn() + Send + Sync + 'static>,
    transfers: &Arc<Semaphore>,
    shutdown: &CancelSignal,
    observer: Option<&Observer>,
) {
    let Job {
        tab,
        navigation,
        task,
        cancel,
    } = queued.job;
    // Keep room for the job's terminal result before reading from the socket.
    // If streaming backpressure hits the deadline, the response is dropped
    // and its error can still reach the UI after any parts already queued.
    let terminal = tokio::select! {
        biased;
        () = cancel.cancelled() => return,
        slot = results.reserve() => match slot {
            Ok(slot) => slot,
            Err(_) => return,
        },
    };
    let message = |payload| Message {
        tab,
        navigation,
        payload,
    };
    let key = |request| Key {
        tab,
        navigation,
        request,
    };
    let payload = match task {
        Task::Navigate(request) => {
            let deadline = queued.queued + NAVIGATION_TIMEOUT;
            let trace = observer.and_then(|observer| {
                observer.begin(key(Request::Navigate), &request.method, &request.url)
            });
            if trace.is_some() {
                wake();
            }
            let result = loader
                .stream_navigation_cancellable(&request, &cancel, deadline, |part| {
                    if let (Some(trace), NavigationPart::Open(opened)) = (trace, &part)
                        && let Some((status, response_time)) = opened.http_response()
                    {
                        trace.record(Phase::NavigationOpened {
                            status,
                            response_time,
                        });
                    }
                    async move {
                        results
                            .send(message(Payload::Navigation(part)))
                            .await
                            .map_err(|_| "Browser result channel closed")?;
                        wake();
                        Ok(())
                    }
                })
                .await;
            if let Some(trace) = trace {
                trace.record(match &result {
                    Ok(()) => Phase::NavigationFinished,
                    Err(_) if cancel.is_cancelled() => Phase::Cancelled,
                    Err(_) => Phase::Failed,
                });
                // A successful stream has no separate terminal page message.
                if result.is_ok() || cancel.is_cancelled() {
                    wake();
                }
            }
            match result {
                Ok(()) => return,
                Err(error) => Payload::NavigationError(error),
            }
        }
        Task::Fetch(job) => {
            let FetchJob {
                id,
                request,
                upload,
            } = *job;
            let trace = observer.and_then(|observer| {
                observer.begin(key(Request::Fetch(id)), &request.method, &request.url)
            });
            if trace.is_some() {
                wake();
            }
            let demand = upload.as_ref().map(|body| body.demand());
            let response = loader.open_web_fetch_with_upload(
                &request,
                upload,
                &cancel,
                queued.queued + FETCH_TIMEOUT,
            );
            let result = if let Some(demand) = demand {
                tokio::select! {
                    biased;
                    result = response => result,
                    () = async {
                        loop {
                            demand.next().await;
                            if results.send(message(Payload::FetchUploadDemand(id))).await.is_err() {
                                break;
                            }
                            wake();
                        }
                    } => return,
                }
            } else {
                response.await
            };
            if let (Some(trace), Ok((head, _))) = (trace, &result) {
                let status = head.metadata.status;
                trace.record(Phase::FetchResponse {
                    status: (status != 0).then_some(status),
                });
            }
            let result = match result {
                Ok((head, Some(body))) => match transfers.clone().try_acquire_owned() {
                    Ok(permit) if !cancel.is_cancelled() => {
                        // Send the header event before any progress from this body.
                        terminal.send(message(Payload::Fetch {
                            id,
                            result: Ok(head),
                        }));
                        wake();
                        let results = results.clone();
                        let wake = wake.clone();
                        let shutdown = shutdown.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            tokio::select! {
                                biased;
                                () = shutdown.cancelled() => {}
                                () = body.run(|| async {
                                    if results.send(Message {
                                        tab,
                                        navigation,
                                        payload: Payload::FetchBodyProgress(id),
                                    }).await.is_ok() {
                                        wake();
                                    }
                                }) => {}
                            }
                        });
                        return;
                    }
                    Ok(_) => {
                        if let Some(trace) = trace {
                            trace.record(Phase::Cancelled);
                            wake();
                        }
                        return;
                    }
                    Err(_) => Err(FetchError::new(
                        FetchErrorKind::Limit,
                        "Too many open Fetch response bodies",
                    )),
                },
                Ok((head, None)) => Ok(head),
                Err(error) => Err(error),
            };
            if let (Some(trace), Err(error)) = (trace, &result) {
                trace.record(if error.kind == FetchErrorKind::Aborted {
                    Phase::Cancelled
                } else {
                    Phase::Failed
                });
                if cancel.is_cancelled() {
                    wake();
                }
            }
            Payload::Fetch { id, result }
        }
        Task::Resource(load) => {
            let started = observer.map(|_| Instant::now());
            let result = loader
                .load_document_resource_cancellable(&load.request, &cancel)
                .await;
            if let (Some(observer), Some(started), Ok(resource)) = (observer, started, &result) {
                observer.resource_loaded(key(Request::Resource(load.id)), started, &resource.url);
            }
            Payload::Resource {
                id: load.id,
                result,
            }
        }
    };
    if !cancel.is_cancelled() {
        terminal.send(message(payload));
        wake();
    }
}

#[cfg(test)]
mod tests;
