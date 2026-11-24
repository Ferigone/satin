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

use self::devtools::{Key, Observer, Phase, Request, ResponseDetails, Trace};
use rust_browser_engine::loader::DownloadResponse;

const FETCH_TRANSFERS: usize = 64;

pub(crate) enum Task {
    Navigate(Box<NavigationRequest>),
    DownloadLink(Box<NavigationRequest>, String),
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
    DownloadOffer {
        response: OfferedDownload,
        from_link: bool,
    },
    DownloadLinkError(String),
    DownloadProgress(u64),
    DownloadDone(Result<u64, String>),
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
pub(crate) struct OfferedDownload {
    pub response: DownloadResponse,
    transfer: DownloadTrace,
}

// Dropping an offer (dismissed chooser, closed tab or stale navigation) ends its observation.
struct DownloadTrace(Option<Trace>);

impl Drop for DownloadTrace {
    fn drop(&mut self) {
        if let Some(trace) = self.0.take() {
            trace.record(Phase::Cancelled);
        }
    }
}

impl OfferedDownload {
    fn new(response: DownloadResponse, trace: Option<Trace>) -> Self {
        if let Some(trace) = &trace {
            let (url, status, content) = response.http_metadata();
            let mut details = ResponseDetails::from_url(url.as_str());
            details.mime = content.mime;
            details.declared_bytes = content.declared_bytes;
            trace.record(Phase::Response {
                status: Some(status),
                headers_at: Some(response.headers_at),
                details,
            });
        }
        Self {
            response,
            transfer: DownloadTrace(trace),
        }
    }
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
    results: mpsc::Sender<Message>,
    wake: Arc<dyn Fn() + Send + Sync>,
    downloads: Vec<tokio::task::JoinHandle<()>>,
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
            results: results.clone(),
            wake: wake.clone(),
            downloads: Vec::new(),
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
            Task::Navigate(_) | Task::DownloadLink(_, _) => &self.pages,
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

