//! Media element state. The application supplies decoded frames and playback
//! events from its platform backend. Downloads use DocumentSession.
mod mse;
mod quality;

use super::{
    ErrorKind, JsError, RootId, Shared, Value, Vm, abort, arg, blobs, diagnostic, dispatch_checked,
    document_base, drain_microtask_queue, getter, mark_dirty, node, scheduler, set_attribute, webidl,
};
use crate::{
    canvas::Frame,
    dom::{Document, Namespace, NodeId},
    loader::{ResourceKind, ResourceRequest},
    mime::MimeType,
    network::CredentialsMode,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Instant,
};

const MAX_ELEMENTS: usize = 12;
const MAX_PENDING_PLAYS: usize = 32;
const MAX_COMMANDS: usize = 128;
const MAX_PLAYED_RANGES: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MediaKey {
    pub node: NodeId,
    pub generation: u64,
}

pub enum MediaCommand {
    Open {
        key: MediaKey,
        url: String,
        bytes: Arc<[u8]>,
    },
    OpenStream {
        key: MediaKey,
    },
    AddBuffer {
        key: MediaKey,
        buffer: u64,
        mime: String,
    },
    Append {
        key: MediaKey,
        buffer: u64,
        operation: u64,
        bytes: Arc<[u8]>,
        timestamp_offset: f64,
        append_window: (f64, f64),
    },
    RemoveRange {
        key: MediaKey,
        buffer: u64,
        operation: u64,
        start: f64,
        end: f64,
    },
    RemoveBuffer {
        key: MediaKey,
        buffer: u64,
    },
    AbortBuffer {
        key: MediaKey,
        buffer: u64,
    },
    SetDuration {
        key: MediaKey,
        duration: f64,
    },
    EndStream {
        key: MediaKey,
        error: Option<&'static str>,
    },
    Play(MediaKey),
    Pause(MediaKey),
    Seek {
        key: MediaKey,
        time: f64,
    },
    PlaybackRate {
        key: MediaKey,
        revision: u64,
        value: f64,
    },
    Volume {
        key: MediaKey,
        value: f64,
    },
    Muted {
        key: MediaKey,
        muted: bool,
    },
    Close(MediaKey),
}

pub enum MediaUpdate {
    Metadata {
        width: u32,
        height: u32,
        duration: f64,
        has_audio: bool,
    },
    Ready,
    StreamOpened,
    BufferUpdated {
        buffer: u64,
        operation: u64,
        result: Result<Vec<(f64, f64)>, String>,
    },
    Playing,
    Paused,
    RateRejected {
        revision: u64,
        actual: f64,
    },
    Waiting,
    Seeking,
    SeekNeedsData,
    Seeked(f64),
    Time(f64),
    Ended,
    Frame {
        pixels: Arc<image::RgbaImage>,
        sequence: u64,
    },
    DecoderFrameStats {
        dropped: u32,
        corrupted: u32,
    },
    Error(String),
}

#[cfg(feature = "media-startup-diagnostics")]
fn trace_media_update(key: MediaKey, update: &MediaUpdate) {
    use std::sync::{OnceLock, atomic::{AtomicUsize, Ordering}};
    static ENABLED: OnceLock<bool> = OnceLock::new();
    static TRACES: AtomicUsize = AtomicUsize::new(0);
    if !*ENABLED.get_or_init(|| std::env::var_os("SATIN_QA_MEDIA_TRANSITIONS").is_some()) {
        return;
    }
    let message = match update {
        MediaUpdate::BufferUpdated { buffer, operation, result } => match result {
            Ok(ranges) => format!("append {buffer} {operation} {:?}", &ranges[..ranges.len().min(12)]),
            Err(error) => format!("append-error {buffer} {operation} {error}"),
        },
        MediaUpdate::Seeked(time) => format!("seeked {time}"),
        MediaUpdate::Seeking => "seeking".into(),
        MediaUpdate::SeekNeedsData => "seeking-needs-data".into(),
        MediaUpdate::Ready => "ready".into(),
        MediaUpdate::Waiting => "waiting".into(),
        MediaUpdate::Playing => "playing".into(),
        MediaUpdate::Paused => "paused".into(),
        MediaUpdate::Error(error) => format!("error {error}"),
        _ => return,
    };
    if TRACES.fetch_add(1, Ordering::Relaxed) < 160 {
        eprintln!("SATIN_QA_MEDIA node={} generation={} {message}", key.node.0, key.generation);
    }
}

#[derive(serde::Serialize)]
pub struct MediaStatus {
    pub node: usize,
    pub current_time: f64,
    pub ready_state: u8,
    pub video_width: u32,
    pub video_height: u32,
    pub has_audio: bool,
    pub decoded_frames: u64,
    pub paused: bool,
    pub ended: bool,
}

struct Track {
    key: MediaKey,
    url: String,
    paused: bool,
    ended: bool,
    seeking: bool,
    opened: bool,
    network: u8,
    ready: u8,
    loaded_data: bool,
    time: f64,
    played: Vec<(f64, f64)>,
    duration: f64,
    width: u32,
    height: u32,
    muted: bool,
    volume: f64,
    playback_rate: f64,
    default_playback_rate: f64,
    rate_revision: u64,
    last_event_time: f64,
    has_audio: bool,
    decoded_frames: u64,
    last_frame_sequence: u64,
    decoder_drops: u32,
    corrupted_frames: u32,
    error: Option<String>,
    plays: Vec<(RootId, RootId)>,
}

