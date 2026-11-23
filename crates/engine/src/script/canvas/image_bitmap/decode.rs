//! Blob decoding stays off the document thread. A permit covers both work and
//! completed pixels waiting for consumption, including across separate tabs.

use super::{Source, format::Options};
use crate::{loader::decode_oriented_bitmap, script::blobs::Blob};
use rust_browser_javascript::{PromiseCapability, RootId, Vm};
use std::{
    ops::Range,
    rc::Rc,
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicUsize, Ordering},
        mpsc::{Receiver, Sender, SyncSender, TryRecvError, channel, sync_channel},
    },
    time::{Duration, Instant},
};

const PER_DOCUMENT: usize = 4;
const GLOBAL_JOBS: usize = 6;
const POLL_DELAY: Duration = Duration::from_millis(5);

static OUTSTANDING: AtomicUsize = AtomicUsize::new(0);
static WORKER: OnceLock<Option<SyncSender<Job>>> = OnceLock::new();

struct Permit;

impl Permit {
    fn acquire() -> Option<Self> {
        if OUTSTANDING.fetch_add(1, Ordering::Relaxed) >= GLOBAL_JOBS {
            OUTSTANDING.fetch_sub(1, Ordering::Relaxed);
            None
        } else {
            Some(Self)
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        OUTSTANDING.fetch_sub(1, Ordering::Relaxed);
    }
}

struct Job {
    bytes: Arc<Vec<u8>>,
    range: Range<usize>,
    options: Options,
    live: Weak<()>,
    reply: Sender<Reply>,
    permit: Permit,
}

impl Job {
    fn run(self) {
        if self.live.strong_count() == 0 {
            return;
        }
        let result = self.decode();
        let _ = self.reply.send(Reply {
            result,
            _permit: self.permit,
        });
    }

    fn decode(&self) -> Result<Source, &'static str> {
        let image = decode_oriented_bitmap(
            &self.bytes[self.range.clone()],
            self.options.uses_metadata_orientation(),
        )
        .map_err(|_| "Unable to decode the Blob as an image")?;
        let size = [image.width(), image.height()];
        let source = Source {
            size,
            bitmap: Some(Arc::new(image)),
            origin_clean: true,
            alpha: true,
            premultiplied: false,
        };
        let raster = self
            .options
            .raster(&source)
            .ok_or("ImageBitmap dimensions exceed the bitmap limit")?;
        raster
            .apply(source)
            .map_err(|_| "Unable to allocate ImageBitmap pixels")
    }
}

fn worker() -> Option<&'static SyncSender<Job>> {
    WORKER
        .get_or_init(|| {
            let (send, receive) = sync_channel::<Job>(GLOBAL_JOBS);
            std::thread::Builder::new()
                .name("image-bitmap".into())
                .spawn(move || {
                    while let Ok(job) = receive.recv() {
                        job.run();
                    }
                })
                .ok()?;
            Some(send)
        })
        .as_ref()
}

struct Reply {
    result: Result<Source, &'static str>,
    _permit: Permit,
}

struct Pending {
    resolve: RootId,
    reject: RootId,
    reply: Receiver<Reply>,
    _live: Arc<()>,
    _blob: Rc<Blob>,
}

pub(super) struct Completed {
    pub(super) resolve: RootId,
    pub(super) reject: RootId,
    pub(super) result: Result<Source, &'static str>,
    _permit: Option<Permit>,
}

#[derive(Default)]
pub(in crate::script::canvas) struct DecodeQueue {
    pending: Vec<Pending>,
    deadline: Option<Instant>,
}

impl DecodeQueue {
    pub(in crate::script::canvas) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) fn submit(
        &mut self,
        vm: &mut Vm,
        promise: &PromiseCapability,
        blob: Rc<Blob>,
        options: Options,
    ) -> Result<(), &'static str> {
        const BUSY: &str = "ImageBitmap decode queue is full";
        if self.pending.len() >= PER_DOCUMENT {
            return Err(BUSY);
        }
        let permit = Permit::acquire().ok_or(BUSY)?;
        let worker = worker().ok_or("ImageBitmap decoder is unavailable")?;
        let (bytes, range) = blob.shared_bytes();
        let (reply, receive) = channel();
        let live = Arc::new(());
        worker
            .try_send(Job {
                bytes,
                range,
                options,
                live: Arc::downgrade(&live),
                reply,
                permit,
            })
            .map_err(|_| "ImageBitmap decoder is unavailable")?;
        self.pending.push(Pending {
            resolve: vm.root(promise.resolve.clone()),
            reject: vm.root(promise.reject.clone()),
            reply: receive,
            _live: live,
            _blob: blob,
        });
        self.deadline
            .get_or_insert_with(|| Instant::now() + POLL_DELAY);
        Ok(())
    }

    pub(super) fn take_completed(&mut self) -> Vec<Completed> {
        let now = Instant::now();
        if self.deadline.is_none_or(|deadline| deadline > now) {
            return Vec::new();
        }
        let mut completed = Vec::new();
        let mut index = 0;
        while index < self.pending.len() {
            let (result, permit) = match self.pending[index].reply.try_recv() {
                Ok(reply) => (reply.result, Some(reply._permit)),
                Err(TryRecvError::Disconnected) => {
                    (Err("ImageBitmap decoder is unavailable"), None)
                }
                Err(TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
            };
            let pending = self.pending.remove(index);
            completed.push(Completed {
                resolve: pending.resolve,
                reject: pending.reject,
                result,
                _permit: permit,
            });
        }
        self.deadline = (!self.pending.is_empty()).then(|| now + POLL_DELAY);
        completed
    }
}
