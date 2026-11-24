use rust_browser_engine::{
    Fonts, Layout, LoadedPage, Rect,
    css::{self, Color, Overflow, Position},
    dom::NodeId,
    layout::{ScrollOffset, Viewport},
    paint::Canvas,
};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};
fn with_layout(
    source: &str,
    viewport: Viewport,
    scroll: &HashMap<NodeId, ScrollOffset>,
) -> (LoadedPage, Layout) {
    static FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();
    let fonts = FONTS
        .get_or_init(|| Mutex::new(Fonts::new().unwrap()))
        .lock()
        .unwrap();
    let page = LoadedPage::from_html(
        "https://position.test/",
        &format!("<!doctype html><style>body{{margin:0}}{source}"),
    );
    let layout = Layout::build_with_scroll(&page, &fonts, viewport, scroll);
    (page, layout)
}
fn fixture(source: &str) -> (LoadedPage, Layout) {
    with_layout(
        &format!("</style>{source}"),
        Viewport {
            width: 400.,
            height: 300.,
            ..Viewport::default()
        },
        &HashMap::new(),
    )
}
fn node(page: &LoadedPage, name: &str) -> NodeId {
    page.document
        .connected_nodes()
        .find(|&id| page.document.attr(id, "id") == Some(name))
        .unwrap()
}
fn r(page: &LoadedPage, layout: &Layout, name: &str) -> Rect {
    layout.rect_for(node(page, name)).unwrap()
}
fn close(a: f32, b: f32) {
    assert!((a - b).abs() < 0.05, "{a} != {b}");
}
fn rect(actual: Rect, expected: [f32; 4]) {
    for (a, b) in [actual.x, actual.y, actual.width, actual.height]
        .into_iter()
        .zip(expected)
    {
        close(a, b);
    }
}

#[test]
fn positioned_css_cascade_shorthands_wide_keywords_and_invalid_values() {
    let page = LoadedPage::from_html(
        "https://position.test/",
        "<style>#p{position:relative;left:20px;overflow:hidden;transform:translate(10%,2px)}#i{position:inherit;inset:1px 2px 3px 4px;left:5px;position:wat;overflow:visible scroll;transform:rotate(90deg);transform:matrix(1,2)}</style><div id=p><div id=i></div></div>",
    );
    let sheet = css::Stylesheet::new(css::parse(&page.stylesheets[0], 400.), 400.);
    let p = sheet.compute(
        &page.document,
        node(&page, "p"),
        &css::Style::default(),
        16.,
    );
    let i = sheet.compute(&page.document, node(&page, "i"), &p, 16.);
    assert_eq!(i.position, Position::Relative);
    assert_eq!(
        i.inset,
        [
            css::Length::Px(1.),
            css::Length::Px(2.),
            css::Length::Px(3.),
            css::Length::Px(5.)
        ]
    );
    assert_eq!(i.overflow, [Overflow::Auto, Overflow::Scroll]);
    assert_eq!(i.transform.len(), 1);
    assert!(css::supports_declaration("position", "sticky"));
    assert!(!css::supports_declaration(
        "transform",
        "translate(1px) junk"
    ));
}
#[test]
fn relative_offsets_leave_flow_and_absolute_uses_padding_containing_block() {
    let (p, l) = fixture(
        "<div id=p style='position:relative;width:200px;height:100px;padding:10px;border:5px solid red;left:20px;top:30px'><div id=a style='position:absolute;left:10%;top:20%;width:50%;height:50%'></div></div><div id=next style='height:10px'></div>",
    );
    rect(r(&p, &l, "p"), [20., 30., 230., 130.]);
    rect(
        l.flow_rect_for(node(&p, "p")).unwrap(),
        [20., 30., 230., 130.],
    );
    rect(r(&p, &l, "a"), [47., 59., 110., 60.]);
    close(r(&p, &l, "next").y, 130.);
}