impl Track {
    fn new(node: NodeId, muted: bool) -> Self {
        Self {
            key: MediaKey {
                node,
                generation: 0,
            },
            url: String::new(),
            paused: true,
            ended: false,
            seeking: false,
            opened: false,
            network: 0,
            ready: 0,
            loaded_data: false,
            time: 0.,
            played: Vec::new(),
            duration: f64::NAN,
            width: 0,
            height: 0,
            muted,
            volume: 1.,
            playback_rate: 1.,
            default_playback_rate: 1.,
            rate_revision: 0,
            last_event_time: 0.,
            has_audio: false,
            decoded_frames: 0,
            last_frame_sequence: 0,
            decoder_drops: 0,
            corrupted_frames: 0,
            error: None,
            plays: Vec::new(),
        }
    }

    fn advance_time(&mut self, time: f64) {
        if !time.is_finite() {
            return;
        }
        if !self.paused && !self.seeking && time > self.time {
            let first = self.played.partition_point(|&(_, end)| end < self.time);
            let after = self.played.partition_point(|&(start, _)| start <= time);
            if first != after || self.played.len() < MAX_PLAYED_RANGES {
                let (mut start, mut end) = (self.time, time);
                if first < after {
                    start = start.min(self.played[first].0);
                    end = end.max(self.played[after - 1].1);
                }
                self.played.splice(first..after, [(start, end)]);
            }
        }
        self.time = time;
    }
}

#[derive(Default)]
pub(super) struct Store {
    tracks: HashMap<NodeId, Track>,
    activate: bool,
    exception: Value,
    loads: VecDeque<(MediaKey, ResourceRequest)>,
    commands: VecDeque<MediaCommand>,
    mse: mse::Store,
}

impl Store {
    pub(super) fn activate(&mut self) {
        self.activate = true;
    }
    pub(super) fn has_loads(&self) -> bool {
        !self.loads.is_empty()
    }
    pub(super) fn take_load(&mut self) -> Option<(MediaKey, ResourceRequest)> {
        self.loads.pop_front()
    }
    pub(super) fn take_commands(&mut self) -> Vec<MediaCommand> {
        self.commands.drain(..).collect()
    }
    pub(super) fn has_commands(&self) -> bool {
        !self.commands.is_empty()
    }
    pub(super) fn has_stream_tasks(&self) -> bool {
        self.mse.has_tasks()
    }

    pub(super) fn resolve_blob(&self, vm: &Vm, url: &url::Url) -> Option<std::rc::Rc<blobs::Blob>> {
        blobs::get(vm, &self.mse.resolve_url(vm, url)?)
    }

    fn check_command_room(&self, count: usize) -> Result<(), JsError> {
        if self.commands.len() + count > MAX_COMMANDS {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Media command limit exceeded",
            ));
        }
        Ok(())
    }

    fn enqueue(&mut self, command: MediaCommand) -> Result<(), JsError> {
        self.check_command_room(1)?;
        self.commands.push_back(command);
        Ok(())
    }
}

pub(super) struct Prototypes {
    media: Value,
    video: Value,
    audio: Value,
}

impl Prototypes {
    pub(super) fn new(vm: &mut Vm, parent: &Value) -> Result<Self, JsError> {
        let media = vm.new_object_with_prototype(parent.clone())?;
        let video = vm.new_object_with_prototype(media.clone())?;
        let audio = vm.new_object_with_prototype(media.clone())?;
        for proto in [&media, &video, &audio] {
            vm.root(proto.clone());
        }
        Ok(Self {
            media,
            video,
            audio,
        })
    }

    pub(super) fn element(&self, document: &Document, id: NodeId) -> Option<Value> {
        if document.namespace(id) != Namespace::Html {
            return None;
        }
        match document.tag(id) {
            "video" => Some(self.video.clone()),
            "audio" => Some(self.audio.clone()),
            _ => None,
        }
    }
}

pub(super) fn is_media(document: &Document, id: NodeId) -> bool {
    document.namespace(id) == Namespace::Html && matches!(document.tag(id), "audio" | "video")
}

pub(super) fn has_controls(document: &Document, id: NodeId) -> bool {
    is_media(document, id) && document.attr(id, "controls").is_some()
}

fn receiver(state: &Shared, this: &Value) -> Result<NodeId, JsError> {
    let id = node(state, this)?;
    if is_media(&state.borrow().document, id) {
        Ok(id)
    } else {
        Err(JsError::type_error("Expected an HTMLMediaElement receiver"))
    }
}

fn ensure_track(state: &mut super::State, id: NodeId) -> Result<&mut Track, JsError> {
    if state.media.tracks.len() >= MAX_ELEMENTS && !state.media.tracks.contains_key(&id) {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Media element limit exceeded",
        ));
    }
    let muted = state.document.attr(id, "muted").is_some();
    Ok(state
        .media
        .tracks
        .entry(id)
        .or_insert_with(|| Track::new(id, muted)))
}

