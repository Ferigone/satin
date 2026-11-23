use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

fn check(script: &str) {
    let page = LoadedPage::from_html(
        "https://drawing.test/",
        "<!doctype html><canvas id=canvas width=32 height=32></canvas>",
    );
    let mut browser = ScriptRuntime::new(&page).unwrap();
    let result = browser
        .eval(&format!(
            r#"
            const canvas = document.getElementById('canvas');
            const c = canvas.getContext('2d');
            const pixel = (x, y) => c.getImageData(x, y, 1, 1).data.join(',');
            const same = (value, expected) => {{ if (value !== expected) throw new Error(`Expected ${{expected}}, received ${{value}}`); }};
            {script}
            true
            "#
        ))
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn canvas_path_errors_use_the_original_dom_exception() {
    check(
        r#"
        const OriginalDOMException = DOMException;
        let calls = 0;
        globalThis.DOMException = function () {
            calls++;
            return {name: 'Substituted'};
        };
        let error;
        try { c.arc(0, 0, -1, 0, 1); }
        catch (caught) { error = caught; }
        same(error instanceof OriginalDOMException, true);
        same(error.name, 'IndexSizeError');
        same(calls, 0);
        "#,
    );
}

#[test]
fn canvas_drawing_transform_is_baked_into_current_path_and_matrix_is_a_snapshot() {
    check(
        r#"
        c.setTransform({a: 2, d: 2, e: 5, f: 7});
        c.save();
        c.beginPath();
        c.rect(0, 0, 10, 5);
        c.translate(4, 6);
        const matrix = c.getTransform();
        same(matrix instanceof DOMMatrix, true);
        same(matrix.e, 13);
        same(matrix.m42, 19);
        matrix.e = 123;
        same(c.getTransform().e, 13);
        c.restore();
        same(c.getTransform().e, 5);
        c.resetTransform();
        same(c.isPointInPath(6, 8), true);
        same(c.isPointInPath(25, 17), true);
        same(c.isPointInPath(4, 8), false);
        c.fillStyle = '#0f0';
        c.fill();
        same(pixel(5, 7), '0,255,0,255');
        same(pixel(24, 16), '0,255,0,255');
        same(pixel(25, 16), '0,0,0,0');
        c.setTransform(new DOMMatrix([1, 0, 0, 1, 2, 3]));
        same(c.getTransform().f, 3);
        "#,
    );
}

#[test]
fn canvas_drawing_nested_clip_and_copy_keep_the_clipped_out_pixels() {
    check(
        r#"
        c.fillStyle = 'red';
        c.fillRect(0, 0, 32, 32);
        c.save();
        const ring = new Path2D();
        ring.rect(0, 0, 20, 20);
        ring.rect(5, 5, 10, 10);
        c.clip(ring, 'evenodd');
        const bound = new Path2D();
        bound.rect(1, 1, 24, 24);
        c.clip(bound);
        c.globalCompositeOperation = 'copy';
        c.fillStyle = 'blue';
        c.fillRect(1, 1, 2, 2);
        same(pixel(1, 1), '0,0,255,255');
        same(pixel(4, 4), '0,0,0,0');
        same(pixel(10, 10), '255,0,0,255');
        same(pixel(0, 0), '255,0,0,255');
        same(pixel(25, 25), '255,0,0,255');
        c.restore();
        same(c.globalCompositeOperation, 'source-over');
        same(c.fillStyle, '#ff0000');
        c.clearRect(0, 0, 32, 32);
        same(pixel(10, 10), '0,0,0,0');
        "#,
    );
}

#[test]
fn canvas_drawing_paths_round_rectangles_and_stroke_attributes() {
    check(
        r#"
        const path = new Path2D('M2 2h6v6h-6z');
        path.addPath(path, {e: 8});
        same(c.isPointInPath(path, 3, 3), true);
        same(c.isPointInPath(path, 11, 3), true);
        same(c.isPointInPath(path, 9, 3), false);
        c.fillStyle = '#ff00ff';
        c.fill(path);
        same(pixel(11, 3), '255,0,255,255');
        c.beginPath();
        c.roundRect(20, 1, -10, 8, [3, 3]);
        same(c.isPointInPath(19, 2), true);
        same(c.isPointInPath(10, 1), false);
        c.beginPath();
        c.moveTo(2, 18);
        c.lineTo(30, 18);
        c.lineWidth = 2;
        c.lineCap = 'butt';
        c.setLineDash([4, 3, 1]);
        same(c.getLineDash().join(','), '4,3,1,4,3,1');
        same(c.isPointInStroke(3, 18), true);
        same(c.isPointInStroke(7, 18), false);
        c.stroke();
        same(pixel(3, 17), '0,0,0,255');
        same(pixel(7, 17), '0,0,0,0');
        c.beginPath();
        c.moveTo(4, 22);
        c.arcTo(12, 22, 12, 30, 4);
        c.lineTo(12, 30);
        c.lineTo(4, 30);
        c.closePath();
        same(c.isPointInPath(8, 27), true);
        same(c.isPointInPath(12, 22), false);
        let badRadius = '';
        try { c.arc(4, 4, -1, 0, 1); } catch (error) { badRadius = error.name; }
        same(badRadius, 'IndexSizeError');
        "#,
    );
}

#[test]
fn canvas_drawing_gradients_and_canvas_pattern_preserve_style_identity_and_bitmap_snapshot() {
    check(
        r#"
        const src = document.createElement('canvas');
        src.width = 2;
        src.height = 1;
        const sc = src.getContext('2d');
        sc.fillStyle = 'red';
        sc.fillRect(0, 0, 1, 1);
        sc.fillStyle = 'blue';
        sc.fillRect(1, 0, 1, 1);
        const pat = c.createPattern(src, 'repeat');
        c.fillStyle = pat;
        c.imageSmoothingEnabled = false;
        c.save();
        const grad = c.createLinearGradient(0.5, 0, 3.5, 0);
        grad.addColorStop(0, '#000');
        grad.addColorStop(1, '#fff');
        c.fillStyle = grad;
        c.fillRect(0, 0, 4, 1);
        same(pixel(0, 0), '0,0,0,255');
        same(pixel(1, 0), '85,85,85,255');
        same(pixel(3, 0), '255,255,255,255');
        c.restore();
        same(c.fillStyle, pat);
        sc.fillStyle = 'green';
        sc.fillRect(0, 0, 2, 1);
        pat.setTransform({e: 1});
        c.fillRect(0, 1, 4, 1);
        same(pixel(0, 1), '0,0,255,255');
        same(pixel(1, 1), '255,0,0,255');
        same(pixel(2, 1), '0,0,255,255');
        same(pixel(3, 1), '255,0,0,255');
        "#,
    );
}

#[test]
fn canvas_drawing_compositing_uses_the_source_and_backdrop_alpha() {
    check(
        r#"
        c.fillStyle = 'blue';
        c.fillRect(0, 0, 2, 1);
        c.fillStyle = 'red';
        c.globalCompositeOperation = 'multiply';
        c.fillRect(0, 0, 1, 1);
        same(pixel(0, 0), '0,0,0,255');
        same(pixel(1, 0), '0,0,255,255');
        c.globalCompositeOperation = 'screen';
        c.fillRect(1, 0, 1, 1);
        same(pixel(1, 0), '255,0,255,255');
        c.globalCompositeOperation = 'destination-out';
        c.globalAlpha = 0.5;
        c.fillRect(1, 0, 1, 1);
        same(pixel(1, 0), '255,0,255,127');
        c.globalAlpha = 1;
        c.globalCompositeOperation = 'source-over';
        c.fillStyle = 'rgba(14, 20, 30, .5)';
        same(c.fillStyle, 'rgba(14, 20, 30, 0.5)');
        c.fillRect(4, 4, 1, 1);
        same(pixel(4, 4), '14,20,30,128');
        c.setTransform({e: 3, f: 3});
        c.clearRect(1, 1, 1, 1);
        same(pixel(4, 4), '0,0,0,0');
        "#,
    );
}

#[test]
fn canvas_drawing_shadows_use_device_offsets_blur_and_the_clip() {
    check(
        r#"
        c.scale(2, 2);
        c.shadowColor = '#00ff00';
        c.shadowOffsetX = 5;
        c.shadowOffsetY = 3;
        c.fillStyle = 'red';
        c.fillRect(1, 1, 1, 1);
        same(pixel(2, 2), '255,0,0,255');
        same(pixel(7, 5), '0,255,0,255');
        same(pixel(6, 5), '0,0,0,0');
        c.resetTransform();
        c.clearRect(7, 5, 2, 2);
        same(pixel(7, 5), '0,0,0,0');
        same(pixel(12, 8), '0,0,0,0');
        c.save();
        c.beginPath();
        c.rect(0, 0, 24, 18);
        c.clip();
        c.shadowBlur = 4;
        c.fillRect(15, 13, 4, 4);
        const softened = c.getImageData(21, 17, 1, 1).data;
        same(softened[1], 255);
        same(softened[3] > 0 && softened[3] < 255, true);
        same(pixel(21, 18), '0,0,0,0');
        c.restore();
        same(c.shadowBlur, 0);
        canvas.width = canvas.width;
        same(c.shadowColor, 'rgba(0, 0, 0, 0)');
        same(c.shadowOffsetX, 0);
    "#,
    );
}

#[test]
fn canvas_drawing_current_color_uses_the_canvas_computed_foreground() {
    check(
        r#"
        document.body.appendChild(canvas);
        document.body.setAttribute('style', 'color: rgb(13, 59, 87)');
        c.fillStyle = 'currentColor';
        same(c.fillStyle, '#0d3b57');
        c.fillRect(3, 3, 2, 2);
        same(pixel(3, 3), '13,59,87,255');
        canvas.setAttribute('style', 'color: #11aa33');
        c.strokeStyle = 'currentColor';
        c.shadowColor = 'currentColor';
        same(c.strokeStyle, '#11aa33');
        same(c.shadowColor, '#11aa33');
        same(c.fillStyle, '#0d3b57');
    "#,
    );
}
