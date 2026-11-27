//! GStreamer playback of progressive HTML media resources already fetched by Satin.
use super::{Command, EventKind, Key, Shared};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app as gst_app;
use gstreamer_video::{self as gst_video, prelude::*};
use std::{
    ops::Range,
    sync::{Arc, Mutex, OnceLock},
};

const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

pub(super) struct Player {
    pipeline: gst::Element,
    video: gst_app::AppSink,
    bus: gst::Bus,
    ready: bool,
    wants_playback: bool,
    playing: bool,
    ended: bool,
    target: gst::State,
    rate: f64,
    pending_rate: Option<(u64, f64)>,
    pending_seek: Option<f64>,
    seeking: Option<Seek>,
    metadata: Metadata,
    last_position: Option<gst::ClockTime>,
    last_frame: Option<gst::ClockTime>,
    sequence: u64,
}

enum Seek {
    Position(f64),
    Rate,
}

#[derive(Default, PartialEq)]
struct Metadata {
    width: u32,
    height: u32,
    duration: Option<gst::ClockTime>,
    has_audio: bool,
}

impl Player {
    pub(super) fn open(bytes: Arc<[u8]>) -> Result<Self, String> {
        if !super::valid_mp4(&bytes) {
            return Err("Invalid MP4 media header".into());
        }
        static INIT: OnceLock<Result<(), String>> = OnceLock::new();
        INIT.get_or_init(|| gst::init().map_err(|error| error.to_string()))
            .as_ref()
            .map_err(Clone::clone)?;

        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .build();
        let video = gst_app::AppSink::builder()
            .caps(&caps)
            .max_buffers(2)
            .leaky_type(gst_app::AppLeakyType::Downstream)
            .wait_on_eos(false)
            .sync(true)
            .build();
        let pipeline = gst::ElementFactory::make("playbin")
            .property("uri", "appsrc://")
            .property("video-sink", &video)
            .build()
            .map_err(|error| error.to_string())?;
        pipeline.connect("source-setup", false, move |args| {
            if let Ok(source) = args[1].get::<gst::Element>()
                && let Ok(source) = source.downcast::<gst_app::AppSrc>()
            {
                configure_source(&source, bytes.clone());
            }
            None
        });
        let bus = pipeline.bus().ok_or("GStreamer playback has no bus")?;
        let mut player = Self {
            pipeline,
            video,
            bus,
            ready: false,
            wants_playback: false,
            playing: false,
            ended: false,
            target: gst::State::Null,
            rate: 1.,
            pending_rate: None,
            pending_seek: None,
            seeking: None,
            metadata: Metadata::default(),
            last_position: None,
            last_frame: None,
            sequence: 0,
        };
        player.set_state(gst::State::Paused)?;
        Ok(player)
    }

    pub(super) fn control(&mut self, command: Command, shared: &Shared) -> Result<(), String> {
        match command {
            Command::Play(_) => {
                self.wants_playback = true;
                if self.ended {
                    self.pending_seek = Some(0.);
                }
            }
            Command::Pause(_) => self.wants_playback = false,
            Command::Seek { key, time } => {
                self.pending_seek = Some(time);
                shared.publish(key, EventKind::Seeking);
            }
            Command::PlaybackRate {
                key,
                revision,
                value,
            } => {
                if value.is_finite() && value > 0. {
                    self.pending_rate = Some((revision, value));
                } else {
                    shared.publish(
                        key,
                        EventKind::RateRejected {
                            revision,
                            actual: self.rate,
                        },
                    );
                }
            }
            Command::Volume { value, .. } => self.pipeline.set_property("volume", value),
            Command::Muted { muted, .. } => self.pipeline.set_property("mute", muted),
            _ => return Err("This command requires a MediaSource stream".into()),
        }
        Ok(())
    }

    pub(super) fn needs_poll(&self) -> bool {
        !self.ready
            || self.playing
            || self.wants_playback
            || self.pending_seek.is_some()
            || self.pending_rate.is_some()
            || self.seeking.is_some()
    }

