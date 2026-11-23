//! Request bytes travel from the document task to the HTTP body on demand.
use super::{FetchError, FetchErrorKind, WebRequest};
use hyper::body::{Body, Bytes, Frame};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::sync::{Notify, mpsc, watch};

#[cfg(test)]
mod tests;

pub struct FetchJob {
    pub id: u64,
    pub request: WebRequest,
    pub upload: Option<UploadBody>,
}

enum UploadItem {
    Data(Bytes),
    End,
    Failed(FetchError),
}

pub(crate) struct UploadSender(mpsc::Sender<UploadItem>);

impl UploadSender {
    pub(crate) fn data(&self, bytes: Bytes) -> Result<(), FetchError> {
        self.send(UploadItem::Data(bytes))
    }

    pub(crate) fn end(&self) -> Result<(), FetchError> {
        self.send(UploadItem::End)
    }

    pub(crate) fn fail(&self, error: FetchError) -> Result<(), FetchError> {
        self.send(UploadItem::Failed(error))
    }

    fn send(&self, item: UploadItem) -> Result<(), FetchError> {
        self.0.try_send(item).map_err(|_| interrupted())
    }
}

#[derive(Clone)]
pub struct UploadDemand(Arc<Notify>);

impl UploadDemand {
    pub async fn next(&self) {
        self.0.notified().await;
    }
}

pub struct UploadBody {
    chunks: mpsc::Receiver<UploadItem>,
    demand: UploadDemand,
    completion: watch::Sender<Option<Result<(), FetchError>>>,
    waiting: bool,
    ended: bool,
}

pub(crate) fn upload_pipe() -> (UploadSender, UploadBody) {
    let (tx, rx) = mpsc::channel(1);
    let (completion, _) = watch::channel(None);
    (
        UploadSender(tx),
        UploadBody {
            chunks: rx,
            demand: UploadDemand(Arc::new(Notify::new())),
            completion,
            waiting: false,
            ended: false,
        },
    )
}

fn interrupted() -> FetchError {
    FetchError::new(FetchErrorKind::Network, "Fetch upload ended before EOF")
}

impl UploadBody {
    pub fn demand(&self) -> UploadDemand {
        self.demand.clone()
    }

    pub(super) fn completion(&self) -> watch::Receiver<Option<Result<(), FetchError>>> {
        self.completion.subscribe()
    }

    fn complete(&mut self, result: Result<(), FetchError>) {
        self.ended = true;
        self.completion.send_replace(Some(result));
    }
}

impl Body for UploadBody {
    type Data = Bytes;
    type Error = FetchError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, FetchError>>> {
        if self.ended {
            return Poll::Ready(None);
        }
        loop {
            match self.chunks.poll_recv(cx) {
                Poll::Ready(Some(UploadItem::Data(bytes))) => {
                    self.waiting = false;
                    if !bytes.is_empty() {
                        return Poll::Ready(Some(Ok(Frame::data(bytes))));
                    }
                }
                Poll::Ready(Some(UploadItem::End)) => {
                    self.complete(Ok(()));
                    return Poll::Ready(None);
                }
                Poll::Ready(result) => {
                    let error = match result {
                        Some(UploadItem::Failed(error)) => error,
                        _ => interrupted(),
                    };
                    self.complete(Err(error.clone()));
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Pending => {
                    if !self.waiting {
                        self.waiting = true;
                        self.demand.0.notify_one();
                    }
                    return Poll::Pending;
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.ended
    }
}

impl Drop for UploadBody {
    fn drop(&mut self) {
        if !self.ended {
            self.complete(Err(interrupted()));
        }
    }
}

pub(super) async fn wait_for_eof(
    mut completion: watch::Receiver<Option<Result<(), FetchError>>>,
) -> Result<(), FetchError> {
    completion
        .wait_for(Option::is_some)
        .await
        .map_err(|_| interrupted())?
        .clone()
        .unwrap_or_else(|| Err(interrupted()))
}
