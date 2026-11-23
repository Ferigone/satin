use rust_browser_engine::{
    dom::{Document, NodeId, NodeKind, QuirksMode},
    html::{self, FragmentContext},
};

fn assert_arena(document: &Document) {
    let mut inbound = vec![0_u8; document.nodes.len()];
    for (index, node) in document.nodes.iter().enumerate() {
        for &child in &node.children {
            assert_eq!(document.nodes[child.0].parent, Some(NodeId(index)));
            inbound[child.0] += 1;
            assert_eq!(inbound[child.0], 1);
        }
    }
    for (index, node) in document.nodes.iter().enumerate() {
        assert_eq!(node.parent.is_some(), inbound[index] == 1);
    }
    for (&template, &content) in &document.template_contents {
        assert_eq!(document.tag(template), "template");
        assert!(document.nodes[template.0].children.is_empty());
        assert!(document.nodes[content.0].parent.is_none());
        assert!(matches!(document.nodes[content.0].kind, NodeKind::Document));
        assert_ne!(content, document.root);
    }
    // Follow ordinary edges plus template ownership edges: detached contents
    // must not form a cycle through their host template or another fragment.
    let mut pending = vec![document.root];
    let mut seen = vec![false; document.nodes.len()];
    while let Some(id) = pending.pop() {
        assert!(!seen[id.0], "multiply owned node or template cycle");
        seen[id.0] = true;
        pending.extend(document.nodes[id.0].children.iter().copied());
        pending.extend(document.template_contents.get(&id).copied());
    }
}

#[test]
fn fragment_has_explicit_roots_and_stable_arena_links() {
    let fragment = html::parse_fragment("text<!-- c --><p>A<p>B<b><i>C</b>D</i>", "div", true);
    assert!(!fragment.truncated);
    assert_eq!(
        fragment.serialize(),
        "text<!-- c --><p>A</p><p>B<b><i>C</i></b><i>D</i></p>"
    );
    assert_eq!(fragment.serialize_node(fragment.root), fragment.serialize());
    assert_eq!(fragment.inner_html(fragment.root), fragment.serialize());
    assert_eq!(fragment.elements("html").count(), 0);
    assert_eq!(fragment.elements("head").count(), 0);
    assert_eq!(fragment.elements("body").count(), 0);
    assert_eq!(fragment.nodes[fragment.root.0].children.len(), 4);
    assert_arena(&fragment);
}

#[test]
fn context_controls_table_modes_and_foster_parenting() {
    for (context, input, expected) in [
        (
            "table",
            "<tr><td>A<td>B",
            "<tbody><tr><td>A</td><td>B</td></tr></tbody>",
        ),
        ("tbody", "<tr><td>A", "<tr><td>A</td></tr>"),
        ("tr", "<td>A<th>B", "<td>A</td><th>B</th>"),
        ("td", "<p>A<p>B", "<p>A</p><p>B</p>"),
        (
            "table",
            "x<tr><td>A</td></tr>y",
            "x<tbody><tr><td>A</td></tr></tbody>y",
        ),
        (
            "table",
            "<table><tr><td>A",
            "<tbody><tr><td>A</td></tr></tbody>",
        ),
        ("colgroup", "<col><div>A</div><col>", "<col><col>"),
        ("template", "<tr><td>A", "<tr><td>A</td></tr>"),
    ] {
        let document = html::parse_fragment(input, context, false);
        assert_eq!(document.serialize(), expected, "context={context}");
        assert_arena(&document);
    }
}

#[test]
fn raw_fragment_context_has_no_appropriate_end_tag_or_initial_lf_skip() {
    for context in ["title", "textarea"] {
        let source = format!("\n&amp;</{context}><b>\0");
        let document = html::parse_fragment(&source, context, false);
        assert_eq!(document.text(document.root), format!("\n&</{context}><b>�"));
        assert_eq!(document.nodes[document.root.0].children.len(), 1);
    }
    for context in [
        "style",
        "xmp",
        "iframe",
        "noembed",
        "noframes",
        "script",
        "plaintext",
    ] {
        let source = format!("&amp;</{context}><b>\0\r\nx");
        let document = html::parse_fragment(&source, context, true);
        assert_eq!(
            document.text(document.root),
            format!("&amp;</{context}><b>�\nx")
        );
        assert_eq!(document.nodes[document.root.0].children.len(), 1);
        assert_arena(&document);
    }
    let enabled = html::parse_fragment("<b>&amp;</noscript>", "noscript", true);
    assert_eq!(enabled.text(enabled.root), "<b>&amp;</noscript>");
    let disabled = html::parse_fragment("<b>&amp;</noscript>", "noscript", false);
    assert_eq!(disabled.serialize(), "<b>&amp;</b>");
}

#[test]
fn context_inherits_owner_quirks_and_nearest_form() {
    let mut context = FragmentContext::new("div", true);
    context.form_ancestor = true;
    let document = html::parse_fragment_with_context(
        "<form id=ignored><input></form><form id=accepted><input>",
        context,
    );
    let forms: Vec<_> = document.elements("form").collect();
    assert_eq!(forms.len(), 1);
    assert_eq!(document.attr(forms[0], "id"), Some("accepted"));
    assert_eq!(
        html::parse_fragment("<form><input>", "form", false)
            .elements("form")
            .count(),
        0
    );
    let source = "<!doctype html><p>A<table><tr><td>B";
    let standards = html::parse_fragment_with_context(source, context);
    assert!(standards.serialize().starts_with("<p>A</p><table>"));
    context.quirks_mode = QuirksMode::Quirks;
    let quirks = html::parse_fragment_with_context(source, context);
    assert_eq!(quirks.quirks_mode, QuirksMode::Quirks);
    assert!(quirks.serialize().starts_with("<p>A<table>"));
    assert_arena(&quirks);
}