#[test]
fn viewport_sticky_moves_its_paint_and_hit_area_without_moving_flow_siblings() {
    let source = "</style><main style='position:relative;height:330px'>\
        <div style='height:90px'></div>\
        <div id=sticky style='position:sticky;top:12px;height:30px;background:#156f23'>\
        <div id=absolute style='position:absolute;top:3px;left:12px;width:8px;height:6px'></div>\
        </div><div id=following style='height:20px'></div></main>\
        <div style='height:700px'></div>";
    let viewport = Viewport {
        width: 400.,
        height: 200.,
        ..Viewport::default()
    };
    let (page, initial) = with_layout(source, viewport, &HashMap::new());
    let (_, stuck) = with_layout(
        source,
        Viewport {
            scroll_y: 140.,
            ..viewport
        },
        &HashMap::new(),
    );
    let (_, past) = with_layout(
        source,
        Viewport {
            scroll_y: 350.,
            ..viewport
        },
        &HashMap::new(),
    );
    close(r(&page, &initial, "sticky").y, 90.);
    close(r(&page, &stuck, "sticky").y, 152.);
    close(r(&page, &stuck, "following").y, 120.);
    close(r(&page, &past, "sticky").y, 300.);
    rect(r(&page, &stuck, "absolute"), [12., 155., 8., 6.]);
    close(stuck.height, initial.height);
    assert!(stuck.depends_on_viewport_scroll);
    assert_eq!(stuck.element_at(4., 156.), Some(node(&page, "sticky")));
    let mut fonts = Fonts::new().unwrap();
    let mut canvas = Canvas::new(400, 200, stuck.background);
    canvas.document(&stuck, &mut fonts, 0., 140.);
    assert_eq!(canvas.pixels[16 * 400 + 4], 0x0015_6f23);
}

#[test]
fn scroll_updates_match_fresh_layout_and_reuse_formatted_styles() {
    let page = LoadedPage::from_html(
        "https://position.test/",
        "<!doctype html>
        <style>body{margin:0} aside{position:fixed;right:2px;top:3px}
        header{position:sticky;top:4px;height:20px}
        section{overflow:auto;width:160px;height:80px}
        a{display:block;margin-top:80px;width:300px;height:180px;background:lime}
        main{position:relative;height:350px} footer{height:600px}</style>
        <main><header id=sticky>Menu</header><section id=scroller>
        <a href=target id=link>Link</a></section></main><footer></footer>
        <aside id=fixed><span id=label>Label</span></aside>",
    );
    let fonts = Fonts::new().unwrap();
    let initial = Viewport {
        width: 320.,
        height: 180.,
        ..Viewport::default()
    };
    let mut layout = Layout::for_scrolling(&page, &fonts, initial, &HashMap::new(), None);
    let styles = layout.computed_styles.clone();
    let scroller = node(&page, "scroller");
    for (y, nested_x, nested_y) in [(110., 17., 65.), (340., 99., 130.), (0., 0., 0.)] {
        let view = Viewport {
            scroll_y: y,
            ..initial
        };
        let offsets = HashMap::from([(
            scroller,
            ScrollOffset {
                x: nested_x,
                y: nested_y,
            },
        )]);
        layout = Layout::for_scrolling(&page, &fonts, view, &offsets, Some(&layout));
        let fresh = Layout::build_with_scroll(&page, &fonts, view, &offsets);
        assert!(std::sync::Arc::ptr_eq(&styles, &layout.computed_styles));
        assert_eq!(layout.scroll_regions, fresh.scroll_regions);
        for id in page.document.connected_nodes() {
            assert_eq!(layout.rect_for(id), fresh.rect_for(id));
            assert_eq!(layout.flow_rect_for(id), fresh.flow_rect_for(id));
        }
        for (x, y) in [(1., 26.), (1., 98.), (315., y + 10.)] {
            assert_eq!(layout.element_at(x, y), fresh.element_at(x, y));
            assert_eq!(layout.link_at(x, y), fresh.link_at(x, y));
        }
    }
    let resized = Layout::for_scrolling(
        &page,
        &fonts,
        Viewport {
            width: 350.,
            ..initial
        },
        &HashMap::new(),
        Some(&layout),
    );
    assert!(!std::sync::Arc::ptr_eq(&styles, &resized.computed_styles));
    close(
        r(&page, &resized, "fixed").x - r(&page, &layout, "fixed").x,
        30.,
    );
}

