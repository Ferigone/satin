//! Worker-owned Fetch state and a Rust-only channel to the transport reactor.
use super::Wake;
use crate::{
    Loader,
    cookies::CookieStore,
    loader::{ConnectPolicy, LoadedPage, transport},
    network::{
        CancelSignal, FETCH_TIMEOUT, FetchError, FetchErrorKind, FetchJob, StreamingResponse,
    },
    script::{
        abort::AbortHost, blobs, fetch::FetchHost, streams::StreamHost, text_encoding, url_api,
    },
};
use rust_browser_javascript::{JsError, Vm};
use std::{
    collections::VecDeque,
    sync::{Arc, OnceLock},
    time::Instant,
};
use tokio::sync::{Semaphore, mpsc};
use url::Url;

const PROCESS_TRANSFERS: usize = 64;
const WORKER_TRANSFERS: usize = 8;
const EVENT_SLOTS: usize = 32;
static TRANSFERS: Semaphore = Semaphore::const_new(PROCESS_TRANSFERS);

#[derive(Clone)]
pub(super) struct Network {
    client: Url,
    loader: Arc<OnceLock<Result<Loader, String>>>,
    cookies: CookieStore,
    headers: ConnectPolicy,
    policy: ConnectPolicy,
}

impl Network {
    pub(super) fn from_page(page: &LoadedPage) -> Self {
        let loader = OnceLock::new();
        if let Some(mut shared) = page.fetch_loader.clone() {
            shared.cookies = page.cookies.clone();
            let _ = loader.set(Ok(shared));
        }
        Self {
            client: Url::parse(&page.url).unwrap_or_else(|_| page.base.clone()),
            loader: Arc::new(loader),
            cookies: page.cookies.clone(),
            headers: page.connect_policy.clone(),
            policy: page.meta_connect_policy.clone(),
        }
    }

    pub(super) fn inherit_meta(&self, policy: &ConnectPolicy) -> Self {
        Self {
            policy: self.headers.intersect(policy),
            ..self.clone()
        }
    }
}

pub(super) enum FetchEvent {
    Head(u64, Result<StreamingResponse, FetchError>),
    Body(u64),
    Upload(u64),
}

#[derive(Clone)]
struct Sender {
    tx: mpsc::Sender<FetchEvent>,
    generation: CancelSignal,
    wake: Wake,
}

impl Sender {
    async fn send(&self, event: FetchEvent) -> bool {
        tokio::select! {
            biased;
            () = self.generation.cancelled() => false,
            sent = self.tx.send(event) => {
                if sent.is_ok() {
                    (self.wake)();
                    true
                } else {
                    false
                }
            }
        }
    }
}

pub(super) struct WorkerFetch {
    host: FetchHost,
    network: Network,
    sender: Sender,
    incoming: mpsc::Receiver<FetchEvent>,
    rejected: VecDeque<(u64, FetchError)>,
    prefer_rejected: bool,
    transfers: Arc<Semaphore>,
}

impl WorkerFetch {
    pub(super) fn install(
        vm: &mut Vm,
        address: &Url,
        abort: AbortHost,
        network: Network,
        generation: CancelSignal,
        wake: Wake,
    ) -> Result<Self, JsError> {
        url_api::install(vm)?;
        text_encoding::install(vm)?;
        let streams = StreamHost::install(vm, abort.clone())?;
        let blobs = blobs::install(vm, streams.clone())?;
        // Worker URL resolution uses the Blob URL as its base. Transport
        // security uses the creator's environment, not the `blob:` scheme.
        let host = FetchHost::install_with_urls(
            vm,
            address.clone(),
            network.client.clone(),
            abort,
            streams,
            blobs,
        )?;
        let (tx, incoming) = mpsc::channel(EVENT_SLOTS);
        Ok(Self {
            host,
            network,
            sender: Sender {
                tx,
                generation,
                wake,
            },
            incoming,
            rejected: VecDeque::new(),
            prefer_rejected: false,
            transfers: Arc::new(Semaphore::new(WORKER_TRANSFERS)),
        })
    }

