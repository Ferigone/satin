use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

// The same assertions run in a Chromium page with an existing HTML document.
fn check(script: &str) {
    let page = LoadedPage::from_html(
        "https://example.test/",
        "<!doctype html><title>Live</title><body><main id='live'></main>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(script).unwrap(), Value::from("ok"));
}

#[test]
fn document_constructor_creates_an_independent_xml_tree() {
    check(
        r#"
        (() => {
            const d = new Document();
            class Other extends Document {}
            const other = new Other();
            const assert = (condition, label) => { if (!condition) throw Error(label); };
            assert(document instanceof Document && d !== document && other !== d, 'distinct documents');
            assert(other instanceof Other && other instanceof Document && other.constructor === Other, 'subclass');
            assert(Object.getPrototypeOf(Document) === Node &&
                   Object.getPrototypeOf(Document.prototype) === Node.prototype, 'inheritance');
            assert(d.nodeType === 9 && d.nodeName === '#document' && d.ownerDocument === null &&
                   d.isConnected && d.getRootNode() === d && d.defaultView === null, 'root');
            assert(d.documentElement === null && d.body === null && d.head === null &&
                   d.getElementById('live') === null && d.contentType === 'application/xml', 'empty XML');
            assert(d.URL === 'about:blank' && d.documentURI === d.URL && d.baseURI === d.URL &&
                   d.readyState === 'complete' && d.compatMode === 'CSS1Compat', 'metadata');
            let name = '';
            try { Document(); } catch (e) { name = e.name; }
            assert(name === 'TypeError', 'requires new');
            const e = d.createElement('TiTlE'), h = d.createElementNS('http://www.w3.org/1999/xhtml', 'section');
            assert(e instanceof Element && !(e instanceof HTMLElement) && e.namespaceURI === null &&
                   e.nodeName === 'TiTlE' && e.localName === 'TiTlE' && e.ownerDocument === d, 'XML element');
            assert(h instanceof HTMLElement && h.nodeName === 'section' && h.ownerDocument === d, 'XHTML element');
            e.setAttribute('Key', 'Value');
            assert(e.getAttribute('key') === null && e.getAttribute('Key') === 'Value', 'case-sensitive attributes');
            assert(d.appendChild(e) === e && d.documentElement === e && e.isConnected, 'insert root');
            assert(d.querySelector('TiTlE[Key]') === e && d.querySelector('title[key]') === null &&
                   d.querySelector(':root') === e, 'XML selector casing and root');
            try { d.append(h); } catch (e) { name = e.name; }
            assert(name === 'HierarchyRequestError' && h.parentNode === null, 'two roots');
            return 'ok';
        })()
    "#,
    );
}

#[test]
fn moving_and_importing_nodes_preserves_document_ownership_and_identity() {
    check(
        r#"
        (() => {
            const assert = (condition, label) => { if (!condition) throw Error(label); };
            const a = new Document(), b = new Document();
            const root = a.createElement('Book'), t = a.createTextNode('one'), f = a.createDocumentFragment();
            root.id = 'book'; root.appendChild(t); f.appendChild(root);
            assert(a.getElementById('book') === null && !root.isConnected && f.ownerDocument === a, 'detached fragment');
            assert(a.appendChild(f) === f && f.firstChild === null && a.getElementById('book') === root, 'insert fragment');
            assert(b.appendChild(root) === root && root.ownerDocument === b && t.ownerDocument === b &&
                   root.getRootNode() === b && root.isConnected && a.documentElement === null, 'recursive adoption');
            const copy = a.importNode(root, true);
            assert(copy !== root && copy.ownerDocument === a && copy.firstChild.ownerDocument === a &&
                   copy.nodeName === 'Book' && copy.namespaceURI === null && copy.textContent === 'one', 'import');
            const cloned = root.cloneNode(true);
            assert(cloned.ownerDocument === b && cloned.firstChild.ownerDocument === b, 'clone');
            f.appendChild(copy); root.appendChild(f);
            assert(f.ownerDocument === a && copy.ownerDocument === b && copy.firstChild.ownerDocument === b, 'adopt fragment');
            document.body.appendChild(root);
            assert(root.ownerDocument === document && t.ownerDocument === document && copy.ownerDocument === document &&
                   root.tagName === 'Book' && document.getElementById('book') === root && b.documentElement === null, 'adopt into page');
            return 'ok';
        })()
    "#,
    );
}
