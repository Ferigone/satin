use rust_browser_engine::{
    dom::{AttributeNamespace, Namespace, NodeKind},
    svg::{
        MAX_SVG_DOCUMENT_SOURCE, SvgDocumentErrorKind as ErrorKind, SvgRect, SvgScene,
        parse_document,
    },
};
use std::collections::BTreeMap;

#[test]
fn xml_namespace_scopes_keep_xhtml_metadata_from_changing_later_svg_shapes() {
    let doc = parse_document(
        "<?xml version='1.0' encoding='UTF-8'?><svg xmlns='http://www.w3.org/2000/svg'><metadata><meta xmlns='http://www.w3.org/1999/xhtml' content='assertion'/></metadata><g xmlns:s='http://www.w3.org/2000/svg'><s:linearGradient id='paint'/><circle r='3'/></g></svg>",
    ).unwrap();
    assert_eq!(doc.nodes[doc.root.0].children.len(), 1);
    assert_eq!(
        doc.namespace(doc.elements("meta").next().unwrap()),
        Namespace::Html
    );
    assert_eq!(
        doc.namespace(doc.elements("circle").next().unwrap()),
        Namespace::Svg
    );
    assert_eq!(
        doc.namespace(doc.elements("linearGradient").next().unwrap()),
        Namespace::Svg
    );
    assert_eq!(doc.elements("html").count(), 0);
}

#[test]
fn entities_newlines_cdata_comments_and_nonrecursive_references_preserve_xml_text() {
    let doc = parse_document(
        "\u{feff}<?xml version='1.0' standalone='yes'?><!--ok--><svg xmlns='http://www.w3.org/2000/svg'><title value='a\r\nb\tc&#x9;d'>&amp;lt;&lt;&gt;&quot;&apos;&#x1F642;\r\n<![CDATA[<literal>&raw;]]></title></svg>",
    ).unwrap();
    let title = doc.elements("title").next().unwrap();
    assert_eq!(doc.attr(title, "value"), Some("a b c\td"));
    assert_eq!(doc.text(title), "&lt;<>\"'🙂\n<literal>&raw;");
    assert!(matches!(doc.nodes[1].kind, NodeKind::Comment(_)));
    assert!(!doc.scripting_enabled);
}

#[test]
fn xlink_expanded_attribute_names_work_with_arbitrary_declared_prefixes() {
    let doc = parse_document(
        "<svg xmlns='http://www.w3.org/2000/svg' xmlns:xl='http://www.w3.org/1999/xlink'><defs><rect id='base' width='10' height='10' fill='red'/></defs><use xl:href='#base' x='5'/></svg>",
    ).unwrap();
    let root = doc.elements("svg").next().unwrap();
    let instance = doc.elements("use").next().unwrap();
    assert_eq!(
        doc.attribute_namespace(instance, "xl:href"),
        Some(AttributeNamespace::XLink)
    );
    assert_eq!(doc.attribute_local_name(instance, "xl:href"), "href");
    let scene = SvgScene::build(&doc, root, 20., 20., &BTreeMap::new());
    assert!(!scene.diagnostics.unsupported);
    assert_eq!(scene.hit_test(8., 5.), Some(instance));
}

