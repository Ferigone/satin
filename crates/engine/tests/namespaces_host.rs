use rust_browser_engine::{
    LoadedPage,
    dom::{AttributeNamespace, Namespace},
    script::ScriptRuntime,
};
use rust_browser_javascript::Value;

#[test]
fn foreign_fragment_clone_and_replacement_preserve_namespace_metadata() {
    let mut page = LoadedPage::from_html(
        "https://example.test/",
        "<!doctype html><title>HTML title</title><div id=target></div><svg id=svg><title>SVG title</title></svg>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(r#"document.getElementById('svg').innerHTML='<linearGradient id="gradient" viewBox="1 2 3 4"><a xlink:href="/safe">link</a></linearGradient>';var clone=document.getElementById('gradient').cloneNode(true);document.getElementById('svg').appendChild(clone);clone.namespaceURI==='http://www.w3.org/2000/svg'&&clone.localName==='linearGradient'&&clone.tagName==='linearGradient'&&clone.getAttribute('viewBox')==='1 2 3 4'&&clone.getAttribute('viewbox')===null&&document.title==='HTML title';"#).unwrap(),Value::Bool(true));
    runtime.snapshot(&mut page);
    let gradients = page.document.elements("linearGradient").collect::<Vec<_>>();
    assert_eq!(gradients.len(), 2);
    for id in gradients {
        assert_eq!(page.document.namespace(id), Namespace::Svg);
        let anchor = page.document.nodes[id.0].children[0];
        assert_eq!(
            page.document.attribute_namespace(anchor, "xlink:href"),
            Some(AttributeNamespace::XLink)
        );
    }
}
#[test]
fn native_namespace_creation_and_attribute_case_are_preserved() {
    let mut page = LoadedPage::from_html("https://example.test/", "<div id=target></div>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval("var svg=document.createElementNS('http://www.w3.org/2000/svg','svg');var g=document.createElementNS('http://www.w3.org/2000/svg','linearGradient');g.setAttribute('viewBox','0 0 10 10');svg.appendChild(g);document.getElementById('target').appendChild(svg);g.namespaceURI==='http://www.w3.org/2000/svg'&&g.localName==='linearGradient'&&g.tagName==='linearGradient'&&g.getAttribute('viewBox')==='0 0 10 10'&&g.getAttribute('viewbox')===null;").unwrap(),Value::Bool(true));
    runtime.snapshot(&mut page);
    assert!(
        page.document
            .serialize()
            .contains("<linearGradient viewBox=")
    );
}
#[test]
fn foreign_scripts_and_base_do_not_become_html_resource_elements() {
    let mut page = LoadedPage::from_html(
        "https://example.test/document",
        "<!doctype html><title>HTML</title><svg><script>document.title='foreign ran';</script><base href='https://evil.test/'/></svg><script>var htmlRan=true;</script>",
    );
    assert_eq!(page.scripts.len(), 1);
    let runtime = ScriptRuntime::for_page(&mut page).unwrap().unwrap();
    assert_eq!(page.title(), "HTML");
    assert_eq!(
        runtime.resolve_reference("child").as_deref(),
        Some("https://example.test/child")
    );
}