#[test]
fn an_unpainted_fixed_descendant_does_not_invalidate_a_plain_scroll() {
    let page = LoadedPage::from_html(
        "https://position.test/",
        "<main style='height:900px'><div style='display:none'>
        <span style='position:fixed'>Hidden</span></div></main>",
    );
    let runtime = rust_browser_engine::script::ScriptRuntime::new(&page).unwrap();
    let initial = runtime.layout_for_viewport(Viewport::default()).unwrap();
    assert!(!initial.depends_on_viewport_scroll);
    let scrolled = runtime
        .layout_for_viewport(Viewport {
            scroll_y: 60.,
            ..Viewport::default()
        })
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&initial, &scrolled));
}

#[test]
fn single_axis_scrollers_keep_separate_sticky_scrollports() {
    let source = "</style><div id=outer style='width:160px;height:120px;overflow:auto'>\
        <div id=inner style='margin-left:30px;width:210px;height:130px;overflow:clip auto'>\
        <div style='height:50px'></div>\
        <div id=sticky style='position:sticky;left:7px;top:9px;width:40px;height:25px'></div>\
        <div style='height:220px'></div></div>\
        <div style='width:360px;height:300px'></div></div>";
    let viewport = Viewport {
        width: 400.,
        height: 300.,
        ..Viewport::default()
    };
    let (page, _) = with_layout(source, viewport, &HashMap::new());
    let inner = node(&page, "inner");
    let outer = node(&page, "outer");
    let (_, layout) = with_layout(
        source,
        viewport,
        &HashMap::from([
            (outer, ScrollOffset { x: 80., y: 15. }),
            (inner, ScrollOffset { x: 100., y: 90. }),
        ]),
    );
    assert_eq!(
        layout.computed_styles[inner.0].overflow,
        [Overflow::Clip, Overflow::Auto]
    );
    let region = layout
        .scroll_regions
        .iter()
        .find(|region| region.node == inner)
        .unwrap();
    assert_eq!(region.offset, ScrollOffset { x: 0., y: 90. });
    rect(r(&page, &layout, "sticky"), [7., -6., 40., 25.]);
    assert!(!layout.depends_on_viewport_scroll);
}

#[test]
fn body_overflow_propagates_to_the_viewport_without_becoming_an_inner_scrollport() {
    let source = "body{height:100px;overflow:hidden}</style>\
        <section style='height:430px'><div style='height:70px'></div>\
        <div id=sticky style='position:sticky;top:7px;height:25px'></div></section>";
    let viewport = Viewport {
        width: 400.,
        height: 180.,
        scroll_y: 130.,
        ..Viewport::default()
    };
    let (page, layout) = with_layout(source, viewport, &HashMap::new());
    assert_eq!(layout.viewport_overflow[1], Overflow::Hidden);
    assert!(!layout.viewport_overflow[1].user_scrollable());
    close(r(&page, &layout, "sticky").y, 137.);
    close(layout.height, 430.);
    assert!(layout.scroll_regions.is_empty());
}
#[test]
fn absolute_sides_stretch_auto_margins_and_out_of_flow_flex_grid() {
    let (p, l) = fixture(
        "<div id=p style='position:relative;width:200px;height:100px'><div id=stretch style='position:absolute;inset:10px 20px'></div><div id=center style='position:absolute;inset:0;width:40px;height:20px;margin:auto'></div></div><div id=f style='display:flex;width:200px'><div id=out style='position:absolute;width:300px;height:500px'></div><div id=flex style='width:50px;height:20px'></div></div><div id=g style='display:grid;grid-template-columns:100px 100px'><div style='position:absolute;width:500px;height:500px'></div><div id=grid style='height:30px'></div></div>",
    );
    rect(r(&p, &l, "stretch"), [20., 10., 160., 80.]);
    rect(r(&p, &l, "center"), [80., 40., 40., 20.]);
    close(r(&p, &l, "f").height, 20.);
    close(r(&p, &l, "flex").width, 50.);
    close(r(&p, &l, "g").height, 30.);
    close(r(&p, &l, "grid").x, 0.);
}

