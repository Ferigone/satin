use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

#[test]
fn live_attributes_support_jquery_data_enumeration_and_identity() {
    let page = LoadedPage::from_html(
        "https://example.test",
        "<!doctype html><div id='target' data-zoom='4' data-map='yes'></div>",
    );
    let mut realm = ScriptRuntime::new(&page).unwrap();
    assert_eq!(realm.eval(r#"
        var element = document.getElementById('target');
        var attrs = element.attributes;
        var data = {};
        for (var i = attrs.length; i--;) {
            if (attrs[i] && attrs[i].name.indexOf('data-') === 0) {
                data[attrs[i].name.slice(5)] = element.getAttribute(attrs[i].name);
            }
        }
        var zoom = attrs.getNamedItem('DATA-ZOOM');
        var initial = data.zoom === '4' && data.map === 'yes' &&
            attrs === element.attributes && attrs instanceof NamedNodeMap &&
            zoom instanceof Attr && zoom instanceof Node && zoom.nodeType === 2 &&
            zoom === attrs.item(1) && zoom === element.getAttributeNode('data-zoom') &&
            zoom.ownerElement === element && zoom.ownerDocument === document &&
            attrs.item(99) === null && attrs[99] === undefined;
        element.setAttribute('data-zoom', '5');
        var updated = zoom.value === '5' && zoom.nodeValue === '5';
        zoom.value = '6';
        var reflected = element.getAttribute('data-zoom') === '6';
        element.removeAttribute('data-zoom');
        var detached = zoom.ownerElement === null && zoom.value === '6' && attrs.length === 2;
        element.setAttribute('data-zoom', '7');
        zoom.value = 'old';
        initial && updated && reflected && detached &&
            attrs.length === 3 && attrs[2] !== zoom && attrs[2].name === 'data-zoom' &&
            attrs[2].value === '7' && zoom.value === 'old'
    "#).unwrap(), Value::Bool(true));
}

#[test]
fn named_attribute_namespace_lookup_preserves_svg_qualified_names() {
    let page = LoadedPage::from_html(
        "https://example.test",
        "<!doctype html><svg viewBox='0 0 1 1'><use xlink:href='#x'/></svg>",
    );
    let mut realm = ScriptRuntime::new(&page).unwrap();
    assert_eq!(realm.eval(r#"
        var svg = document.querySelector('svg');
        var use = document.querySelector('use');
        var attr = use.getAttributeNodeNS('http://www.w3.org/1999/xlink', 'href');
        svg.attributes.getNamedItem('viewBox').localName === 'viewBox' &&
        svg.attributes.getNamedItem('viewbox') === null &&
        attr === use.attributes.getNamedItemNS('http://www.w3.org/1999/xlink', 'href') &&
        attr.name === 'xlink:href' && attr.localName === 'href' &&
        attr.prefix === 'xlink' && attr.namespaceURI === 'http://www.w3.org/1999/xlink' &&
        attr.value === '#x'
    "#).unwrap(), Value::Bool(true));
}
