use rust_browser_engine::{
    Fonts, Layout, LoadedPage,
    css::{Color, Style, compute, supports_declaration},
    layout::Viewport,
    paint::Canvas,
    script::ScriptRuntime,
};

const NESTED_MIX: &str =
    "color-mix(in srgb, color-mix(in srgb, currentColor 50%, color(srgb 0 1 0)), white)";
const BLUE_RESULT: &str = "color(srgb .5 .75 .75)";
const RED_RESULT: &str = "color(srgb .75 .75 .5)";

fn foreground_page(foreground: &str) -> LoadedPage {
    // From WPT css/css-color/color-mix-currentcolor-nested-for-color-property.html.
    LoadedPage::from_html(
        "https://css-color.test/",
        &format!(
            "<!doctype html><style>
                body {{ color: color(srgb 0 0 1); }}
                #mix {{ color: {foreground}; }}
                #child {{ color: inherit; }}
                #literal {{ color: {BLUE_RESULT}; }}
            </style><body>
                <div id=mix><div id=child>Nested color mix</div></div>
                <div id=literal></div>
            </body>"
        ),
    )
}

#[test]
fn nested_foreground_mix_updates_inherited_computed_color() {
    let mut runtime = ScriptRuntime::new(&foreground_page(NESTED_MIX)).unwrap();
    runtime
        .eval(
            r#"
            var mix = getComputedStyle(document.getElementById("mix"));
            var child = getComputedStyle(document.getElementById("child"));
            var literal = document.getElementById("literal");
            var control = getComputedStyle(literal);
            "#,
        )
        .unwrap();

    let initial = runtime.eval("control.color").unwrap();
    assert_ne!(
        initial,
        runtime
            .eval("getComputedStyle(document.body).color")
            .unwrap(),
        "the literal control must differ from plain inherited blue"
    );
    assert_eq!(runtime.eval("mix.color").unwrap(), initial, "on blue");
    assert_eq!(
        runtime.eval("child.color").unwrap(),
        initial,
        "child inherits the resolved mix on blue"
    );

    runtime
        .eval(&format!(
            "document.body.style.color = 'red'; literal.style.color = '{RED_RESULT}';"
        ))
        .unwrap();
    let updated = runtime.eval("control.color").unwrap();
    assert_ne!(initial, updated, "the literal control must change");
    assert_eq!(runtime.eval("mix.color").unwrap(), updated, "on red");
    assert_eq!(
        runtime.eval("child.color").unwrap(),
        updated,
        "the same live computed-style object sees the updated inherited mix"
    );
}

#[test]
fn nested_foreground_mix_paints_like_its_literal() {
    const WIDTH: u32 = 192;
    const HEIGHT: u32 = 64;
    let mut fonts = Fonts::new().unwrap();
    let mut render = |foreground| {
        let page = foreground_page(foreground);
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
        assert!(!layout.limited && !canvas.limited);
        canvas.pixels
    };
    let literal = render(BLUE_RESULT);
    assert!(
        literal.iter().any(|&pixel| pixel != literal[0]),
        "the reference must paint visible text"
    );
    let mixed = render(NESTED_MIX);
    assert!(mixed == literal, "the mix must paint the same text color");
}

fn compute_foreground(value: &str) -> Color {
    let page = LoadedPage::from_html(
        "https://css-color.test/",
        &format!("<div style='color: #123456; color: {value}'></div>"),
    );
    let id = page.document.elements("div").next().unwrap();
    compute(&page.document, id, &Style::default(), &[]).color
}

#[test]
fn foreground_mix_rejects_bad_weights_without_replacing_the_fallback() {
    for invalid in [
        "color-mix(in srgb, , blue)",
        "color-mix(in srgb, red, )",
        "color-mix(in srgb, 50%, blue)",
        "color-mix(in srgb, red %, blue)",
        "color-mix(in srgb, red 0%, 0% blue)",
        "color-mix(in srgb, red 101%, blue)",
        "color-mix(in srgb, red -1%, blue)",
        "color-mix(in srgb, red 1e999%, blue)",
        "color-mix(in srgb, red 1.%, blue)",
        "color-mix(in srgb, red 50px, blue)",
        "color-mix(in srgb, 25% red 25%, blue)",
        "color-mix(in srgb, red, blue, white)",
        "color-mix(in srgb, red, color-mix(in srgb, white 0%, blue 0%))",
        "color-mix(in srgb, rgb(0 0 0)extra, blue)",
        "color-mix(in oklab, red, blue)",
        "color-mix(in srgb longer hue, red, blue)",
    ] {
        assert!(!supports_declaration("color", invalid), "{invalid}");
        assert_eq!(
            compute_foreground(invalid),
            Color(18, 52, 86, 255),
            "{invalid}"
        );
    }
    assert!(!supports_declaration("background-color", NESTED_MIX));
}

