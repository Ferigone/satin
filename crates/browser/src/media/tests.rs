use super::*;
use std::{
    collections::HashMap,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

const CLIP: &[u8] = include_bytes!("../../tests/fixtures/media/movie_5.mp4");
// Synthetic FFmpeg testsrc picture and sine audio, encoded independently as
// H.264 and AAC LC with empty_moov, default_base_moof and 1 s fragments.
const VIDEO: &[u8] = include_bytes!("../../../engine/data/pages/mse-video.mp4");
const AUDIO: &[u8] = include_bytes!("../../../engine/data/pages/mse-audio.mp4");
const KEY: Key = Key {
    tab: 1,
    navigation: 1,
    node: 1,
    generation: 1,
};
const DEADLINE: Duration = Duration::from_secs(8);

#[derive(Default)]
struct Playback {
    dimensions: (u32, u32),
    duration: f64,
    audio: bool,
    ready: bool,
    playing: bool,
    paused: bool,
    seeking: bool,
    seeked: bool,
    position: f64,
    source_open: bool,
    ended: bool,
    ranges: HashMap<(u64, u64), Vec<(f64, f64)>>,
    frames: usize,
    frame: Option<Arc<image::RgbaImage>>,
}

struct Harness {
    backend: Backend,
    notifications: Receiver<()>,
    playback: Playback,
}

impl Harness {
    fn new() -> Self {
        let (wake, notifications) = mpsc::channel();
        Self {
            backend: Backend::new(move || {
                let _ = wake.send(());
            }),
            notifications,
            playback: Playback::default(),
        }
    }

    fn command(&mut self, command: Command) {
        self.backend.submit(command).unwrap();
    }

    fn update(&mut self) {
        for event in self.backend.drain() {
            assert_eq!(event.key, KEY);
            match event.kind {
                EventKind::RateRejected { revision, actual } => {
                    panic!("native media rejected speed change {revision}, retained {actual}")
                }
                EventKind::StreamOpened => self.playback.source_open = true,
                EventKind::BufferUpdated {
                    buffer,
                    operation,
                    result,
                } => {
                    let ranges = result.unwrap_or_else(|error| {
                        panic!("buffer {buffer} operation {operation}: {error}")
                    });
                    self.playback.ranges.insert((buffer, operation), ranges);
                }
                EventKind::Metadata {
                    width,
                    height,
                    duration,
                    has_audio,
                } => {
                    self.playback.dimensions = (width, height);
                    self.playback.duration = duration;
                    self.playback.audio = has_audio;
                }
                EventKind::Ready => self.playback.ready = true,
                EventKind::Playing => self.playback.playing = true,
                EventKind::Paused => {
                    self.playback.playing = false;
                    self.playback.paused = true;
                }
                EventKind::Seeking => self.playback.seeking = true,
                EventKind::Seeked(position) => {
                    self.playback.position = position;
                    self.playback.seeked = true;
                }
                EventKind::Time(position) => self.playback.position = position,
                EventKind::Frame { pixels: frame, .. } => {
                    self.playback.frames += 1;
                    self.playback.frame = Some(frame);
                }
                EventKind::Ended => self.playback.ended = true,
                EventKind::Waiting | EventKind::DecoderFrameStats { .. } => {}
                EventKind::Error(error) => panic!("playback failed: {error}"),
            }
        }
    }

    fn until(&mut self, state: &str, done: impl Fn(&Playback) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            self.update();
            if done(&self.playback) {
                return;
            }
            self.notifications
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!(
                        "waiting for {state} at {:.3}s after {} frames: {error}",
                        self.playback.position, self.playback.frames
                    )
                });
        }
    }
}

fn fragments(file: &[u8]) -> Vec<&[u8]> {
    let (mut start, mut cursor) = (0, 0);
    let mut pieces = Vec::new();
    while cursor < file.len() {
        let size = u32::from_be_bytes(file[cursor..cursor + 4].try_into().unwrap()) as usize;
        let kind = &file[cursor + 4..cursor + 8];
        if kind == b"mfra" {
            break;
        }
        if kind == b"moof" {
            pieces.push(&file[start..cursor]);
            start = cursor;
        }
        assert!(size >= 8 && size <= file.len() - cursor);
        cursor += size;
    }
    if start < cursor {
        pieces.push(&file[start..cursor]);
    }
    pieces
}

