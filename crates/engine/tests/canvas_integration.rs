use rust_browser_engine::{
    Fonts, Layout, LoadedPage, css::Color, dom::NodeId, layout::DrawCommand, paint::Canvas,
    script::ScriptRuntime,
};
use rust_browser_javascript::Value;
use std::sync::Arc;

fn page(body: &str) -> LoadedPage {
    LoadedPage::from_html(
        "https://canvas.test/",
        &format!("<!doctype html><style>body{{margin:0}}</style>{body}"),
    )
}

fn node(page: &LoadedPage, id: &str) -> NodeId {
    page.document
        .connected_nodes()
        .find(|&node| page.document.attr(node, "id") == Some(id))
        .unwrap()
}

fn rgb(layout: &Layout, fonts: &mut Fonts, x: usize, y: usize) -> u32 {
    let mut paint = Canvas::new(320, 240, Color::WHITE);
    paint.document(layout, fonts, 0., 0.);
    paint.pixels[y * 320 + x]
}

fn run(runtime: &mut ScriptRuntime, source: &str) {
    assert_eq!(runtime.eval(source).unwrap(), Value::Bool(true));
}

#[test]
fn canvas_element_context_identity_dimension_reflection_and_resize() {
    let page =
        page("<canvas id=a></canvas><canvas id=b width=' invalid ' height=' +007px'></canvas>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    run(
        &mut runtime,
        r#"
        const a = document.getElementById('a');
        const b = document.getElementById('b');
        const ctx = a.getContext('2d', {alpha: false});
        const attributes = ctx.getContextAttributes();
        const initial = a instanceof HTMLCanvasElement && a.width === 300 && a.height === 150
            && b.width === 300 && b.height === 7 && ctx instanceof CanvasRenderingContext2D
            && ctx.canvas === a && ctx === a.getContext('2d', {alpha: true})
            && ctx !== b.getContext('2d') && a.getContext('2D') === null
            && !attributes.alpha && attributes.colorSpace === 'srgb';
        ctx.fillStyle = '#f00';
        ctx.fillRect(0, 0, 2, 2);
        ctx.save();
        ctx.translate(1, 1);
        a.width = a.width;
        ctx.restore();
        const sameWidth = a.getAttribute('width') === '300' && ctx.fillStyle === '#000000'
            && ctx.getTransform().e === 0 && ctx.getImageData(0, 0, 1, 1).data.join(',') === '0,0,0,255';
        ctx.fillRect(0, 0, 2, 2);
        a.setAttribute('height', '7');
        a.removeAttribute('height');
        const clone = a.cloneNode();
        const cloneContext = clone.getContext('2d');
        initial && sameWidth && a.height === 150 && a.getContext('2d') === ctx
            && clone !== a && clone.width === a.width && cloneContext !== ctx
            && cloneContext.getImageData(0, 0, 1, 1).data[3] === 0
        "#,
    );
}

#[test]
fn live_bitmap_repaint_uses_existing_geometry_and_css_scale() {
    let mut fonts = Fonts::new().unwrap();
    let page = page(
        "<canvas id=a width=20 height=10 style='display:block;width:80px;height:40px;\
         margin:8px;background:#ffff00;transform:translate(10px,5px)'>\
         <div id=fallback>Fallback</div></canvas>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let id = node(&page, "a");
    let layout = runtime.layout_for_viewport(runtime.viewport()).unwrap();
    assert_eq!(layout.rect_for(id).unwrap().width, 80.);
    assert!(layout.rect_for(node(&page, "fallback")).is_none());
    assert!(
        layout
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Canvas { node, .. } if *node == id))
    );
    assert_eq!(rgb(&layout, &mut fonts, 28, 22), 0x00ff_ff00);
    run(
        &mut runtime,
        "const ctx = document.getElementById('a').getContext('2d'); \
         ctx.fillStyle = '#00ff00'; ctx.fillRect(0, 0, 10, 10); true",
    );
    assert!(!runtime.layout_needs_refresh());
    assert!(runtime.publish_canvas_frames());
    assert!(!runtime.publish_canvas_frames());
    assert!(Arc::ptr_eq(
        &layout,
        &runtime.layout_for_viewport(runtime.viewport()).unwrap()
    ));
    assert_eq!(rgb(&layout, &mut fonts, 28, 22), 0x0000_ff00);
    assert_eq!(rgb(&layout, &mut fonts, 82, 22), 0x00ff_ff00);
    run(&mut runtime, "ctx.clearRect(0, 0, 20, 10); true");
    assert!(runtime.publish_canvas_frames());
    assert_eq!(rgb(&layout, &mut fonts, 28, 22), 0x00ff_ff00);
}

#[test]
fn canvas_frames_are_scoped_to_their_document_realm() {
    let mut fonts = Fonts::new().unwrap();
    let mut page = page("<canvas id=a style=display:block width=8 height=8></canvas>");
    let mut first = ScriptRuntime::new(&page).unwrap();
    let second = ScriptRuntime::new(&page).unwrap();
    run(
        &mut first,
        "const c = document.getElementById('a').getContext('2d', {alpha:false}); true",
    );
    first.snapshot(&mut page);
    let first_layout = Layout::build(&page, &fonts, 320.);
    let second_layout = second.layout_for_viewport(second.viewport()).unwrap();
    assert_eq!(rgb(&first_layout, &mut fonts, 2, 2), 0);
    assert_eq!(rgb(&second_layout, &mut fonts, 2, 2), 0x00ff_ffff);
}

#[test]
fn foreign_canvas_elements_keep_normal_dom_behavior() {
    let page = page("");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    run(
        &mut runtime,
        r#"
        const foreign = document.createElementNS('http://www.w3.org/2000/svg', 'canvas');
        const html = document.createElementNS('http://www.w3.org/1999/xhtml', 'canvas');
        let brandError = false;
        try { HTMLCanvasElement.prototype.getContext.call(foreign, '2d'); }
        catch (error) { brandError = error instanceof TypeError; }
        brandError && foreign.getContext === undefined && html.getContext('2d').canvas === html
        "#,
    );
}
