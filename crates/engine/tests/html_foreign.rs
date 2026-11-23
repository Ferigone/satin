//! Focused invariants alongside the unchanged primary html5lib corpus. Expected
//! behavior follows WHATWG HTML 13.2.6.5 and the fragment parsing algorithm.
use rust_browser_engine::{
    dom::{AttributeNamespace, Document, Namespace, NodeId, NodeKind},
    html::{self, FragmentContext},
};
use std::collections::BTreeMap;

fn id(document: &Document, value: &str) -> NodeId {
    document
        .connected_nodes()
        .find(|&id| document.attr(id, "id") == Some(value))
        .unwrap()
}

fn assert_arena(document: &Document) {
    let mut inbound = vec![0; document.nodes.len()];
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
    for &id in document.namespaces.keys() {
        assert!(matches!(
            document.nodes[id.0].kind,
            NodeKind::Element { .. }
        ));
    }
    for (id, name) in document.attribute_namespaces.keys() {
        assert!(document.attr(*id, name).is_some());
        assert_ne!(document.namespace(*id), Namespace::Html);
    }
    let mut pending = vec![document.root];
    let mut seen = vec![false; document.nodes.len()];
    while let Some(id) = pending.pop() {
        assert!(!seen[id.0], "cycle or multiply owned template contents");
        seen[id.0] = true;
        pending.extend(document.nodes[id.0].children.iter().copied());
        pending.extend(document.template_contents.get(&id).copied());
    }
}

#[test]
fn foreign_names_attributes_and_namespace_apis() {
    let document = html::parse(
        "<svg id=s viewbox='0 0 2 2' xmlns='http://www.w3.org/2000/svg'><lineargradient id=g gradientunits=userSpaceOnUse xlink:href='#a' xml:lang=en xmlns:xlink='http://www.w3.org/1999/xlink' xlink:custom=x /><fegaussianblur stddeviation=2 /></svg><math id=m definitionurl=spec><mi>x</mi></math>",
    );
    let svg = id(&document, "s");
    let gradient = id(&document, "g");
    let math = id(&document, "m");
    assert_eq!(document.namespace(svg), Namespace::Svg);
    assert_eq!(
        document.namespace_uri(svg),
        Some("http://www.w3.org/2000/svg")
    );
    assert_eq!(document.local_name(gradient), "linearGradient");
    assert_eq!(document.attr(svg, "viewBox"), Some("0 0 2 2"));
    assert_eq!(
        document.attr(gradient, "gradientUnits"),
        Some("userSpaceOnUse")
    );
    assert_eq!(document.attr(math, "definitionURL"), Some("spec"));
    assert_eq!(document.namespace(math), Namespace::MathMl);
    assert_eq!(document.namespace_uri(document.root), None);
    for (node, name, namespace, local, prefix) in [
        (
            gradient,
            "xlink:href",
            AttributeNamespace::XLink,
            "href",
            Some("xlink"),
        ),
        (
            gradient,
            "xml:lang",
            AttributeNamespace::Xml,
            "lang",
            Some("xml"),
        ),
        (
            gradient,
            "xmlns:xlink",
            AttributeNamespace::Xmlns,
            "xlink",
            Some("xmlns"),
        ),
        (svg, "xmlns", AttributeNamespace::Xmlns, "xmlns", None),
    ] {
        assert_eq!(document.attribute_namespace(node, name), Some(namespace));
        assert_eq!(document.attribute_local_name(node, name), local);
        assert_eq!(document.attribute_prefix(node, name), prefix);
    }
    assert_eq!(document.attribute_namespace(gradient, "xlink:custom"), None);
    assert_eq!(
        document.attribute_local_name(gradient, "xlink:custom"),
        "xlink:custom"
    );
    assert_arena(&document);
}