fn append_pair(harness: &mut Harness, operation: u64, video: &[u8], audio: &[u8]) {
    for (buffer, bytes) in [(1, video), (2, audio)] {
        harness.command(Command::Append {
            key: KEY,
            buffer,
            operation,
            bytes: bytes.into(),
            timestamp_offset: 0.,
            append_window: (0., f64::INFINITY),
        });
    }
    harness.until("updated audio and video source buffers", |media| {
        media.ranges.contains_key(&(1, operation)) && media.ranges.contains_key(&(2, operation))
    });
}

#[test]
fn plays_incrementally_appended_audio_and_video_before_end_of_stream() {
    let video = fragments(VIDEO);
    let audio = fragments(AUDIO);
    assert_eq!((video.len(), audio.len()), (6, 6));
    let mut harness = Harness::new();
    harness.command(Command::OpenStream { key: KEY });
    harness.until("native MediaSource open", |media| media.source_open);
    for (buffer, mime) in [
        (1, "video/mp4; codecs=\"avc1.42C01E\""),
        (2, "audio/mp4; codecs=\"mp4a.40.2\""),
    ] {
        harness.command(Command::AddBuffer {
            key: KEY,
            buffer,
            mime: mime.into(),
        });
    }
    harness.command(Command::Muted {
        key: KEY,
        muted: true,
    });
    harness.command(Command::Play(KEY));
    append_pair(&mut harness, 1, video[0], audio[0]);
    harness.command(Command::SetDuration {
        key: KEY,
        duration: 4.8,
    });

    // Keep the final media fragment for each track outside the decoder until
    // frames are visible and the audio-backed playback clock has advanced.
    for segment in 1..5 {
        append_pair(
            &mut harness,
            segment as u64 + 1,
            video[segment],
            audio[segment],
        );
    }
    assert!(harness.playback.ranges[&(1, 5)][0].1 > 3.8);
    assert!(harness.playback.ranges[&(2, 5)][0].1 > 3.8);
    harness.until(
        "decoded streaming frames before final appends and EOS",
        |media| media.playing && media.audio && media.frames >= 2 && media.position >= 0.25,
    );
    let first = harness.playback.frame.clone().unwrap();
    assert_eq!(first.dimensions(), (160, 90));
    assert!(!harness.playback.ended);
    harness.command(Command::Pause(KEY));
    harness.until("stream playback pauses", |media| media.paused);
    append_pair(&mut harness, 6, video[5], audio[5]);
    assert!(harness.playback.ranges[&(1, 6)][0].1 > 4.75);
    assert!(harness.playback.ranges[&(2, 6)][0].1 > 4.75);
    harness.command(Command::EndStream {
        key: KEY,
        error: None,
    });

    harness.command(Command::Seek {
        key: KEY,
        time: 4.1,
    });
    harness.until("seek into the appended fragment while paused", |media| {
        media.seeked
            && media
                .frame
                .as_ref()
                .is_some_and(|current| current.as_raw() != first.as_raw())
    });
    assert!((harness.playback.position - 4.1).abs() < 0.15);
    harness.command(Command::Play(KEY));
    harness.until("open stream completes naturally", |media| media.ended);
    assert!((harness.playback.position - 4.8333332).abs() < 0.04);
}

