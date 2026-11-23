use rust_browser_engine::{
    Fonts, Layout, LoadedPage,
    css::{Color, color},
    layout::Viewport,
    paint::Canvas,
};

#[test]
fn functional_colors_keep_legacy_and_modern_separators_distinct() {
    let reds = [
        ("RGB(100%, 0%, 0%, 1)", 255),
        ("rgba(255,0,0)", 255),
        ("rgba(255 0% 0/80%)", 204),
        ("rgb(255 0 0 / .8)", 204),
        ("hsl(0turn, 100%, 50%, 80%)", 204),
        ("hsla(360deg 100% 50%)", 255),
    ];
    for (source, alpha) in reds {
        assert_eq!(color(source), Some(Color(255, 0, 0, alpha)), "{source}");
    }
    for source in [
        "rgb(255, 0 0)",
        "rgba(255,,0,0)",
        "rgb(255 0 0 1)",
        "rgb(255 0 0 /)",
        "rgba(255, 0, 0 / 1)",
        "rgb(255, 0%, 0%)",
        "hsl(0 100%, 50%)",
        "hsla(0 100% 50% // 1)",
        "rgb(255, 0, 0,)",
        "rgb(255 / 0 0 1)",
        "rgb(255 0 0 / 1 1)",
    ] {
        assert_eq!(color(source), None, "{source}");
    }
}

#[test]
fn invalid_functional_colors_do_not_override_a_painted_fallback() {
    const WIDTH: u32 = 160;
    const HEIGHT: u32 = 224;
    let page = LoadedPage::from_html(
        "https://css-color.test/",
        include_str!("fixtures/css_color_syntax.html"),
    );
    let mut fonts = Fonts::new().unwrap();
    let layout = Layout::build_with_viewport(
        &page,
        &fonts,
        Viewport {
            width: WIDTH as f32,
            height: HEIGHT as f32,
            ..Viewport::default()
        },
    );
    let mut canvas = Canvas::new(WIDTH, HEIGHT, layout.background);
    canvas.document(&layout, &mut fonts, 0., 0.);
    assert!(!layout.limited);
    assert!(!canvas.limited);
    let expected = [
        0x00ff00, 0x00ff00, 0x00ff00, 0x00ff00, 0x00ff00, 0x00ff00, 0x00ff00, 0x00ff00, 0x00ff00,
        0xff0000, 0xff3333, 0xff3333, 0xff0000,
    ];
    let actual: Vec<_> = (0..expected.len())
        .map(|row| canvas.pixels[(row * 16 + 8) * WIDTH as usize + 8])
        .collect();
    assert_eq!(actual, expected, "each entry is a swatch center");
}