#[test]
fn select_fragment_uses_current_body_rules_with_context_exceptions() {
    let document = html::parse_fragment(
        "<input><select><option>A<option>B<hr><b>C</b><textarea><option>",
        "select",
        true,
    );
    assert_eq!(
        document.serialize(),
        "<option>A</option><option>B<hr><b>C</b><textarea>&lt;option&gt;</textarea></option>"
    );
    assert_eq!(document.elements("input").count(), 0);
    assert_eq!(document.elements("select").count(), 0);
    assert_arena(&document);
}

#[test]
fn template_contents_are_detached_inert_and_serialized() {
    let document = html::parse_with_scripting(
        "<!doctype html><body>visible<template id=outer><b>hidden</b><template><i>nested</i></template><script>unexecuted()</script></template>after",
        true,
    );
    let template = document.elements("template").next().unwrap();
    let content = document.template_contents[&template];
    assert_eq!(document.text(template), "");
    assert_eq!(document.text(document.root), "visibleafter");
    assert_eq!(document.text(content), "hiddenunexecuted()");
    assert_eq!(document.elements("i").count(), 0);
    assert_eq!(document.elements("script").count(), 0);
    assert_eq!(document.elements("template").count(), 1);
    assert!(!document.connected_nodes().any(|id| id == content));
    assert_eq!(
        document.inner_html(template),
        "<b>hidden</b><template><i>nested</i></template><script>unexecuted()</script>"
    );
    assert_eq!(document.inner_html(content), document.inner_html(template));
    assert_eq!(
        document.serialize_node(template),
        format!(
            "<template id=\"outer\">{}</template>",
            document.inner_html(template)
        )
    );
    assert_eq!(document.template_contents.len(), 2);
    assert_arena(&document);
}

#[test]
fn html_serialization_escapes_attributes_and_uses_doctype_name() {
    let document = html::parse(
        "<!doctype html PUBLIC 'legacy' 'system'><p title='&quot;&amp;&lt;&gt;&nbsp;'>A&amp;&lt;&gt;&nbsp;</p>",
    );
    let p = document.elements("p").next().unwrap();
    assert_eq!(
        document.serialize_node(p),
        "<p title=\"&quot;&amp;&lt;&gt;&nbsp;\">A&amp;&lt;&gt;&nbsp;</p>"
    );
    assert!(document.serialize().starts_with("<!DOCTYPE html><html>"));
    assert_eq!(document.inner_html(p), "A&amp;&lt;&gt;&nbsp;");
}

#[test]
fn fragment_templates_table_redirection_and_eof_recovery() {
    let document = html::parse_fragment(
        "<template><table>foster<tr><td>A<template><col><col></template>B",
        "div",
        true,
    );
    assert_eq!(
        document.serialize(),
        "<template>foster<table><tbody><tr><td>A<template><col><col></template>B</td></tr></tbody></table></template>"
    );
    assert_eq!(document.text(document.root), "");
    assert_arena(&document);
    let document = html::parse_fragment(
        "<template><form><form>X</form>Y</form></template><form>Z",
        "template",
        false,
    );
    assert_eq!(
        document.serialize(),
        "<template><form><form>X</form>Y</form></template><form>Z</form>"
    );
    assert_arena(&document);
}

#[test]
fn fragment_and_template_budgets_leave_complete_metadata() {
    let deep = html::parse_fragment(&"<template>".repeat(2000), "template", false);
    assert!(deep.truncated);
    assert_arena(&deep);
    assert!(deep.nodes.len() < 1000);
    let _ = deep.serialize();
    let broad = html::parse_fragment(
        &"<template></template>".repeat(html::MAX_NODES),
        "div",
        false,
    );
    assert!(broad.truncated);
    assert!(broad.nodes.len() <= html::MAX_NODES);
    assert_arena(&broad);
    let source = "é".repeat(html::MAX_INPUT_BYTES / 2 + 2);
    let unicode = html::parse_fragment(&source, "textarea", false);
    assert!(unicode.truncated);
    assert!(unicode.text(unicode.root).ends_with('é'));
    assert_arena(&unicode);
}

#[test]
fn malformed_fragment_contexts_preserve_template_ownership() {
    let contexts = [
        "div", "body", "html", "head", "table", "tbody", "tr", "td", "caption", "colgroup",
        "template", "select", "form", "textarea", "noscript", "script", "frameset",
    ];
    let tokens = [
        "<p>",
        "</p>",
        "<b>",
        "</b>",
        "<i>",
        "</i>",
        "<table>",
        "</table>",
        "<tbody>",
        "</tbody>",
        "<tr>",
        "</tr>",
        "<td>",
        "</td>",
        "<caption>",
        "</caption>",
        "<template>",
        "</template>",
        "<form>",
        "</form>",
        "<select>",
        "</select>",
        "<option>",
        "<col>",
        "<div>",
        "</div>",
        "x",
        " \n",
        "</>",
        "<!--c-->",
        "&amp;é",
    ];
    let mut state = 0x9e37_79b9_u64;
    for context in contexts {
        for case in 0..64 {
            let mut source = String::new();
            for _ in 0..96 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                source.push_str(tokens[state as usize % tokens.len()]);
            }
            let document = html::parse_fragment(&source, context, case % 2 == 0);
            assert!(
                !document.truncated,
                "unexpected abort context={context}: {source:?}"
            );
            assert_arena(&document);
            let _ = document.serialize();
        }
    }
}
