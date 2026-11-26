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

#[test]
fn extended_range_hsl_paints_like_its_srgb_equivalent() {
    // WPT 9b8b1beec730, css/css-color/hsl-009.html through hsl-011.html.
    let cases: &[(&str, &[&str])] = &[
        (
            "color(srgb 0.462304 0.462304 1.556896)",
            &["hsl(60deg, 5701%, 100.96%)", "hsl(60 5701% 100.96%)"],
        ),
        (
            "color(srgb 0.462304 0.462304 1.556896 / 60%)",
            &[
                "hsla(60deg, 5701%, 100.96%, 60%)",
                "hsl(60 5701% 100.96% / .6)",
            ],
        ),
        (
            "color(srgb 0.60146 0.60146 -0.61846)",
            &["hsl(240 7176% -0.85%)"],
        ),
        ("color(srgb -0.2 1.2 -0.2)", &["hsl(120 140% 50%)"]),
        // The first f64 above 100 is 100 + 2^-46. The chosen saturation
        // amplifies that exact percentage offset to a half-unit amplitude;
        // dividing the lightness before subtracting instead gives 0.78125.
        (
            "color(srgb 0.5 0.5 1.5)",
            &["hsl(60 351843720888320000% 100.00000000000001%)"],
        ),
        // Finite f32 tokens whose product would overflow in f32. The
        // extended sRGB channels are near [-x, -x, x], mapping to black.
        ("#000000", &["hsl(60 3e38% 3e38%)"]),
        ("#999999", &["hsl(30, -50%, 60%)"]),
        ("#008000", &["hsl(120 100% 25%)"]),
        ("#ff3333", &["hsla(0turn, 100%, 50%, 80%)"]),
        (
            "#ff0000",
            &[
                "hsl(120, 140% 50%)",
                "hsl(60 1e100% 50%)",
                "hsl(60 50% -1e100%)",
                "hsl(60 NaN% 50%)",
            ],
        ),
    ];
    const WIDTH: u32 = 16;
    let mut html =
        String::from("<style>body{margin:0;background:white}div{height:4px;width:8px}</style>");
    let mut rows = 0;
    for (reference, inputs) in cases {
        for value in std::iter::once(reference).chain(inputs.iter()) {
            html.push_str(&format!(
                "<div style='background-color:red;background-color:{value}'></div>"
            ));
            rows += 1;
        }
    }
    let page = LoadedPage::from_html("https://css-color.test/", &html);
    let mut fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, WIDTH as f32);
    let mut canvas = Canvas::new(WIDTH, rows * 4, layout.background);
    canvas.document(&layout, &mut fonts, 0., 0.);
    assert!(!layout.limited && !canvas.limited);
    let pixel = |row: usize| canvas.pixels[(row * 4 + 2) * WIDTH as usize + 2];
    assert_ne!(pixel(0), pixel(3), "alpha must change the painted swatch");
    let mut row = 0;
    for (reference, inputs) in cases {
        let expected = pixel(row);
        if reference.starts_with("color(") {
            assert_ne!(expected, 0xffffff, "{reference} painted canvas white");
            assert_ne!(expected, 0xff0000, "{reference} painted fallback red");
        }
        row += 1;
        for input in *inputs {
            assert_eq!(pixel(row), expected, "{input} vs {reference}");
            row += 1;
        }
    }
}

#[test]
fn named_colors_and_hwb_resolve_to_srgb_in_paint() {
    let cases = [
        ("DarkSlateGrey", 0x2f4f4f),
        ("goldenrod", 0xdaa520),
        ("lightgoldenrodyellow", 0xfafad2),
        ("RebeccaPurple", 0x663399),
        ("hwb(120 0% 50%)", 0x008000),
        ("hwb(320deg 30% 40%)", 0x994d80),
        ("hwb(-180deg 20 30)", 0x33b3b3),
        ("HWb(1turn 70% 60%)", 0x898989),
        ("hwb(none none 0%)", 0xff0000),
        ("hwb(0 0% 0% / none)", 0xffffff),
        ("hwb(0 0% 0% / 80%)", 0xff3333),
        ("hwb(210 -30 20)", 0x0040cc),
        ("hwb(120 300% 10%)", 0xf7f7f7),
        ("hwb(0, 10%, 10%)", 0x123456),
        ("hwb(0 0% / 1)", 0x123456),
        ("hwb(1.deg 0 0)", 0x123456),
        ("hwb(0 0.% 0)", 0x123456),
        ("hwb(0 0 0 / 1.)", 0x123456),
    ];
    let mut html =
        String::from("<style>body{margin:0;background:white}div{height:4px;width:8px}</style>");
    for (value, _) in cases {
        html.push_str(&format!(
            "<div style='background:#123456;background-color:{value}'></div>"
        ));
    }
    let page = LoadedPage::from_html("https://css-color.test/", &html);
    let mut fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 16.);
    let mut canvas = Canvas::new(16, cases.len() as u32 * 4, layout.background);
    canvas.document(&layout, &mut fonts, 0., 0.);
    for (row, (value, expected)) in cases.into_iter().enumerate() {
        assert_eq!(canvas.pixels[(row * 4 + 2) * 16 + 2], expected, "{value}");
    }
}

#[test]
fn all_absolute_named_colors_paint_their_reference_values() {
    // WPT 9b8b1beec730: css/css-color/parsing/color-computed-named-color.html
    // and css/css-color/rebeccapurple-ref.html. The implementation uses the CSS Color 4 table.
    let cases: Vec<_> = include_str!("fixtures/css_named_colors.tsv")
        .lines()
        .map(|line| line.split_once('\t').unwrap())
        .collect();
    assert_eq!(cases.len(), 148);
    let mut html = String::from("<style>body{margin:0}div{height:2px;width:8px}</style>");
    for (name, _) in &cases {
        html.push_str(&format!(
            "<div style='background:#123456;background-color:{}'></div>",
            name.to_ascii_uppercase()
        ));
    }
    let mut fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html("https://css-color.test/", &html);
    let layout = Layout::build(&page, &fonts, 16.);
    let mut canvas = Canvas::new(16, cases.len() as u32 * 2, layout.background);
    canvas.document(&layout, &mut fonts, 0., 0.);
    assert!(!layout.limited && !canvas.limited);
    let mismatches: Vec<_> = cases
        .into_iter()
        .enumerate()
        .filter_map(|(i, (name, hex))| {
            let expected = u32::from_str_radix(hex, 16).unwrap();
            (canvas.pixels[(2 * i + 1) * 16 + 4] != expected).then_some(name)
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "{} named colors differed: {mismatches:?}",
        mismatches.len()
    );
}
