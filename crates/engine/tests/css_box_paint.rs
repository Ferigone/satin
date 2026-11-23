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