fn source(state: &super::State, id: NodeId) -> Option<url::Url> {
    let raw = state
        .document
        .attr(id, "src")
        .or_else(|| {
            state.document.nodes[id.0]
                .children
                .iter()
                .find_map(|&child| {
                    let document = &state.document;
                    if document.namespace(child) != Namespace::Html
                        || document.tag(child) != "source"
                    {
                        return None;
                    }
                    if let Some(kind) = document.attr(child, "type")
                        && can_play_type(kind).is_empty()
                    {
                        return None;
                    }
                    document.attr(child, "src")
                })
        })?
        .trim();
    if raw.is_empty() {
        return None;
    }
    let url = document_base(state).join(raw).ok()?;
    matches!(url.scheme(), "https" | "http" | "file" | "data" | "blob").then_some(url)
}

fn can_play_type(input: &str) -> &'static str {
    if !cfg!(windows) {
        return "";
    }
    let Some(mime) = MimeType::parse(input) else {
        return "";
    };
    if !matches!(mime.essence.as_str(), "video/mp4" | "audio/mp4") {
        return "";
    }
    let Some(codecs) = mime.parameter("codecs") else {
        return "maybe";
    };
    if codecs.split(',').map(str::trim).all(|codec| {
        codec == "mp4a.40.2"
            || codec == "avc1"
            || codec.starts_with("avc1.")
            || codec == "avc3"
            || codec.starts_with("avc3.")
    }) {
        "probably"
    } else {
        ""
    }
}

fn get_exception(vm: &mut Vm, state: &Shared, name: &str, message: &str) -> Result<Value, JsError> {
    let class = state.borrow().media.exception.clone();
    abort::exception(vm, class, name, message)
}

fn settle(
    vm: &mut Vm,
    promises: Vec<(RootId, RootId)>,
    error: Option<Value>,
) -> Result<(), JsError> {
    let mut result = Ok(());
    for (resolve, reject) in promises {
        let function = vm
            .root_value(if error.is_some() { reject } else { resolve })
            .unwrap_or_default();
        if result.is_ok() {
            result = vm
                .call(
                    function,
                    Value::Undefined,
                    &[error.clone().unwrap_or_default()],
                )
                .map(|_| ());
        }
        vm.unroot(resolve);
        vm.unroot(reject);
    }
    result
}

fn event(vm: &mut Vm, state: &Shared, id: NodeId, name: &str) -> Result<(), JsError> {
    dispatch_checked(vm, state, Some(id), name, false, false, &[]).map(|_| ())
}

fn load(vm: &mut Vm, state: &Shared, id: NodeId) -> Result<bool, JsError> {
    let mut state = state.borrow_mut();
    let Some(url) = source(&state, id) else {
        return Ok(false);
    };
    if url.scheme() == "blob" {
        return attach_object_url(vm, &mut state, id, url);
    }
    let client = Arc::new(state.url.clone());
    let cors = state.document.attr(id, "crossorigin").map(|value| {
        if value.eq_ignore_ascii_case("use-credentials") {
            CredentialsMode::Include
        } else {
            CredentialsMode::SameOrigin
        }
    });
    let track = ensure_track(&mut state, id)?;
    if track.network != 0 {
        return Ok(track.error.is_none());
    }
    vm.charge_work(url.as_str().len())?;
    track.key.generation = track.key.generation.wrapping_add(1);
    track.url = url.as_str().into();
    track.network = 2;
    let key = track.key;
    state.media.loads.push_back((
        key,
        ResourceRequest {
            url,
            client,
            kind: ResourceKind::Media,
            script_encoding: None,
            cors,
            integrity: None,
            deadline: Instant::now() + std::time::Duration::from_secs(18),
        },
    ));
    Ok(true)
}

fn attach_object_url(
    vm: &mut Vm,
    state: &mut super::State,
    id: NodeId,
    url: url::Url,
) -> Result<bool, JsError> {
    let track = ensure_track(state, id)?;
    if track.network != 0 {
        return Ok(track.error.is_none());
    }
    let Some(object) = state.media.mse.resolve_url(vm, &url) else {
        return Ok(false);
    };
    let track = state.media.tracks.get(&id).expect("track already exists");
    let key = MediaKey {
        generation: track.key.generation.wrapping_add(1),
        node: id,
    };
    let rate = track.playback_rate;
    let revision = track.rate_revision;
    state
        .media
        .check_command_room(3 + usize::from(rate != 1.))?;
    let command = if let Some(blob) = vm.host_data::<blobs::Blob>(&object) {
        vm.charge_work(blob.bytes().len().div_ceil(64))?;
        MediaCommand::Open {
            key,
            url: url.as_str().into(),
            bytes: Arc::from(blob.bytes()),
        }
    } else if state.media.mse.attach(vm, object, key) {
        MediaCommand::OpenStream { key }
    } else {
        return Ok(false);
    };
    let track = state
        .media
        .tracks
        .get_mut(&id)
        .expect("track already exists");
    track.key = key;
    track.url = url.into();
    track.network = 2;
    track.opened = true;
    let volume = track.volume;
    let muted = track.muted;
    state.media.commands.extend([
        command,
        MediaCommand::Volume { key, value: volume },
        MediaCommand::Muted { key, muted },
    ]);
    if rate != 1. {
        state.media.commands.push_back(MediaCommand::PlaybackRate {
            key,
            revision,
            value: rate,
        });
    }
    Ok(true)
}

