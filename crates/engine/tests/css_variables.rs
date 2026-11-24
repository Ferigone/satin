use rust_browser_engine::{
    css::{Color, Style, Stylesheet},
    dom::{Document, NodeId},
    html,
};

fn computed_style(sheet: &Stylesheet, doc: &Document, node: NodeId) -> Style {
    let parent = doc.nodes[node.0]
        .parent
        .map(|parent| computed_style(sheet, doc, parent))
        .unwrap_or_default();
    sheet.compute(doc, node, &parent, 16.)
}

fn assert_colors(fixture: &str, expected: &[(&str, Color)]) {
    let doc = html::parse(fixture);
    let sheet = Stylesheet::new(vec![], 800.);
    for &(id, color) in expected {
        let node = doc
            .connected_nodes()
            .find(|&node| doc.attr(node, "id") == Some(id))
            .expect(id);
        assert_eq!(computed_style(&sheet, &doc, node).color, color, "#{id}");
    }
}

#[test]
fn var_function_name_is_ascii_case_insensitive() {
    let green = Color::rgb(0, 128, 0);
    let purple = Color::rgb(128, 0, 128);
    assert_colors(
        include_str!("fixtures/css-case-insensitive-var.html"),
        &[
            ("nested", green),
            ("cycle", green),
            ("sensitive", green),
            ("inherited", green),
            ("invalid", purple),
            ("other-function", purple),
            ("empty", green),
        ],
    );
}

#[test]
fn custom_property_css_wide_keywords_are_ascii_case_insensitive() {
    let green = Color::rgb(0, 128, 0);
    let purple = Color::rgb(128, 0, 128);
    let red = Color::rgb(255, 0, 0);
    assert_colors(
        include_str!("fixtures/css-custom-wide-keywords.html"),
        &[
            ("initial", green),
            ("inherit", red),
            ("unset", red),
            ("absent", green),
            ("quoted", purple),
            ("multiple-tokens", purple),
        ],
    );
}
