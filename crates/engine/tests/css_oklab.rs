use rust_browser_engine::{
    Fonts, Layout, LoadedPage,
    css::{Color, color},
    paint::Canvas,
};

#[test]
fn absolute_ok_and_profile_colors_reach_srgb_paint_and_invalid_declarations_keep_the_fallback() {
    // WPT css/css-color/oklab-001 and oklch-011 use the green swatches.
    // The saturated CSS gamut-mapping examples were also checked with
    // Color.js 0.5.2, toGamut({space: "srgb", method: "css"}).
    let cases = [
        ("color(srgb .2 50% .8)", 0x3380cc),
        ("COLOR(srgb-linear .2 .5 .8)", 0x7cbce7),
        ("color(display-p3 0 1 0)", 0x00fb29),
        ("color(display-p3 .2 .5 .8)", 0x0082d2),
        ("color(display-p3-linear .2 .5 .8)", 0x66beec),
        ("color(srgb 1.2 .4 -.1)", 0xff8d62),
        ("color(srgb 1 none none / 80%)", 0xcc0000),
        ("color(srgb 1 0 0 1)", 0x123456),
        ("color(srgb, 1, 0, 0)", 0x123456),
        ("color(oklch .5 .2 90)", 0x123456),
        ("oklab(51.975% -0.1403 0.10768)", 0x008000),
        ("oklch(51.975% 44.215% 142.495)", 0x008000),
        ("oklab(70% -25% 0%)", 0x4bb3a1),
        ("oklch(.627 .194 149.214deg)", 0x00a63e),
        ("oklch(70% 0.4 25)", 0xff5655),
        ("oklch(.7 100% -.25turn)", 0x7494ff),
        ("OKLCH(.5 -.25 2rad)", 0x636363),
        ("oklab(none none none)", 0x000000),
        ("oklch(110% 0.8 none / 1)", 0xffffff),
        ("oklch(1 0 0 / 80%)", 0xcccccc),
        ("oklab(.5 .2 .1 / none)", 0x000000),
        ("oklch(1. 0 0)", 0x123456),
        ("oklch(50%, .2, 25)", 0x123456),
        ("oklab(.5 .12 0 1)", 0x123456),
        ("oklch(.5 .1 30% / 1)", 0x123456),
    ];
    let mut html =
        String::from("<style>body{margin:0;background:black}div{width:8px;height:4px}</style>");
    for (value, _) in cases {
        html.push_str(&format!(
            "<div style='background:#123456;background-color:{value}'></div>"
        ));
    }
    let page = LoadedPage::from_html("https://colors.test/", &html);
    let mut fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 16.);
    let mut canvas = Canvas::new(16, cases.len() as u32 * 4, layout.background);
    canvas.document(&layout, &mut fonts, 0., 0.);
    for (row, (value, expected)) in cases.into_iter().enumerate() {
        assert_eq!(canvas.pixels[(row * 4 + 2) * 16 + 2], expected, "{value}");
    }
}

#[test]
fn very_large_chroma_preserves_hue_and_extreme_lightness_preserves_alpha() {
    let bounded = color("oklch(0.7 3 25)").unwrap();
    assert_eq!(color("oklch(0.7 1e300 25)"), Some(bounded));
    assert_eq!(color("oklab(0.7 1e308 -1e308)"), color("oklch(.7 3 -45)"));
    assert_eq!(color("oklch(-1 2 none / 50%)"), Some(Color(0, 0, 0, 128)));
    assert_eq!(
        color("oklch(4 2 500grad / -5)"),
        Some(Color(255, 255, 255, 0))
    );
}

#[test]
fn extreme_finite_rgb_profile_components_have_bounded_srgb_results() {
    assert_eq!(color("color(display-p3 1e308 1e308 0)"), Some(Color::WHITE));
    assert_eq!(
        color("color(srgb-linear -1e308 -1e308 0)"),
        Some(Color::BLACK)
    );
}