pub(super) fn source_changed(vm: &mut Vm, state: &Shared, id: NodeId) -> Result<(), JsError> {
    reset(vm, state, id)?;
    let object_url = source(&state.borrow(), id).is_some_and(|url| url.scheme() == "blob");
    if object_url {
        load(vm, state, id)?;
    }
    Ok(())
}

pub(super) fn reset(vm: &mut Vm, state: &Shared, id: NodeId) -> Result<(), JsError> {
    let promises = {
        let mut state = state.borrow_mut();
        let Some(track) = state.media.tracks.get(&id) else {
            return Ok(());
        };
        let old_key = track.key;
        let opened = track.opened;
        let next_generation = track.key.generation.wrapping_add(1);
        let mut fresh = Track::new(id, track.muted);
        fresh.volume = track.volume;
        fresh.playback_rate = track.default_playback_rate;
        fresh.default_playback_rate = track.default_playback_rate;
        let rate_changed = fresh.playback_rate != track.playback_rate;
        state.media.check_command_room(usize::from(opened))?;
        state.media.mse.reset_rate_changes(id, rate_changed)?;
        fresh.key.generation = next_generation;
        if opened {
            state.media.enqueue(MediaCommand::Close(old_key))?;
        }
        let prior = state
            .media
            .tracks
            .insert(id, fresh)
            .expect("track already exists");
        state.media.loads.retain(|(key, _)| *key != old_key);
        state.media.mse.detach(vm, old_key)?;
        state.canvases.frames.update(std::iter::once((id, None)));
        mark_dirty(&mut state);
        prior.plays
    };
    if !promises.is_empty() {
        let error = get_exception(vm, state, "AbortError", "Media source changed")?;
        settle(vm, promises, Some(error))?;
    }
    Ok(())
}

fn play(vm: &mut Vm, state: &Shared, id: NodeId) -> Result<Value, JsError> {
    let capability = vm.new_promise_capability()?;
    let active = {
        let state = state.borrow();
        state.media.activate
            || state.media.tracks.get(&id).map_or_else(
                || state.document.attr(id, "muted").is_some(),
                |track| track.muted,
            )
    };
    if !active || !load(vm, state, id)? {
        let (name, message) = if active {
            (
                "NotSupportedError",
                "No supported media source is available",
            )
        } else {
            (
                "NotAllowedError",
                "Audible playback requires a page interaction",
            )
        };
        let reason = get_exception(vm, state, name, message)?;
        vm.call(capability.reject, Value::Undefined, &[reason])?;
        return Ok(capability.promise);
    }
    let (was_paused, ready) = {
        let mut state = state.borrow_mut();
        let track = ensure_track(&mut state, id)?;
        if track.plays.len() >= MAX_PENDING_PLAYS {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Pending media play limit exceeded",
            ));
        }
        let was_paused = track.paused;
        let ready = !track.paused && track.ready >= 3 && track.plays.is_empty();
        let opened = track.opened;
        let key = track.key;
        if opened && was_paused {
            state.media.enqueue(MediaCommand::Play(key))?;
        }
        let track = state
            .media
            .tracks
            .get_mut(&id)
            .expect("track already exists");
        if !ready {
            track.plays.push((
                vm.root(capability.resolve.clone()),
                vm.root(capability.reject),
            ));
        }
        track.paused = false;
        track.ended = false;
        (was_paused, ready)
    };
    if was_paused {
        event(vm, state, id, "play")?;
    }
    if ready {
        vm.call(capability.resolve, Value::Undefined, &[Value::Undefined])?;
    }
    Ok(capability.promise)
}

fn pause(vm: &mut Vm, state: &Shared, id: NodeId) -> Result<(), JsError> {
    let (paused, promises) = {
        let mut state = state.borrow_mut();
        let track = ensure_track(&mut state, id)?;
        let prior = track.paused;
        if track.opened && !prior {
            let key = track.key;
            state.media.enqueue(MediaCommand::Pause(key))?;
        }
        let track = state
            .media
            .tracks
            .get_mut(&id)
            .expect("track already exists");
        track.paused = true;
        let promises = std::mem::take(&mut track.plays);
        (prior, promises)
    };
    if !promises.is_empty() {
        let error = get_exception(vm, state, "AbortError", "Playback was paused")?;
        settle(vm, promises, Some(error))?;
    }
    if !paused {
        event(vm, state, id, "pause")?;
    }
    Ok(())
}

