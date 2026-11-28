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

#[test]
fn dom_implementation_creates_independent_html_documents() {
    check(
        r#"
        (() => {
            const assert = (ok, label) => { if (!ok) throw Error(label); };
            const impl = document.implementation;
            assert(impl === document.implementation && impl instanceof DOMImplementation, 'implementation');
            const d = impl.createHTMLDocument('');
            assert(d instanceof HTMLDocument && d instanceof Document && d !== document &&
                   d.implementation !== impl && d.defaultView === null &&
                   d.contentType === 'text/html' && d.URL === 'about:blank', 'independent HTML document');
            assert(d.firstChild === d.doctype && d.doctype instanceof DocumentType &&
                   d.doctype.name === 'html' && d.doctype.publicId === '' && d.doctype.systemId === '' &&
                   d.documentElement.tagName === 'HTML' && d.head.ownerDocument === d &&
                   d.body.ownerDocument === d && d.head.firstChild.firstChild.nodeType === 3, 'initial tree');
            d.body.innerHTML = '<form id="local"></form><form></form>';
            const f = d.getElementById('local');
            assert(d.body.childNodes.length === 2 && f === d.querySelector('FORM') &&
                   f.ownerDocument === d && document.getElementById('local') === null, 'inert HTML parsing');
            assert(d.createElement('MiXeD').tagName === 'MIXED' &&
                   d.createElement('form') instanceof HTMLElement, 'HTML element factory');
            const other = new Document().implementation.createHTMLDocument();
            assert(other.head.firstChild === null && other.title === '', 'omitted title');
            d.title = '  <test>    value';
            assert(d.title === '<test> value' && document.title === 'Live' && other.title === '', 'isolated titles');
            assert(impl.createHTMLDocument(undefined).head.firstChild === null &&
                   impl.createHTMLDocument(null).title === 'null', 'optional DOMString title');
            for (const operation of [() => new DOMImplementation(), () => impl.createHTMLDocument.call({})]) {
                let type;
                try { operation(); } catch (err) { type = err.name; }
                assert(type === 'TypeError', 'receiver and construction');
            }
            document.body.appendChild(f);
            assert(f.ownerDocument === document && d.getElementById('local') === null &&
                   document.getElementById('local') === f, 'cross-document adoption');
            return 'ok';
        })()
        "#,
    );
}

#[test]
fn dom_parser_creates_inert_independent_html_documents() {
    check(
        r#"
        (() => {
            const assert = (ok, label) => { if (!ok) throw Error(label); };
            const parser = new DOMParser();
            const d = parser.parseFromString('<!doctype html><title>Parsed</title><main id="local"><svg><circle/></svg><template><b>nested</b></template></main><script>globalThis.parserRan = true</script>', 'text/html');
            assert(parser instanceof DOMParser && window.DOMParser === DOMParser, 'constructor');
            assert(d instanceof Document && d !== document && d.defaultView === null &&
                   d.contentType === 'text/html' && d.title === 'Parsed' && d.doctype.name === 'html', 'document');
            const main = d.getElementById('local');
            assert(main === d.body.querySelector('main') && main.ownerDocument === d &&
                   main.querySelector('circle').namespaceURI === 'http://www.w3.org/2000/svg' &&
                   main.querySelector('template').content.firstChild.textContent === 'nested', 'tree and namespaces');
            assert(document.getElementById('local') === null && document.title === 'Live' &&
                   globalThis.parserRan === undefined, 'inert and isolated');
            assert(parser.parseFromString('<p>another</p>', 'text/html') !== d, 'fresh documents');
            for (const call of [() => DOMParser(), () => parser.parseFromString('x'),
                () => parser.parseFromString('x', 'text/plain'),
                () => parser.parseFromString.call({}, 'x', 'text/html')]) {
                let name;
                try { call(); } catch (error) { name = error.name; }
                assert(name === 'TypeError', 'Web IDL receiver and arguments');
            }
            document.body.appendChild(main);
            assert(main.ownerDocument === document && d.getElementById('local') === null, 'adoption');
            document.body.appendChild(d.querySelector('script'));
            assert(globalThis.parserRan === undefined, 'adopted parsed scripts stay inert');
            return 'ok';
        })()
        "#,
    );
}

