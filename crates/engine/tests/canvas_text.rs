use rust_browser_engine::{
    LoadedPage, css,
    fonts::{FontFace, FontFaceEntry, FontSet},
    script::ScriptRuntime,
};
use rust_browser_javascript::Value;
use std::sync::Arc;

fn realm() -> ScriptRuntime {
    let mut page = LoadedPage::from_html(
        "https://canvas.test/",
        "<!doctype html><canvas id=c width=100 height=80 style='font-size:20px;font-weight:700'></canvas>",
    );
    let rule = css::font_faces(
        "@font-face { font-family: 'Audit Wide'; src: url(/wide.ttf); }",
        800.,
    )
    .pop()
    .unwrap();
    page.fonts = Arc::new(
        FontSet::new(vec![FontFaceEntry {
            rule,
            face: FontFace::decode(include_bytes!("fixtures/fonts/wide.ttf")).unwrap(),
        }])
        .unwrap(),
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime
        .eval("var c = document.getElementById('c'); var ctx = c.getContext('2d');")
        .unwrap();
    runtime
}

#[test]
fn text_state_accepts_css_font_shorthand_and_survives_save_restore() {
    let mut runtime = realm();
    assert_eq!(
        runtime
            .eval(
                r#"var defaults = [ctx.font,ctx.textAlign,ctx.textBaseline,ctx.direction,ctx.lang,ctx.letterSpacing].join("|");
                   ctx.font = "italic 400 12px / 2 Audit Wide, monospace";
                   var serialized = ctx.font;
                   ctx.font = "inherit";
                   var ignored = ctx.font === serialized;
                   ctx.save();
                   ctx.font = "bolder 1em monospace";
                   var relative = ctx.font;
                   ctx.textAlign = "right";
                   ctx.textBaseline = "bottom";
                   ctx.direction = "rtl";
                   ctx.letterSpacing = "2px";
                   ctx.wordSpacing = "3px";
                   ctx.restore();
                   var restored = ctx.font === serialized && ctx.textAlign === "start" &&
                     ctx.textBaseline === "alphabetic" && ctx.direction === "inherit" &&
                     ctx.letterSpacing === "0px" && ctx.wordSpacing === "0px";
                   var detached = document.createElement("canvas").getContext("2d");
                   detached.font = "1000% serif";
                   c.width = c.width;
                   [defaults,serialized,ignored,relative,restored,detached.font,ctx.font].join("|");"#,
            )
            .unwrap(),
        Value::from(
            "10px sans-serif|start|alphabetic|inherit|inherit|0px|italic 12px \"Audit Wide\", monospace|true|900 20px monospace|true|100px serif|10px sans-serif"
        )
    );
}

#[test]
fn text_metrics_uses_the_document_font_and_is_independent_of_the_transform() {
    let mut runtime = realm();
    assert_eq!(
        runtime
            .eval(
                r#"ctx.font = "20px Audit Wide";
                   var m = ctx.measureText("AA");
                   var near = function(a,b) { return Math.abs(a-b) < 0.001; };
                   var first = m instanceof TextMetrics && Object.prototype.toString.call(m) === "[object TextMetrics]" &&
                     m.width === 32 && m.actualBoundingBoxLeft === 0 &&
                     near(m.actualBoundingBoxRight,30.8) &&
                     m.fontBoundingBoxAscent === 16 && m.fontBoundingBoxDescent === 4 &&
                     m.actualBoundingBoxAscent === 12 && m.actualBoundingBoxDescent === 0 &&
                     m.emHeightAscent === 16 && m.emHeightDescent === 4;
                   var getter = Object.getOwnPropertyDescriptor(TextMetrics.prototype,"width").get;
                   var branded = false;
                   try { getter.call({}); } catch (err) { branded = err.name === "TypeError"; }
                   ctx.scale(2,3);
                   ctx.textBaseline = "top";
                   c.setAttribute("dir","rtl");
                   var aligned = ctx.measureText("AA");
                   var same = aligned.width === 32 && aligned.actualBoundingBoxLeft === 32 &&
                     near(aligned.actualBoundingBoxRight,-1.2) &&
                     aligned.actualBoundingBoxAscent === -4 && aligned.actualBoundingBoxDescent === 16 &&
                     aligned.emHeightAscent === 0;
                   ctx.letterSpacing = "3px";
                   ctx.wordSpacing = "2px";
                   var spaced = ctx.measureText("A\tA").width === 48;
                   var ignoredNull = ctx.measureText("\u0000").width === 0;
                   first && branded && same && spaced && ignoredNull;"#,
            )
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn text_draws_outlines_with_the_shared_transform_clip_and_stroke_state() {
    let mut runtime = realm();
    assert_eq!(
        runtime
            .eval(
                r##"var pixel = function(x,y) { return Array.from(ctx.getImageData(x,y,1,1).data).join(","); };
                   ctx.font = "20px Audit Wide";
                   ctx.fillStyle = "#00ff00";
                   ctx.globalAlpha = 0.5;
                   ctx.save();
                   ctx.beginPath();
                   ctx.rect(10,6,20,22);
                   ctx.clip();
                   ctx.translate(10,10);
                   ctx.fillText("A",0,16);
                   var filled = pixel(11,15) === "0,255,0,128" && pixel(9,15) === "0,0,0,0" &&
                     ctx.isPointInPath(12,10) && !ctx.isPointInPath(29,30);
                   ctx.restore();
                   ctx.globalAlpha = 1;
                   ctx.strokeStyle = "blue";
                   ctx.lineWidth = 2;
                   ctx.strokeText("A",5,50);
                   var stroked = pixel(5,45) === "0,0,255,255" && pixel(11,45) === "0,0,0,0";
                   ctx.fillText("AA",1,75,10);
                   var fitted = pixel(2,70) === "0,255,0,255" && pixel(8,70) === "0,255,0,255" &&
                     pixel(13,70) === "0,0,0,0";
                   filled && stroked && fitted;"##,
            )
            .unwrap(),
        Value::Bool(true)
    );
}