#[test]
fn absolute_auto_margins_distribute_negative_space_by_axis() {
    let page = LoadedPage::from_html(
        "https://position.test/",
        include_str!("fixtures/css_abspos_auto_margins.html"),
    );
    let fonts = Fonts::new().unwrap();
    let layout = Layout::build_with_viewport(&page, &fonts, Viewport::default());
    assert!(!layout.limited);
    rect(r(&page, &layout, "inline"), [50., 160., 120., 10.]);
    rect(r(&page, &layout, "block"), [50., 30., 20., 80.]);
}

#[test]
fn fixed_viewport_geometry_tracks_scroll_without_increasing_document_height() {
    let (p, l) = with_layout(
        "</style><div style='height:1000px'></div><div id=f style='position:fixed;right:10px;bottom:20px;width:30px;height:40px'></div>",
        Viewport {
            width: 400.,
            height: 300.,
            scroll_x: 0.,
            scroll_y: 500.,
        },
        &HashMap::new(),
    );
    rect(r(&p, &l, "f"), [360., 740., 30., 40.]);
    rect(
        l.flow_rect_for(node(&p, "f")).unwrap(),
        [360., 240., 30., 40.],
    );
    close(l.height, 1000.);
    assert_eq!(l.element_at(370., 750.), Some(node(&p, "f")));
    assert!(l.depends_on_viewport_scroll);
}
#[test]
fn transformed_ancestor_captures_fixed_and_rotation_hits_use_inverse_geometry() {
    let (p, l) = fixture(
        "<div id=p style='width:200px;height:100px;margin-left:30px;transform:translate(10px,20px)'><div id=f style='position:fixed;left:0;top:0;width:20px;height:20px'></div></div><div id=rot style='width:100px;height:100px;transform:rotate(45deg);background:red'></div>",
    );
    rect(r(&p, &l, "f"), [40., 20., 20., 20.]);
    let rot = r(&p, &l, "rot");
    close(rot.width, 100. * 2f32.sqrt());
    assert_ne!(l.element_at(rot.x + 1., rot.y + 1.), Some(node(&p, "rot")));
    assert_eq!(l.element_at(50., 150.), Some(node(&p, "rot")));
}
#[test]
fn overflow_scroll_offsets_clip_paint_and_exact_hits_but_not_dom_rects() {
    let (p, _) = fixture(
        "<div id=p style='width:100px;height:50px;overflow:auto;border:2px solid blue'><div id=child style='width:200px;height:200px;background:red'></div></div>",
    );
    let mut scroll = HashMap::new();
    scroll.insert(node(&p, "p"), ScrollOffset { x: 40., y: 80. });
    let source = "</style><div id=p style='width:100px;height:50px;overflow:auto;border:2px solid blue'><div id=child style='width:200px;height:200px;background:red'></div></div>";
    let (p, l) = with_layout(
        source,
        Viewport {
            width: 400.,
            height: 300.,
            ..Viewport::default()
        },
        &scroll,
    );
    let region = l
        .scroll_regions
        .iter()
        .find(|s| s.node == node(&p, "p"))
        .unwrap();
    close(region.max_x, 100.);
    close(region.max_y, 150.);
    rect(r(&p, &l, "child"), [-38., -78., 200., 200.]);
    assert_eq!(l.element_at(20., 20.), Some(node(&p, "child")));
    assert_ne!(l.element_at(120., 20.), Some(node(&p, "child")));
    let mut fonts = Fonts::new().unwrap();
    let mut canvas = Canvas::new(250, 150, Color::WHITE);
    canvas.document(&l, &mut fonts, 0., 0.);
    assert_eq!(canvas.pixels[20 * 250 + 20], 0xff0000);
    assert_eq!(canvas.pixels[20 * 250 + 120], 0xffffff);
}
#[test]
fn stacking_contexts_are_atomic_and_negative_layers_follow_context_background() {
    let (p, l) = fixture(
        "<div id=a style='position:absolute;left:0;top:0;width:100px;height:100px;z-index:1;background:red'><div id=high style='position:absolute;width:100px;height:100px;z-index:999;background:green'></div></div><div id=b style='position:absolute;left:0;top:0;width:100px;height:100px;z-index:2;background:blue'></div>",
    );
    assert_eq!(l.element_at(10., 10.), Some(node(&p, "b")));
    let mut fonts = Fonts::new().unwrap();
    let mut canvas = Canvas::new(120, 120, Color::WHITE);
    canvas.document(&l, &mut fonts, 0., 0.);
    assert_eq!(canvas.pixels[10 * 120 + 10], 0x0000ff);
}
#[test]
fn no_overflow_creates_no_phantom_scroll_extent() {
    let (p, l) = fixture(
        "<div id=p style='width:100px;height:50px;padding:10px;border:2px solid red;overflow:auto'></div>",
    );
    let s = l
        .scroll_regions
        .iter()
        .find(|s| s.node == node(&p, "p"))
        .unwrap();
    close(s.max_x, 0.);
    close(s.max_y, 0.);
}

