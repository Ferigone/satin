//! Agent-local asynchronous wait records ordered by backing store, byte offset
//! and arrival. The deadline index avoids scanning unrelated waiting promises.
use crate::{ErrorKind, JsError, ObjectId, Value, Vm};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

type Key = (ObjectId, usize, u64);

#[derive(Default)]
pub(crate) struct Waiters {
    records: BTreeMap<Key, (ObjectId, Option<Instant>)>,
    deadlines: BTreeSet<(Instant, Key)>,
    sequence: u64,
}

impl Waiters {
    const RECORD_BYTES: usize = 256;

    pub(crate) fn timed_roots(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.records
            .iter()
            .filter(|(_, (_, deadline))| deadline.is_some())
            .flat_map(|(key, (promise, _))| [key.0, *promise])
    }

    pub(crate) fn indefinite_edges(&self) -> impl Iterator<Item = (ObjectId, ObjectId)> + '_ {
        self.records
            .iter()
            .filter(|(_, (_, deadline))| deadline.is_none())
            .map(|(key, (promise, _))| (key.0, *promise))
    }

    pub(crate) fn retain_live(&mut self, heap: &crate::heap::Heap) {
        // Timed records are always rooted. An indefinite wait cannot resolve
        // after the final reference to its backing store has disappeared.
        self.records.retain(|key, _| heap.object(key.0).is_ok());
    }

    pub(crate) fn estimated_bytes(&self) -> usize {
        self.records.len().saturating_mul(Self::RECORD_BYTES)
    }
}

impl Vm {
    pub(super) fn add_atomic_waiter(
        &mut self,
        store: ObjectId,
        offset: usize,
        milliseconds: f64,
        promise: ObjectId,
    ) -> Result<(), JsError> {
        if self.atomic_waiters.records.len() >= self.limits.max_pending_jobs {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Pending atomic wait budget exhausted",
            ));
        }
        self.reserve_allocation(Waiters::RECORD_BYTES)?;
        let sequence = self.atomic_waiters.sequence.checked_add(1).ok_or_else(|| {
            JsError::new(ErrorKind::ExecutionLimit, "Atomic wait sequence exhausted")
        })?;
        self.atomic_waiters.sequence = sequence;
        let key = (store, offset, sequence);
        // Infinite timeouts and finite times beyond the representable clock horizon
        // both remain notifyable. A positive sub-nanosecond timeout is asynchronous.
        let deadline = Duration::try_from_secs_f64(milliseconds / 1000.0)
            .ok()
            .and_then(|delay| Instant::now().checked_add(delay.max(Duration::from_nanos(1))));
        if let Some(instant) = deadline {
            self.atomic_waiters.deadlines.insert((instant, key));
        }
        self.atomic_waiters.records.insert(key, (promise, deadline));
        Ok(())
    }

    fn wake_atomic_waiter(&mut self, key: Key, result: &'static str) -> Result<(), JsError> {
        let (promise, deadline) = self.atomic_waiters.records[&key];
        // Every backing store is local to this single agent. NotifyWaiter
        // fulfills its intrinsic promise now, enqueuing attached reactions.
        self.settle_promise(promise, Value::from(result), false)?;
        self.atomic_waiters.records.remove(&key);
        if let Some(deadline) = deadline {
            self.atomic_waiters.deadlines.remove(&(deadline, key));
        }
        Ok(())
    }

    /// Earliest monotonic timeout to schedule on the embedding event loop.
    pub fn next_atomic_wait_deadline(&self) -> Option<Instant> {
        self.atomic_waiters.deadlines.first().map(|entry| entry.0)
    }

    pub(crate) fn poll_atomic_timeouts(&mut self) -> Result<(), JsError> {
        if self.atomic_waiters.deadlines.is_empty() {
            return Ok(());
        }
        let now = Instant::now();
        while let Some(&(deadline, key)) = self.atomic_waiters.deadlines.first() {
            if deadline > now || self.jobs.len() >= self.limits.max_pending_jobs {
                break;
            }
            self.consume_work(1)?;
            self.wake_atomic_waiter(key, "timed-out")?;
        }
        Ok(())
    }

    pub(super) fn notify_atomic_waiters(
        &mut self,
        store: ObjectId,
        offset: usize,
        count: usize,
    ) -> Result<usize, JsError> {
        let mut notified = 0;
        while notified < count {
            let Some((&key, _)) = self
                .atomic_waiters
                .records
                .range((store, offset, 0)..=(store, offset, u64::MAX))
                .next()
            else {
                break;
            };
            self.consume_work(1)?;
            self.wake_atomic_waiter(key, "ok")?;
            notified += 1;
        }
        Ok(notified)
    }
}
