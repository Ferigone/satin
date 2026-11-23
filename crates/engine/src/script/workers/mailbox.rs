use rust_browser_javascript::{ErrorKind, JsError};
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    time::Instant,
};

pub(super) const MAX_PACKET_BYTES: usize = 16 * 1024 * 1024;
const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;
const MAX_QUEUED_MESSAGES: usize = 256;

#[derive(Default)]
struct Usage {
    bytes: usize,
    messages: usize,
}

#[derive(Clone, Default)]
pub(super) struct Budget(Arc<Mutex<Usage>>);

impl Budget {
    pub(super) fn reserve(&self, bytes: usize) -> Result<Reservation, JsError> {
        let mut usage = self.0.lock().expect("worker message budget");
        if usage.messages == MAX_QUEUED_MESSAGES || bytes > MAX_QUEUED_BYTES - usage.bytes {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Worker message queue budget exhausted",
            ));
        }
        usage.bytes += bytes;
        usage.messages += 1;
        Ok(Reservation {
            budget: self.clone(),
            bytes,
        })
    }
}

pub(super) struct Reservation {
    budget: Budget,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut usage = self.budget.0.lock().expect("worker message budget");
        usage.bytes -= self.bytes;
        usage.messages -= 1;
    }
}

pub(super) struct Envelope<T>(pub(super) T, pub(super) Reservation);

struct Entry<T> {
    queued: Instant,
    message: Envelope<T>,
}

pub(super) enum Received<T> {
    Message(Envelope<T>),
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

    pub(super) fn receive_until(&self, deadline: Option<Instant>) -> Received<T> {
        let mut messages = self.messages.lock().expect("worker mailbox");
        loop {
            let Some(queue) = messages.as_mut() else {
                return Received::Closed;
            };
            // Keep FIFO within the mailbox but let a due timer run ahead of
            // newly arriving tasks. A busy producer cannot starve timers.
            if queue
                .front()
                .is_some_and(|entry| deadline.is_none_or(|due| entry.queued <= due))
            {
                return Received::Message(queue.pop_front().expect("ready entry").message);
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
    use std::time::Duration;

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
