//! Browser-owned input and platform playback for HTML media elements.
use std::sync::Arc;

mod bridge;
pub(crate) use bridge::Bridge;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub tab: u64,
    pub navigation: u64,
    pub node: usize,
    pub generation: u64,
}

pub(crate) enum Command {
    Open {
        key: Key,
        bytes: Arc<[u8]>,
    },
    OpenStream {
        key: Key,
    },
    AddBuffer {
        key: Key,
        buffer: u64,
        mime: String,
    },
    Append {
        key: Key,
        buffer: u64,
        operation: u64,
        bytes: Arc<[u8]>,
        timestamp_offset: f64,
        append_window: (f64, f64),
    },
    RemoveRange {
        key: Key,
        buffer: u64,
        operation: u64,
        start: f64,
        end: f64,
    },
    RemoveBuffer {
        key: Key,
        buffer: u64,
    },
    AbortBuffer {
        key: Key,
        buffer: u64,
    },
    SetDuration {
        key: Key,
        duration: f64,
    },
    EndStream {
        key: Key,
        error: Option<&'static str>,
    },
    Play(Key),
    Pause(Key),
    Seek {
        key: Key,
        time: f64,
    },
    Volume {
        key: Key,
        value: f64,
    },
    Muted {
        key: Key,
        muted: bool,
    },
    Close(Key),
    ClosePage {
        tab: u64,
        navigation: u64,
    },
}

impl Command {
    fn player_key(&self) -> Option<Key> {
        match *self {
            Self::Open { key, .. }
            | Self::OpenStream { key }
            | Self::AddBuffer { key, .. }
            | Self::Append { key, .. }
            | Self::RemoveRange { key, .. }
            | Self::RemoveBuffer { key, .. }
            | Self::AbortBuffer { key, .. }
            | Self::SetDuration { key, .. }
            | Self::EndStream { key, .. }
            | Self::Play(key)
            | Self::Pause(key)
            | Self::Seek { key, .. }
            | Self::Volume { key, .. }
            | Self::Muted { key, .. }
            | Self::Close(key) => Some(key),
            Self::ClosePage { .. } => None,
        }
    }

    #[cfg(windows)]
    fn input_size(&self) -> usize {
        match self {
            Self::Open { bytes, .. } | Self::Append { bytes, .. } => bytes.len(),
            _ => 0,
        }
    }
}

pub(crate) struct Event {
    pub key: Key,
    pub kind: EventKind,
}

pub(crate) enum EventKind {
    StreamOpened,
    BufferUpdated {
        buffer: u64,
        operation: u64,
        result: Result<Vec<(f64, f64)>, String>,
    },
    Metadata {
        width: u32,
        height: u32,
        duration: f64,
        has_audio: bool,
    },
    Ready,
    Playing,
    Paused,
    Waiting,
    Seeking,
    Seeked(f64),
    Time(f64),
    Ended,
    Frame(Arc<image::RgbaImage>),
    Error(String),
}

#[cfg(windows)]
mod foundation;
#[cfg(windows)]
pub(crate) use foundation::Backend;

#[cfg(not(windows))]
pub(crate) struct Backend;

#[cfg(not(windows))]
impl Backend {
    pub(crate) fn new(_wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self
    }

    pub(crate) fn submit(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Open { .. } | Command::OpenStream { .. } => {
                Err("Native media playback is unavailable on this platform".into())
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<Event> {
        Vec::new()
    }
}

#[cfg(all(test, windows))]
mod tests;