#[test]
fn malformed_xml_fails_without_html_repair_or_namespace_guessing() {
    for source in [
        "<svg xmlns='http://www.w3.org/2000/svg'><g></svg>",
        "<svg xmlns='http://www.w3.org/2000/svg' width=1/>",
        "<svg xmlns='http://www.w3.org/2000/svg' width='1' width='2'/>",
        "<svg xmlns='http://www.w3.org/2000/svg' xmlns:a='http://www.w3.org/1999/xlink' xmlns:b='http://www.w3.org/1999/xlink' a:href='x' b:href='y'/>",
        "<svg xmlns='http://www.w3.org/2000/svg'><p:g/></svg>",
        "<svg xmlns='http://www.w3.org/2000/svg' xmlns:xml='wrong'/>",
        "<svg xmlns='http://www.w3.org/2000/svg' xmlns:a=''/>",
        "<svg xmlns='http://www.w3.org/2000/svg' a:b:c='x'/>",
        "<svg xmlns='http://www.w3.org/2000/svg'><!--bad--comment--></svg>",
        "<svg xmlns='http://www.w3.org/2000/svg'>&notDeclared;</svg>",
        "<svg xmlns='http://www.w3.org/2000/svg'>&#0;</svg>",
        "<svg xmlns='http://www.w3.org/2000/svg'>&#+32;</svg>",
        "<svg xmlns='http://www.w3.org/2000/svg'>&#x+20;</svg>",
        "<svg xmlns='http://www.w3.org/2000/svg'>]]></svg>",
        "&#32;<svg xmlns='http://www.w3.org/2000/svg'/>",
        "<svg xmlns='http://www.w3.org/2000/svg'/><svg xmlns='http://www.w3.org/2000/svg'/>",
        " <?xml version='1.0'?><svg xmlns='http://www.w3.org/2000/svg'/>",
        "<?xml standalone='yes' version='1.0'?><svg xmlns='http://www.w3.org/2000/svg'/>",
        "<?xml?><svg xmlns='http://www.w3.org/2000/svg'/>",
    ] {
        let error = parse_document(source).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Invalid, "{source}: {error}");
    }
}

#[test]
fn unsupported_dtd_encoding_and_vocabularies_are_distinguished_from_malformed_xml() {
    for source in [
        "<!DOCTYPE svg SYSTEM 'https://example.invalid/svg.dtd'><svg xmlns='http://www.w3.org/2000/svg'/>",
        "<!DOCTYPE svg [<!ENTITY x 'expanded'>]><svg xmlns='http://www.w3.org/2000/svg'/>",
        "<?xml version='1.1'?><svg xmlns='http://www.w3.org/2000/svg'/>",
        "<?xml version='1.0' encoding='ISO-8859-1'?><svg xmlns='http://www.w3.org/2000/svg'/>",
        "<svg/>",
        "<svg xmlns='http://www.w3.org/2000/svg'><foreign xmlns='urn:unrepresented'/></svg>",
    ] {
        assert_eq!(
            parse_document(source).unwrap_err().kind,
            ErrorKind::Unsupported,
            "{source}"
        );
    }
}

#[test]
fn xml_and_scene_limits_parse_render_and_drop_on_a_one_mib_native_stack() {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let prefix = "<svg xmlns='http://www.w3.org/2000/svg'>";
            let deep = format!(
                "{prefix}{}{}</svg>",
                "<g>".repeat(1000),
                "</g>".repeat(1000)
            );
            assert_eq!(parse_document(&deep).unwrap_err().kind, ErrorKind::Limit);
            let many = format!("{prefix}{}</svg>", "<circle/>".repeat(20_000));
            assert_eq!(parse_document(&many).unwrap_err().kind, ErrorKind::Limit);
            let attrs = (0..257).map(|i| format!(" a{i}='v'")).collect::<String>();
            assert_eq!(
                parse_document(&format!("{prefix}<g{attrs}/></svg>"))
                    .unwrap_err()
                    .kind,
                ErrorKind::Limit
            );
            assert_eq!(
                parse_document(&"x".repeat(MAX_SVG_DOCUMENT_SOURCE + 1))
                    .unwrap_err()
                    .kind,
                ErrorKind::Limit
            );
            let valid = format!(
                "{prefix}{}<path d='M2 2 C30 0 30 30 2 30 Z' fill='blue'/>{}</svg>",
                "<g>".repeat(40),
                "</g>".repeat(40)
            );
            let doc = parse_document(&valid).unwrap();
            let root = doc.elements("svg").next().unwrap();
            let scene = SvgScene::build(&doc, root, 40., 40., &BTreeMap::new());
            assert!(!scene.diagnostics.truncated);
            let mut pixels = vec![0xffffff; 1600];
            let bounds = SvgRect {
                x: 0.,
                y: 0.,
                width: 40.,
                height: 40.,
            };
            assert!(!scene.paint(&mut pixels, 40, 40, bounds, bounds).truncated);
            assert_eq!(scene.hit_test(10., 10.), doc.elements("path").next());
        })
        .unwrap()
        .join()
        .unwrap();
}
