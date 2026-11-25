use rust_browser_engine::script::ScriptRuntime;
use rust_browser_engine::{Fonts, Layout, LoadedPage, Rect, dom::NodeId, layout::Viewport};
use serde_json::{Value as Json, json};
use std::sync::{Mutex, OnceLock};
fn layout(body: &str, width: f32) -> (LoadedPage, Layout) {
    let html = format!("<!doctype html><style>body{{margin:0}}</style>{body}");
    layout_document(&html, width)
}
fn layout_document(html: &str, width: f32) -> (LoadedPage, Layout) {
    static FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();
    let fonts = FONTS
        .get_or_init(|| Mutex::new(Fonts::new().unwrap()))
        .lock()
        .unwrap();
    let page = LoadedPage::from_html("https://example.test/", html);
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
fn noscript_only_renders_fallback_when_scripting_is_disabled() {
    use rust_browser_engine::css::{self, Display, Style};

    let source = "<body><noscript id=fallback style='display:block!important'>\
        <style>#results{display:none!important}</style><b>Fallback</b>\
        </noscript><div id=results>Results</div>";
    let (page, rendered) = layout(source, 500.);
    let fallback = id(&page, "fallback");
    assert!(page.document.text(fallback).contains("<style>"));
    assert_eq!(
        rendered.computed_styles.get(fallback.0).unwrap().display,
        Display::None
    );
    assert!(rendered.rect_for(fallback).is_none());
    let (reference, expected) = layout("<div id=results>Results</div>", 500.);
    close(
        rect(&page, &rendered, "results").y,
        rect(&reference, &expected, "results").y,
    );

    let disabled = rust_browser_engine::html::parse_with_scripting(source, false);
    let fallback = disabled.html_elements("noscript").next().unwrap();
    assert_eq!(disabled.html_elements("b").count(), 1);
    assert_eq!(
        css::compute(&disabled, fallback, &Style::default(), &[]).display,
        Display::Block
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
fn nested_flex_rows_measure_all_visible_items_and_gaps() {
    let (p, l) = layout(
        "<nav style='display:flex;width:500px'><div id='tabs' style='display:flex;gap:8px;overflow:auto'>
        <div style='width:50px'></div><div hidden></div><div style='position:absolute;width:200px'></div>
        <div style='width:60px;margin:0 2px'></div><div id='last' style='width:70px'></div>
        </div><button id='next' style='padding:0;border:0;width:20px'>+</button></nav>",
        500.,
    );
    close(rect(&p, &l, "tabs").width, 200.);
    close(rect(&p, &l, "last").x, 130.);
    close(rect(&p, &l, "next").x, 200.);
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
fn flex_resolves_partial_factors_and_freezes_only_net_violations() {
    let (p, l) = layout_document(include_str!("fixtures/css_flex_free_space.html"), 800.);
    // Edge 158.0.4370.33, headless at scale 1; no text metrics involved.
    for (name, expected) in [
        ("ordinary-a", [0., 0., 72., 10.]),
        ("ordinary-b", [72., 0., 48., 10.]),
        ("partial-shrink-a", [0., 10., 96., 10.]),
        ("partial-shrink-b", [96., 10., 64., 10.]),
        ("partial-grow-a", [0., 20., 120., 10.]),
        ("partial-grow-b", [120., 20., 150., 10.]),
        ("initial-freeze-a", [0., 30., 140., 10.]),
        ("initial-freeze-b", [140., 30., 80., 10.]),
        ("mixed-min", [0., 40., 175., 10.]),
        ("mixed-max", [175., 40., 100., 10.]),
        ("mixed-free", [275., 40., 175., 10.]),
    ] {
        let r = rect(&p, &l, name);
        let actual = [r.x, r.y, r.width, r.height];
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 0.05),
            "{name}: expected {expected:?}, got {actual:?}"
        );
    }
}
#[test]
fn flex_shrink_weights_use_inner_basis_before_definite_minimums() {
    let (p, l) = layout_document(
        include_str!("fixtures/css_flex_inner_shrink_basis.html"),
        800.,
    );
    // Edge 158.0.4370.33. Each row starts with 150 and 100 px outer bases.
    for (name, expected) in [
        ("plain-a", [0., 0., 120., 10.]),
        ("plain-b", [120., 0., 80., 10.]),
        ("inset-a", [0., 10., 125., 10.]),
        ("inset-b", [125., 10., 75., 10.]),
        ("border-box-a", [0., 20., 122., 10.]),
        ("min-b", [122., 20., 78., 10.]),
    ] {
        let r = rect(&p, &l, name);
        let actual = [r.x, r.y, r.width, r.height];
        assert!(
            actual
                .into_iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 0.05),
            "{name}: expected {expected:?}, got {actual:?}"
        );
    }
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
fn flex_line_wrapping_and_alignment_match_chromium() {
    // Border rectangles relative to each flex container, Chromium 131.0.6778.0.
    let (p, l) = layout_document(include_str!("fixtures/css_flex_line_alignment.html"), 800.);
    assert!(!l.limited);
    let mut failures = Vec::new();
    let mut checked = 0;
    for node in p.document.connected_nodes() {
        let Some(reference) = p.document.attr(node, "data-rect") else {
            continue;
        };
        let parent = p.document.nodes[node.0].parent.unwrap();
        let container = l.rect_for(parent).unwrap();
        let item = l.rect_for(node).unwrap();
        let rect = [
            item.x - container.x,
            item.y - container.y,
            item.width,
            item.height,
        ];
        let expected: Vec<f32> = reference
            .split_whitespace()
            .map(|v| v.parse().unwrap())
            .collect();
        assert_eq!(expected.len(), rect.len());
        if !rect.iter().zip(expected).all(|(a, b)| (a - b).abs() < 0.05) {
            failures.push(format!(
                "case {:?}, item {node:?}: expected {reference}, got {rect:?}",
                p.document.attr(parent, "style"),
            ));
        }
        checked += 1;
    }
    assert_eq!(checked, 63);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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
fn used_flex_item_height_is_only_a_percentage_basis_if_definite() {
    let (p, l) = layout(
        "<style>.column{display:flex;flex-direction:column;min-height:100px}
         .column>div{flex-grow:1}.fill{height:100%}</style>
         <div class=column><div id=automatic><div id=auto-fill class=fill></div></div></div>
         <div class=column style='flex-wrap:wrap'><div><div id=wrap-fill class=fill></div></div></div>
         <div class=column style='height:0'><div><div id=definite-fill class=fill></div></div></div>
         <div class=column><div style='flex-basis:0px'><div id=basis-fill class=fill></div></div></div>
         <div class=column><div style='height:10px'><div id=size-fill class=fill></div></div></div>",
        400.,
    );
    close(rect(&p, &l, "automatic").height, 100.);
    for (name, expected) in [
        ("auto-fill", 0.),
        ("wrap-fill", 0.),
        ("definite-fill", 100.),
        ("basis-fill", 100.),
        ("size-fill", 100.),
    ] {
        close(rect(&p, &l, name).height, expected);
    }
}

#[test]
fn flex_fieldset_min_height_constrains_the_outer_box() {
    let (p, l) = layout(
        "<style>fieldset{margin:0;padding:0;border:1px solid;display:flex;align-items:center;
         flex-direction:column;justify-content:center;width:200px;min-height:200px}
         fieldset>div{width:100px;height:100px}</style>
         <fieldset id=auto-field><div id=first></div></fieldset>
         <fieldset id=fixed-field style='height:200px'><div id=second></div></fieldset>",
        400.,
    );
    for (parent, child, expected_y) in [("auto-field", "first", 1.), ("fixed-field", "second", 51.)]
    {
        let field = rect(&p, &l, parent);
        let item = rect(&p, &l, child);
        close(field.height, 202.);
        close(item.x - field.x, 51.);
        close(item.y - field.y, expected_y);
    }
}

#[test]
fn physical_flex_justification_handles_reversed_axes_and_rtl() {
    let (p, l) = layout(
        "<style>section{direction:rtl;display:flex;width:100px;height:40px}
         section>div{width:20px;height:10px}</style>
         <section style='justify-content:left'><div id=left></div></section>
         <section style='flex-direction:row-reverse;justify-content:right'><div id=right></div></section>
         <section id=column style='flex-direction:column-reverse;justify-content:left'><div id=vertical></div></section>",
        400.,
    );
    close(rect(&p, &l, "left").x, 0.);
    close(rect(&p, &l, "right").x, 80.);
    close(rect(&p, &l, "vertical").y - rect(&p, &l, "column").y, 0.);
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
fn viewport_height_resolves_through_percent_sized_flex_shell() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        include_str!("fixtures/css_flex_viewport_shell.html"),
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    for source in page
        .scripts
        .iter()
        .filter_map(|script| script.source.as_deref())
    {
        runtime.eval(source).unwrap();
    }

    // Headless Chrome 157.0.8103.79 at device scale 1, with a fresh
    // navigation per viewport. Satin also checks live viewport updates.
    for (width, height, indicator) in [
        (1280, 800, [629, 414, 22, 22]),
        (640, 480, [309, 254, 22, 22]),
    ] {
        runtime.set_viewport(Viewport {
            width: width as f32,
            height: height as f32,
            ..Default::default()
        });
        let measurements = runtime.eval("JSON.stringify(shellMeasurements())").unwrap();
        let actual: Json =
            serde_json::from_str(&measurements.as_string().unwrap().to_string_lossy()).unwrap();
        let full = [0, 0, width, height];
        assert_eq!(
            actual,
            json!({
                "html": full,
                "body": full,
                "#shell": full,
                "#bar": [0, 0, width, 50],
                "#loader": full,
                "#indicator": indicator,
            }),
            "viewport {width} × {height}"
        );
    }
}

#[test]
fn flex_auto_minimum_uses_computed_main_axis_overflow() {
    let (p, l) = layout_document(
        include_str!("fixtures/css_flex_overflow_minimum.html"),
        800.,
    );
    // REA Chrome 157.0.8103.79 at 800 × 700 CSS px and device scale 1.
    let expected = [
        ("hidden-caption", [160., 0., 120., 40.]),
        ("paired-caption", [160., 46., 220., 40.]),
        ("visible-caption", [160., 92., 220., 40.]),
        ("clipped-caption", [160., 138., 220., 40.]),
        ("fixed-min-caption", [160., 184., 164., 40.]),
        ("scrolling-messages", [0., 230., 240., 84.]),
        ("scrolling-composer", [0., 322., 240., 28.]),
        ("clipped-messages", [0., 356., 240., 160.]),
        ("clipped-composer", [0., 524., 240., 28.]),
        ("fixed-min-messages", [0., 482., 240., 100.]),
        ("fixed-min-composer", [0., 590., 240., 28.]),
    ];
    for (name, expected) in expected {
        let r = rect(&p, &l, name);
        let actual = [r.x, r.y, r.width, r.height];
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 0.05),
            "{name}: expected {expected:?}, got {actual:?}"
        );
    }
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

#[test]
fn list_markers_follow_inherited_type_and_computed_display() {
    use rust_browser_engine::{
        css::{Display, MarkerType},
        layout::DrawCommand,
    };
    let (page, rendered) = layout(
        r#"
        <style>
          .tabs { display: flex; list-style: none }
          .box { display: block } .flex { display: flex }
          .number { display: list-item; list-style: outside decimal }
        </style>
        <ul class=tabs><li id=tab></li><li></li></ul>
        <ul><li></li><li class=box></li><li class=flex></li>
          <li style='list-style: square'></li><li style='list-style-type:circle'></li></ul>
        <ol start=9><li></li><li class=box></li><li value=15></li><li></li></ol>
        <ol reversed><li></li><li></li></ol>
        <section><div class=number></div><div class=number></div></section>
    "#,
        800.,
    );
    let tab = &rendered.computed_styles[id(&page, "tab").0];
    assert_eq!(tab.display, Display::ListItem);
    assert_eq!(tab.marker, MarkerType::None);
    let (_, expected) = layout(
        "<div>•</div><div>▪</div><div>◦</div><div>9.</div><div>15.</div><div>16.</div><div>2.</div><div>1.</div><div>1.</div><div>2.</div>",
        800.,
    );
    let glyphs = |layout: &Layout| {
        layout
            .commands
            .iter()
            .filter_map(|cmd| match cmd {
                DrawCommand::Text { run, .. } => {
                    Some(run.glyphs.iter().map(|g| g.id).collect::<Vec<_>>())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(glyphs(&rendered), glyphs(&expected));
}

#[test]
fn fit_content_container_shrinks_to_children_and_centers_with_auto_margins() {
    let (page, rendered) = layout(
        r#"<style>
        .parent { width: 400px }
        .pill { display:flex; width:fit-content; margin:0 auto; padding:0 2px }
        .pill i { width:60px; height:10px }
        .column { display:flex; flex-direction:column }
        .grid { display:grid; grid-template-columns:400px }
        #capped { max-width:84px; box-sizing:border-box }
        #lower { min-width:140px }
        </style>
        <div class=parent><div class=pill id=normal><i></i><i></i></div>
          <div class=pill id=capped><i></i><i></i></div>
          <div class=pill id=lower><i></i><i></i></div></div>
        <div class="column parent"><div class=pill id=flexitem><i></i><i></i></div></div>
        <div class="grid parent"><div class=pill id=griditem><i></i><i></i></div></div>"#,
        700.,
    );
    for (name, width) in [
        ("normal", 124.),
        ("capped", 84.),
        ("lower", 144.),
        ("flexitem", 124.),
        ("griditem", 124.),
    ] {
        let item = rect(&page, &rendered, name);
        close(item.width, width);
        close(item.x, (400. - width) / 2.);
    }
}