    pub fn save_download(
        &mut self,
        tab: u64,
        id: u64,
        path: std::path::PathBuf,
        offer: OfferedDownload,
    ) -> CancelSignal {
        let token = self.shutdown.child_token();
        let cancel = token.clone();
        let results = self.results.clone();
        let wake = self.wake.clone();
        let shutdown = self.shutdown.clone();
        self.downloads.retain(|task| !task.is_finished());
        self.downloads.push(self.handle.spawn(async move {
            let OfferedDownload{response,mut transfer}=offer;
            let mut observed=Instant::now();
            let result=crate::downloads::save(response,&path,&cancel,|count| {
                if observed.elapsed() >= std::time::Duration::from_millis(200)
                    && results.try_send(Message{tab,navigation:id,payload:Payload::DownloadProgress(count)}).is_ok() {
                    observed=Instant::now(); wake();
                }
            }).await;
            if let Some(trace) = transfer.0.take() {
                trace.record(match &result {
                    Ok(n) => Phase::Finished {body_bytes: Some(*n)},
                    Err(_) if cancel.is_cancelled() => Phase::Cancelled,
                    Err(_) => Phase::Failed,
                });
                wake();
            }
            tokio::select!{
                ()=shutdown.cancelled()=>{},
                _=results.send(Message{tab,navigation:id,payload:Payload::DownloadDone(result)})=>wake(),
            }
        }));
        token
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
        if !self.downloads.is_empty() {
            self.handle.block_on(async {
                for task in self.downloads.drain(..) {
                    let _ = task.await;
                }
            });
        }
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
                observer.begin(key(Request::Navigate), &request.method, &request.url, None)
            });
            if trace.is_some() {
                wake();
            }
            let mut body_bytes = None;
            let result = loader
                .stream_desktop_navigation(&request, &cancel, deadline, |part| {
                    if let (Some(trace), NavigationPart::Open(opened)) = (trace.as_ref(), &part)
                        && let Some((status, _)) = opened.http_response()
                    {
                        body_bytes = Some(opened.buffered_body_bytes() as u64);
                        let mut details = ResponseDetails::from_url(opened.response_url());
                        details.mime = opened.http_content.mime.clone();
                        details.declared_bytes = opened.http_content.declared_bytes;
                        trace.record(Phase::Response {
                            status: Some(status),
                            headers_at: Some(opened.response_started_at()),
                            details,
                        });
                    }
                    if let (Some(total), NavigationPart::Text(_, received)) =
                        (&mut body_bytes, &part)
                    {
                        *total = total.saturating_add(*received as u64);
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
            if let Ok(Some(response)) = result {
                let response = OfferedDownload::new(response, trace);
                if !cancel.is_cancelled() {
                    terminal.send(message(Payload::DownloadOffer {
                        response,
                        from_link: false,
                    }));
                    wake();
                }
                return;
            }
            if let Some(trace) = trace {
                trace.record(match &result {
                    Ok(_) => Phase::Finished { body_bytes },
                    Err(_) if cancel.is_cancelled() => Phase::Cancelled,
                    Err(_) => Phase::Failed,
                });
                // A successful stream has no separate terminal page message.
                if result.is_ok() || cancel.is_cancelled() {
                    wake();
                }
            }
            match result {
                Ok(_) => return,
                Err(error) => Payload::NavigationError(error),
            }
        }
        Task::DownloadLink(request, hint) => {
            static NEXT_LINK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let trace = observer.and_then(|observer| {
                let id = NEXT_LINK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                observer.begin(
                    key(Request::Download(id)),
                    &request.method,
                    &request.url,
                    Some("Download"),
                )
            });
            match loader
                .open_download_link(&request, &hint, &cancel, queued.queued + NAVIGATION_TIMEOUT)
                .await
            {
                Ok(response) => Payload::DownloadOffer {
                    response: OfferedDownload::new(response, trace),
                    from_link: true,
                },
                Err(e) => {
                    if let Some(trace) = trace {
                        trace.record(if cancel.is_cancelled() {
                            Phase::Cancelled
                        } else {
                            Phase::Failed
                        });
                        wake();
                    }
                    Payload::DownloadLinkError(e)
                }
            }
        }
        Task::Fetch(job) => {
            let FetchJob {
                id,
                request,
                upload,
            } = *job;
            let trace = observer.and_then(|observer| {
                observer.begin(key(Request::Fetch(id)), &request.method, &request.url, None)
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
            if let (Some(trace), Ok((head, _))) = (trace.as_ref(), &result) {
                let status = head.metadata.status;
                trace.record(Phase::Response {
                    status: (status != 0).then_some(status),
                    headers_at: Some(Instant::now()),
                    details: ResponseDetails::from_fetch(&head.metadata),
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
                        let trace = trace.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            tokio::select! {
                                biased;
                                () = shutdown.cancelled() => {
                                    if let Some(trace) = &trace { trace.record(Phase::Cancelled); }
                                }
                                () = body.run_observed(|| async {
                                    if results.send(Message {
                                        tab,
                                        navigation,
                                        payload: Payload::FetchBodyProgress(id),
                                    }).await.is_ok() {
                                        wake();
                                    }
                                }, |outcome| {
                                    if let Some(trace) = &trace {
                                        trace.record(match outcome {
                                            Ok(Some(bytes)) => Phase::Finished { body_bytes: Some(bytes) },
                                            Ok(None) | Err(FetchErrorKind::Aborted) => Phase::Cancelled,
                                            Err(_) => Phase::Failed,
                                        });
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
                Ok((head, None)) => {
                    if let Some(trace) = &trace {
                        // Opaque Fetch can finish without exposing the transfer.
                        if head.metadata.status != 0 {
                            trace.record(Phase::Finished {
                                body_bytes: head
                                    .metadata
                                    .body
                                    .as_ref()
                                    .map(|body| body.len() as u64),
                            });
                        }
                    }
                    Ok(head)
                }
                Err(error) => Err(error),
            };
            if let (Some(trace), Err(error)) = (trace.as_ref(), &result) {
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
            let trace = observer.and_then(|observer| {
                observer.begin(
                    key(Request::Resource(load.id)),
                    "GET",
                    load.request.url(),
                    Some(devtools::resource_label(load.request.kind())),
                )
            });
            let result = if let Some(trace) = trace {
                wake();
                let (result, observation) = loader
                    .load_document_resource_observed_cancellable(&load.request, &cancel)
                    .await;
                if let Some(http) = observation {
                    let mut details = ResponseDetails::from_url(&http.url);
                    details.body_bytes = http.body_bytes.map(|n| n as u64);
                    details.mime = http.content.mime;
                    details.declared_bytes = http.content.declared_bytes;
                    trace.record(Phase::Response {
                        status: Some(http.status),
                        headers_at: http.headers_at,
                        details,
                    });
                }
                trace.record(match &result {
                    Ok(resource) => Phase::Finished {
                        body_bytes: Some(resource.transferred as u64),
                    },
                    Err(_) if cancel.is_cancelled() => Phase::Cancelled,
                    Err(_) => Phase::Failed,
                });
                if cancel.is_cancelled() {
                    wake();
                }
                result
            } else {
                loader
                    .load_document_resource_cancellable(&load.request, &cancel)
                    .await
            };
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
