use rust_browser_engine::{LoadedPage, layout::Viewport, script::ScriptRuntime};
use rust_browser_javascript::Value;

#[test]
fn clamped_title_reflows_on_style_and_viewport_changes() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"
        <style>
            body { margin: 0 }
            #title { font: 16px/24px monospace; }
            .preview { display: -webkit-box; -webkit-box-orient: vertical;
                       -webkit-line-clamp: var(--lines, 1); overflow: hidden }
        </style>
        <div id="title" class="preview">First result with several words and an <a href="/">inline link</a></div>
        <div id="next">Next result</div>
    "#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.set_viewport(Viewport {
        width: 150.,
        height: 200.,
        ..Default::default()
    });
    let observed = runtime
        .eval(
            r#"
        const t = document.getElementById('title');
        const cs = getComputedStyle(t);
        function height() { return t.getBoundingClientRect().height; }
        const first = height();
        t.style.setProperty('--lines', '2');
        const second = height();
        t.className = '';
        const expanded = height();
        t.className = 'preview';
        first === 24 && second === 48 && expanded > second &&
            cs.getPropertyValue('-webkit-line-clamp') === '2' &&
            document.getElementById('next').getBoundingClientRect().top === 48
    "#,
        )
        .unwrap();
    assert_eq!(observed, Value::Bool(true));
    runtime.set_viewport(Viewport {
        width: 800.,
        ..runtime.viewport()
    });
    assert_eq!(runtime.eval("height() === 24").unwrap(), Value::Bool(true));
}

#[test]
fn clamped_lines_paint_the_authored_ellipsis_and_keep_positioned_children() {
    use rust_browser_engine::{Fonts, Layout, paint::Canvas};

    let mut fonts = Fonts::new().unwrap();
    let mut render = |clamp: bool, content: &str| {
        let clamp = if clamp { "-webkit-line-clamp:2" } else { "" };
        let source = format!(
            r#"<style>body{{margin:0}}
            div{{display:-webkit-box;-webkit-box-orient:vertical;{clamp};
              white-space:pre;position:relative;background:yellow;font:16px/24px serif}}
            i{{position:absolute;right:2px;top:2px;width:8px;height:8px;background:blue}}
            </style><div>{content}</div>"#
        );
        let page = LoadedPage::from_html("https://example.test/", &source);
        let layout = Layout::build(&page, &fonts, 200.);
        let mut canvas = Canvas::new(200, 120, layout.background);
        canvas.document(&layout, &mut fonts, 0., 0.);
        assert!(!layout.limited && !canvas.limited);
        canvas.pixels
    };
    let expected = render(false, "First\nLast…<i></i>");
    assert!(expected.contains(&0x0000ff));
    assert_ne!(expected, render(false, "First\nLast<i></i>"));
    assert_eq!(expected, render(true, "First\nLast\nHidden<i></i>"));
    assert_eq!(
        render(false, "First\nLast"),
        render(true, "First\nLast<br>")
    );
}