#[test]
fn array_like_map_preserves_dom_callback_receiver() {
    // Reduced from the DOM traversal and callback shape of jQuery wrapAll.
    // Reference: Chromium, including the detached HTML document and adoption.
    check(
        r#"
        (() => {
            const assert = (ok, label) => { if (!ok) throw Error(label); };
            const subject = document.getElementById('live');
            const inert = document.implementation.createHTMLDocument('');
            const container = inert.createElement('div');
            container.innerHTML = '<section><div><span></span></div></section>';
            const parsed = container.childNodes[0];
            assert(parsed && parsed.firstElementChild.localName === 'div', 'parsed wrapper');
            const wrapper = parsed.cloneNode(true);
            assert(wrapper !== parsed &&
                   wrapper.firstElementChild.firstElementChild.localName === 'span', 'deep clone');
            subject.parentNode.insertBefore(wrapper, subject);
            assert(wrapper.ownerDocument === document &&
                   wrapper.parentNode === subject.parentNode, 'wrapper adoption');
            const collection = {0: wrapper, length: 1};
            function staticMap(obj, callback, arg) {
                var length = obj.length, result = [];
                for (var i = 0; i < length; i++) {
                    var value = callback(obj[i], i, arg);
                    if (value != null) result.push(value);
                }
                return result;
            }
            function collectionMap(callback) {
                return staticMap(this, function(elem, i) { return callback.call(elem, i, elem); });
            }
            const result = collectionMap.call(collection, function(i, elem) {
                'use strict';
                assert(this === wrapper && elem === wrapper && i === 0, 'map callback receiver');
                for (var t = this; t.firstElementChild;) t = t.firstElementChild;
                return t;
            });
            assert(result.length === 1 && result[0].localName === 'span', 'deepest element');
            result[0].appendChild(subject);
            assert(subject.parentNode === result[0] && subject.ownerDocument === document, 'wrapped subject');
            return 'ok';
        })()
        "#,
    );
}

#[test]
fn element_navigation_mixins_skip_non_elements_and_track_mutations() {
    check(
        r#"
        (() => {
            const assert = (ok, label) => { if (!ok) throw Error(label); };
            const f = document.createDocumentFragment();
            const a = document.createElement('div');
            const b = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
            const t = document.createTextNode('text');
            const c = document.createComment('comment');
            f.append(t, a, c, b, document.createTextNode('tail'));
            assert(f.firstElementChild === a && f.lastElementChild === b &&
                   f.childElementCount === 2, 'fragment skips non-elements');
            assert(a.nextElementSibling === b && b.previousElementSibling === a &&
                   t.nextElementSibling === a && c.previousElementSibling === a &&
                   c.nextElementSibling === b, 'sibling traversal and identity');
            assert(a.previousElementSibling === null && b.nextElementSibling === null &&
                   a.firstElementChild === null && a.lastElementChild === null &&
                   a.childElementCount === 0, 'empty and boundaries');
            f.removeChild(a);
            assert(f.firstElementChild === b && f.lastElementChild === b &&
                   f.childElementCount === 1 && a.nextElementSibling === null &&
                   b.previousElementSibling === null, 'live mutation');
            a.appendChild(f);
            assert(a.firstElementChild === b && a.lastElementChild === b &&
                   a.childElementCount === 1 && f.firstElementChild === null &&
                   f.childElementCount === 0, 'fragment insertion');
            assert(document.firstElementChild === document.documentElement &&
                   document.lastElementChild === document.documentElement &&
                   document.childElementCount === 1, 'document skips doctype');
            assert(t.firstElementChild === undefined &&
                   document.doctype.nextElementSibling === undefined, 'interface placement');
            const getter = Object.getOwnPropertyDescriptor(Element.prototype, 'firstElementChild').get;
            let invalid = false;
            try { getter.call(t); } catch (e) { invalid = e.name === 'TypeError'; }
            assert(invalid, 'invalid receiver');
            return 'ok';
        })()
        "#,
    );
}

#[test]
fn css_escape_serializes_domstrings_as_identifiers() {
    check(
        r#"
        (() => {
            const assert = (ok, label) => { if (!ok) throw Error(label); };
            for (const [input, expected] of [
                ['', ''], ['\0', '\uFFFD'], ['\x01\x7f', '\\1 \\7f '],
                ['0a', '\\30 a'], ['-1a', '-\\31 a'], ['-', '\\-'],
                ['--', '--'], ['a b#', 'a\\ b\\#'], ['a_b-9', 'a_b-9'],
                ['é😀', 'é😀'], ['\ud800', '\ud800'],
                [undefined, 'undefined'], [null, 'null'], [42, '\\34 2'],
            ]) {
                assert(CSS.escape(input) === expected, 'serialization: ' + input);
            }
            assert(CSS.escape.length === 1 && CSS.escape.call(null, 'a:b') === 'a\\:b', 'operation');
            assert(CSS.escape({toString(){return 'a.b'}}) === 'a\\.b', 'DOMString conversion');
            let missing = false, symbol = false;
            try { CSS.escape(); } catch(e) { missing = e instanceof TypeError; }
            try { CSS.escape(Symbol()); } catch(e) { symbol = e instanceof TypeError; }
            assert(missing && symbol, 'required argument and symbol rejection');
            return 'ok';
        })()
    "#,
    );
}