    pub(super) fn poll(&mut self, shared: &Shared, key: Key) -> Result<(), String> {
        let mut prepared = false;
        let mut completed_seek = None;
        for message in std::iter::from_fn(|| self.bus.pop()).take(128) {
            match message.view() {
                gst::MessageView::Error(error) => return Err(error.error().to_string()),
                gst::MessageView::AsyncDone(_) if self.is_pipeline_message(&message) => {
                    if !self.ready {
                        self.ready = true;
                        prepared = true;
                    } else if let Some(Seek::Position(position)) = self.seeking.take()
                        && self.pending_seek.is_none()
                    {
                        completed_seek = Some(position);
                    }
                }
                gst::MessageView::Eos(_) => {
                    self.wants_playback = false;
                    self.playing = false;
                    self.ended = true;
                    shared.publish(key, EventKind::Ended);
                }
                _ => {}
            }
        }

        let sample = self
            .video
            .try_pull_sample(gst::ClockTime::ZERO)
            .or_else(|| self.video.try_pull_preroll(gst::ClockTime::ZERO));
        let frame = sample
            .as_ref()
            .map(|sample| self.frame(sample))
            .transpose()?
            .flatten();
        if self.ready {
            self.publish_metadata(shared, key, frame.as_ref(), prepared);
        }
        if prepared {
            shared.publish(key, EventKind::Ready);
        }
        if let Some(requested) = completed_seek {
            let time = self
                .position()
                .map_or(requested, gst::ClockTime::seconds_f64);
            shared.publish(key, EventKind::Seeked(time));
        }
        if let Some(pixels) = frame {
            self.sequence += 1;
            shared.publish(
                key,
                EventKind::Frame {
                    pixels: Arc::new(pixels),
                    sequence: self.sequence,
                },
            );
        }

        if self.ready && self.seeking.is_none() {
            self.start_pending_seek(shared, key)?;
        }
        if self.ready {
            self.set_state(if self.wants_playback {
                gst::State::Playing
            } else {
                gst::State::Paused
            })?;
        }
        let state = self.pipeline.current_state();
        if !self.playing && self.wants_playback && !self.ended && state == gst::State::Playing {
            self.playing = true;
            shared.publish(key, EventKind::Playing);
        } else if self.playing && !self.wants_playback && state == gst::State::Paused {
            self.playing = false;
            shared.publish(key, EventKind::Paused);
        }
        if self.ready
            && let Some(position) = self.position()
            && self.last_position != Some(position)
        {
            self.last_position = Some(position);
            shared.publish(key, EventKind::Time(position.seconds_f64()));
        }
        Ok(())
    }

    fn is_pipeline_message(&self, message: &gst::MessageRef) -> bool {
        message
            .src()
            .is_some_and(|source| source == self.pipeline.upcast_ref::<gst::Object>())
    }

    fn set_state(&mut self, state: gst::State) -> Result<(), String> {
        if state != self.target {
            self.pipeline
                .set_state(state)
                .map_err(|error| error.to_string())?;
            self.target = state;
        }
        Ok(())
    }

    fn position(&self) -> Option<gst::ClockTime> {
        self.pipeline.query_position::<gst::ClockTime>()
    }