#[test]
fn foreground_mix_normalizes_percentages_and_preserves_premultiplied_alpha() {
    for (value, expected) in [
        ("color-mix(in SrGb, 25% red, blue)", Color(64, 0, 191, 255)),
        ("color-mix(in srgb, red, 25% blue)", Color(191, 0, 64, 255)),
        ("color-mix(in srgb, red 0%, blue)", Color(0, 0, 255, 255)),
        (
            "color-mix(in srgb, red 80%, blue 80%)",
            Color(128, 0, 128, 255),
        ),
        (
            "color-mix(in srgb, red 20%, blue 20%)",
            Color(128, 0, 128, 102),
        ),
        (
            "color-mix(in srgb, red, transparent)",
            Color(255, 0, 0, 128),
        ),
        (
            "color-mix(in srgb, rgba(255, 0, 0, .5), blue)",
            Color(85, 0, 170, 191),
        ),
        (
            "color-mix(in srgb, color-mix(in srgb, red 25%, blue 25%), white)",
            Color(213, 170, 213, 191),
        ),
        (
            "color-mix(in srgb, color(srgb .5 .5 .5 / 0), transparent)",
            Color(0, 0, 0, 0),
        ),
        (
            "color-mix(in srgb, color(srgb .5 .5 .5), white)",
            Color(191, 191, 191, 255),
        ),
    ] {
        assert!(supports_declaration("color", value), "{value}");
        assert_eq!(compute_foreground(value), expected, "{value}");
    }
}

#[test]
fn foreground_mix_has_depth_node_and_input_budgets() {
    fn chain(depth: usize) -> String {
        (0..depth).fold("red".into(), |inner, _| {
            format!("color-mix(in srgb, {inner}, blue)")
        })
    }
    fn tree(depth: usize) -> String {
        (0..depth).fold("red".into(), |inner, _| {
            format!("color-mix(in srgb, {inner}, {inner})")
        })
    }
    assert!(supports_declaration("color", &chain(16)));
    assert!(!supports_declaration("color", &chain(17)));
    assert!(supports_declaration("color", &tree(5)));
    assert!(!supports_declaration("color", &tree(6)));
    let large = format!("color-mix(in{}srgb, red, blue)", " ".repeat(16 * 1024));
    assert!(!supports_declaration("color", &large));
}

#[test]
fn foreground_mix_preserves_extended_srgb_until_final_mapping() {
    for (value, expected) in [
        (
            "color-mix(in srgb, color(srgb 1.2 0 0), black)",
            Color(153, 0, 0, 255),
        ),
        (
            "color-mix(in srgb, color(srgb -.4 .2 0), color(srgb .8 0 0))",
            Color(51, 26, 0, 255),
        ),
        (
            "color-mix(in srgb, color-mix(in srgb, color(srgb 1.4 0 0), red), black)",
            Color(153, 0, 0, 255),
        ),
        (
            concat!(
                "color-mix(in srgb, color(srgb 1.7976931348623157e308 .4 .8), ",
                "color(srgb -1.7976931348623157e308 .6 0))",
            ),
            Color(0, 128, 102, 255),
        ),
    ] {
        assert_eq!(compute_foreground(value), expected, "{value}");
    }
    let literal = "color(srgb -.2 .4 .7)";
    assert_eq!(
        compute_foreground(&format!("color-mix(in srgb, {literal} 100%, black)")),
        compute_foreground(literal),
        "an out-of-gamut result uses the same final mapper as an absolute color"
    );
}

#[test]
fn foreground_mix_borrows_missing_components_before_premultiplication() {
    for (value, expected) in [
        (
            "color-mix(in srgb, rgb(0 255 0 / .5), rgb(0 255 0 / none))",
            Color(0, 255, 0, 128),
        ),
        (
            "color-mix(in srgb, color(srgb none .4 0 / .25), rgb(204 0 255 / .75))",
            Color(204, 26, 191, 128),
        ),
        (
            concat!(
                "color-mix(in srgb, ",
                "color-mix(in srgb, color(srgb none .2 0 / none), rgb(none 102 0 / none)), ",
                "color(srgb .8 .7 .5 / .4))",
            ),
            Color(204, 128, 64, 102),
        ),
        (
            "color-mix(in srgb, rgb(none 204 0), color(srgb none .4 .2))",
            Color(0, 153, 26, 255),
        ),
        (
            "color-mix(in srgb, rgb(255 0 0 / none), rgb(0 0 255 / none))",
            Color(0, 0, 0, 0),
        ),
    ] {
        assert!(supports_declaration("color", value), "{value}");
        assert_eq!(compute_foreground(value), expected, "{value}");
    }
}

#[test]
fn foreground_mix_rejects_nonfinite_channels_and_invalid_none_syntax() {
    for invalid in [
        "color-mix(in srgb, color(srgb 1e999 0 0), black)",
        "color-mix(in srgb, color(srgb none% 0 0), black)",
        "color-mix(in srgb, color(srgb none 0 / .5), black)",
        "color-mix(in srgb, color(srgb none, 0, 0), black)",
        "color-mix(in srgb, rgb(none, 0, 0), black)",
        "color-mix(in srgb, rgba(0, 0, 0, none), black)",
    ] {
        assert!(!supports_declaration("color", invalid), "{invalid}");
        assert_eq!(
            compute_foreground(invalid),
            Color(18, 52, 86, 255),
            "{invalid}"
        );
    }
}