fn set_rate(vm: &mut Vm, host: &Shared, id: NodeId, name: &str, value: f64) -> Result<(), JsError> {
    // The native media path supports forward playback. Reject rates outside the
    // supported range before changing observable state or sending a command.
    if !(0.25..=4.).contains(&value) {
        let class = host.borrow().media.exception.clone();
        return Err(abort::dom_exception_error(
            vm,
            class,
            "NotSupportedError",
            "Unsupported playback rate",
        )?);
    }
    let mut state = host.borrow_mut();
    let media_default = name == "defaultPlaybackRate";
    let track = ensure_track(&mut state, id)?;
    let prior = if media_default {
        track.default_playback_rate
    } else {
        track.playback_rate
    };
    if prior == value {
        return Ok(());
    }
    let key = track.key;
    let send = track.opened && !media_default;
    state.media.check_command_room(usize::from(send))?;
    state.media.mse.queue_rate_change(id)?;
    let track = state
        .media
        .tracks
        .get_mut(&id)
        .expect("track already exists");
    if media_default {
        track.default_playback_rate = value;
    } else {
        track.playback_rate = value;
        track.rate_revision = track.rate_revision.wrapping_add(1);
        let revision = track.rate_revision;
        if send {
            state.media.commands.push_back(MediaCommand::PlaybackRate {
                key,
                revision,
                value,
            });
        }
    }
    Ok(())
}

fn set_number(
    vm: &mut Vm,
    host: &Shared,
    id: NodeId,
    name: &str,
    value: Value,
) -> Result<(), JsError> {
    let value = vm.to_number(&value)?;
    if !value.is_finite() {
        return Err(JsError::type_error("Expected a finite media value"));
    }
    if matches!(name, "playbackRate" | "defaultPlaybackRate") {
        return set_rate(vm, host, id, name, value);
    }
    if name == "volume" && !(0. ..=1.).contains(&value) {
        let class = host.borrow().media.exception.clone();
        return Err(abort::dom_exception_error(
            vm,
            class,
            "IndexSizeError",
            "Volume must be between 0 and 1",
        )?);
    }
    let mut state = host.borrow_mut();
    let track = ensure_track(&mut state, id)?;
    if name == "volume" && track.volume == value {
        return Ok(());
    }
    let key = track.key;
    let opened = track.opened;
    let command = if name == "volume" {
        MediaCommand::Volume { key, value }
    } else {
        MediaCommand::Seek {
            key,
            time: value.max(0.),
        }
    };
    if opened {
        state.media.enqueue(command)?;
    }
    let track = state
        .media
        .tracks
        .get_mut(&id)
        .expect("track already exists");
    if name == "volume" {
        track.volume = value;
        drop(state);
        event(vm, host, id, "volumechange")
    } else {
        track.time = value.max(0.);
        track.seeking = track.opened;
        Ok(())
    }
}

fn property(state: &super::State, id: NodeId, name: &str) -> Value {
    let track = state.media.tracks.get(&id);
    match name {
        "currentSrc" => track.map_or("", |track| track.url.as_str()).into(),
        "currentTime" => track.map_or(0., |track| track.time).into(),
        "duration" => track.map_or(f64::NAN, |track| track.duration).into(),
        "networkState" => u32::from(track.map_or(0, |track| track.network)).into(),
        "readyState" => u32::from(track.map_or(0, |track| track.ready)).into(),
        "paused" => track.is_none_or(|track| track.paused).into(),
        "ended" => track.is_some_and(|track| track.ended).into(),
        "seeking" => track.is_some_and(|track| track.seeking).into(),
        "videoWidth" => track.map_or(0, |track| track.width).into(),
        "videoHeight" => track.map_or(0, |track| track.height).into(),
        "volume" => track.map_or(1., |track| track.volume).into(),
        "playbackRate" => track.map_or(1., |track| track.playback_rate).into(),
        "defaultPlaybackRate" => track.map_or(1., |track| track.default_playback_rate).into(),
        "muted" => track
            .map_or_else(
                || state.document.attr(id, "muted").is_some(),
                |track| track.muted,
            )
            .into(),
        _ => unreachable!("registered media property"),
    }
}

