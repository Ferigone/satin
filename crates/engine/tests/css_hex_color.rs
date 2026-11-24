use rust_browser_engine::{
    Fonts, Layout, LoadedPage,
    css::{Color, color},
    layout::Viewport,
    paint::Canvas,
};

#[test]
fn css_hex_color_components_are_unsigned_hex_digits() {
    for source in ["#+f0", "#+f00", "#+f0000", "#+f0000ff", "#ff00_0"] {
        assert_eq!(color(source), None, "{source}");
    }
    for (source, expected) in [
        ("#F00", Color(255, 0, 0, 255)),
        ("#F008", Color(255, 0, 0, 136)),
        ("#0F0000", Color(15, 0, 0, 255)),
        ("#FF0000CC", Color(255, 0, 0, 204)),
    ] {
        assert_eq!(color(source), Some(expected), "{source}");
    }
}

#[test]
fn signed_hex_color_declarations_do_not_override_a_painted_fallback() {
    const WIDTH: u32 = 160;
    const HEIGHT: u32 = 112;
    let page = LoadedPage::from_html(
        "https://css-color.test/",
        include_str!("fixtures/css_hex_color.html"),
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
    let expected = [0x00ff00, 0x00ff00, 0xff0000, 0xff7777, 0x0f0000, 0xff3333];
    let actual: Vec<_> = (0..expected.len())
        .map(|row| canvas.pixels[(row * 16 + 8) * WIDTH as usize + 8])
        .collect();
    assert_eq!(actual, expected, "each entry is a swatch center");
}