#[test]
fn integration_points_select_html_rules_without_losing_foreign_ancestors() {
    let document = html::parse(
        "<svg><foreignObject><div id=d><svg><circle id=c /></svg></div></foreignObject><desc><p id=p>x</p></desc><title><b id=b>y</b></title></svg><math><mtext><span id=s>x</span><mglyph id=g /><malignmark id=a /></mtext><annotation-xml encoding=TeXt/HtMl><p id=h>x</p></annotation-xml><annotation-xml encoding=unknown><svg id=v><path/></svg></annotation-xml></math>",
    );
    for value in ["d", "p", "b", "s", "h"] {
        assert_eq!(document.namespace(id(&document, value)), Namespace::Html);
    }
    for value in ["c", "v"] {
        assert_eq!(document.namespace(id(&document, value)), Namespace::Svg);
    }
    for value in ["g", "a"] {
        assert_eq!(document.namespace(id(&document, value)), Namespace::MathMl);
    }
    assert_arena(&document);
}

#[test]
fn foreign_fragments_use_context_namespace_attributes_and_data_state() {
    let mut context = FragmentContext::new("script", true);
    context.namespace = Namespace::Svg;
    let document = html::parse_fragment_with_context("&amp;<path/><![CDATA[<x>&]]>", context);
    assert_eq!(document.serialize(), "&amp;<path></path>&lt;x&gt;&amp;");
    assert_eq!(
        document.namespace(document.elements("path").next().unwrap()),
        Namespace::Svg
    );
    assert_eq!(document.text(document.root), "&<x>&");

    context.tag = "title";
    let document = html::parse_fragment_with_context("<b>html</b><svg><path/></svg>", context);
    assert_eq!(
        document.namespace(document.elements("b").next().unwrap()),
        Namespace::Html
    );
    assert_eq!(
        document.namespace(document.elements("path").next().unwrap()),
        Namespace::Svg
    );

    context.tag = "mtext";
    context.namespace = Namespace::MathMl;
    let document = html::parse_fragment_with_context("<mglyph/><b>html</b>", context);
    assert_eq!(
        document.namespace(document.elements("mglyph").next().unwrap()),
        Namespace::MathMl
    );
    assert_eq!(
        document.namespace(document.elements("b").next().unwrap()),
        Namespace::Html
    );

    context.tag = "annotation-xml";
    let attrs = BTreeMap::from([("encoding".into(), "APPLICATION/XHTML+XML".into())]);
    context.attrs = Some(&attrs);
    let document = html::parse_fragment_with_context("<custom/><svg><g/></svg>", context);
    assert_eq!(
        document.namespace(document.elements("custom").next().unwrap()),
        Namespace::Html
    );
    assert_eq!(
        document.namespace(document.elements("g").next().unwrap()),
        Namespace::Svg
    );
    context.attrs = None;
    let document = html::parse_fragment_with_context("<custom/><svg><g/></svg>", context);
    assert_eq!(
        document.namespace(document.elements("custom").next().unwrap()),
        Namespace::MathMl
    );
    assert_eq!(
        document.namespace(document.elements("g").next().unwrap()),
        Namespace::Svg
    );
    assert_arena(&document);
}

#[test]
fn cdata_is_literal_and_foreign_text_uses_html_serialization_escaping() {
    let document = html::parse_with_scripting(
        "<!doctype html><svg><script><![CDATA[a<&amp;\0]]></script><style><![CDATA[<x>&]]></style><title><![CDATA[<x>&\0]]></title><g><![CDATA[a]]]]>b</g></svg><script>a<b &amp;</script><div><![CDATA[html]]></div>",
        true,
    );
    let svg_script = document.elements("script").next().unwrap();
    assert_eq!(document.text(svg_script), "a<&amp;�");
    assert_eq!(document.inner_html(svg_script), "a&lt;&amp;amp;�");
    assert_eq!(
        document.serialize_node(svg_script),
        "<script>a&lt;&amp;amp;�</script>"
    );
    assert_eq!(
        document.text(document.elements("title").next().unwrap()),
        "<x>&"
    );
    let html_script = document.html_elements("script").next().unwrap();
    assert_eq!(document.inner_html(html_script), "a<b &amp;");
    assert_eq!(document.html_elements("style").count(), 0);
    assert_eq!(document.html_elements("title").count(), 0);
    assert_eq!(document.title(), "");
    assert!(document.serialize().contains("<!--[CDATA[html]]-->"));
    assert_arena(&document);
}

