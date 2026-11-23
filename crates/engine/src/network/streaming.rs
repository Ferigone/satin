//! Fetch exposes authorized headers before the response body is buffered.
use super::{
    FETCH_BODY_LIMIT, FetchError, UploadBody, WebRequest, WebResponse, interrupted_fetch, net_error,
};
use crate::{
    Loader,
    loader::transport::{CancelSignal, RequestControl},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::Instant,
};
use tokio::sync::mpsc;

pub const FETCH_CHUNK_BYTES: usize = 32 * 1024;
const PIPE_SLOTS: usize = 2;

pub struct StreamingResponse {
    pub metadata: WebResponse,
    pub transfer: Option<FetchBody>,
}

impl From<WebResponse> for StreamingResponse {
    fn from(metadata: WebResponse) -> Self {
        Self {
            metadata,
            transfer: None,
        }
    }
}

#[derive(Default)]
struct Progress {
    terminal: Mutex<Option<Result<(), FetchError>>>,
    notified: AtomicBool,
}

pub struct FetchBody {
    chunks: mpsc::Receiver<Vec<u8>>,
    progress: Arc<Progress>,
}

impl FetchBody {
    /// Acknowledge the event before looking at the pipe. A racing send will
    /// either be visible now or post another event after this acknowledgment.
    pub fn acknowledge(&self) {
        self.progress.notified.store(false, Ordering::Release);
    }

    pub fn outcome(&self) -> Option<Result<(), FetchError>> {
        match self.progress.terminal.lock().unwrap().as_ref()? {
            Err(error) => Some(Err(error.clone())),
            Ok(()) if self.chunks.is_empty() => Some(Ok(())),
            Ok(()) => None,
        }
    }

    pub fn read(&mut self) -> Poll<Result<Option<Vec<u8>>, FetchError>> {
        if let Some(terminal) = self.outcome() {
            return Poll::Ready(terminal.map(|()| None));
        }
        match self.chunks.try_recv() {
            Ok(chunk) => Poll::Ready(Ok(Some(chunk))),
            Err(_) => self
                .outcome()
                .map_or(Poll::Pending, |result| Poll::Ready(result.map(|()| None))),
        }
    }
}

pub struct FetchTransfer {
    response: reqwest::Response,
    chunks: mpsc::Sender<Vec<u8>>,
    progress: Arc<Progress>,
    generation: CancelSignal,
    request: Option<CancelSignal>,
    deadline: Instant,
}

impl FetchTransfer {
    pub async fn run<F: Future<Output = ()>>(mut self, mut notify: impl FnMut() -> F) {
        let generation = self.generation.clone();
        let request = self.request.clone();
        let progress = self.progress.clone();
        let control =
            RequestControl::new(&generation, self.deadline).with_request(request.as_ref());
        let mut progress_event =
            || (!progress.notified.swap(true, Ordering::AcqRel)).then(&mut notify);
        let result = self.receive(&control, &mut progress_event).await;
        // Release the socket before publishing any terminal event to a busy UI.
        drop(self.response);
        *progress.terminal.lock().unwrap() = Some(result);
        if !self.chunks.is_closed() && !generation.is_cancelled() {
            notify().await;
        }
    }

    async fn receive<F: Future<Output = ()>>(
        &mut self,
        control: &RequestControl<'_>,
        notify: &mut impl FnMut() -> Option<F>,
    ) -> Result<(), FetchError> {
        loop {
            let Ok(permit) = control
                .wait(self.chunks.reserve())
                .await
                .map_err(interrupted_fetch)?
            else {
                return Ok(());
            };
            let chunk = tokio::select! {
                biased;
                () = self.chunks.closed() => return Ok(()),
                result = control.wait(self.response.chunk()) => {
                    result.map_err(interrupted_fetch)?.map_err(net_error)?
                }
            };
            let Some(chunk) = chunk else {
                return Ok(());
            };
            let mut first = Some(permit);
            for piece in chunk.chunks(FETCH_CHUNK_BYTES) {
                let slot = match first.take() {
                    Some(permit) => permit,
                    None => match control
                        .wait(self.chunks.reserve())
                        .await
                        .map_err(interrupted_fetch)?
                    {
                        Ok(permit) => permit,
                        Err(_) => return Ok(()),
                    },
                };
                slot.send(piece.to_vec());
                if let Some(signal) = notify() {
                    tokio::select! {
                        biased;
                        () = self.chunks.closed() => return Ok(()),
                        sent = control.wait(signal) => sent.map_err(interrupted_fetch)?,
                    }
                }
            }
        }
    }
}

impl Loader {
    pub async fn open_web_fetch(
        &self,
        request: &WebRequest,
        generation: &CancelSignal,
        deadline: Instant,
    ) -> Result<(StreamingResponse, Option<FetchTransfer>), FetchError> {
        self.open_web_fetch_with_upload(request, None, generation, deadline)
            .await
    }

    pub async fn open_web_fetch_with_upload(
        &self,
        request: &WebRequest,
        upload: Option<UploadBody>,
        generation: &CancelSignal,
        deadline: Instant,
    ) -> Result<(StreamingResponse, Option<FetchTransfer>), FetchError> {
        let control =
            RequestControl::new(generation, deadline).with_request(request.signal.as_ref());
        let opened = self
            .open_fetch(request, &control, FETCH_BODY_LIMIT, true, upload)
            .await?;
        let Some(response) = opened.transfer else {
            return Ok((opened.metadata.into(), None));
        };
        let (tx, rx) = mpsc::channel(PIPE_SLOTS);
        let progress = Arc::new(Progress::default());
        Ok((
            StreamingResponse {
                metadata: opened.metadata,
                transfer: Some(FetchBody {
                    chunks: rx,
                    progress: progress.clone(),
                }),
            },
            Some(FetchTransfer {
                response,
                chunks: tx,
                progress,
                generation: generation.clone(),
                request: request.signal.clone(),
                deadline,
            }),
        ))
    }
}