#[test]
fn pinned_chrome_157_position_transform_reference() {
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/css_position_reference.json")).unwrap();
    let page = LoadedPage::from_html("https://position.test/", golden["html"].as_str().unwrap());
    let fonts = Fonts::new().unwrap();
    let layout = Layout::build_with_viewport(
        &page,
        &fonts,
        Viewport {
            width: golden["width"].as_f64().unwrap() as f32,
            height: golden["height"].as_f64().unwrap() as f32,
            ..Viewport::default()
        },
    );
    assert!(!layout.limited);
    let mut failures = vec![];
    let mut maximum = 0f32;
    for (name, expected) in golden["rects"].as_object().unwrap() {
        let actual = r(&page, &layout, name);
        for (axis, (actual, expected)) in [actual.x, actual.y, actual.width, actual.height]
            .into_iter()
            .zip(expected.as_array().unwrap())
            .enumerate()
        {
            let expected = expected.as_f64().unwrap() as f32;
            let difference = (actual - expected).abs();
            maximum = maximum.max(difference);
            if difference > 0.05 {
                failures.push(format!("{name}[{axis}]:{actual} != {expected}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    eprintln!("45 Chrome reference boxes, max difference {maximum} CSS px");
}

#[test]
fn fixed_outside_a_transformed_containing_block_escapes_intermediate_overflow() {
    let (p, l) = fixture(
        "<div style='width:50px;height:50px;overflow:hidden'><div id=f style='position:fixed;left:100px;top:20px;width:30px;height:30px;background:red'></div></div><div style='position:relative;width:50px;height:50px;overflow:hidden'><div id=a style='position:absolute;left:100px;top:0;width:30px;height:30px;background:blue'></div></div>",
    );
    assert_eq!(l.element_at(110., 30.), Some(node(&p, "f")));
    assert_ne!(l.element_at(110., 60.), Some(node(&p, "a")));
}
#[test]
fn covering_nonlink_blocks_prevent_link_hit_and_negative_context_order_is_shared() {
    let (p, l) = fixture(
        "<a id=link href='/go' style='position:absolute;left:0;top:0;width:100px;height:100px;z-index:1'></a><div id=cover style='position:absolute;left:0;top:0;width:50px;height:50px;z-index:2'></div><div id=parent style='position:relative;width:100px;height:100px;margin-left:200px;z-index:0;background:red'><div id=neg style='position:absolute;left:0;top:0;width:50px;height:50px;z-index:-1;background:green'></div></div>",
    );
    assert_eq!(l.link_at(70., 70.), Some("https://position.test/go"));
    assert_eq!(l.link_at(20., 20.), None);
    assert_eq!(l.element_at(210., 10.), Some(node(&p, "neg")));
    let mut fonts = Fonts::new().unwrap();
    let mut canvas = Canvas::new(400, 150, Color::WHITE);
    canvas.document(&l, &mut fonts, 0., 0.);
    assert_eq!(canvas.pixels[10 * 400 + 210], 0x008000);
}
#[test]
fn svg_outer_box_uses_replaced_sizing_and_css_winners_preserve_presentation_inheritance() {
    let (p, l) = fixture(
        "<style>svg{color:blue}#shape{fill:var(--fill);--fill:green;stroke:red!important;rx:10px}svg rect{stroke:blue}</style><svg id=s width=120 height=60 viewBox='0 0 240 120'><rect id=shape fill=red style='stroke:green' width=20 height=20/></svg>",
    );
    rect(r(&p, &l, "s"), [0., 0., 120., 60.]);
    let styles = l.computed_svg_styles(&p.document, node(&p, "s"));
    let shape = &styles[&node(&p, "shape")];
    assert_eq!(
        shape.fill,
        Some(rust_browser_engine::svg::SvgPaint::Color(Color::rgb(
            0, 128, 0
        )))
    );
    assert_eq!(
        shape.stroke,
        Some(rust_browser_engine::svg::SvgPaint::Color(Color::rgb(
            255, 0, 0
        )))
    );
    assert_eq!(shape.color, None);
    assert_eq!(styles[&node(&p, "s")].color, Some(Color::rgb(0, 0, 255)));
    assert_eq!(shape.geometry.get("rx").map(String::as_str), Some("10px"));
}
#[test]
fn deep_positioned_transformed_layout_keeps_stack_bounded() {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let mut html = String::from("<style>body{margin:0}</style>");
            for _ in 0..253 {
                html.push_str("<div style='position:relative;transform:translate(0,0)'>");
            }
            html.push_str("<a id=leaf href='/end'>end</a>");
            for _ in 0..253 {
                html.push_str("</div>");
            }
            let (_, layout) = fixture(&html);
            assert!(layout.limited);
            assert!(
                layout
                    .links
                    .iter()
                    .any(|l| l.url == "https://position.test/end")
            );
            assert!(layout.commands.len() < 10000);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn inherited_link_urls_share_storage_and_resolved_metadata_has_an_aggregate_budget() {
    let href = format!("https://position.test/{}", "x".repeat(8000));
    let text = "word ".repeat(512);
    let (_, layout) = fixture(&format!("<a href='{href}'>{text}</a>"));
    assert!(!layout.limited);
    assert!(layout.links.len() > 100);
    let first = layout.links[0].url.as_str().as_ptr();
    assert!(
        layout
            .links
            .iter()
            .all(|l| l.url.as_str().as_ptr() == first)
    );
    let mut source = format!("<base href='{href}'>");
    for index in 0..1500 {
        source.push_str(&format!("<a href='?id={index}'>x</a>"));
    }
    let (_, layout) = fixture(&source);
    assert!(layout.limited);
    assert!(layout.links.len() < 1500);
    assert!(layout.links.len() > 500);
}
#[test]
fn visible_and_clip_overflow_report_scroll_area_without_creating_scroll_offsets() {
    let (p, l) = fixture(
        "<div id=visible style='width:100px;height:50px;padding:10px'><div style='width:200px;height:200px'></div></div><div id=clip style='width:100px;height:50px;overflow:clip'><div style='width:200px;height:200px'></div></div>",
    );
    assert_eq!(l.scroll_size_for(node(&p, "visible")), Some((220., 220.)));
    assert_eq!(l.scroll_size_for(node(&p, "clip")), Some((200., 200.)));
    let clip = l
        .scroll_regions
        .iter()
        .find(|s| s.node == node(&p, "clip"))
        .unwrap();
    assert_eq!(clip.max_y, 0.);
}

#[test]
fn visibility_preserves_layout_and_visible_descendants_but_excludes_hidden_paint_and_hits() {
    let (page, layout) = fixture(
        "<style>
            #link { display:block; width:120px; height:30px; background:blue }
            #cover { position:fixed; top:0; left:0; width:120px; height:30px; z-index:2 }
            .hidden { visibility:hidden; background:red }
            #child { display:block; visibility:visible; width:20px; height:30px; background:green }
            #transparent { position:fixed; top:0; left:100px; width:20px; height:30px; opacity:0 }
            #space { height:40px }
        </style>
        <a id=link href='/forum'></a>
        <div id=space class=hidden>invisible</div><div id=after>after</div>
        <div id=cover class=hidden><div><a id=child href='/child'></a></div></div>
        <div id=transparent></div>",
    );
    rect(r(&page, &layout, "space"), [0., 30., 400., 40.]);
    close(r(&page, &layout, "after").y, 70.);
    assert_eq!(
        layout.link_at(50., 10.),
        Some("https://position.test/forum")
    );
    assert_eq!(
        layout.link_at(10., 10.),
        Some("https://position.test/child")
    );
    assert_eq!(
        layout.element_at(110., 10.),
        Some(node(&page, "transparent"))
    );
    let mut canvas = Canvas::new(400, 120, Color::WHITE);
    canvas.document(&layout, &mut Fonts::new().unwrap(), 0., 0.);
    assert_eq!(canvas.pixels[10 * 400 + 50], 0x0000ff);
    assert_eq!(canvas.pixels[10 * 400 + 10], 0x008000);
    assert!(
        canvas.pixels[30 * 400..70 * 400]
            .iter()
            .all(|&pixel| pixel == 0xffffff)
    );
}

#[test]
fn inline_svg_can_restore_visibility_inside_a_hidden_html_parent() {
    let (page, layout) = fixture(
        "<div style='visibility:hidden; --show:visible'><svg width='150' height='30'>
          <rect width='50' height='30' fill='red'/>
          <rect id='visible' x='50' width='50' height='30' fill='green' visibility='visible'/>
          <rect id='variable' x='100' width='50' height='30' fill='blue' visibility='var(--show)'/>
        </svg></div>",
    );
    assert_eq!(layout.element_at(60., 10.), Some(node(&page, "visible")));
    assert_eq!(layout.element_at(110., 10.), Some(node(&page, "variable")));
    let mut canvas = Canvas::new(400, 50, Color::WHITE);
    canvas.document(&layout, &mut Fonts::new().unwrap(), 0., 0.);
    assert_eq!(canvas.pixels[10 * 400 + 10], 0xffffff);
    assert_eq!(canvas.pixels[10 * 400 + 60], 0x008000);
    assert_eq!(canvas.pixels[10 * 400 + 110], 0x0000ff);
}

#[test]
fn shrink_to_fit_sizes_adjacent_floats_to_one_row_until_cleared() {
    let (page, layout) = fixture(
        "<style>
            #menu { position:absolute; left:20px }
            #menu a { display:block; float:inline-start; width:60px; height:25px; margin:0 3px 0 2px }
        </style>
        <div id=menu>
            <a id=first></a> <a id=second style='clear:right'></a>
            <a id=third></a> <a id=next style='clear:both'></a>
        </div>",
    );
    rect(r(&page, &layout, "menu"), [20., 0., 195., 50.]);
    rect(r(&page, &layout, "first"), [22., 0., 60., 25.]);
    rect(r(&page, &layout, "second"), [87., 0., 60., 25.]);
    rect(r(&page, &layout, "third"), [152., 0., 60., 25.]);
    rect(r(&page, &layout, "next"), [22., 25., 60., 25.]);
}

#[test]
fn source_indentation_does_not_widen_text_floats_or_wrap_an_exact_subpixel_fit() {
    let (page, layout) = fixture(
        "<style>nav {position:absolute; left:0} a {display:block;float:left}
            #fraction {left:370px; top:40px; width:404.6px} #fraction a {width:101.15px;height:10px}
        </style><nav><a id=formatted>  Store
               and    Support    </a><a id=compact>Store and Support</a></nav>
        <nav id=fraction><a></a><a></a><a></a><a id=last></a></nav>",
    );
    close(
        r(&page, &layout, "formatted").width,
        r(&page, &layout, "compact").width,
    );
    close(r(&page, &layout, "last").y, 40.);
    close(r(&page, &layout, "fraction").height, 10.);
}
