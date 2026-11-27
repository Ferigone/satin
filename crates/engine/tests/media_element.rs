use rust_browser_engine::{
    LoadedPage,
    script::{MediaCommand, ScriptRuntime},
};

#[test]
fn played_reports_snapshots_of_playback_without_covering_seek_gaps() {
    use rust_browser_engine::script::MediaUpdate;

    let page = LoadedPage::from_html("https://example.test/", "<video id='clip' muted></video>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(r#"
        const clip = document.getElementById("clip");
        const before = clip.played;
        clip.src = URL.createObjectURL(new Blob(["sample"], {type:"video/mp4"}));
        clip.play();
        before instanceof TimeRanges && before.length === 0
    "#).unwrap().as_bool(), Some(true));
    let commands = runtime.take_media_commands();
    let [MediaCommand::Open { key, .. }, ..] = commands.as_slice() else { panic!("expected a media source"); };
    let key = *key;
    runtime.update_media(key, MediaUpdate::Playing);
    runtime.update_media(key, MediaUpdate::Time(1.));
    runtime.update_media(key, MediaUpdate::Time(2.));
    runtime.eval("const first = clip.played; clip.currentTime = 10;").unwrap();
    runtime.update_media(key, MediaUpdate::Time(9.));
    runtime.update_media(key, MediaUpdate::Seeked(10.));
    runtime.update_media(key, MediaUpdate::Time(11.));
    runtime.eval("clip.currentTime = 1").unwrap();
    runtime.update_media(key, MediaUpdate::Seeked(1.));
    runtime.update_media(key, MediaUpdate::Time(3.));
    runtime.eval("clip.pause()").unwrap();
    runtime.update_media(key, MediaUpdate::Time(15.));
    assert_eq!(runtime.eval(r#"
        const now = clip.played;
        before.length === 0 && first.length === 1 && first.end(0) === 2 &&
            now.length === 2 && now.start(0) === 0 && now.end(0) === 3 &&
            now.start(1) === 10 && now.end(1) === 11
    "#).unwrap().as_bool(), Some(true));
    assert_eq!(runtime.eval("clip.load(); clip.played.length").unwrap().as_number(), Some(0.));
}

#[test]
fn seeking_listener_observes_missing_target_data_even_when_paused() {
    use rust_browser_engine::script::MediaUpdate;
    use rust_browser_javascript::Value;

    let page = LoadedPage::from_html("https://example.test/", "<video id='clip'></video>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.eval(r#"
        const clip = document.getElementById('clip');
        let observed;
        clip.src = URL.createObjectURL(new Blob(['sample'], {type: 'video/mp4'}));
        clip.onseeking = () => { observed = [clip.currentTime, clip.readyState, clip.paused].join('/'); };
    "#).unwrap();
    let commands = runtime.take_media_commands();
    let [MediaCommand::Open { key, .. }, ..] = commands.as_slice() else {
        panic!("expected a media source");
    };
    runtime.update_media(*key, MediaUpdate::Ready);
    runtime.eval("clip.currentTime = 30").unwrap();
    runtime.update_media(*key, MediaUpdate::Waiting);
    runtime.update_media(*key, MediaUpdate::Seeking);
    assert_eq!(runtime.eval("observed").unwrap(), Value::from("30/1/true"));
}

#[test]
fn only_actual_volume_and_mute_changes_dispatch_volumechange() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        "<video id='clip' muted></video><button type='button' id='sound'>Adjust sound</button>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime
        .eval(
            r#"
        const clip = document.getElementById("clip");
        const events = [];
        clip.addEventListener("volumechange", event => {
          events.push(event.target === clip && !event.bubbles && !event.cancelable);
        });
        clip.volume = "1";
        clip.muted = true;
        clip.src = URL.createObjectURL(new Blob(["sample"], {type: "video/mp4"}));
        document.getElementById("sound").addEventListener("click", () => {
          clip.volume = clip.volume;
          clip.muted = clip.muted;
          clip.volume = {valueOf() { return 0.5; }};
          clip.volume = "0.5";
          clip.muted = 0;
          clip.muted = false;
        });
    "#,
        )
        .unwrap();
    let open = runtime.take_media_commands();
    let [MediaCommand::Open { key, .. }, ..] = open.as_slice() else {
        panic!("expected a progressive media open command");
    };
    assert_eq!(
        runtime.eval("events.length === 0").unwrap().as_bool(),
        Some(true)
    );

    assert!(runtime.click(page.document.elements("button").next().unwrap()));
    runtime.pump();
    assert_eq!(
        runtime
            .eval(r#"events.join(",") === "true,true" && clip.volume === 0.5 && !clip.muted"#)
            .unwrap()
            .as_bool(),
        Some(true)
    );
    assert!(matches!(
        runtime.take_media_commands().as_slice(),
        [
            MediaCommand::Volume {
                key: volume_key,
                value: 0.5
            },
            MediaCommand::Muted {
                key: muted_key,
                muted: false
            }
        ] if volume_key == key && muted_key == key
    ));
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
}

#[test]
fn playback_speed_drives_native_media_and_resets_to_the_default_on_load() {
    use rust_browser_engine::script::MediaUpdate;

    let page = LoadedPage::from_html("https://example.test/", "<video id='clip' muted></video>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let state = runtime.eval(r#"
        const clip = document.getElementById("clip");
        const events = [];
        clip.onratechange = event => events.push(event.target === clip && !event.bubbles);
        const initial = clip.playbackRate === 1 && clip.defaultPlaybackRate === 1;
        clip.defaultPlaybackRate = 1.5;
        const defaultOnly = clip.playbackRate === 1;
        clip.src = URL.createObjectURL(new Blob(["sample"], {type: "video/mp4"}));
        const reset = clip.playbackRate === 1.5;
        clip.playbackRate = "2";
        clip.playbackRate = 2;
        let rejected = false;
        try { clip.playbackRate = -1; } catch (error) { rejected = error.name === "NotSupportedError"; }
        let nonfinite = false;
        try { clip.playbackRate = NaN; } catch (error) { nonfinite = error instanceof TypeError; }
        initial && defaultOnly && reset && rejected && nonfinite && events.length === 0 &&
            clip.playbackRate === 2 && clip.defaultPlaybackRate === 1.5
    "#).unwrap();
    assert_eq!(state.as_bool(), Some(true));
    let rates: Vec<_> = runtime
        .take_media_commands()
        .into_iter()
        .filter_map(|command| {
            if let MediaCommand::PlaybackRate {
                key,
                revision,
                value,
            } = command
            {
                Some((key, revision, value))
            } else {
                None
            }
        })
        .collect();
    let [(key, default, 1.5), (_, current, 2.)] = rates.as_slice() else {
        panic!("expected default and active speed commands")
    };
    runtime.pump();
    assert_eq!(
        runtime
            .eval("events.join(',') === 'true,true'")
            .unwrap()
            .as_bool(),
        Some(true)
    );
    runtime.update_media(
        *key,
        MediaUpdate::RateRejected {
            revision: *default,
            actual: 1.,
        },
    );
    assert_eq!(
        runtime.eval("clip.playbackRate === 2").unwrap().as_bool(),
        Some(true)
    );
    runtime.update_media(
        *key,
        MediaUpdate::RateRejected {
            revision: *current,
            actual: 1.,
        },
    );
    assert_eq!(
        runtime
            .eval("clip.playbackRate === 1 && events.length === 3")
            .unwrap()
            .as_bool(),
        Some(true)
    );
    runtime.eval("clip.load()").unwrap();
    assert_eq!(
        runtime
            .eval("clip.playbackRate === 1.5 && clip.defaultPlaybackRate === 1.5")
            .unwrap()
            .as_bool(),
        Some(true)
    );
}

#[test]
fn video_quality_snapshots_distinguish_new_redrawn_and_dropped_frames() {
    use rust_browser_engine::script::MediaUpdate;
    use std::sync::Arc;

    let page = LoadedPage::from_html("https://example.test/", "<video id='v'></video><audio id='a'></audio>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let initial = runtime.eval(r#"
        const v = document.getElementById("v");
        const before = performance.now();
        const original = v.getVideoPlaybackQuality();
        const after = performance.now();
        function throwsType(f) { try { f(); } catch (e) { return e instanceof TypeError; } return false; }
        v.src = URL.createObjectURL(new Blob(["sample"], {type: "video/mp4"}));
        original instanceof VideoPlaybackQuality && original.totalVideoFrames === 0 &&
            original.creationTime >= before && original.creationTime <= after &&
            v.getVideoPlaybackQuality() !== original &&
            throwsType(() => new VideoPlaybackQuality()) &&
            throwsType(() => v.getVideoPlaybackQuality.call(document.getElementById("a"))) &&
            throwsType(() => Object.getOwnPropertyDescriptor(VideoPlaybackQuality.prototype, "totalVideoFrames").get.call({}))
    "#).unwrap();
    assert_eq!(initial.as_bool(), Some(true));
    let commands = runtime.take_media_commands();
    let [MediaCommand::Open { key, .. }, ..] = commands.as_slice() else { panic!("expected a media source"); };
    let key = *key;
    let pixels = Arc::new(image::RgbaImage::new(2, 2));
    for sequence in [1, 4, 4] {
        runtime.update_media(key, MediaUpdate::Frame { pixels: pixels.clone(), sequence });
    }
    runtime.update_media(key, MediaUpdate::DecoderFrameStats { dropped: 2, corrupted: 1 });
    let result = runtime.eval(r#"
        const measured = v.getVideoPlaybackQuality();
        measured.totalVideoFrames === 6 && measured.droppedVideoFrames === 4 &&
            measured.corruptedVideoFrames === 1 && original.totalVideoFrames === 0
    "#).unwrap();
    assert_eq!(result.as_bool(), Some(true));
    assert_eq!(runtime.media_status()[0].decoded_frames, 2);
    runtime.eval("v.load()").unwrap();
    runtime.update_media(key, MediaUpdate::Frame { pixels, sequence: 5 });
    assert_eq!(runtime.eval("v.getVideoPlaybackQuality().totalVideoFrames === 0 && measured.totalVideoFrames === 6").unwrap().as_bool(), Some(true));
}
