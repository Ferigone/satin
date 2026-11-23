use rust_browser_engine::{Fonts, Layout, LoadedPage, Rect, dom::NodeId};
use std::sync::{Mutex, OnceLock};
fn layout(body: &str, width: f32) -> (LoadedPage, Layout) {
    static FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();
    let fonts = FONTS
        .get_or_init(|| Mutex::new(Fonts::new().unwrap()))
        .lock()
        .unwrap();
    let page = LoadedPage::from_html(
        "https://example.test/",
        &format!("<!doctype html><style>body{{margin:0}}</style>{body}"),
    );
    let layout = Layout::build(&page, &fonts, width);
    (page, layout)
}
fn id(page: &LoadedPage, name: &str) -> NodeId {
    page.document
        .nodes
        .iter()
        .enumerate()
        .find_map(|(i, _)| (page.document.attr(NodeId(i), "id") == Some(name)).then_some(NodeId(i)))
        .unwrap()
}
fn rect(page: &LoadedPage, layout: &Layout, name: &str) -> Rect {
    layout.rect_for(id(page, name)).unwrap()
}
fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.05,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn border_box_min_max_and_percentage_padding() {
    let (p, l) = layout(
        "<div id='a' style='box-sizing:border-box;width:200px;padding:10%;border:5px solid red;height:100px'></div><div id='b' style='box-sizing:border-box;width:500px;max-width:150px;min-width:180px;padding:10px'></div>",
        400.,
    );
    let a = rect(&p, &l, "a");
    close(a.width, 200.);
    close(a.height, 100.);
    let b = rect(&p, &l, "b");
    close(b.width, 180.);
    close(b.y, 100.);
}
#[test]
fn auto_margins_and_definite_percentage_height() {
    let (p, l) = layout(
        "<div id='a' style='width:200px;margin:0 auto;height:120px;padding:10px'><div id='b' style='height:50%;width:50%'></div></div><div id='c' style='height:50%'><div style='height:25px'></div></div>",
        500.,
    );
    let a = rect(&p, &l, "a");
    close(a.x, 140.);
    close(a.width, 220.);
    let b = rect(&p, &l, "b");
    close(b.x, 150.);
    close(b.width, 100.);
    close(b.height, 60.);
    close(rect(&p, &l, "c").height, 25.);
}
#[test]
fn adjacent_vertical_margins_collapse_with_negative_values() {
    let (p, l) = layout(
        "<div id='a' style='height:10px;margin-bottom:20px'></div><div id='b' style='height:10px;margin-top:30px;margin-bottom:-5px'></div><div id='c' style='height:10px;margin-top:8px'></div>",
        300.,
    );
    close(rect(&p, &l, "b").y, 40.);
    close(rect(&p, &l, "c").y, 53.);
}
#[test]
fn row_flex_distributes_growth_and_respects_constraints() {
    let (p, l) = layout(
        "<div style='display:flex;width:500px;gap:20px'><div id='a' style='flex:1;min-width:0;max-width:100px;height:30px'></div><div id='b' style='flex:2;min-width:0;height:20px'></div></div>",
        600.,
    );
    let a = rect(&p, &l, "a");
    let b = rect(&p, &l, "b");
    close(a.width, 100.);
    close(b.x, 120.);
    close(b.width, 380.);
    close(b.height, 20.);
}
#[test]
fn row_flex_shrinks_wraps_orders_and_aligns() {
    let (p, l) = layout(
        "<div style='display:flex;width:300px;gap:10px;align-items:center'><div id='a' style='width:200px;min-width:0;height:40px;order:2'></div><div id='b' style='width:200px;min-width:0;height:20px'></div></div><div id='wrap' style='display:flex;flex-wrap:wrap;width:250px;gap:10px'><div id='c' style='width:120px;height:30px;flex-shrink:0'></div><div style='width:120px;height:20px'></div><div id='d' style='width:120px;height:10px'></div></div>",
        600.,
    );
    let a = rect(&p, &l, "a");
    let b = rect(&p, &l, "b");
    close(a.width, 145.);
    close(b.width, 145.);
    close(b.x, 0.);
    close(a.x, 155.);
    close(b.y, 10.);
    let c = rect(&p, &l, "c");
    let d = rect(&p, &l, "d");
    close(d.y - c.y, 40.);
    close(rect(&p, &l, "wrap").height, 50.);
}
#[test]
fn flex_auto_margins_reverse_and_cross_stretch() {
    let (p, l) = layout(
        "<div style='display:flex;width:400px;height:100px;gap:10px'><div id='a' style='width:50px;margin-left:auto'></div><div id='b' style='width:50px'></div></div><div style='display:flex;flex-direction:row-reverse;width:300px;justify-content:space-between'><div id='c' style='width:50px;height:20px'></div><div id='d' style='width:70px;height:20px'></div></div>",
        600.,
    );
    close(rect(&p, &l, "a").x, 290.);
    close(rect(&p, &l, "b").x, 350.);
    close(rect(&p, &l, "a").height, 100.);
    close(rect(&p, &l, "c").x, 250.);
    close(rect(&p, &l, "d").x, 0.);
}
#[test]
fn column_flex_grows_in_definite_height() {
    let (p, l) = layout(
        "<div style='display:flex;flex-direction:column;width:200px;height:300px;gap:10px'><div id='a' style='flex:1;min-height:0'></div><div id='b' style='flex:2;min-height:0'></div></div>",
        600.,
    );
    close(rect(&p, &l, "a").height, 290. / 3.);
    close(rect(&p, &l, "b").height, 580. / 3.);
    close(rect(&p, &l, "b").y, 290. / 3. + 10.);
    close(rect(&p, &l, "b").width, 200.);
}
#[test]
fn inline_fragment_geometry_has_innermost_hits_and_block_padding_links() {
    let (p, l) = layout(
        "<div id='parent' style='padding:20px'><span id='outer'>one <a id='inner' href='/next'>two three four five six seven</a></span><a id='button' href='/button' style='display:inline-block;width:80px;padding:10px;height:20px'>go</a></div>",
        200.,
    );
    let inner = id(&p, "inner");
    let fragment = l
        .elements
        .iter()
        .find(|e| e.node == inner && e.rect.width > 0.)
        .unwrap();
    assert_eq!(
        l.element_at(fragment.rect.x + 1., fragment.rect.y + 1.),
        Some(inner)
    );
    assert_eq!(
        l.link_at(fragment.rect.x + 1., fragment.rect.y + 1.),
        Some("https://example.test/next")
    );
    let outer = rect(&p, &l, "outer");
    assert!(outer.height > 20.);
    let button = rect(&p, &l, "button");
    close(button.width, 100.);
    assert_eq!(
        l.element_at(button.x + 2., button.y + 2.),
        Some(id(&p, "button"))
    );
    assert_eq!(
        l.link_at(button.x + 2., button.y + 2.),
        Some("https://example.test/button")
    );
    assert_eq!(l.element_at(1., 1.), Some(id(&p, "parent")));
}
#[test]
fn flex_child_link_retains_url_and_hidden_nodes_have_no_geometry() {
    let (p, l) = layout(
        "<div style='display:flex'><a id='a' href='/next' style='width:100px;height:30px;padding:10px'>link</a><span id='hidden' style='display:none'>hidden</span></div>",
        300.,
    );
    let a = rect(&p, &l, "a");
    assert_eq!(
        l.link_at(a.x + 1., a.y + 1.),
        Some("https://example.test/next")
    );
    assert!(l.rect_for(id(&p, "hidden")).is_none());
}
#[test]
fn nonbreaking_space_is_not_collapsed_and_large_inline_line_height_applies() {
    let (p, l) = layout(
        "<div id='line' style='width:50px;line-height:10px'><span id='text' style='font-size:20px;line-height:40px'>A&nbsp;B</span></div>",
        300.,
    );
    close(rect(&p, &l, "line").height, 40.);
    let text = rect(&p, &l, "text");
    assert!(text.width > 20.);
    close(text.height, 40.);
}

