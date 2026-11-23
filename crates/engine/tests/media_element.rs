use rust_browser_engine::{
    LoadedPage,
    script::{MediaCommand, ScriptRuntime},
};

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