    fn start_pending_seek(&mut self, shared: &Shared, key: Key) -> Result<(), String> {
        let seek = self.pending_seek.take();
        let rate_change = self.pending_rate.take();
        if seek.is_none() && rate_change.is_none() {
            return Ok(());
        }
        let start = seek
            .map(clock_time)
            .or_else(|| self.position())
            .unwrap_or(gst::ClockTime::ZERO);
        let rate = rate_change.map_or(self.rate, |(_, rate)| rate);
        let result = self.pipeline.seek(
            rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            start,
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
        if let Err(error) = result {
            if let Some((revision, _)) = rate_change {
                shared.publish(
                    key,
                    EventKind::RateRejected {
                        revision,
                        actual: self.rate,
                    },
                );
                self.pending_seek = seek;
                return Ok(());
            }
            return Err(error.to_string());
        }
        self.rate = rate;
        self.ended = false;
        self.last_frame = None;
        self.seeking = Some(seek.map_or(Seek::Rate, Seek::Position));
        Ok(())
    }

    fn publish_metadata(
        &mut self,
        shared: &Shared,
        key: Key,
        frame: Option<&image::RgbaImage>,
        prepared: bool,
    ) {
        let (width, height) = frame.map_or(
            (self.metadata.width, self.metadata.height),
            image::RgbaImage::dimensions,
        );
        let metadata = Metadata {
            width,
            height,
            duration: self
                .pipeline
                .query_duration::<gst::ClockTime>()
                .or(self.metadata.duration),
            has_audio: self.pipeline.property::<i32>("n-audio") > 0,
        };
        if prepared || metadata != self.metadata {
            shared.publish(
                key,
                EventKind::Metadata {
                    width,
                    height,
                    duration: metadata
                        .duration
                        .map_or(f64::NAN, gst::ClockTime::seconds_f64),
                    has_audio: metadata.has_audio,
                },
            );
            self.metadata = metadata;
        }
    }

    fn frame(&mut self, sample: &gst::Sample) -> Result<Option<image::RgbaImage>, String> {
        let buffer = sample
            .buffer()
            .ok_or("GStreamer video sample has no buffer")?;
        let frame_time = buffer.pts();
        if frame_time.is_some() && frame_time == self.last_frame {
            return Ok(None);
        }
        let caps = sample.caps().ok_or("GStreamer video sample has no caps")?;
        let info = gst_video::VideoInfo::from_caps(caps).map_err(|error| error.to_string())?;
        let width = info.width();
        let height = info.height();
        let row_size = (width as usize)
            .checked_mul(4)
            .ok_or("Invalid GStreamer video width")?;
        let bytes = row_size
            .checked_mul(height as usize)
            .filter(|size| (1..=MAX_FRAME_BYTES).contains(size))
            .ok_or("GStreamer video frame exceeds the supported image size")?;
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
            .map_err(|error| error.to_string())?;
        let stride = usize::try_from(frame.plane_stride()[0])
            .ok()
            .filter(|&stride| stride >= row_size)
            .ok_or("GStreamer returned an invalid video row stride")?;
        let source = frame.plane_data(0).map_err(|error| error.to_string())?;
        let mut rgba = Vec::with_capacity(bytes);
        for row in source.chunks(stride).take(height as usize) {
            let pixels = row
                .get(..row_size)
                .ok_or("Truncated GStreamer video frame")?;
            rgba.extend_from_slice(pixels);
        }
        let pixels = image::RgbaImage::from_raw(width, height, rgba)
            .ok_or("Invalid GStreamer video frame")?;
        self.last_frame = frame_time;
        Ok(Some(pixels))
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn clock_time(seconds: f64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(((seconds.max(0.) * 1e9).round() as u64).min(u64::MAX - 1))
}

struct Chunk {
    bytes: Arc<[u8]>,
    range: Range<usize>,
}

impl AsRef<[u8]> for Chunk {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[self.range.clone()]
    }
}

fn configure_source(source: &gst_app::AppSrc, bytes: Arc<[u8]>) {
    source.set_format(gst::Format::Bytes);
    source.set_caps(Some(&gst::Caps::builder("video/quicktime").build()));
    source.set_stream_type(gst_app::AppStreamType::RandomAccess);
    source.set_size(bytes.len() as i64);
    let length = bytes.len();
    let cursor = Arc::new(Mutex::new(0usize));
    let next = cursor.clone();
    source.set_callbacks(
        gst_app::AppSrcCallbacks::builder()
            .seek_data(move |_, offset| {
                let Ok(offset) = usize::try_from(offset) else {
                    return false;
                };
                if offset > length {
                    return false;
                }
                *cursor.lock().unwrap() = offset;
                true
            })
            .need_data(move |source, requested| {
                let range = {
                    let mut next = next.lock().unwrap();
                    let start = *next;
                    let count = if requested == u32::MAX {
                        64 * 1024
                    } else {
                        requested as usize
                    };
                    *next = start.saturating_add(count).min(length);
                    start..*next
                };
                if range.is_empty() {
                    let _ = source.end_of_stream();
                } else {
                    let mut buffer = gst::Buffer::from_slice(Chunk {
                        bytes: bytes.clone(),
                        range: range.clone(),
                    });
                    let metadata = buffer.get_mut().unwrap();
                    metadata.set_offset(range.start as u64);
                    metadata.set_offset_end(range.end as u64);
                    let _ = source.push_buffer(buffer);
                }
            })
            .build(),
    );
}