#[test]
fn streaming_append_window_offset_and_removal_use_native_ranges() {
    let video = fragments(VIDEO);
    let mut harness = Harness::new();
    harness.command(Command::OpenStream { key: KEY });
    harness.until("native MediaSource open", |media| media.source_open);
    harness.command(Command::AddBuffer {
        key: KEY,
        buffer: 1,
        mime: "video/mp4; codecs=\"avc1.42C01E\"".into(),
    });
    let append = |operation, segment: &[u8], timestamp_offset, append_window| Command::Append {
        key: KEY,
        buffer: 1,
        operation,
        bytes: segment.into(),
        timestamp_offset,
        append_window,
    };
    for (operation, segment, timestamp_offset, append_window) in [
        (1, video[0], 0., (0., f64::INFINITY)),
        (2, video[1], 2., (2., 2.7)),
    ] {
        harness.command(append(operation, segment, timestamp_offset, append_window));
        harness.until("native append result", |media| {
            media.ranges.contains_key(&(1, operation))
        });
    }
    let updated = &harness.playback.ranges[&(1, 2)];
    assert_eq!(updated.len(), 1);
    assert!((updated[0].0 - 2.).abs() < 0.01, "{updated:?}");
    // The native range includes the duration of the last accepted frame.
    assert!(updated[0].1 > 2.45 && updated[0].1 < 2.75, "{updated:?}");

    harness.command(Command::RemoveRange {
        key: KEY,
        buffer: 1,
        operation: 3,
        start: 2.,
        end: 2.75,
    });
    harness.until("native remove result", |media| {
        media.ranges.contains_key(&(1, 3))
    });
    assert!(harness.playback.ranges[&(1, 3)].is_empty());

    harness.command(append(4, video[2], 0., (0., f64::INFINITY)));
    harness.command(Command::AbortBuffer {
        key: KEY,
        buffer: 1,
    });
    harness.command(append(5, video[1], 0., (0., f64::INFINITY)));
    harness.until("new append completes after abort", |media| {
        media.ranges.contains_key(&(1, 5))
    });
    assert!(harness.playback.ranges[&(1, 5)][0].0.abs() < 0.01);
}

#[test]
fn plays_mp4_from_bytes_pauses_and_seeks() {
    let mut harness = Harness::new();
    harness.command(Command::Open {
        key: KEY,
        bytes: CLIP.into(),
    });
    // The MP4 has an AAC track. Mute it while exercising the same native clock and controls.
    harness.command(Command::Muted {
        key: KEY,
        muted: true,
    });
    harness.command(Command::Volume {
        key: KEY,
        value: 0.5,
    });
    harness.command(Command::Play(KEY));
    harness.until("decoded frames and clock advancement", |media| {
        media.ready && media.playing && media.frames >= 2 && media.position >= 0.2
    });
    let media = &harness.playback;
    let frame = media.frame.as_deref().expect("decoded video frame");
    assert_eq!(frame.dimensions(), media.dimensions);
    assert!(
        frame
            .pixels()
            .any(|pixel| pixel.0[..3].iter().any(|&n| n > 32))
    );
    assert!(frame.pixels().all(|pixel| pixel.0[3] == 255));
    assert!(media.audio);
    assert!(media.duration > 3.0, "duration was {}", media.duration);

    harness.command(Command::Pause(KEY));
    harness.until("pause", |media| media.paused);
    let paused_at = harness.playback.position;
    thread::sleep(Duration::from_millis(350));
    harness.update();
    assert!((harness.playback.position - paused_at).abs() < 0.05);

    let target = harness.playback.duration * 0.65;
    let frames_before_seek = harness.playback.frames;
    harness.command(Command::Seek {
        key: KEY,
        time: target,
    });
    harness.until("seek and decoded frame", |media| {
        media.seeked && media.frames > frames_before_seek
    });
    assert!(harness.playback.seeking);
    let seeked_at = harness.playback.position;
    assert!(
        (seeked_at - target).abs() < 0.15,
        "seeked to {seeked_at}, expected {target}"
    );

    harness.command(Command::Play(KEY));
    harness.until("playback after seek", |media| {
        media.position > seeked_at + 0.12
    });
    harness.command(Command::ClosePage {
        tab: KEY.tab,
        navigation: KEY.navigation,
    });
}

#[test]
fn half_speed_changes_the_native_playback_clock() {
    let mut harness = Harness::new();
    harness.command(Command::Open {
        key: KEY,
        bytes: CLIP.into(),
    });
    harness.command(Command::Muted {
        key: KEY,
        muted: true,
    });
    harness.command(Command::PlaybackRate {
        key: KEY,
        revision: 1,
        value: 0.5,
    });
    harness.command(Command::Play(KEY));
    harness.until("half-speed playback starts", |media| {
        media.playing && media.position > 0.2
    });
    let origin = harness.playback.position;
    let start = Instant::now();
    harness.until("one second of media at half speed", |media| {
        media.position >= origin + 1.
    });
    let measured = (harness.playback.position - origin) / start.elapsed().as_secs_f64();
    assert!(
        (0.3..0.75).contains(&measured),
        "observed {measured:.2}× speed"
    );
}
