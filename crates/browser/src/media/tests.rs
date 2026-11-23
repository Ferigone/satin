use super::*;
use std::{
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

const CLIP: &[u8] = include_bytes!("../../tests/fixtures/media/movie_5.mp4");
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
                EventKind::Paused => self.playback.paused = true,
                EventKind::Seeking => self.playback.seeking = true,
                EventKind::Seeked(position) => {
                    self.playback.position = position;
                    self.playback.seeked = true;
                }
                EventKind::Time(position) => self.playback.position = position,
                EventKind::Frame(frame) => {
                    self.playback.frames += 1;
                    self.playback.frame = Some(frame);
                }
                EventKind::Ended | EventKind::Waiting => {}
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
                .unwrap_or_else(|error| panic!("waiting for {state}: {error}"));
        }
    }
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