pub(super) fn install(vm: &mut Vm, state: &Shared) -> Result<(), JsError> {
    let protos = state.borrow();
    let media = protos.media_prototypes.media.clone();
    let video = protos.media_prototypes.video.clone();
    let audio = protos.media_prototypes.audio.clone();
    drop(protos);
    let exception = vm.get_global("DOMException")?;
    vm.root(exception.clone());
    state.borrow_mut().media.exception = exception;
    for (proto, name) in [
        (media.clone(), "HTMLMediaElement"),
        (video.clone(), "HTMLVideoElement"),
        (audio, "HTMLAudioElement"),
    ] {
        webidl::interface(vm, proto, name, 0, |_, _, _, _| {
            Err(JsError::type_error("Illegal media constructor"))
        })?;
    }
    quality::install(vm, state, &video)?;
    for name in [
        "currentSrc",
        "duration",
        "networkState",
        "readyState",
        "paused",
        "ended",
        "seeking",
        "videoWidth",
        "videoHeight",
    ] {
        let owner = if name.starts_with("video") {
            video.clone()
        } else {
            media.clone()
        };
        let ctx = state.clone();
        getter(vm, owner, name, move |_, this, _| {
            let id = receiver(&ctx, &this)?;
            Ok(property(&ctx.borrow(), id, name))
        })?;
    }
    for name in [
        "volume",
        "muted",
        "currentTime",
        "playbackRate",
        "defaultPlaybackRate",
    ] {
        let ctx = state.clone();
        let get = vm.new_native_function(name, move |_, this, _| {
            let id = receiver(&ctx, &this)?;
            Ok(property(&ctx.borrow(), id, name))
        });
        let ctx = state.clone();
        let set = vm.new_native_function(name, move |vm, this, args| {
            let id = receiver(&ctx, &this)?;
            let value = arg(args, 0);
            if name == "muted" {
                let mut state = ctx.borrow_mut();
                let track = ensure_track(&mut state, id)?;
                let muted = value.is_truthy();
                if track.muted == muted {
                    return Ok(Value::Undefined);
                }
                if track.opened {
                    let command = MediaCommand::Muted {
                        key: track.key,
                        muted,
                    };
                    state.media.enqueue(command)?;
                }
                state
                    .media
                    .tracks
                    .get_mut(&id)
                    .expect("track already exists")
                    .muted = muted;
                drop(state);
                event(vm, &ctx, id, "volumechange")?;
            } else {
                set_number(vm, &ctx, id, name, value)?;
            }
            Ok(Value::Undefined)
        });
        vm.define_accessor(media.clone(), name, Some(get), Some(set))?;
    }
    let ctx = state.clone();
    let get = vm.new_native_function("src", move |_, this, _| {
        let id = receiver(&ctx, &this)?;
        let state = ctx.borrow();
        let Some(raw) = state.document.attr(id, "src") else {
            return Ok("".into());
        };
        Ok(document_base(&state)
            .join(raw)
            .map_or_else(|_| raw.to_owned(), |url| url.into())
            .into())
    });
    let ctx = state.clone();
    let set = vm.new_native_function("src", move |vm, this, args| {
        let id = receiver(&ctx, &this)?;
        let src = webidl::usv_string(vm, &arg(args, 0), 256 * 1024)?;
        set_attribute(vm, &ctx, id, "src", Some(src))?;
        Ok(Value::Undefined)
    });
    vm.define_accessor(media.clone(), "src", Some(get), Some(set))?;
    for (name, attribute) in [
        ("muted", "muted"),
        ("controls", "controls"),
        ("autoplay", "autoplay"),
        ("playsInline", "playsinline"),
    ] {
        let name = if name == "muted" {
            "defaultMuted"
        } else {
            name
        };
        let ctx = state.clone();
        let get = vm.new_native_function(name, move |_, this, _| {
            let id = receiver(&ctx, &this)?;
            Ok(ctx.borrow().document.attr(id, attribute).is_some().into())
        });
        let ctx = state.clone();
        let set = vm.new_native_function(name, move |vm, this, args| {
            let id = receiver(&ctx, &this)?;
            set_attribute(
                vm,
                &ctx,
                id,
                attribute,
                arg(args, 0).is_truthy().then(String::new),
            )?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(media.clone(), name, Some(get), Some(set))?;
    }
    let ctx = state.clone();
    webidl::operation(
        vm,
        media.clone(),
        "canPlayType",
        1,
        move |vm, this, args| {
            receiver(&ctx, &this)?;
            webidl::required(args, 1, "HTMLMediaElement.canPlayType")?;
            let input = webidl::usv_string(vm, &arg(args, 0), 4096)?;
            Ok(can_play_type(&input).into())
        },
    )?;
    let ctx = state.clone();
    webidl::operation(vm, media.clone(), "play", 0, move |vm, this, _| {
        play(vm, &ctx, receiver(&ctx, &this)?)
    })?;
    let ctx = state.clone();
    webidl::operation(vm, media.clone(), "pause", 0, move |vm, this, _| {
        pause(vm, &ctx, receiver(&ctx, &this)?)?;
        Ok(Value::Undefined)
    })?;
    let ctx = state.clone();
    webidl::operation(vm, media.clone(), "load", 0, move |vm, this, _| {
        let id = receiver(&ctx, &this)?;
        reset(vm, &ctx, id)?;
        if load(vm, &ctx, id)? {
            event(vm, &ctx, id, "loadstart")?;
        }
        Ok(Value::Undefined)
    })?;
    super::install_event_handler_list(
        vm,
        state,
        media,
        &[
            "onloadstart",
            "onloadedmetadata",
            "onloadeddata",
            "oncanplay",
            "onplay",
            "onplaying",
            "onpause",
            "ontimeupdate",
            "onended",
            "onwaiting",
            "onseeking",
            "onseeked",
            "ondurationchange",
            "onvolumechange",
            "onratechange",
        ],
    )
}

pub(super) fn install_streaming(
    vm: &mut Vm,
    state: &Shared,
    events: super::event_target::Events,
) -> Result<(), JsError> {
    mse::install(vm, state, events)?;
    let prototype = state.borrow().media_prototypes.media.clone();
    let host = state.clone();
    getter(vm, prototype.clone(), "played", move |vm, this, _| {
        let id = receiver(&host, &this)?;
        let realm = host.borrow();
        let ranges = realm.media.tracks.get(&id).map(|track| track.played.clone()).unwrap_or_default();
        mse::make_ranges(vm, &realm.media.mse, ranges)
    })?;
    for (name, seekable) in [("buffered", false), ("seekable", true)] {
        let host = state.clone();
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            let id = receiver(&host, &this)?;
            let (key, duration) = {
                let realm = host.borrow();
                realm
                    .media
                    .tracks
                    .get(&id)
                    .map_or((None, f64::NAN), |track| {
                        let duration = if seekable || track.ready >= 2 {
                            track.duration
                        } else {
                            f64::NAN
                        };
                        (Some(track.key), duration)
                    })
            };
            mse::element_ranges(vm, &host, key, seekable, duration)
        })?;
    }
    Ok(())
}

