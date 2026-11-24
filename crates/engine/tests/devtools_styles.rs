use rust_browser_engine::{LoadedPage, dom::NodeId, script::ScriptRuntime};

fn evaluate(runtime: &mut ScriptRuntime, source: &str) -> String {
    runtime
        .eval(source)
        .unwrap()
        .as_string()
        .unwrap()
        .to_string_lossy()
}

#[test]
fn live_inline_edits_follow_cssom_mutations_and_reveal_stylesheet_on_removal() {
    let mut page = LoadedPage::from_html(
        "https://devtools.test/",
        "<style>#item { width: 45px }</style><div id='item' style='color: red'></div>",
    );
    let node = page.document.elements("div").next().unwrap();
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.eval("const item=document.getElementById('item'); let changes=0; new MutationObserver(()=>changes++).observe(item,{attributes:true,attributeFilter:['style']});").unwrap();
    runtime
        .set_inspected_style(node, "WIDTH", "73px", true)
        .unwrap();
    runtime.pump();
    assert_eq!(
        evaluate(
            &mut runtime,
            "[getComputedStyle(item).width,item.style.getPropertyPriority('width'),item.style.color,changes].join('/')"
        ),
        "73px/important/red/1"
    );
    assert!(runtime.snapshot(&mut page));
    assert!(
        page.document
            .attr(node, "style")
            .unwrap()
            .contains("width:73px!important")
    );
    assert!(
        runtime
            .set_inspected_style(node, "width", "71px;color:blue", false)
            .is_err()
    );
    assert!(
        runtime
            .set_inspected_style(node, "bad name", "3", false)
            .is_err()
    );
    assert!(
        runtime
            .set_inspected_style(node, "width", "arbitrary()", false)
            .is_err()
    );
    assert_eq!(
        evaluate(&mut runtime, "getComputedStyle(item).width"),
        "73px"
    );
    runtime
        .set_inspected_style(node, "width", "", false)
        .unwrap();
    assert_eq!(
        evaluate(&mut runtime, "getComputedStyle(item).width"),
        "45px"
    );
    runtime.eval("item.parentNode.removeChild(item)").unwrap();
    assert!(
        runtime
            .set_inspected_style(node, "color", "blue", false)
            .is_err()
    );
    assert!(
        runtime
            .set_inspected_style(NodeId(usize::MAX), "color", "red", false)
            .is_err()
    );
}

#[test]
fn page_overrides_cannot_intercept_native_declared_style_edits() {
    let page = LoadedPage::from_html("https://devtools.test/", "<div id='item'></div>");
    let node = page.document.elements("div").next().unwrap();
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.eval("const item=document.getElementById('item'); let hooks=0; item.style.setProperty=()=>hooks++; item.setAttribute=()=>hooks++;").unwrap();
    runtime
        .set_inspected_style(node, "--swatch", "rgb(17,34,51)", false)
        .unwrap();
    runtime
        .set_inspected_style(node, "color", "var(--swatch)", false)
        .unwrap();
    assert_eq!(
        evaluate(
            &mut runtime,
            "[getComputedStyle(item).color,hooks].join('/')"
        ),
        "rgb(17, 34, 51)/0"
    );
}

#[test]
fn author_matches_follow_selector_and_media_gates_and_keep_source_order() {
    use rust_browser_engine::css::InspectorStylesheets;
    let page = LoadedPage::from_html(
        "https://devtools.test/",
        r#"<style>.missing {color:white} main > .box {color:red} main .box {padding:3px!important} @media (min-width: 700px) {.box {width:90px}}</style><style media="(max-width: 500px)">#item {display:grid}</style><main><div class=box id=item></div></main>"#,
    );
    let node = page.document.elements("div").next().unwrap();
    let mut sheets = InspectorStylesheets::default();
    assert!(sheets.refresh(&page, 800.));
    assert!(!sheets.refresh(&page, 800.));
    let (rules, limited) = sheets.matching(&page.document, node);
    assert!(!limited);
    assert_eq!(
        rules
            .iter()
            .map(|(sheet, r)| (*sheet, r.selector.as_str()))
            .collect::<Vec<_>>(),
        [(0, "main > .box"), (0, "main .box"), (0, ".box")]
    );
    assert!(rules[1].1.declarations[0].important);
    assert!(sheets.refresh(&page, 400.));
    let (rules, limited) = sheets.matching(&page.document, node);
    assert!(!limited);
    assert_eq!(
        rules
            .iter()
            .map(|(sheet, r)| (*sheet, r.selector.as_str()))
            .collect::<Vec<_>>(),
        [(0, "main > .box"), (0, "main .box"), (1, "#item")]
    );
}

#[test]
fn rule_inspector_discloses_limited_matches() {
    use rust_browser_engine::css::InspectorStylesheets;
    let style = ".match{width:1px}".repeat(70);
    let page = LoadedPage::from_html(
        "https://devtools.test/",
        &format!("<style>{style}</style><div class=match></div>"),
    );
    let mut sheets = InspectorStylesheets::default();
    sheets.refresh(&page, 800.);
    let node = page.document.elements("div").next().unwrap();
    let (rules, partial) = sheets.matching(&page.document, node);
    assert_eq!(rules.len(), 64);
    assert!(partial);
}
