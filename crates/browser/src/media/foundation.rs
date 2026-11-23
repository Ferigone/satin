//! A single COM apartment owns the players. The browser receives only owned frames and state.
mod native;

use super::{Command, Event, EventKind, Key};
use native::{Interop, Player};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_PLAYERS: usize = 16;
const MAX_COMMANDS: usize = 128;
const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_NATIVE_EVENTS: usize = 256;
const MAX_UPDATES: usize = 512;
const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

pub(crate) struct Backend {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Backend {
    pub(crate) fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            shared: Arc::new(Shared {
                input: Mutex::default(),
                ready: Condvar::new(),
                updates: Mutex::default(),
                wake: Box::new(wake),
            }),
            worker: None,
        }
    }

    pub(crate) fn submit(&mut self, command: Command) -> Result<(), String> {
        if self.worker.is_none() {
            if !matches!(&command, Command::Open { .. } | Command::OpenStream { .. }) {
                return Ok(());
            }
            let shared = self.shared.clone();
            self.worker = Some(
                thread::Builder::new()
                    .name("rust-browser-media".into())
                    .spawn(move || run(&shared))
                    .map_err(|error| error.to_string())?,
            );
        }
        self.shared.command(command)
    }

    pub(crate) fn drain(&mut self) -> Vec<Event> {
        self.shared.updates.lock().unwrap().drain(..).collect()
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.shared.input.lock().unwrap().closed = true;
        self.shared.ready.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Signal {
    StreamOpened,
    StreamEnded,
    BufferComplete {
        buffer: u64,
        operation: u64,
        status: i32,
    },
    Metadata,
    Loaded,
    FirstFrame,
    Playing,
    Paused,
    Waiting,
    Seeking,
    Seeked,
    Time,
    Ended,
    Error {
        code: usize,
        detail: u32,
    },
}

enum Work {
    Command(Command),
    Native(Key, Signal),
    Tick,
}

#[derive(Default)]
struct Inbox {
    work: VecDeque<Work>,
    closed: bool,
}

struct Shared {
    input: Mutex<Inbox>,
    ready: Condvar,
    updates: Mutex<VecDeque<Event>>,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn command(&self, command: Command) -> Result<(), String> {
        let mut input = self.input.lock().unwrap();
        if input.closed {
            return Err("The media worker is closed".into());
        }
        let contains = |key: Key| match command {
            Command::Close(closed) => key == closed,
            Command::ClosePage { tab, navigation } => {
                key.tab == tab && key.navigation == navigation
            }
            _ => false,
        };
        if matches!(command, Command::Close(_) | Command::ClosePage { .. }) {
            // A navigation can discard unsent work for its old page. Cleanup must
            // reach the apartment even if pages have saturated its command budget.
            input.work.retain(|work| match work {
                Work::Command(prior) => prior.player_key().is_none_or(|key| !contains(key)),
                Work::Native(key, _) => !contains(*key),
                Work::Tick => true,
            });
        } else {
            let mut bytes = command.input_size();
            let mut count = 0;
            for work in &input.work {
                if let Work::Command(queued) = work {
                    bytes = bytes.saturating_add(queued.input_size());
                    count += 1;
                }
            }
            if count >= MAX_COMMANDS || bytes > MAX_INPUT_BYTES {
                return Err("The media command queue is full".into());
            }
        }
        input.work.push_back(Work::Command(command));
        self.ready.notify_one();
        Ok(())
    }

    fn signal(&self, key: Key, signal: Signal) {
        let mut input = self.input.lock().unwrap();
        if input.closed {
            return;
        }
        if signal == Signal::Time
            && input
                .work
                .iter()
                .any(|work| matches!(work, Work::Native(id, Signal::Time) if *id == key))
        {
            return;
        }
        if input.work.len() >= MAX_COMMANDS + MAX_NATIVE_EVENTS {
            let oldest = input
                .work
                .iter()
                .position(|work| matches!(work, Work::Native(_, Signal::Time)))
                .or_else(|| {
                    input
                        .work
                        .iter()
                        .position(|work| matches!(work, Work::Native(_, signal)
                            if !matches!(signal, Signal::StreamOpened | Signal::StreamEnded | Signal::BufferComplete { .. })))
                });
            if let Some(index) = oldest {
                input.work.remove(index);
            }
        }
        input.work.push_back(Work::Native(key, signal));
        self.ready.notify_one();
    }

    fn wait(&self, next_frame: Option<Instant>) -> Option<Work> {
        let mut input = self.input.lock().unwrap();
        while !input.closed {
            if let Some(work) = input.work.pop_front() {
                return Some(work);
            }
            input = if let Some(deadline) = next_frame {
                let now = Instant::now();
                if deadline <= now {
                    return Some(Work::Tick);
                }
                self.ready.wait_timeout(input, deadline - now).unwrap().0
            } else {
                self.ready.wait(input).unwrap()
            };
        }
        None
    }

    fn publish(&self, key: Key, kind: EventKind) {
        let mut updates = self.updates.lock().unwrap();
        let needs_wake = updates.is_empty();
        let droppable = |kind: &EventKind| matches!(kind, EventKind::Frame(_) | EventKind::Time(_));
        if droppable(&kind)
            && let Some(index) = updates.iter().position(|event| {
                event.key == key
                    && std::mem::discriminant(&event.kind) == std::mem::discriminant(&kind)
            })
        {
            updates.remove(index);
        }
        if updates.len() == MAX_UPDATES {
            if let Some(index) = updates.iter().position(|event| droppable(&event.kind)) {
                updates.remove(index);
            } else if droppable(&kind) {
                return;
            } else {
                updates.pop_front();
            }
        }
        updates.push_back(Event { key, kind });
        drop(updates);
        if needs_wake {
            (self.wake)();
        }
    }

    fn forget(&self, matches: impl Fn(Key) -> bool) {
        self.updates
            .lock()
            .unwrap()
            .retain(|event| !matches(event.key));
    }
}

fn run(shared: &Arc<Shared>) {
    let interop = Interop::new();
    let mut players = HashMap::<Key, Player>::new();
    let mut next_frame = Instant::now();
    while let Some(work) = shared.wait(
        players
            .values()
            .any(Player::needs_frame)
            .then_some(next_frame),
    ) {
        match work {
            Work::Command(command @ (Command::Open { .. } | Command::OpenStream { .. })) => {
                let key = command.player_key().unwrap();
                let same_element = |candidate: Key| {
                    candidate.tab == key.tab
                        && candidate.navigation == key.navigation
                        && candidate.node == key.node
                };
                players.retain(|candidate, _| !same_element(*candidate));
                shared.forget(same_element);
                let result = if players.len() >= MAX_PLAYERS {
                    Err("The native media player limit was reached".into())
                } else {
                    interop
                        .as_ref()
                        .map_err(ToString::to_string)
                        .and_then(|interop| {
                            match command {
                                Command::Open { bytes, .. } => {
                                    interop.open(key, &bytes, shared.clone())
                                }
                                Command::OpenStream { .. } => {
                                    interop.open_stream(key, shared.clone())
                                }
                                _ => unreachable!(),
                            }
                            .map_err(|error| error.to_string())
                        })
                };
                match result {
                    Ok(player) => {
                        players.insert(key, player);
                    }
                    Err(error) => shared.publish(key, EventKind::Error(error)),
                }
            }
            Work::Command(Command::Close(key)) => {
                players.remove(&key);
                shared.forget(|candidate| candidate == key);
            }
            Work::Command(Command::ClosePage { tab, navigation }) => {
                let same_page = |key: Key| key.tab == tab && key.navigation == navigation;
                players.retain(|key, _| !same_page(*key));
                shared.forget(same_page);
            }
            Work::Command(command) => {
                let key = command.player_key().unwrap();
                if let Some(player) = players.get_mut(&key)
                    && let Err(error) = player.control(command)
                {
                    shared.publish(key, EventKind::Error(error.to_string()));
                    players.remove(&key);
                }
            }
            Work::Native(key, signal) => {
                if let Some(player) = players.get_mut(&key)
                    && let Err(error) = player.notification(signal, shared, key)
                {
                    shared.publish(key, EventKind::Error(error));
                    players.remove(&key);
                }
            }
            Work::Tick => {}
        }
        if Instant::now() >= next_frame {
            next_frame = Instant::now() + FRAME_INTERVAL;
            players.retain(|key, player| {
                if !player.needs_frame() {
                    return true;
                }
                match player.frame(false, shared, *key) {
                    Ok(Some(frame)) => shared.publish(*key, EventKind::Frame(frame)),
                    Ok(None) => {}
                    Err(error) => {
                        shared.publish(*key, EventKind::Error(error.to_string()));
                        return false;
                    }
                }
                true
            });
        }
    }
}
