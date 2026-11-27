use rust_browser_javascript::{ErrorKind, JsError};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

pub(super) const MAX_PACKET_BYTES: usize = 16 * 1024 * 1024;
const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;
const MAX_QUEUED_MESSAGES: usize = 256;
const HIGH_WATER_BYTES: usize = MAX_QUEUED_BYTES / 2;
const HIGH_WATER_MESSAGES: usize = MAX_QUEUED_MESSAGES / 2;

#[derive(Default)]
struct Usage {
    bytes: usize,
    messages: usize,
}

impl Usage {
    fn at_high_water(&self) -> bool {
        self.bytes >= HIGH_WATER_BYTES || self.messages >= HIGH_WATER_MESSAGES
    }
}

#[derive(Default)]
struct BudgetState {
    usage: Mutex<Usage>,
    available: Condvar,
}

#[derive(Clone, Default)]
pub(super) struct Budget(Arc<BudgetState>);

impl Budget {
    pub(super) fn reserve(&self, bytes: usize) -> Result<Reservation, JsError> {
        self.try_reserve(bytes, MAX_QUEUED_BYTES, MAX_QUEUED_MESSAGES)
            .ok_or_else(|| JsError::new(
                ErrorKind::HeapLimit,
                "Worker message queue budget exhausted",
            ))
    }

    pub(super) fn reserve_notice(&self, bytes: usize) -> Option<Reservation> {
        // Console/error notices are best-effort. Keep headroom for messages
        // sent by a worker that has already started its next turn.
        self.try_reserve(bytes, HIGH_WATER_BYTES - 1, HIGH_WATER_MESSAGES - 1)
    }

    fn try_reserve(
        &self,
        bytes: usize,
        byte_limit: usize,
        message_limit: usize,
    ) -> Option<Reservation> {
        let mut usage = self.0.usage.lock().expect("worker message budget");
        if usage.messages >= message_limit || bytes > byte_limit.checked_sub(usage.bytes)? {
            return None;
        }
        usage.bytes += bytes;
        usage.messages += 1;
        Some(Reservation {
            budget: self.clone(),
            bytes,
        })
    }

    pub(super) fn wait_for_turn(&self, cancelled: &AtomicBool) -> bool {
        let usage = self.0.usage.lock().expect("worker message budget");
        let _usage = self
            .0
            .available
            .wait_while(usage, |usage| {
                usage.at_high_water() && !cancelled.load(Ordering::Acquire)
            })
            .expect("worker message budget");
        !cancelled.load(Ordering::Acquire)
    }

    pub(super) fn notify_cancelled(&self) {
        // Cancellation is published before taking this lock. Synchronize with
        // the predicate and wait even if queued bytes belong to another worker.
        let _usage = self.0.usage.lock().expect("worker message budget");
        self.0.available.notify_all();
    }
}

pub(super) struct Reservation {
    budget: Budget,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let budget = &self.budget.0;
        let mut usage = budget.usage.lock().expect("worker message budget");
        let was_full = usage.at_high_water();
        usage.bytes -= self.bytes;
        usage.messages -= 1;
        if was_full && !usage.at_high_water() {
            budget.available.notify_all();
        }
    }
}

pub(super) struct Envelope<T>(pub(super) T, pub(super) Reservation);

struct Entry<T> {
    queued: Instant,
    message: Envelope<T>,
}

pub(super) enum Received<T, U = ()> {
    Message(Envelope<T>),
    External(U),
    Deadline,
    Closed,
}

pub(super) struct Mailbox<T> {
    messages: Mutex<Option<VecDeque<Entry<T>>>>,
    ready: Condvar,
}

impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Self {
            messages: Mutex::new(Some(VecDeque::new())),
            ready: Condvar::new(),
        }
    }
}

impl<T> Mailbox<T> {
    pub(super) fn send(&self, message: Envelope<T>) -> bool {
        let mut messages = self.messages.lock().expect("worker mailbox");
        let Some(queue) = messages.as_mut() else {
            return false;
        };
        queue.push_back(Entry {
            queued: Instant::now(),
            message,
        });
        self.ready.notify_one();
        true
    }

    pub(super) fn try_receive(&self) -> Option<Envelope<T>> {
        self.messages
            .lock()
            .expect("worker mailbox")
            .as_mut()?
            .pop_front()
            .map(|entry| entry.message)
    }

    #[cfg(test)]
    pub(super) fn receive_until(&self, deadline: Option<Instant>) -> Received<T> {
        self.receive_until_or(deadline, || None, false)
    }

