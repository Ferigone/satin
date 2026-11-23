use rust_browser_engine::{
    LoadedPage,
    css::{self, Color, Style, Stylesheet},
    dom::{Document, Namespace, NodeId},
    html,
    layout::Viewport,
    script::ScriptRuntime,
};

fn by_id(doc: &Document, id: &str) -> NodeId {
    (0..doc.nodes.len())
        .map(NodeId)
        .find(|&node| doc.attr(node, "id") == Some(id))
        .unwrap()
}

#[test]
fn nth_selectors_preserve_anb_token_boundaries() {
    let doc = html::parse("<main><p></p><p id='target'></p><p></p></main>");
    let target = by_id(&doc, "target");
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
    stylesheet_fixture(
        include_str!("../../../tools/fixtures/css-nth-anb.html"),
        &[
            (
                "data-syntax",
                "rgb(0, 0, 0);rgb(0, 0, 0);rgb(0, 0, 0);0px;0px;0px;rgb(0, 128, 0);rgb(0, 128, 0);rgba(0, 0, 0, 0)",
            ),
            ("data-live", "60px;120px;60px;120px"),
        ],
    );
}

#[test]
fn nth_of_type_counts_same_expanded_type_in_both_directions() {
    let mut doc = html::parse(
        "<main><a id=first></a><a id=foreign></a><a id=second></a><span></span></main>",
    );
    let foreign = by_id(&doc, "foreign");
    doc.namespaces.insert(foreign, Namespace::Svg);
    let rules = css::parse(
        "a:nth-of-type(2) { color: green } a:nth-last-of-type(2) { background-color: blue }",
        800.,
    );
    let sheet = Stylesheet::new(rules, 800.);
    let first = sheet.compute(&doc, by_id(&doc, "first"), &Style::default(), 16.);
    let second = sheet.compute(&doc, by_id(&doc, "second"), &Style::default(), 16.);
    assert_ne!(first.color, Color::rgb(0, 128, 0));
    assert_eq!(first.background, Some(Color::rgb(0, 0, 255)));
    assert_eq!(second.color, Color::rgb(0, 128, 0));
    assert_eq!(second.background, None);
    let foreign = sheet.compute(&doc, foreign, &Style::default(), 16.);
    assert_ne!(foreign.color, Color::rgb(0, 128, 0));
    assert_eq!(foreign.background, None);
}

#[test]
fn stylesheet_of_type_ignores_other_namespaces_after_live_mutation() {
    stylesheet_fixture(
        include_str!("../../../tools/fixtures/css-nth-expanded-type.html"),
        &[
            (
                "data-types",
                "8px;9px;rgb(0, 128, 0)|8px;9px;rgb(0, 128, 0)|8px;9px;rgb(0, 128, 0)|8px;9px;rgb(0, 128, 0)",
            ),
            (
                "data-live",
                "120px;40px;rgb(0, 128, 0)|60px;40px;rgb(0, 0, 0)|120px;20px;rgb(0, 0, 0)|120px;40px;rgb(0, 128, 0)",
            ),
        ],
    );
}

#[test]
fn stylesheet_forgiving_functional_lists_remain_valid_when_empty() {
    stylesheet_fixture(
        include_str!("../../../tools/fixtures/css-functional-selector-lists.html"),
        &[
            (
                "data-control",
                "120px;40px;rgb(0, 0, 0);16px;8px|60px;20px;rgb(0, 0, 0);16px;0px",
            ),
            (
                "data-empty",
                "9px;10px;11px;12px;13px;14px|9px;10px;0px;0px;13px;14px",
            ),
        ],
    );
}

fn stylesheet_fixture(fixture: &str, expected_attributes: &[(&str, &str)]) {
    let mut page = LoadedPage::from_html("https://example.test/", fixture);
    let mut runtime = ScriptRuntime::for_page_with_viewport(&mut page, Viewport::default())
        .unwrap()
        .unwrap();
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
    for &(attr, expected) in expected_attributes {
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
