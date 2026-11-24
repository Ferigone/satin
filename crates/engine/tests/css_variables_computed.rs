use rust_browser_engine::{
    css::{Color, Style, Stylesheet},
    html,
};

#[test]
fn custom_property_substituting_to_initial_is_guaranteed_invalid() {
    let doc = html::parse(include_str!("fixtures/css-substituted-custom-initial.html"));
    let sheet = Stylesheet::new(vec![], 800.);
    let initial = Style::default();
    let mut computed = vec![initial.clone(); doc.nodes.len()];
    for node in doc.connected_nodes() {
        let parent = doc.nodes[node.0]
            .parent
            .map_or(&initial, |parent| &computed[parent.0]);
        let style = sheet.compute(&doc, node, parent, 16.);
        computed[node.0] = style;
    }

    let green = Color::rgb(0, 128, 0);
    let purple = Color::rgb(128, 0, 128);
    for (id, color, invalid) in [
        ("direct", green, Some("--paint")),
        ("nested", green, Some("--paint")),
        ("inherited-invalid", green, Some("--source")),
        ("inherited-resolved", Color::rgb(0, 0, 255), None),
        ("ordinary", Color::rgb(0, 0, 0), None),
        ("quoted", purple, None),
        ("multiple-tokens", purple, None),
    ] {
        let node = doc
            .connected_nodes()
            .find(|&node| doc.attr(node, "id") == Some(id))
            .expect(id);
        let style = &computed[node.0];
        assert_eq!(style.color, color, "#{id}");
        if let Some(name) = invalid {
            assert_eq!(style.custom_property(name), None, "#{id}");
        }
    }
}
