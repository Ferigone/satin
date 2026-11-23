use base64::{Engine, engine::general_purpose::STANDARD};
use ring::digest::{SHA256, digest};
use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;
use serde::Deserialize;
use std::{fs, path::Path};

const WPT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/wpt-canvas-pixels"
);
const WPT_REVISION: &str = "9e1907a13cbbc338975302097a4bafcbc4d12dc5";

fn realm(html: &str) -> ScriptRuntime {
    let page = LoadedPage::from_html("https://canvas.example/", html);
    ScriptRuntime::new(&page).unwrap()
}

fn text(runtime: &mut ScriptRuntime, code: &str) -> String {
    runtime
        .eval(code)
        .unwrap()
        .as_string()
        .expect("JavaScript string")
        .to_string_lossy()
}

#[test]
fn image_data_keeps_the_native_view_identity_after_gc_and_rechecks_detachment() {
    let mut runtime = realm("<canvas id='c' width='2' height='1'></canvas>");
    let result = runtime
        .eval(
            r#"
            var c = document.getElementById('c');
            var ctx = c.getContext('2d');
            var backing = new ArrayBuffer(16);
            var view = new Uint8ClampedArray(backing, 4, 8);
            view.set([9, 11, 13, 255, 70, 33, 17, 255]);
            var data = new ImageData(view, 2);
            function readPixels() { return Array.from(ctx.getImageData(0,0,2,1).data).join(','); }
            data.data === view && data.data.byteOffset === 4 && data.width === 2 && data.height === 1
            "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
    runtime
        .eval("view = null; ctx.putImageData(data, 0, 0);")
        .unwrap();
    runtime.pump();
    assert_eq!(
        text(&mut runtime, "readPixels()"),
        "9,11,13,255,70,33,17,255"
    );

    let result = text(
        &mut runtime,
        r#"
        let result = 'no exception';
        try {
            ctx.putImageData(data, {valueOf() {backing.transfer(); return 0;}}, 0);
        } catch (error) {
            result = error.name;
        }
        result + ' ' + data.data.length + ' ' + readPixels()
        "#,
    );
    assert_eq!(result, "InvalidStateError 0 9,11,13,255,70,33,17,255");
}

#[test]
fn read_and_dirty_writes_normalize_negative_extents_and_ignore_drawing_state() {
    let mut runtime = realm("<canvas id='c' width='3' height='2'></canvas>");
    let result = text(
        &mut runtime,
        r#"
        const ctx = document.getElementById('c').getContext('2d');
        ctx.rect(0, 0, 0, 0);
        ctx.clip();
        ctx.translate(100, 100);
        ctx.globalAlpha = 0;
        ctx.globalCompositeOperation = 'destination-out';
        const data = new ImageData(new Uint8ClampedArray([
            1,2,3,255, 4,5,6,255,
            7,8,9,255, 10,11,12,255
        ]), 2, 2);
        ctx.putImageData(data, 1, 0, 2, 2, -1, -2);
        Array.from(ctx.getImageData(4, 2, -3, -2).data).join(',')
        "#,
    );
    assert_eq!(
        result,
        "0,0,0,0,4,5,6,255,0,0,0,0,0,0,0,0,10,11,12,255,0,0,0,0"
    );
}

#[test]
fn float16_and_p3_image_data_convert_to_the_canvas_color_space() {
    let mut runtime = realm("<canvas id='c' width='2' height='1'></canvas>");
    let result = text(
        &mut runtime,
        r#"
        const ctx = document.getElementById('c').getContext('2d');
        const halfBacking = new ArrayBuffer(16);
        const halfView = new Float16Array(halfBacking, 4, 4);
        halfView.set([1, 0, 0, 1]);
        const half = new ImageData(halfView, 1, 1, {pixelFormat:'rgba-float16'});
        const p3 = new ImageData(new Uint8ClampedArray([234, 51, 35, 255]), 1, 1, {colorSpace:'display-p3'});
        ctx.putImageData(half, 0, 0);
        ctx.putImageData(p3, 1, 0);
        const p3Read = ctx.getImageData(0, 0, 1, 1, {colorSpace:'display-p3'});
        const copy = ctx.createImageData(half);
        const read = ctx.getImageData(1, 0, 1, 1, {pixelFormat:'rgba-float16'});
        JSON.stringify([
            Array.from(p3Read.data), p3Read.colorSpace, half.data === halfView,
            copy.data instanceof Float16Array, copy.pixelFormat, copy.data[0],
            read.data instanceof Float16Array, read.data[0] > 0.99, read.data[3]
        ])
        "#,
    );
    assert_eq!(
        result,
        r#"[[234,51,35,255],"display-p3",true,true,"rgba-float16",0,true,true,1]"#
    );
}

#[test]
fn an_opaque_canvas_uses_opaque_black_and_ignores_written_alpha() {
    let mut runtime = realm("<canvas id='c' width='2' height='1'></canvas>");
    let result = text(
        &mut runtime,
        r#"
        const ctx = document.getElementById('c').getContext('2d', {alpha:false});
        ctx.putImageData(new ImageData(new Uint8ClampedArray([123, 77, 66, 0]), 1), 1, 0);
        Array.from(ctx.getImageData(-1, 0, 3, 1).data).join(',')
        "#,
    );
    assert_eq!(result, "0,0,0,0,0,0,0,255,123,77,66,255");
}

#[test]
fn draw_image_applies_transforms_and_uses_an_immutable_source_for_overlapping_copies() {
    let mut runtime = realm(
        "<canvas id='source' width='3' height='1'></canvas><canvas id='dst' width='5' height='1'></canvas>",
    );
    let result = text(
        &mut runtime,
        r#"
        const src = document.getElementById('source');
        const srcCtx = src.getContext('2d');
        srcCtx.putImageData(new ImageData(new Uint8ClampedArray([
            255,0,0,255, 0,255,0,255, 0,0,255,255
        ]), 3), 0, 0);
        const dst = document.getElementById('dst').getContext('2d');
        dst.translate(1, 0);
        dst.rect(0, 0, 2, 1);
        dst.clip();
        dst.drawImage(src, 0, 0);
        const destination = Array.from(dst.getImageData(0,0,5,1).data).join(',');
        srcCtx.imageSmoothingEnabled = false;
        srcCtx.drawImage(src, 0,0,2,1, 1,0,2,1);
        const selfCopy = Array.from(srcCtx.getImageData(0,0,3,1).data).join(',');
        destination + ';' + selfCopy
        "#,
    );
    assert_eq!(
        result,
        "0,0,0,0,255,0,0,255,0,255,0,255,0,0,0,0,0,0,0,0;255,0,0,255,255,0,0,255,0,255,0,255"
    );
}

#[test]
fn canvas_encoders_serialize_the_source_bitmap_and_to_blob_is_a_later_task() {
    let mut runtime = realm("<canvas id='c' width='1' height='1'></canvas>");
    runtime
        .eval(
            r#"
            var c = document.getElementById('c');
            var ctx = c.getContext('2d');
            ctx.putImageData(new ImageData(new Uint8ClampedArray([5, 23, 73, 255]), 1), 0, 0);
            "#,
        )
        .unwrap();
    for (requested, mime) in [
        ("image/unavailable", "image/png"),
        ("image/jpeg", "image/jpeg"),
        ("image/webp", "image/webp"),
    ] {
        let data_url = text(&mut runtime, &format!("c.toDataURL('{requested}')"));
        let (prefix, payload) = data_url.split_once(',').unwrap();
        assert_eq!(prefix, format!("data:{mime};base64"));
        let bytes = STANDARD.decode(payload).unwrap();
        let rgba = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(rgba.dimensions(), (1, 1));
        let expected = [5, 23, 73, 255];
        let tolerance = if mime == "image/jpeg" { 4 } else { 0 };
        for (actual, expected) in rgba.get_pixel(0, 0).0.into_iter().zip(expected) {
            assert!(actual.abs_diff(expected) <= tolerance);
        }
    }
    let result = text(
        &mut runtime,
        r#"
        var order = [];
        var bytes;
        c.toBlob(function(blob) {
            'use strict';
            order.push(this === undefined ? blob.type : 'unexpected this');
            blob.arrayBuffer().then(function(buffer) { bytes = new Uint8Array(buffer); });
        });
        c.width = 0;
        c.toBlob(function(blob) { order.push(blob === null ? 'empty' : 'unexpected blob'); });
        Promise.resolve().then(function() { order.push('microtask'); });
        c.toDataURL()
        "#,
    );
    assert_eq!(result, "data:,");
    assert_eq!(text(&mut runtime, "order.join(',')"), "");
    runtime.pump();
    assert_eq!(
        text(&mut runtime, "order.join(',')"),
        "microtask,image/png,empty"
    );
    let png: Vec<u8> =
        serde_json::from_str(&text(&mut runtime, "JSON.stringify(Array.from(bytes))")).unwrap();
    let pixel = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(pixel.dimensions(), (1, 1));
    assert_eq!(pixel.get_pixel(0, 0).0, [5, 23, 73, 255]);
    assert!(runtime.diagnostics().is_empty());
}

#[derive(Deserialize)]
struct Manifest {
    revision: String,
    files: Vec<Fixture>,
}

#[derive(Deserialize)]
struct Fixture {
    file: String,
    upstream: String,
    bytes: usize,
    sha256: String,
    subtests: Option<usize>,
    expected_failure: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Subtest {
    name: String,
    status: String,
    message: Option<String>,
}

#[test]
fn pinned_canvas_pixel_wpt() {
    let manifest: Manifest =
        serde_json::from_str(include_str!("fixtures/wpt-canvas-pixels/manifest.json")).unwrap();
    assert_eq!(manifest.revision, WPT_REVISION);
    let mut results = Vec::new();
    for fixture in manifest.files {
        assert_eq!(
            Path::new(&fixture.upstream).file_name().unwrap(),
            fixture.file.as_str()
        );
        let bytes = fs::read(Path::new(WPT_PATH).join(&fixture.file)).unwrap();
        assert_eq!(bytes.len(), fixture.bytes, "{}", fixture.file);
        let hash = digest(&SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(hash, fixture.sha256, "{}", fixture.file);
        let Some(expected) = fixture.subtests else {
            continue;
        };
        let page = LoadedPage::from_html(
            &format!("https://wpt.example/{}", fixture.upstream),
            std::str::from_utf8(&bytes).unwrap(),
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        for script in [
            include_str!("fixtures/wpt-url/bridge.js"),
            include_str!("fixtures/wpt-canvas-pixels/canvas-tests.js"),
            include_str!("fixtures/wpt-canvas-pixels/bridge.js"),
        ] {
            runtime.eval(script).unwrap();
        }
        for script in page
            .scripts
            .iter()
            .filter_map(|script| script.source.as_deref())
        {
            runtime
                .eval(script)
                .unwrap_or_else(|error| panic!("{}: {error}", fixture.file));
        }
        let mut subtests: Vec<Subtest> =
            serde_json::from_str(&text(&mut runtime, "JSON.stringify(wptResults)")).unwrap();
        let count = runtime.eval("canvasLoadChecks.length").unwrap();
        let Value::Number(count) = count else {
            panic!("Canvas load callback count must be numeric");
        };
        for index in 0..count as usize {
            let name = text(&mut runtime, &format!("canvasLoadChecks[{index}].name"));
            let result = runtime.eval(&format!(
                r#"
                {{
                    const canvas = document.getElementById('c');
                    const entry = canvasLoadChecks[{index}];
                    const run = entry.run;
                    run(canvas, canvas.getContext('2d', entry.options));
                }}
                "#
            ));
            subtests.push(Subtest {
                name,
                status: if result.is_ok() { "PASS" } else { "FAIL" }.into(),
                message: result.err().map(|error| error.to_string()),
            });
        }
        assert_eq!(subtests.len(), expected, "{}: {subtests:#?}", fixture.file);
        let expected_status =
            if let Some(reason) = fixture.expected_failure {
                assert_eq!(
                runtime.eval(
                    "document.createElement('canvas').getContext('2d', {colorType:'float16'})",
                ).unwrap(),
                Value::Null,
                "{reason}",
            );
                for subtest in &subtests {
                    assert!(
                        subtest
                            .message
                            .as_deref()
                            .is_some_and(|message| message.starts_with("TypeError:")),
                        "{reason}: {subtest:#?}",
                    );
                }
                "FAIL"
            } else {
                "PASS"
            };
        for item in subtests {
            results.push((fixture.file.clone(), item, expected_status));
        }
    }

    assert_eq!(results.len(), 26);
    let differences = results
        .iter()
        .filter(|(_, item, expected)| &item.status != expected)
        .map(|(file, item, expected)| {
            format!(
                "{file}: {}: {} (expected {expected}, {:?})",
                item.name, item.status, item.message
            )
        })
        .collect::<Vec<_>>();
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    let passed = results
        .iter()
        .filter(|(_, test, _)| test.status == "PASS")
        .count();
    println!(
        "WPT Canvas pixel readback {passed}/{} passed at {WPT_REVISION}",
        results.len()
    );
}