    pub(super) fn receive_until_or<U>(
        &self,
        deadline: Option<Instant>,
        mut external: impl FnMut() -> Option<U>,
        external_first: bool,
    ) -> Received<T, U> {
        let mut messages = self.messages.lock().expect("worker mailbox");
        loop {
            let Some(queue) = messages.as_mut() else {
                return Received::Closed;
            };
            let timer_pending = deadline.is_some_and(|due| Instant::now() >= due);
            if !timer_pending
                && external_first
                && let Some(event) = external()
            {
                return Received::External(event);
            }
            // Keep FIFO within the mailbox but let a due timer run ahead of
            // newly arriving tasks. A busy producer cannot starve timers.
            if queue
                .front()
                .is_some_and(|entry| deadline.is_none_or(|due| entry.queued <= due))
            {
                return Received::Message(queue.pop_front().expect("ready entry").message);
            }
            if !timer_pending
                && !external_first
                && let Some(event) = external()
            {
                return Received::External(event);
            }
            messages = if let Some(due) = deadline {
                let remaining = due.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Received::Deadline;
                }
                self.ready
                    .wait_timeout(messages, remaining)
                    .expect("worker mailbox")
                    .0
            } else {
                self.ready.wait(messages).expect("worker mailbox")
            };
        }
    }

    pub(super) fn notify_external(&self) {
        // Producers publish outside this lock. Taking it once after publishing
        // prevents a lost wake between the consumer's predicate and wait.
        let _messages = self.messages.lock().expect("worker mailbox");
        self.ready.notify_one();
    }

    pub(super) fn has_messages(&self) -> bool {
        self.messages
            .lock()
            .expect("worker mailbox")
            .as_ref()
            .is_some_and(|queue| !queue.is_empty())
    }

    pub(super) fn close(&self) {
        let pending = self.messages.lock().expect("worker mailbox").take();
        self.ready.notify_all();
        drop(pending);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn releasing_output_allows_the_next_worker_turn() {
        let output = Budget::default();
        let held = output.reserve(HIGH_WATER_BYTES).unwrap();
        let (finished, result) = mpsc::channel();
        let waiting = output.clone();
        let worker = thread::spawn(move || {
            finished
                .send(waiting.wait_for_turn(&AtomicBool::new(false)))
                .unwrap();
        });

        assert!(result.recv_timeout(Duration::from_millis(30)).is_err());
        drop(held);
        assert!(result.recv_timeout(Duration::from_secs(3)).unwrap());
        worker.join().unwrap();
    }

    #[test]
    fn cancellation_allows_a_worker_to_leave_a_full_output_budget() {
        let output = Budget::default();
        let _held = output.reserve(HIGH_WATER_BYTES).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (finished, result) = mpsc::channel();
        let waiting = output.clone();
        let flag = cancelled.clone();
        let worker = thread::spawn(move || {
            finished.send(waiting.wait_for_turn(&flag)).unwrap();
        });

        assert!(result.recv_timeout(Duration::from_millis(30)).is_err());
        cancelled.store(true, Ordering::Release);
        output.notify_cancelled();
        assert!(!result.recv_timeout(Duration::from_secs(3)).unwrap());
        worker.join().unwrap();
    }

    #[test]
    fn messages_remain_charged_through_receipt_and_close_reclaims_the_rest() {
        let budget = Budget::default();
        let queue = Mailbox::default();
        let first = budget.reserve(MAX_PACKET_BYTES).unwrap();
        let second = budget.reserve(MAX_PACKET_BYTES).unwrap();
        assert!(queue.send(Envelope(1, first)));
        assert!(queue.send(Envelope(2, second)));
        let Received::Message(received) = queue.receive_until(None) else {
            panic!("first queued message was lost");
        };
        assert_eq!(received.0, 1);
        assert!(budget.reserve(1).is_err());
        queue.close();
        assert!(matches!(queue.receive_until(None), Received::Closed));
        assert!(budget.reserve(MAX_PACKET_BYTES).is_ok());
        drop(received);
        assert!(budget.reserve(MAX_QUEUED_BYTES).is_ok());
    }

    #[test]
    fn an_older_timer_goes_first_without_losing_the_queued_message() {
        let queue = Mailbox::default();
        let charge = Budget::default().reserve(1).unwrap();
        let earlier = Instant::now() - Duration::from_secs(1);
        assert!(queue.send(Envelope(42, charge)));
        assert!(matches!(
            queue.receive_until(Some(earlier)),
            Received::Deadline
        ));
        assert!(matches!(
            queue.receive_until(Some(Instant::now())),
            Received::Message(Envelope(42, _))
        ));
    }
}