    /// Called between worker VM tasks, after the microtask checkpoint.
    pub(super) fn dispatch(&mut self) {
        if self.sender.generation.is_cancelled() {
            return;
        }
        // Aborts can retire pending requests before the local rejection is
        // delivered. Do not retain such results across worker tasks.
        self.rejected
            .retain(|(id, _)| self.host.has_pending_request(*id));
        for job in self.host.take_jobs() {
            let id = job.id;
            if let Err(error) = self.submit(job) {
                self.rejected.push_back((id, error));
            }
        }
    }

    fn submit(&self, job: FetchJob) -> Result<(), FetchError> {
        let at_capacity = || FetchError::limit("Too many open Worker Fetch transfers");
        let process = TRANSFERS.try_acquire().map_err(|_| at_capacity())?;
        let worker = self
            .transfers
            .clone()
            .try_acquire_owned()
            .map_err(|_| at_capacity())?;
        let runtime = transport::runtime()
            .map_err(|error| FetchError::new(FetchErrorKind::Network, error))?;
        let sender = self.sender.clone();
        let network = self.network.clone();
        let deadline = Instant::now() + FETCH_TIMEOUT;
        runtime.spawn(async move {
            let (_process, _worker) = (process, worker);
            transfer(network, sender, job, deadline).await;
        });
        Ok(())
    }

    pub(super) fn next_event(&mut self) -> Option<FetchEvent> {
        if !self.prefer_rejected
            && let Ok(event) = self.incoming.try_recv()
        {
            self.prefer_rejected = true;
            return Some(event);
        }
        if let Some((id, error)) = self.rejected.pop_front() {
            self.prefer_rejected = false;
            return Some(FetchEvent::Head(id, Err(error)));
        }
        self.incoming.try_recv().ok()
    }

    /// Only the worker thread delivers events to JavaScript.
    pub(super) fn receive(&self, vm: &mut Vm, event: FetchEvent) -> Result<(), JsError> {
        if self.sender.generation.is_cancelled() {
            return Ok(());
        }
        match event {
            FetchEvent::Head(id, result) => self.host.complete(vm, id, result),
            FetchEvent::Body(id) => self.host.progress(vm, id),
            FetchEvent::Upload(id) => self.host.progress_upload(vm, id),
        }
    }
}

impl Drop for WorkerFetch {
    fn drop(&mut self) {
        self.sender.generation.cancel();
    }
}

async fn transfer(network: Network, sender: Sender, job: FetchJob, deadline: Instant) {
    if sender.generation.is_cancelled() {
        return;
    }
    let FetchJob {
        id,
        request,
        upload,
    } = job;
    let loader = match network
        .loader
        .get_or_init(|| Loader::with_cookies(network.cookies))
    {
        Ok(loader) => loader,
        Err(error) => {
            sender
                .send(FetchEvent::Head(
                    id,
                    Err(FetchError::new(FetchErrorKind::Network, error.clone())),
                ))
                .await;
            return;
        }
    };
    let demand = upload.as_ref().map(|body| body.demand());
    let response = loader.open_web_fetch_with_connect_policy(
        &request,
        upload,
        &sender.generation,
        deadline,
        Some(&network.policy),
    );
    let result = if let Some(demand) = demand {
        tokio::select! {
            biased;
            result = response => result,
            () = async {
                loop {
                    demand.next().await;
                    if !sender.send(FetchEvent::Upload(id)).await {
                        break;
                    }
                }
            } => return,
        }
    } else {
        response.await
    };
    match result {
        Ok((head, body)) => {
            if sender.send(FetchEvent::Head(id, Ok(head))).await
                && let Some(body) = body
            {
                body.run(|| async {
                    sender.send(FetchEvent::Body(id)).await;
                })
                .await;
            }
        }
        Err(error) => {
            sender.send(FetchEvent::Head(id, Err(error))).await;
        }
    }
}
