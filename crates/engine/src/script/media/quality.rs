use super::{JsError, Shared, Value, Vm, getter, receiver, webidl};
use std::rc::Rc;

#[derive(Default)]
struct Quality {
    creation_time: f64,
    total: u32,
    dropped: u32,
    corrupted: u32,
}

fn count(value: u64) -> u32 {
    value.try_into().unwrap_or(u32::MAX)
}

pub(super) fn install(vm: &mut Vm, state: &Shared, video: &Value) -> Result<(), JsError> {
    let prototype = vm.new_object();
    let root = vm.root(prototype.clone());
    webidl::interface(vm, prototype.clone(), "VideoPlaybackQuality", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal VideoPlaybackQuality constructor"))
    })?;
    for name in ["creationTime", "totalVideoFrames", "droppedVideoFrames", "corruptedVideoFrames"] {
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            let snapshot = vm.host_data::<Quality>(&this)
                .ok_or_else(|| JsError::type_error("Expected a VideoPlaybackQuality receiver"))?;
            Ok(match name {
                "creationTime" => snapshot.creation_time.into(),
                "totalVideoFrames" => snapshot.total.into(),
                "droppedVideoFrames" => snapshot.dropped.into(),
                "corruptedVideoFrames" => snapshot.corrupted.into(),
                _ => unreachable!(),
            })
        })?;
    }
    let state = state.clone();
    webidl::operation(vm, video.clone(), "getVideoPlaybackQuality", 0, move |vm, this, _| {
        let id = receiver(&state, &this)?;
        let state = state.borrow();
        if state.document.tag(id) != "video" {
            return Err(JsError::type_error("Expected an HTMLVideoElement receiver"));
        }
        let mut quality = Quality {
            creation_time: state.scheduler.performance_now(),
            ..Quality::default()
        };
        if let Some(track) = state.media.tracks.get(&id) {
            let drops = u64::from(track.decoder_drops);
            quality.total = count(track.last_frame_sequence.saturating_add(drops));
            quality.dropped = count(track.last_frame_sequence.saturating_sub(track.decoded_frames).saturating_add(drops));
            quality.corrupted = track.corrupted_frames;
        }
        let prototype = vm.root_value(root).expect("rooted playback quality prototype");
        let object = vm.new_object_with_prototype(prototype)?;
        vm.mark_platform_object(&object)?;
        vm.set_host_data(object.clone(), Rc::new(quality))?;
        Ok(object)
    })?;
    Ok(())
}
