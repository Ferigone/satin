use rust_browser_engine::{Fonts, Layout, LoadedPage, layout::Viewport, paint::Canvas};

#[test]
fn uniform_translucent_border_corners_blend_once_with_or_without_ancestor_clip() {
    let mut fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html(
        "https://paint.test/",
        include_str!("fixtures/css_box_border_corner.html"),
    );
    let layout = Layout::build_with_viewport(
        &page,
        &fonts,
        Viewport {
            width: 640.,
            height: 480.,
            ..Viewport::default()
        },
    );
    let mut canvas = Canvas::new(640, 480, layout.background);
    canvas.document(&layout, &mut fonts, 0., 0.);
    assert!(!layout.limited);
    assert!(!canvas.limited);

    for (x, y, rgb) in [
        // All four corners, straight sides, and the intact yellow background.
        (10, 10, 0xff7f00),
        (60, 10, 0xff7f00),
        (60, 43, 0xff7f00),
        (10, 43, 0xff7f00),
        (25, 10, 0xff7f00),
        (9, 24, 0xff7f00),
        (25, 20, 0xffff00),
        (7, 10, 0xffffff),
        // Same box through the ancestor clip with a negative left margin.
        (10, 56, 0xff7f00),
        (25, 56, 0xff7f00),
        (43, 56, 0xff7f00),
        (7, 56, 0xffffff),
        (44, 56, 0xffffff),
        // Only visible strips of the 1,000,000-by-1,000,000 CSS px box.
        (10, 104, 0xff7f7f),
        (300, 104, 0xff7f7f),
        (10, 150, 0xff7f7f),
        (12, 106, 0xffffff),
    ] {
        assert_eq!(canvas.pixels[y * 640 + x], rgb, "pixel ({x},{y})");
    }
}

#[test]
fn rounded_boxes_and_outlines_paint_at_their_own_edges_without_moving_content() {
    let mut fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html(
        "https://paint.test/",
        r#"
        <!doctype html><style>
        body {margin:0;background:white}
        #bar {position:absolute;left:12px;top:12px;width:80px;height:32px;
              --radius:1000px;border-radius:var(--radius);background:#eef;
              outline:2px solid #369;outline-offset:2px}
        #card {position:absolute;left:12px;top:62px;width:80px;height:32px;
               box-sizing:border-box;border:4px solid red;background:yellow;
               border-radius:50% 0 8px 3px / 25% 0 4px 7px; border-top-left-radius:20px 10px}
        </style><form id=bar></form><div id=card></div>"#,
    );
    let layout = Layout::build(&page, &fonts, 160.);
    let node = page.document.html_elements("form").next().unwrap();
    let rect = layout.rect_for(node).unwrap();
    assert_eq!(
        (rect.x, rect.y, rect.width, rect.height),
        (12., 12., 80., 32.)
    );
    let style = layout
        .inspect_computed_style(&page.document, node)
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(style["border-top-left-radius"], "1000px");
    assert_eq!(style["outline-width"], "2px");
    assert_eq!(style["outline-offset"], "2px");

    let mut canvas = Canvas::new(160, 120, layout.background);
    canvas.document(&layout, &mut fonts, 0., 0.);
    assert!(!canvas.limited);
    for (x, y, color) in [
        (12, 12, 0xffffff),
        (52, 8, 0x336699),
        (52, 10, 0xffffff),
        (52, 12, 0xeeeeff),
        (8, 28, 0x336699),
        (10, 28, 0xffffff),
        (12, 28, 0xeeeeff),
        (12, 62, 0xffffff),
        (32, 62, 0xff0000),
        (32, 67, 0xffff00),
        (91, 62, 0xff0000),
    ] {
        assert_eq!(canvas.pixels[y * 160 + x], color, "pixel ({x},{y})");
    }
}

#[test]
fn zero_opacity_hides_all_paint_but_preserves_layout_and_link_hits() {
    let page = LoadedPage::from_html(
        "https://paint.test/",
        r#"<style>
        body {margin:0;background:lime}
        section {opacity:0;width:80px;height:40px;background:blue}
        a {display:block;position:relative;z-index:2;opacity:1;width:32px;height:24px;background:red}
        aside {width:10px;height:10px;background:black}
    </style><section><a href="/inside"></a></section><aside></aside>"#,
    );
    let mut fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 100.);
    let mut canvas = Canvas::new(100, 60, layout.background);
    canvas.document(&layout, &mut fonts, 0., 0.);
    let anchor = page.document.html_elements("a").next().unwrap();
    let rect = layout.rect_for(anchor).unwrap();
    assert_eq!((rect.width, rect.height), (32., 24.));
    assert_eq!(layout.element_at(5., 5.), Some(anchor));
    assert_eq!(layout.link_at(5., 5.), Some("https://paint.test/inside"));
    assert_eq!(canvas.pixels[5 * 100 + 5], 0x00ff00);
    assert_eq!(canvas.pixels[35 * 100 + 50], 0x00ff00);
    assert_eq!(canvas.pixels[45 * 100 + 5], 0x000000);
}

#[test]
fn root_opacity_hides_propagated_background_but_body_opacity_preserves_it() {
    let mut fonts = Fonts::new().unwrap();
    for (target, expected) in [("html", 0xffffff), ("body", 0xff0000)] {
        let html = format!(
            "<style>body{{background:red;margin:0}}{target}{{opacity:0}}b{{display:block;width:20px;height:20px;background:blue}}</style><b></b>"
        );
        let page = LoadedPage::from_html("https://paint.test/", &html);
        let layout = Layout::build(&page, &fonts, 100.);
        let mut canvas = Canvas::new(100, 60, layout.background);
        canvas.document(&layout, &mut fonts, 0., 0.);
        assert_eq!(canvas.pixels[3 * 100 + 3], expected);
    }
}
