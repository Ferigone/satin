use rust_browser_engine::{
    Fonts, Layout, LoadedPage, layout::Viewport, paint::Canvas, script::ScriptRuntime,
};
use rust_browser_javascript::Value;

#[test]
fn layer_order_survives_sheet_boundaries_and_reverses_for_important_values() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"
        <style>@layer theme, components; body { margin: 0 }</style>
        <style>
            @layer components {
                .card { width: var(--size); height: 24px; background: blue !important; --size: 80px }
                @media (max-width: 400px) { .card { --size: 40px } }
            }
            @layer theme {
                #card { width: 8px; --size: 8px; background: green !important }
            }
            #card { background: red !important }
        </style>
        <div class="card" id="card"></div>
    "#,
    );
    let mut fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 320.);
    let mut canvas = Canvas::new(320, 64, layout.background);
    canvas.document(&layout, &mut fonts, 0., 0.);
    assert!(!layout.limited && !canvas.limited);
    assert_eq!(canvas.pixels[10 * 320 + 39], 0x008000);
    assert_eq!(canvas.pixels[10 * 320 + 40], 0xffffff);

    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.set_viewport(Viewport {
        width: 320.,
        height: 400.,
        ..Default::default()
    });
    assert_eq!(
        runtime
            .eval(
                r#"
        const e = document.getElementById('card'), cs = getComputedStyle(e);
        function width() { return e.getBoundingClientRect().width; }
        const initial = width();
        e.className = '';
        const basic = width();
        e.className = 'card';
        e.style.setProperty('background-color', 'purple', 'important');
        initial === 40 && basic === 8 && width() === 40 && cs.backgroundColor === 'rgb(128, 0, 128)'
    "#
            )
            .unwrap(),
        Value::Bool(true)
    );
    runtime.set_viewport(Viewport {
        width: 800.,
        ..runtime.viewport()
    });
    assert_eq!(runtime.eval("width() === 80").unwrap(), Value::Bool(true));
}

#[test]
fn constructed_sheet_retains_layer_order_statements_and_valid_blocks() {
    let page = LoadedPage::from_html("https://example.test/", "");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(r#"
        const sheet = new CSSStyleSheet();
        sheet.replaceSync('@layer reset, theme; @layer theme { p { color: green } } @layer bad.2 {} @layer yes, -;');
        sheet.cssRules.length === 2
            && sheet.cssRules[0].cssText === '@layer reset, theme;'
            && sheet.cssRules[1].cssText.includes('color: green')
    "#).unwrap(), Value::Bool(true));
}