#[test]
fn foreign_breakout_and_template_ownership_are_namespace_sensitive() {
    let document = html::parse(
        "<svg><g><font id=foreign>f</font><font color=red id=html>h</font><p id=p>p</p><svg><template id=foreign-template><path/></template><foreignObject><template id=html-template><svg><g/></svg></template></foreignObject></svg>",
    );
    assert_eq!(document.namespace(id(&document, "foreign")), Namespace::Svg);
    assert_eq!(document.namespace(id(&document, "html")), Namespace::Html);
    assert_eq!(document.namespace(id(&document, "p")), Namespace::Html);
    let foreign_template = id(&document, "foreign-template");
    assert_eq!(document.namespace(foreign_template), Namespace::Svg);
    assert!(!document.template_contents.contains_key(&foreign_template));
    let html_template = id(&document, "html-template");
    let contents = document.template_contents[&html_template];
    assert!(document.nodes[contents.0].parent.is_none());
    assert_eq!(
        document.namespace(document.nodes[contents.0].children[0]),
        Namespace::Svg
    );
    assert!(
        document
            .inner_html(html_template)
            .contains("<svg><g></g></svg>")
    );
    assert_arena(&document);
}

#[test]
fn html_void_names_in_foreign_namespaces_retain_children_and_closing_tags() {
    let document = html::parse("<svg><input>text</input><source><g/></source></svg>");
    let input = document.elements("input").next().unwrap();
    assert_eq!(document.namespace(input), Namespace::Svg);
    assert_eq!(document.inner_html(input), "text");
    assert_eq!(document.serialize_node(input), "<input>text</input>");
    let source = document.elements("source").next().unwrap();
    assert_eq!(document.serialize_node(source), "<source><g></g></source>");
    assert_arena(&document);
}

#[test]
fn foreign_fragment_limits_leave_consistent_metadata_on_a_small_native_stack() {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let mut context = FragmentContext::new("svg", false);
            context.namespace = Namespace::Svg;
            let deep = html::parse_fragment_with_context(&"<g xlink:href=x>".repeat(2000), context);
            assert!(deep.truncated);
            assert!(deep.nodes.len() < 1000);
            assert_arena(&deep);
            for node in deep.elements("g") {
                assert_eq!(deep.namespace(node), Namespace::Svg);
                assert_eq!(
                    deep.attribute_namespace(node, "xlink:href"),
                    Some(AttributeNamespace::XLink)
                );
            }
            let _ = deep.serialize();
            let broad = html::parse_fragment_with_context(
                &"<path xml:lang=en />".repeat(html::MAX_NODES),
                context,
            );
            assert!(broad.truncated);
            assert_eq!(broad.nodes.len(), html::MAX_NODES);
            assert_arena(&broad);
            let _ = broad.serialize();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn malformed_foreign_contexts_preserve_namespaces_and_template_arena() {
    let tokens = [
        "<svg>",
        "</svg>",
        "<math>",
        "</math>",
        "<g>",
        "</g>",
        "<foreignObject>",
        "</foreignObject>",
        "<mtext>",
        "</mtext>",
        "<annotation-xml encoding=text/html>",
        "</annotation-xml>",
        "<mglyph/>",
        "<path xlink:href=x />",
        "<template>",
        "</template>",
        "<table>",
        "</table>",
        "<tr>",
        "<td>",
        "<b>",
        "</b>",
        "<i>",
        "</i>",
        "<p>",
        "</p>",
        "<font color=red>",
        "</font>",
        "<![CDATA[<x>&\0]]>",
        "é&copy;",
        "<!--x-->",
        " ",
    ];
    let mut state = 0x27d4_eb2d_u64;
    for (namespace, tag) in [
        (Namespace::Html, "div"),
        (Namespace::Svg, "g"),
        (Namespace::Svg, "foreignObject"),
        (Namespace::MathMl, "mtext"),
        (Namespace::MathMl, "annotation-xml"),
    ] {
        for case in 0..128 {
            let mut input = String::new();
            for _ in 0..96 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                input.push_str(tokens[state as usize % tokens.len()]);
            }
            let mut context = FragmentContext::new(tag, case % 2 == 0);
            context.namespace = namespace;
            let document = html::parse_fragment_with_context(&input, context);
            assert!(
                !document.truncated,
                "unexpected abort: {namespace:?} {tag} {input:?}"
            );
            assert_arena(&document);
            let _ = document.serialize();
        }
    }
}
