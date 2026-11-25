#![cfg(windows)]

use rust_browser_engine::{
    LoadedPage,
    script::{MediaCommand, MediaKey, MediaUpdate, ScriptRuntime},
};

fn attached_source() -> (ScriptRuntime, MediaKey) {
    let page = LoadedPage::from_html(
        "https://example.test/watch",
        "<video id='player' muted></video>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let result = runtime
        .eval(
            r#"
        const player = document.getElementById("player");
        const source = new MediaSource();
        const link = URL.createObjectURL(source);
        const events = [];
        source.onsourceopen = event => events.push(event.type);
        source.onsourceclose = event => events.push(event.type);
        player.src = link;
        URL.revokeObjectURL(link);
        source.readyState === "closed" && Number.isNaN(source.duration) &&
          link.indexOf("blob:https://example.test/") === 0 &&
          source instanceof EventTarget && source.sourceBuffers instanceof SourceBufferList &&
          source.sourceBuffers === source.sourceBuffers &&
          MediaSource.isTypeSupported('video/mp4; codecs="avc1.42C01E"') &&
          !MediaSource.isTypeSupported('text/html');
    "#,
        )
        .unwrap();
    assert_eq!(result.as_bool(), Some(true));
    let key = runtime
        .take_media_commands()
        .into_iter()
        .find_map(|command| {
            if let MediaCommand::OpenStream { key } = command {
                Some(key)
            } else {
                None
            }
        })
        .expect("setting src attaches the source");
    runtime.update_media(key, MediaUpdate::StreamOpened);
    runtime.pump();
    assert_eq!(
        runtime
            .eval(r#"source.readyState === "open" && events.join(",") === "sourceopen""#)
            .unwrap()
            .as_bool(),
        Some(true)
    );
    (runtime, key)
}

#[test]
fn add_source_buffer_uses_the_same_capabilities_as_the_player_probe() {
    let (mut runtime, _) = attached_source();
    let result = runtime
        .eval(
            r#"
        [
          'video/mp4; codecs="avc1.42C01E"',
          'video/mp4; codecs="av01.0.05M.08"',
          'video/mp2t; codecs="avc1.4d001f,mp4a.40.2"',
          'application/vnd.apple.mpegurl'
        ].every(mime => {
          const supported = MediaSource.isTypeSupported(mime);
          try {
            const buffer = source.addSourceBuffer(mime);
            source.removeSourceBuffer(buffer);
            return supported && buffer instanceof SourceBuffer;
          } catch (error) {
            return !supported && error.name === "NotSupportedError";
          }
        });
    "#,
        )
        .unwrap();
    assert_eq!(result.as_bool(), Some(true));
}

#[test]
fn media_source_appends_independent_tracks_and_exposes_buffered_intersection() {
    let (mut runtime, key) = attached_source();
    let result = runtime.eval(r#"
        const videoBuffer = source.addSourceBuffer('video/mp4; codecs="avc1.42C01E"');
        const audioBuffer = source.addSourceBuffer('audio/mp4; codecs="mp4a.40.2"');
        for (const buffer of source.sourceBuffers) {
          for (const name of ["updatestart", "update", "updateend"]) {
            buffer.addEventListener(name, event => events.push(event.type + ":" + buffer.updating));
          }
        }
        source.duration = 9;
        const input = new Uint8Array([11, 22, 33]);
        videoBuffer.timestampOffset = 1.5;
        videoBuffer.appendWindowStart = 2;
        videoBuffer.appendWindowEnd = 7;
        videoBuffer.appendBuffer(input.subarray(1));
        input[1] = 99;
        audioBuffer.appendBuffer(new Uint8Array([44]));
        let busy = false;
        try { source.endOfStream(); }
        catch (error) { busy = error.name === "InvalidStateError"; }
        source.sourceBuffers.length === 2 && source.sourceBuffers[0] === videoBuffer &&
          videoBuffer instanceof SourceBuffer && videoBuffer.updating && audioBuffer.updating && busy &&
          events.length === 1 && player.buffered.length === 0;
    "#).unwrap();
    assert_eq!(result.as_bool(), Some(true));
    let mut appended = Vec::new();
    let mut buffers = Vec::new();
    for command in runtime.take_media_commands() {
        match command {
            MediaCommand::AddBuffer { buffer, .. } => buffers.push(buffer),
            MediaCommand::Append {
                buffer,
                operation,
                bytes,
                timestamp_offset,
                append_window,
                ..
            } => {
                appended.push((buffer, operation, bytes, timestamp_offset, append_window));
            }
            _ => {}
        }
    }
    assert_eq!(appended.len(), 2);
    assert_eq!(&*appended[0].2, &[22, 33]);
    assert_eq!(
        (appended[0].0, appended[0].3, appended[0].4),
        (buffers[0], 1.5, (2., 7.))
    );
    for (index, (buffer, operation, ..)) in appended.into_iter().enumerate() {
        let ranges = if index == 0 {
            vec![(2., 6.), (7., 8.)]
        } else {
            vec![(3., 7.5)]
        };
        runtime.update_media(
            key,
            MediaUpdate::BufferUpdated {
                buffer,
                operation,
                result: Ok(ranges),
            },
        );
    }
    runtime.pump();
    let result = runtime.eval(r#"
        const ranges = player.buffered;
        let invalid = false;
        try { ranges.end(5); } catch (error) { invalid = error.name === "IndexSizeError"; }
        source.endOfStream();
        ranges instanceof TimeRanges && ranges.length === 2 && ranges.start(0) === 3 &&
          ranges.end(0) === 6 && ranges.start(1) === 7 && ranges.end(1) === 7.5 && invalid &&
          source.activeSourceBuffers.length === 2 && source.readyState === "ended" &&
          events.join(",") === "sourceopen,updatestart:true,updatestart:true,update:false,updateend:false,update:false,updateend:false";
    "#).unwrap();
    assert_eq!(result.as_bool(), Some(true));
    assert!(runtime.take_media_commands().iter().any(|command| {
        matches!(command, MediaCommand::EndStream { key: ended, error: None } if *ended == key)
    }));
    runtime.eval(r#"player.removeAttribute("src")"#).unwrap();
    runtime.pump();
    let result = runtime
        .eval(
            r#"
        source.readyState === "closed" && source.sourceBuffers.length === 0 &&
          source.activeSourceBuffers.length === 0 && events[events.length - 1] === "sourceclose";
    "#,
        )
        .unwrap();
    assert_eq!(result.as_bool(), Some(true));
}

#[test]
fn late_completion_does_not_finish_a_new_append_after_abort() {
    let (mut runtime, key) = attached_source();
    runtime
        .eval(
            r#"
        const buffer = source.addSourceBuffer("video/mp4");
        buffer.appendBuffer(new Uint8Array(1));
        buffer.abort();
        buffer.appendBuffer(new Uint8Array(2));
    "#,
        )
        .unwrap();
    let operations: Vec<_> = runtime
        .take_media_commands()
        .into_iter()
        .filter_map(|command| match command {
            MediaCommand::Append {
                buffer, operation, ..
            } => Some((buffer, operation)),
            _ => None,
        })
        .collect();
    let [(buffer, aborted), (_, current)] = operations.as_slice() else {
        panic!("expected two appends")
    };
    runtime.update_media(
        key,
        MediaUpdate::BufferUpdated {
            buffer: *buffer,
            operation: *aborted,
            result: Ok(vec![(0., 1.)]),
        },
    );
    runtime.pump();
    assert_eq!(
        runtime
            .eval("buffer.updating && buffer.buffered.length === 0")
            .unwrap()
            .as_bool(),
        Some(true)
    );
    runtime.update_media(
        key,
        MediaUpdate::BufferUpdated {
            buffer: *buffer,
            operation: *current,
            result: Ok(vec![(1., 3.)]),
        },
    );
    runtime.pump();
    assert_eq!(
        runtime
            .eval("!buffer.updating && buffer.buffered.start(0) === 1")
            .unwrap()
            .as_bool(),
        Some(true)
    );
}

#[test]
fn media_waiting_drops_readiness_and_recovery_does_not_repeat_loadeddata() {
    let (mut runtime, key) = attached_source();
    runtime
        .eval(
            r#"
        for (const name of ["loadeddata", "canplay", "waiting", "playing"]) {
          player.addEventListener(name, () => events.push(name + ":" + player.readyState));
        }
        player.play();
    "#,
        )
        .unwrap();
    runtime.update_media(
        key,
        MediaUpdate::Metadata {
            width: 160,
            height: 90,
            duration: f64::INFINITY,
            has_audio: true,
        },
    );
    for update in [
        MediaUpdate::Ready,
        MediaUpdate::Playing,
        MediaUpdate::Waiting,
        MediaUpdate::Ready,
        MediaUpdate::Playing,
        MediaUpdate::Ready,
        MediaUpdate::Waiting,
        MediaUpdate::Playing,
        MediaUpdate::Ready,
    ] {
        runtime.update_media(key, update);
    }
    assert_eq!(
        runtime
            .eval(
                r#"events.join(",") === "sourceopen,loadeddata:4,canplay:4,playing:4,waiting:2,canplay:4,playing:4,waiting:2,canplay:4,playing:4""#,
            )
            .unwrap()
            .as_bool(),
        Some(true)
    );
}
