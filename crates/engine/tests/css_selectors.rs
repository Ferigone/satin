use rust_browser_engine::{
    LoadedPage,
    css::{self, Color, Style, Stylesheet},
    dom::NodeId,
    html,
    layout::Viewport,
    script::ScriptRuntime,
};

#[test]
fn nth_selectors_preserve_anb_token_boundaries() {
    let doc = html::parse("<main><p></p><p id='target'></p><p></p></main>");
    let target = (0..doc.nodes.len())
        .map(NodeId)
        .find(|&id| doc.attr(id, "id") == Some("target"))
        .unwrap();
    for anb in [
        "2",
        "+2",
        "EVEN",
        "2N",
        "+n + 2",
        "2n/**/+0",
        "n- 3",
        "n /**/ -/**/ 3",
        "n-3",
        "n-/**/3",
        "-n + 2",
        "0n + 2",
        r"2\6e",
        r"n\-3",
        r"\65 ven",
    ] {
        let rules = css::parse(&format!("p:nth-child({anb}) {{ color: green }}"), 800.);
        assert_eq!(rules.len(), 1, "{anb:?}");
        let sheet = Stylesheet::new(rules, 800.);
        assert_eq!(
            sheet.compute(&doc, target, &Style::default(), 16.).color,
            Color::rgb(0, 128, 0),
            "{anb:?}",
        );
    }
    for anb in [
        "1 2",
        "n 2",
        "2 n + 1",
        "+ n",
        "- n",
        "+ 2",
        "o dd",
        "2/**/n",
        "n/**/2",
        "1/**/2",
        "+/**/2",
        "n + -1",
        "2n--1",
        "2n- +1",
        "2.0n",
        "2e0n",
        "1.0",
        "n+2/**/3",
        "n+\u{000b}2",
        "n-1 + 2",
    ] {
        assert!(
            css::parse(
                &format!("#target, p:nth-child({anb}) {{ color: red }}"),
                800.
            )
            .is_empty(),
            "{anb:?}: one invalid selector must invalidate the entire rule",
        );
    }
}

#[test]
fn stylesheet_nth_anb_admission_and_live_invalidation() {
    let mut page = LoadedPage::from_html(
        "https://example.test/css-nth-anb.html",
        include_str!("../../../tools/fixtures/css-nth-anb.html"),
    );
    let mut runtime = ScriptRuntime::for_page_with_viewport(&mut page, Viewport::default())
        .unwrap()
        .unwrap();
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
    for (attr, expected) in [
        (
            "data-syntax",
            "rgb(0, 0, 0);rgb(0, 0, 0);rgb(0, 0, 0);0px;0px;0px;rgb(0, 128, 0);rgb(0, 128, 0);rgba(0, 0, 0, 0)",
        ),
        ("data-live", "60px;120px;60px;120px"),
    ] {
        let actual = runtime
            .eval(&format!(
                "document.getElementById('result').getAttribute('{attr}')"
            ))
            .unwrap();
        assert_eq!(
            actual.as_string().unwrap().to_string_lossy(),
            expected,
            "{attr}"
        );
    }
}