#[test]
fn pinned_chrome_157_box_and_flex_reference() {
    // Captured 2026-11-10 with owned headless Chrome 157.0.8103.51, inner viewport
    // 784x505, scale 1. These empty-box fixtures have no font dependency.
    let html = r#"<div id="sizing" style="box-sizing:border-box;width:200px;padding:20px;border:5px solid red;height:80px"></div>
<div style="width:500px"><div id="center" style="width:200px;padding:10px;margin:0 auto;height:20px"></div></div>
<div style="display:flex;width:500px;gap:20px"><div id="growA" style="flex:1;min-width:0;max-width:100px;height:30px"></div><div id="growB" style="flex:2;min-width:0;height:20px"></div></div>
<div style="display:flex;width:300px;gap:10px;align-items:center"><div id="shrinkA" style="width:200px;min-width:0;height:40px"></div><div id="shrinkB" style="width:200px;min-width:0;height:20px"></div></div>
<div id="wrap" style="display:flex;flex-wrap:wrap;width:250px;gap:10px"><div id="wrapA" style="width:120px;height:30px"></div><div style="width:120px;height:20px"></div><div id="wrapB" style="width:120px;height:10px"></div></div>
<div style="display:flex;flex-direction:column;width:200px;height:300px;gap:10px"><div id="colA" style="flex:1;min-height:0"></div><div id="colB" style="flex:2;min-height:0"></div></div>"#;
    let (p, l) = layout(html, 784.);
    let expected = [
        ("sizing", [0., 0., 200., 80.]),
        ("center", [140., 80., 220., 40.]),
        ("growA", [0., 120., 100., 30.]),
        ("growB", [120., 120., 380., 20.]),
        ("shrinkA", [0., 150., 145., 40.]),
        ("shrinkB", [155., 160., 145., 20.]),
        ("wrap", [0., 190., 250., 50.]),
        ("wrapA", [0., 190., 120., 30.]),
        ("wrapB", [0., 230., 120., 10.]),
        ("colA", [0., 240., 200., 96.671875]),
        ("colB", [0., 346.671_88, 200., 193.328_13]),
    ];
    let mut maximum_error = 0f32;
    for (name, expected) in expected {
        let r = rect(&p, &l, name);
        for (actual, expected) in [r.x, r.y, r.width, r.height].into_iter().zip(expected) {
            maximum_error = maximum_error.max((actual - expected).abs());
            assert!(
                (actual - expected).abs() <= 0.02,
                "{name}: {actual} versus Chrome {expected}"
            );
        }
    }
    println!(
        "11 boxes / 44 coordinates versus Chrome 157; max absolute difference = {maximum_error:.6} CSS px"
    );
}
