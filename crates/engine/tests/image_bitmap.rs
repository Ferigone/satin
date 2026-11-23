use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use std::time::{Duration, Instant};

fn check(source: &str) {
    let html = "<canvas id='source' width='2' height='2'></canvas><canvas id='target'></canvas>";
    let page = LoadedPage::from_html("https://example.test/", html);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let program = format!(
        r#"
        var finished = false, receipt;
        async function check() {{ {source} }}
        check().then(value => {{ receipt = value; finished = true; }},
                     error => {{ receipt = error.name + ': ' + error.message; finished = true; }});
        "#
    );
    runtime.eval(&program).unwrap();
    let limit = Instant::now() + Duration::from_secs(4);
    while runtime.eval("finished").unwrap().as_bool() != Some(true) {
        let next = runtime.next_deadline().expect("pending ImageBitmap task");
        assert!(Instant::now() < limit, "ImageBitmap task did not finish");
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        runtime.pump();
    }
    let receipt = runtime.eval("receipt").unwrap();
    assert_eq!(receipt.as_bool(), Some(true), "{receipt:?}");
}

#[test]
fn canvas_capture_resizes_and_flips_the_snapshot_after_microtasks() {
    check(
        r#"
        const source = document.getElementById('source'), target = document.getElementById('target');
        const ctx = source.getContext('2d');
        ctx.putImageData(new ImageData(new Uint8ClampedArray([
            255,0,0,255, 0,255,0,255,
            0,0,255,255, 255,255,255,255
        ]), 2, 2), 0, 0);
        let checkpoint = false;
        const pending = createImageBitmap(source, 2, 0, -3, 2, {
            resizeHeight: 4, imageOrientation: 'flipY', resizeQuality: 'pixelated'
        });
        Promise.resolve().then(() => { checkpoint = true; });
        ctx.clearRect(0, 0, 2, 2);
        const bitmap = await pending;
        const output = target.getContext('2d');
        output.drawImage(bitmap, 0, 0);
        const copied = checkpoint && bitmap instanceof ImageBitmap &&
            bitmap.width === 6 && bitmap.height === 4 &&
            output.getImageData(0,0,1,1).data.join() === '0,0,0,0' &&
            output.getImageData(2,0,1,1).data.join() === '0,0,255,255' &&
            output.getImageData(4,0,1,1).data.join() === '255,255,255,255' &&
            output.getImageData(2,3,1,1).data.join() === '255,0,0,255';
        bitmap.close();
        bitmap.close();
        let error;
        try { output.drawImage(bitmap, 0, 0); }
        catch (thrown) { error = thrown.name; }
        return copied && bitmap.width === 0 && bitmap.height === 0 && error === 'InvalidStateError';
    "#,
    );
}

#[test]
fn cloned_pixels_and_transfer_destinations_survive_intrinsic_changes_and_rollback() {
    check(
        r#"
        const Constructor = ImageBitmap, bitmap = await createImageBitmap(new ImageData(2, 1));
        const clone = structuredClone({one: bitmap, two: bitmap});
        const width = Object.getOwnPropertyDescriptor(Constructor.prototype, 'width').get;
        globalThis.ImageBitmap = undefined;
        Object.defineProperty(bitmap, 'width', {get() { throw 'author getter'; }});
        const moved = structuredClone({bitmap}, {transfer: [bitmap]}).bitmap;
        const buffer = new ArrayBuffer(3);
        const bad = {get sideEffect() { moved.close(); return 1; }};
        let error, detached;
        try { structuredClone(bad, {transfer:[buffer, moved]}); }
        catch (thrown) { error = thrown.name; }
        try { structuredClone(bitmap); }
        catch (thrown) { detached = thrown.name; }
        return clone.one === clone.two && clone.one instanceof Constructor &&
            width.call(clone.one) === 2 && width.call(bitmap) === 0 &&
            moved instanceof Constructor && width.call(moved) === 0 &&
            error === 'DataCloneError' && detached === 'DataCloneError' && buffer.byteLength === 3;
    "#,
    );
}

#[test]
fn real_blob_decoding_settles_through_window_tasks_and_rejects_its_errors() {
    check(
        r#"
        const source = document.getElementById('source'), target = document.getElementById('target');
        const ctx = source.getContext('2d');
        ctx.fillStyle = 'red';
        ctx.fillRect(0, 0, 2, 2);
        const blob = await new Promise(resolve => source.toBlob(resolve, 'image/png'));
        const file = new File([blob], 'actual.png', {type:'image/png'});
        const decoded = await createImageBitmap(file, {resizeWidth: 3});
        const output = target.getContext('2d');
        output.drawImage(decoded, 0, 0);
        const pixel = output.getImageData(2, 2, 1, 1).data.join();
        const errors = [];
        for (const make of [
            () => createImageBitmap(new Blob(['broken'], {type:'image/png'})),
            () => createImageBitmap(source, 0, 0, 0, 5),
            () => createImageBitmap(source, {resizeWidth: 0}),
            () => createImageBitmap(source, {resizeWidth: -1}),
            () => createImageBitmap({}),
        ]) {
            try { await make(); } catch (error) { errors.push(error.name); }
        }
        const original = {};
        let preserved = false;
        try { await createImageBitmap(source, {get resizeWidth() { throw original; }}); }
        catch (error) { preserved = error === original; }
        const matched = decoded.width === 3 && decoded.height === 3 && pixel === '255,0,0,255' &&
            errors.join() === 'InvalidStateError,RangeError,InvalidStateError,TypeError,TypeError' && preserved;
        return matched || JSON.stringify([decoded.width, decoded.height, pixel, errors]);
    "#,
    );
}
