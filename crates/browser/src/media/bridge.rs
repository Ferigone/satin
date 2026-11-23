use super::{Backend, Command, Event, EventKind, Key};
use rust_browser_engine::{
    dom::NodeId,
    script::{MediaCommand, MediaKey, MediaUpdate, ScriptRuntime},
};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
struct PlayerState {
    opening: bool,
    starting: bool,
    seeking: bool,
    updates: HashSet<(u64, u64)>,
}

pub(crate) struct Bridge {
    backend: Backend,
    players: HashMap<Key, PlayerState>,
}

impl Bridge {
    pub(crate) fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            backend: Backend::new(wake),
            players: HashMap::new(),
        }
    }

    pub(crate) fn dispatch(
        &mut self,
        tab: u64,
        navigation: u64,
        runtime: &mut ScriptRuntime,
    ) -> bool {
        let native_key = |key: MediaKey| Key {
            tab,
            navigation,
            node: key.node.0,
            generation: key.generation,
        };
        let mut changed = false;
        for command in runtime.take_media_commands() {
            let command = match command {
                MediaCommand::Open { key, bytes, .. } => Command::Open {
                    key: native_key(key),
                    bytes,
                },
                MediaCommand::OpenStream { key } => Command::OpenStream {
                    key: native_key(key),
                },
                MediaCommand::AddBuffer { key, buffer, mime } => Command::AddBuffer {
                    key: native_key(key),
                    buffer,
                    mime,
                },
                MediaCommand::Append {
                    key,
                    buffer,
                    operation,
                    bytes,
                    timestamp_offset,
                    append_window,
                } => Command::Append {
                    key: native_key(key),
                    buffer,
                    operation,
                    bytes,
                    timestamp_offset,
                    append_window,
                },
                MediaCommand::RemoveRange {
                    key,
                    buffer,
                    operation,
                    start,
                    end,
                } => Command::RemoveRange {
                    key: native_key(key),
                    buffer,
                    operation,
                    start,
                    end,
                },
                MediaCommand::RemoveBuffer { key, buffer } => Command::RemoveBuffer {
                    key: native_key(key),
                    buffer,
                },
                MediaCommand::AbortBuffer { key, buffer } => Command::AbortBuffer {
                    key: native_key(key),
                    buffer,
                },
                MediaCommand::SetDuration { key, duration } => Command::SetDuration {
                    key: native_key(key),
                    duration,
                },
                MediaCommand::EndStream { key, error } => Command::EndStream {
                    key: native_key(key),
                    error,
                },
                MediaCommand::Play(key) => Command::Play(native_key(key)),
                MediaCommand::Pause(key) => Command::Pause(native_key(key)),
                MediaCommand::Seek { key, time } => Command::Seek {
                    key: native_key(key),
                    time,
                },
                MediaCommand::Volume { key, value } => Command::Volume {
                    key: native_key(key),
                    value,
                },
                MediaCommand::Muted { key, muted } => Command::Muted {
                    key: native_key(key),
                    muted,
                },
                MediaCommand::Close(key) => Command::Close(native_key(key)),
            };
            let key = command.player_key().unwrap();
            match &command {
                Command::Open { .. } | Command::OpenStream { .. } => {
                    self.players.retain(|prior, _| {
                        prior.tab != tab || prior.navigation != navigation || prior.node != key.node
                    });
                    self.players.insert(
                        key,
                        PlayerState {
                            opening: true,
                            ..Default::default()
                        },
                    );
                }
                Command::Close(_) => {
                    if self.players.remove(&key).is_none() {
                        continue;
                    }
                }
                other => {
                    let Some(state) = self.players.get_mut(&key) else {
                        continue;
                    };
                    match other {
                        Command::Play(_) => state.starting = true,
                        Command::Pause(_) => state.starting = false,
                        Command::Seek { .. } => state.seeking = true,
                        Command::Append {
                            buffer, operation, ..
                        }
                        | Command::RemoveRange {
                            buffer, operation, ..
                        } => {
                            state.updates.insert((*buffer, *operation));
                        }
                        Command::AbortBuffer { buffer, .. }
                        | Command::RemoveBuffer { buffer, .. } => {
                            state.updates.retain(|(id, _)| id != buffer);
                        }
                        _ => {}
                    }
                }
            }
            if let Err(error) = self.backend.submit(command) {
                self.players.remove(&key);
                let _ = self.backend.submit(Command::Close(key));
                changed |= runtime.update_media(media_key(key), MediaUpdate::Error(error));
            }
        }
        changed
    }

    pub(crate) fn close_page(&mut self, tab: u64, navigation: u64) {
        let count = self.players.len();
        self.players
            .retain(|key, _| key.tab != tab || key.navigation != navigation);
        if self.players.len() != count {
            let _ = self.backend.submit(Command::ClosePage { tab, navigation });
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<Event> {
        let mut events = self.backend.drain();
        events.retain(|event| {
            let Some(state) = self.players.get_mut(&event.key) else {
                return false;
            };
            match event.kind {
                EventKind::Ready | EventKind::StreamOpened => state.opening = false,
                EventKind::BufferUpdated {
                    buffer,
                    operation: 0,
                    result: Err(_),
                } => {
                    state.updates.retain(|(id, _)| *id != buffer);
                }
                EventKind::BufferUpdated {
                    buffer, operation, ..
                } => {
                    state.updates.remove(&(buffer, operation));
                }
                EventKind::Playing | EventKind::Paused | EventKind::Ended => {
                    state.starting = false;
                }
                EventKind::Seeked(_) => state.seeking = false,
                EventKind::Error(_) => {
                    self.players.remove(&event.key);
                }
                _ => {}
            }
            true
        });
        events
    }

    pub(crate) fn has_open(&self) -> bool {
        !self.players.is_empty()
    }

    pub(crate) fn has_pending(&self, tab: u64, navigation: u64) -> bool {
        self.players.iter().any(|(key, state)| {
            key.tab == tab
                && key.navigation == navigation
                && (state.opening || state.starting || state.seeking || !state.updates.is_empty())
        })
    }

    pub(crate) fn apply(runtime: &mut ScriptRuntime, event: Event) -> bool {
        let update = match event.kind {
            EventKind::StreamOpened => MediaUpdate::StreamOpened,
            EventKind::BufferUpdated {
                buffer,
                operation,
                result,
            } => MediaUpdate::BufferUpdated {
                buffer,
                operation,
                result,
            },
            EventKind::Metadata {
                width,
                height,
                duration,
                ..
            } => MediaUpdate::Metadata {
                width,
                height,
                duration,
            },
            EventKind::Ready => MediaUpdate::Ready,
            EventKind::Playing => MediaUpdate::Playing,
            EventKind::Paused => MediaUpdate::Paused,
            EventKind::Waiting => MediaUpdate::Waiting,
            EventKind::Seeking => MediaUpdate::Seeking,
            EventKind::Seeked(time) => MediaUpdate::Seeked(time),
            EventKind::Time(time) => MediaUpdate::Time(time),
            EventKind::Ended => MediaUpdate::Ended,
            EventKind::Frame(frame) => MediaUpdate::Frame(frame),
            EventKind::Error(error) => MediaUpdate::Error(error),
        };
        runtime.update_media(media_key(event.key), update)
    }
}

fn media_key(key: Key) -> MediaKey {
    MediaKey {
        node: NodeId(key.node),
        generation: key.generation,
    }
}