pub(super) fn run_stream_tasks(vm: &mut Vm, state: &Shared) {
    if !state.borrow().media.has_stream_tasks() {
        return;
    }
    for _ in 0..32 {
        let result = scheduler::run_script_task(vm, |vm| {
            if !mse::run_next(vm, state)? {
                return Ok(false);
            }
            drain_microtask_queue(vm, state)?;
            Ok(true)
        });
        match result {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => {
                diagnostic(state, format!("Media Source: {error}"));
                break;
            }
        }
    }
}

pub(super) fn default_click(vm: &mut Vm, state: &Shared, id: NodeId) -> Result<(), JsError> {
    let action = {
        let state = state.borrow();
        has_controls(&state.document, id)
            .then(|| state.media.tracks.get(&id).is_none_or(|track| track.paused))
    };
    match action {
        Some(true) => {
            play(vm, state, id)?;
        }
        Some(false) => pause(vm, state, id)?,
        None => {}
    }
    Ok(())
}

impl super::ScriptRuntime {
    pub fn media_status(&self) -> Vec<MediaStatus> {
        let mut statuses: Vec<_> = self
            .state
            .borrow()
            .media
            .tracks
            .values()
            .map(|track| MediaStatus {
                node: track.key.node.0,
                current_time: track.time,
                ready_state: track.ready,
                video_width: track.width,
                video_height: track.height,
                has_audio: track.has_audio,
                decoded_frames: track.decoded_frames,
                paused: track.paused,
                ended: track.ended,
            })
            .collect();
        statuses.sort_unstable_by_key(|status| status.node);
        statuses
    }

    pub fn take_media_commands(&mut self) -> Vec<MediaCommand> {
        self.state.borrow_mut().media.take_commands()
    }

    pub(super) fn accept_media_bytes(
        &mut self,
        key: MediaKey,
        result: Result<(String, Arc<[u8]>), String>,
    ) -> bool {
        let Ok((url, bytes)) = result else {
            return self.update_media(key, MediaUpdate::Error(result.unwrap_err()));
        };
        let mut state = self.state.borrow_mut();
        let Some(track) = state
            .media
            .tracks
            .get(&key.node)
            .filter(|track| track.key == key)
        else {
            return false;
        };
        let seek = track.time > 0.;
        let play = !track.paused;
        let mut commands = vec![
            MediaCommand::Open { key, url, bytes },
            MediaCommand::Volume {
                key,
                value: track.volume,
            },
            MediaCommand::Muted {
                key,
                muted: track.muted,
            },
        ];
        if track.playback_rate != 1. {
            commands.push(MediaCommand::PlaybackRate {
                key,
                value: track.playback_rate,
                revision: track.rate_revision,
            });
        }
        if seek {
            commands.push(MediaCommand::Seek {
                key,
                time: track.time,
            });
        }
        if play {
            commands.push(MediaCommand::Play(key));
        }
        if let Err(error) = state.media.check_command_room(commands.len()) {
            drop(state);
            self.update_media(key, MediaUpdate::Error(error.to_string()));
            return false;
        }
        let track = state
            .media
            .tracks
            .get_mut(&key.node)
            .expect("track already exists");
        track.network = 1;
        track.opened = true;
        state.media.commands.extend(commands);
        true
    }

