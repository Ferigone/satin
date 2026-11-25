use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

#[test]
fn prototype_methods_reach_elements_but_not_other_nodes() {
    let fixture = include_str!("../../../tools/fixtures/element-prototype.html");
    let page = LoadedPage::from_html("https://example.test/", fixture);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.run_initial(&page);
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
    assert_eq!(
        runtime.eval("document.title").unwrap(),
        Value::from("Element prototype OK"),
        "{:?}",
        runtime
            .eval("document.body.getAttribute('data-checks')")
            .unwrap()
    );
}

#[test]
fn html_element_prototype_is_shared_by_html_elements_only() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        "<!doctype html><app-root></app-root>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(
        runtime
            .eval(
                r#"
        class ReaderElement extends HTMLElement {}
        var svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
        var elements = ["input", "canvas", "iframe", "img", "audio", "script", "section"];
        Object.getPrototypeOf(HTMLElement) === Element &&
        Object.getPrototypeOf(HTMLElement.prototype) === Element.prototype &&
        document.querySelector("app-root") instanceof HTMLElement &&
        elements.every(tag => document.createElement(tag) instanceof HTMLElement) &&
        !(svg instanceof HTMLElement) && svg instanceof Element &&
        !(document.createTextNode("hi") instanceof HTMLElement) &&
        (() => { try { new ReaderElement(); } catch(e) { return e instanceof TypeError; } })()
    "#
            )
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn character_data_and_svg_prototypes_reach_parser_and_script_created_nodes() {
    let html = "<!doctype html><main>A😀<!--note--></main><svg><g></g></svg>";
    let page = LoadedPage::from_html("https://example.test/", html);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let result = runtime.eval(r#"
        const text = document.querySelector("main").firstChild;
        const comment = text.nextSibling;
        const made = document.createTextNode("new");
        const doc = new Document();
        const foreign = doc.createComment("foreign");
        class Label extends Text {}
        const label = new Label("hello");
        const descriptor = Object.getOwnPropertyDescriptor(CharacterData.prototype, "data");
        const size = Object.getOwnPropertyDescriptor(CharacterData.prototype, "length");
        let badReceiver = false, illegal = false;
        try { size.get.call(document.body); } catch (e) { badReceiver = e instanceof TypeError; }
        try { new CharacterData(); } catch (e) { illegal = e instanceof TypeError; }
        CharacterData.prototype.readLabel = function() { return this.data; };
        const passed =
            Object.getPrototypeOf(CharacterData) === Node &&
            Object.getPrototypeOf(Text) === CharacterData && Object.getPrototypeOf(Comment) === CharacterData &&
            Object.getPrototypeOf(CharacterData.prototype) === Node.prototype &&
            Object.getPrototypeOf(Text.prototype) === CharacterData.prototype &&
            Object.getPrototypeOf(Comment.prototype) === CharacterData.prototype &&
            text instanceof Text && !(text instanceof Comment) && text.length === 3 &&
            comment instanceof Comment && comment.length === 4 && comment.readLabel() === "note" &&
            made instanceof Text && made.readLabel() === "new" && foreign instanceof Comment && foreign.ownerDocument === doc &&
            new Text().data === "" && new Comment(undefined).data === "" &&
            new Comment(null).data === "null" && label instanceof Label && label instanceof Text &&
            label.data === "hello" && label.ownerDocument === document && label.parentNode === null &&
            !("readLabel" in document.body) && badReceiver && illegal &&
            Object.getPrototypeOf(SVGElement) === Element &&
            Object.getPrototypeOf(SVGElement.prototype) === Element.prototype &&
            document.querySelector("svg").firstChild instanceof SVGElement &&
            document.createElementNS("http://www.w3.org/2000/svg","path") instanceof SVGElement &&
            !(document.createElement("svg") instanceof SVGElement);
        descriptor.set.call(comment, "edited");
        passed && comment.nodeValue === "edited" && comment.length === 6
    "#).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn contains_follows_current_parent_links_and_includes_the_node_itself() {
    let page = LoadedPage::from_html("https://example.test/", "<main><i>text</i></main>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let result = runtime
        .eval(
            r#"
        const main = document.querySelector("main");
        const element = main.firstChild;
        const text = element.firstChild;
        const root = document.documentElement.contains.bind(document.documentElement);
        const detached = document.createElement("div");
        function throwsTypeError(callback) {
          try { callback(); } catch (error) { return error instanceof TypeError; }
          return false;
        }
        const initial = root(text) && document.contains(text) && main.contains(element) &&
          text.contains(text) && !text.contains(element) && !document.contains(detached) &&
          !main.contains(null) && !main.contains(undefined) &&
          throwsTypeError(() => main.contains()) && throwsTypeError(() => main.contains({})) &&
          throwsTypeError(() => Node.prototype.contains.call({}, text));
        detached.appendChild(element);
        initial && !root(text) && !main.contains(text) && detached.contains(text)
    "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn native_xml_character_nodes_preserve_identity_type_and_document() {
    let page = LoadedPage::from_html("https://example.test/", "<main></main>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let setup = r#"
        const xml = new Document();
        const root = xml.createElement("root");
        const cdata = xml.createCDATASection("A😀<&");
        const pi = xml.createProcessingInstruction("π:render", "before");
        const outside = new ProcessingInstruction("xml-stylesheet");
        class Directive extends ProcessingInstruction {}
        const sub = new Directive("t", undefined);
        xml.appendChild(pi);
        xml.appendChild(root);
        root.appendChild(cdata);
        root.appendChild(outside);
        function errorName(f) { try { f(); } catch (e) { return e.name; } return ""; }
    "#;
    runtime.eval(setup).unwrap();
    for check in [
        "Object.getPrototypeOf(CDATASection) === Text && Object.getPrototypeOf(CDATASection.prototype) === Text.prototype",
        "Object.getPrototypeOf(ProcessingInstruction) === CharacterData && Object.getPrototypeOf(ProcessingInstruction.prototype) === CharacterData.prototype",
        "cdata instanceof CDATASection && cdata instanceof Text && cdata.nodeType === Node.CDATA_SECTION_NODE && cdata.nodeName === '#cdata-section'",
        "pi instanceof ProcessingInstruction && pi instanceof CharacterData && !(pi instanceof Text) && pi.nodeType === Node.PROCESSING_INSTRUCTION_NODE && pi.nodeName === pi.target && pi.target === 'π:render'",
        "sub instanceof Directive && sub instanceof ProcessingInstruction && sub.ownerDocument === document && sub.data === ''",
        "outside.ownerDocument === xml && outside.parentNode === root && outside.data === '' && pi.ownerDocument === xml && pi.parentNode === xml",
        "root.textContent === 'A😀<&' && cdata.length === 5 && cdata.nextSibling === outside && root.contains(cdata)",
        "root.innerHTML === '<![CDATA[A😀<&]]><?xml-stylesheet ?>'",
        "xml.createTreeWalker(root, NodeFilter.SHOW_CDATA_SECTION).nextNode() === cdata && xml.createTreeWalker(xml, NodeFilter.SHOW_PROCESSING_INSTRUCTION).nextNode() === pi",
        "errorName(() => new CDATASection()) === 'TypeError' && errorName(() => new ProcessingInstruction()) === 'TypeError'",
        "errorName(() => xml.createCDATASection(']]>')) === 'InvalidCharacterError' && errorName(() => document.createCDATASection(']]>')) === 'NotSupportedError'",
        "errorName(() => xml.appendChild(cdata)) === 'HierarchyRequestError' && cdata.parentNode === root",
        "errorName(() => document.createProcessingInstruction('1abc', '')) === 'InvalidCharacterError' && errorName(() => new ProcessingInstruction('name','?>')) === 'InvalidCharacterError'",
        r"errorName(() => new ProcessingInstruction('a\ud800')) === 'InvalidCharacterError' && new ProcessingInstruction('a\ufffd').target === 'a\ufffd'",
        "errorName(() => xml.createProcessingInstruction('name')) === 'TypeError' && document.createProcessingInstruction('xml', undefined).data === 'undefined'",
        "errorName(() => Object.getOwnPropertyDescriptor(ProcessingInstruction.prototype,'target').get.call(cdata)) === 'TypeError'",
        "pi.cloneNode(false).target === pi.target && pi.cloneNode(false).ownerDocument === xml && cdata.cloneNode().constructor === CDATASection",
        "document.importNode(cdata).ownerDocument === document && document.importNode(pi).target === pi.target",
        "root.cloneNode(true).lastChild instanceof ProcessingInstruction && root.cloneNode(true).textContent === 'A😀<&'",
    ] {
        assert_eq!(runtime.eval(check).unwrap(), Value::Bool(true), "{check}");
    }
    let changed = runtime.eval(r#"
        const observer = new MutationObserver(() => {});
        observer.observe(root, {characterData: true, characterDataOldValue: true, subtree: true});
        cdata.nodeValue = "B<&";
        outside.textContent = "after";
        const records = observer.takeRecords();
        const host = document.querySelector("main");
        host.appendChild(cdata);
        host.appendChild(outside);
        records.length === 2 && records[0].target === cdata && records[0].oldValue === "A😀<&" &&
        records[1].target === outside && outside.data === "after" && outside.nodeValue === "after" &&
        host.textContent === "B<&" && host.innerHTML === "B&lt;&amp;<?xml-stylesheet after?>" &&
        cdata.ownerDocument === document && cdata instanceof CDATASection
    "#).unwrap();
    assert_eq!(changed, Value::Bool(true));
}