    pub fn update_media(&mut self, key: MediaKey, update: MediaUpdate) -> bool {
        #[cfg(feature = "media-startup-diagnostics")]
        trace_media_update(key, &update);
        let update = match update {
            MediaUpdate::StreamOpened => {
                return self.queue_stream_update(|store| store.enqueue_opened(key));
            }
            MediaUpdate::BufferUpdated {
                buffer,
                operation,
                result,
            } => {
                return self.queue_stream_update(|store| {
                    store.enqueue_result(key, buffer, operation, result)
                });
            }
            update => update,
        };
        let id = key.node;
        let mut state = self.state.borrow_mut();
        if let MediaUpdate::Metadata { duration, .. } = &update {
            state.media.mse.metadata(&self.vm, key, *duration);
        }
        let Some(track) = state
            .media
            .tracks
            .get_mut(&id)
            .filter(|track| track.key == key)
        else {
            return false;
        };
        let mut events: &[&str] = &[];
        let mut plays = Vec::new();
        let mut rejected = None;
        let mut relayout = false;
        let mut frame_updated = false;
        match update {
            MediaUpdate::Metadata {
                width,
                height,
                duration,
                has_audio,
            } => {
                let first = track.ready == 0;
                let resized = (track.width, track.height) != (width, height);
                let duration_changed =
                    duration != track.duration && !(duration.is_nan() && track.duration.is_nan());
                track.width = width;
                track.height = height;
                track.has_audio = has_audio;
                track.duration = duration;
                track.ready = track.ready.max(1);
                events = match (first, resized, duration_changed) {
                    (true, _, _) => &["durationchange", "loadedmetadata"],
                    (false, true, true) => &["durationchange", "resize"],
                    (false, true, false) => &["resize"],
                    (false, false, true) => &["durationchange"],
                    (false, false, false) => &[],
                };
            }
            MediaUpdate::Ready => {
                events = match (track.loaded_data, track.ready) {
                    (false, _) => &["loadeddata", "canplay"],
                    (true, 0..=2) => &["canplay"],
                    _ => &[],
                };
                track.loaded_data = true;
                track.ready = 4;
            }
            MediaUpdate::Playing if !track.paused => {
                // Some native recoveries emit PLAYING without a fresh CANPLAY.
                events = if track.loaded_data && track.ready < 3 {
                    track.ready = 4;
                    &["canplay", "playing"]
                } else {
                    &["playing"]
                };
                plays = std::mem::take(&mut track.plays);
            }
            MediaUpdate::Playing | MediaUpdate::Paused => {}
            MediaUpdate::RateRejected { revision, actual } => {
                if track.rate_revision == revision
                    && actual.is_finite()
                    && actual != track.playback_rate
                {
                    track.playback_rate = actual;
                    events = &["ratechange"];
                }
            }
            MediaUpdate::Waiting => {
                if track.seeking {
                    track.ready = track.ready.min(1);
                    if !track.paused {
                        events = &["waiting"];
                    }
                } else if !track.paused {
                    // The current frame may still be displayed, but there isn't
                    // enough data to continue. Don't leave readyState at 4.
                    track.ready = track.ready.min(2);
                    events = &["waiting"];
                }
            }
            MediaUpdate::Seeking => {
                track.seeking = true;
                events = &["seeking"];
            }
            MediaUpdate::SeekNeedsData => {
                track.seeking = true;
                let interrupted = !track.paused && !track.ended && track.ready >= 3;
                track.ready = track.ready.min(1);
                events = if interrupted {
                    track.last_event_time = track.time;
                    &["seeking", "timeupdate", "waiting"]
                } else {
                    &["seeking"]
                };
            }
            MediaUpdate::Seeked(time) => {
                track.time = time;
                track.seeking = false;
                events = &["timeupdate", "seeked"];
            }
            MediaUpdate::Time(_) if track.seeking => {}
            MediaUpdate::Time(time) => {
                track.advance_time(time);
                if (time - track.last_event_time).abs() >= 0.25 {
                    track.last_event_time = time;
                    events = &["timeupdate"];
                }
            }
            MediaUpdate::Ended => {
                track.ended = true;
                track.paused = true;
                events = &["timeupdate", "pause", "ended"];
            }
            MediaUpdate::DecoderFrameStats { dropped, corrupted } => {
                track.decoder_drops = track.decoder_drops.max(dropped);
                track.corrupted_frames = track.corrupted_frames.max(corrupted);
            }
            MediaUpdate::Frame { pixels: frame, sequence } => {
                frame_updated = true;
                if sequence > track.last_frame_sequence {
                    track.decoded_frames = track.decoded_frames.saturating_add(1);
                    track.last_frame_sequence = sequence;
                }
                let frames = &state.canvases.frames;
                relayout = match frames.get(id) {
                    Some(Frame::Bitmap(prior)) => prior.dimensions() != frame.dimensions(),
                    _ => true,
                };
                frames.update(std::iter::once((id, Some(Frame::Bitmap(frame)))));
            }
            MediaUpdate::Error(error) => {
                track.error = Some(error.clone());
                track.network = 3;
                track.ready = 0;
                track.paused = true;
                plays = std::mem::take(&mut track.plays);
                rejected = Some(error);
                events = &["error"];
            }
            MediaUpdate::StreamOpened | MediaUpdate::BufferUpdated { .. } => {
                unreachable!("media source updates are queued separately")
            }
        }
        if relayout {
            mark_dirty(&mut state);
        }
        drop(state);
        if events.is_empty() && plays.is_empty() {
            return frame_updated;
        }
        let state = &self.state;
        if let Err(error) = scheduler::run_script_task(&mut self.vm, |vm| {
            if rejected.is_some() {
                state.borrow_mut().media.mse.detach(vm, key)?;
            }
            if !plays.is_empty() {
                let cause = rejected
                    .as_deref()
                    .map(|message| get_exception(vm, state, "NotSupportedError", message))
                    .transpose()?;
                settle(vm, plays, cause)?;
            }
            for name in events {
                event(vm, state, id, name)?;
            }
            drain_microtask_queue(vm, state)
        }) {
            diagnostic(state, format!("Media element: {error}"));
        }
        true
    }

    fn queue_stream_update(
        &mut self,
        enqueue: impl FnOnce(&mut mse::Store) -> Result<(), JsError>,
    ) -> bool {
        let result = enqueue(&mut self.state.borrow_mut().media.mse);
        if let Err(error) = result {
            diagnostic(&self.state, error.to_string());
        }
        true
    }
}
