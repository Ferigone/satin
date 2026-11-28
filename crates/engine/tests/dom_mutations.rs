use rust_browser_engine::{
    LoadedPage,
    dom::{Document, NodeKind},
    script::ScriptRuntime,
};
use rust_browser_javascript::{ErrorKind, Value};

fn realm(source: &str) -> (LoadedPage, ScriptRuntime) {
    let page = LoadedPage::from_html("https://example.test/dom", source);
    let runtime = ScriptRuntime::new(&page).unwrap();
    (page, runtime)
}

#[test]
fn namespaced_svg_attributes_follow_the_local_name_and_emit_mutations() {
    let (_, mut runtime) = realm("<svg><use id='target' xlink:href='#old'></use></svg>");
    js(&mut runtime, r##"
        const xlink = "http://www.w3.org/1999/xlink";
        const target = document.getElementById("target");
        const observer = new MutationObserver(() => {});
        observer.observe(target, { attributes: true, attributeOldValue: true });
        const initial = target.getAttributeNS(xlink, "href") === "#old";
        target.setAttributeNS(xlink, "xlink:href", "#new");
        target.setAttributeNS("", "viewBox", "0 0 4 4");
        let invalid = false;
        try { target.setAttributeNS(null, "xlink:title", "bad"); }
        catch (error) { invalid = error.name === "NamespaceError"; }
        const set = target.getAttribute("xlink:href") === "#new" &&
            target.hasAttributeNS(xlink, "href") && !target.hasAttributeNS(null, "href") &&
            target.getAttributeNS(null, "viewBox") === "0 0 4 4" &&
            !target.hasAttributeNS(null, "viewbox");
        target.removeAttributeNS(xlink, "href");
        const records = observer.takeRecords();
        initial && invalid && set && target.getAttributeNS(xlink, "href") === null &&
            records.length === 3 && records[0].attributeName === "href" &&
            records[0].attributeNamespace === xlink && records[0].oldValue === "#old" &&
            records[2].oldValue === "#new"
    "##, true);
}

#[test]
fn connected_inline_stylesheet_rules_change_rendered_style_without_editing_the_dom() {
    let (_, mut runtime) = realm("<main id='target'></main>");
    js(&mut runtime, r#"
        const element = document.createElement("style");
        const detached = element.sheet === null;
        document.head.appendChild(element);
        const sheet = element.sheet;
        const target = document.getElementById("target");
        const initial = sheet instanceof CSSStyleSheet && element instanceof HTMLStyleElement &&
            sheet === element.sheet && sheet.ownerNode === element && sheet.cssRules.length === 0;
        const index = sheet.insertRule("main { color: green; }", 0);
        const applied = getComputedStyle(target).color === "rgb(0, 128, 0)" &&
            sheet.cssRules.length === 1 && sheet.cssRules[0].parentStyleSheet === sheet &&
            element.textContent === "";
        sheet.disabled = true;
        const disabled = getComputedStyle(target).color !== "rgb(0, 128, 0)";
        sheet.disabled = false;
        const reenabled = getComputedStyle(target).color === "rgb(0, 128, 0)";
        sheet.deleteRule(0);
        const removed = sheet.cssRules.length === 0 &&
            getComputedStyle(target).color !== "rgb(0, 128, 0)";
        detached && initial && index === 0 && applied && disabled && reenabled && removed
    "#, true);
}

#[test]
fn div_and_iframe_elements_use_distinct_html_interfaces() {
    let (_, mut runtime) = realm("<div id='parsed' align='left'></div><span id='other'></span><iframe id='frame'></iframe>");
    js(&mut runtime, r#"
        const parsed = document.getElementById("parsed");
        const dynamic = document.createElement("div");
        const other = document.getElementById("other");
        const iframe = document.getElementById("frame");
        dynamic.align = "right";
        parsed instanceof HTMLDivElement &&
            dynamic instanceof HTMLDivElement &&
            !(other instanceof HTMLDivElement) &&
            Object.getPrototypeOf(HTMLDivElement) === HTMLElement &&
            iframe instanceof HTMLIFrameElement && !(other instanceof HTMLIFrameElement) &&
            Object.getPrototypeOf(HTMLIFrameElement) === HTMLElement &&
            iframe.contentDocument !== null &&
            parsed.align === "left" && dynamic.getAttribute("align") === "right" &&
            Object.prototype.toString.call(parsed) === "[object HTMLDivElement]"
    "#, true);
}

#[test]
fn anchors_resolve_their_href_and_buttons_keep_their_native_brand() {
    let (_, mut runtime) = realm("<base href='/path/'><a id='link' href='clip'>Link</a><button id='btn' value='go'></button>");
    js(&mut runtime, r##"
        const link = document.getElementById("link"), btn = document.getElementById("btn");
        const initial = link.href === "https://example.test/path/clip" && String(link) === link.href;
        link.href = "#time";
        link.download = "clip.mp4";
        initial && link instanceof HTMLAnchorElement && btn instanceof HTMLButtonElement &&
            link.getAttribute("href") === "#time" && link.href === "https://example.test/path/#time" &&
            link.getAttribute("download") === "clip.mp4" && btn.value === "go"
    "##, true);
}

#[test]
fn typed_form_controls_expose_prototype_accessors_used_by_web_frameworks() {
    let (_, mut runtime) = realm("<input id='input'><textarea id='area'>first</textarea><select id='select'><option>one</option><option>two</option></select>");
    js(&mut runtime, r#"
        const accessor = (type, name) => Object.getOwnPropertyDescriptor(type.prototype, name);
        const area = document.getElementById("area");
        const select = document.getElementById("select");
        const option = select.options[1];
        const input = document.getElementById("input");
        accessor(HTMLTextAreaElement, "value").set.call(area, "next");
        accessor(HTMLOptionElement, "selected").set.call(option, true);
        accessor(HTMLInputElement, "checked").set.call(input, true);
        let receiverRejected = false;
        try { accessor(HTMLSelectElement, "value").get.call(area); }
        catch (error) { receiverRejected = error instanceof TypeError; }
        area instanceof HTMLTextAreaElement && select instanceof HTMLSelectElement &&
            option instanceof HTMLOptionElement && area.value === "next" &&
            select.selectedIndex === 1 && select.value === "two" && input.checked && receiverRejected
    "#, true);
}

#[test]
fn dataset_reflects_attributes_and_keeps_name_order_and_wrapper_identity() {
    let (_, mut runtime) = realm("<div id='target' data-foo-bar='one' data-order='two'></div>");
    js(&mut runtime, r#"
        const target = document.getElementById("target");
        const data = target.dataset;
        const initial = Object.keys(data).join(",") === "fooBar,order" && data.fooBar === "one";
        data.version = 123;
        data.toString = "custom";
        target.setAttribute("data-order", "updated");
        delete data.fooBar;
        let invalid = false;
        try { data["bad-key"] = "no"; } catch (error) { invalid = error.name === "SyntaxError"; }
        initial && data === target.dataset && data instanceof DOMStringMap &&
            target.getAttribute("data-version") === "123" && data.order === "updated" &&
            data.toString === "custom" && !target.hasAttribute("data-foo-bar") &&
            Object.keys(data).join(",") === "order,version,toString" && invalid
    "#, true);
}

#[test]
fn range_tracks_valid_boundaries_and_parses_contextual_fragments() {
    let (_, mut runtime) = realm("<main><p id='p'>Hi 😀</p><p>there</p></main>");
    js(&mut runtime, r#"
        const p = document.getElementById("p"), r = document.createRange();
        const initiallyCollapsed = r instanceof Range && r.collapsed && r.startContainer === document;
        r.selectNode(p);
        const parsed = r.createContextualFragment("<em>part</em>");
        const copy = r.cloneRange();
        copy.selectNodeContents(p.firstChild);
        let invalid = false;
        try { copy.setEnd(p.firstChild, 6); } catch(error) { invalid = error.name === "IndexSizeError"; }
        const compared = r.compareBoundaryPoints(Range.START_TO_START, copy) === -1;
        copy.collapse(false);
        initiallyCollapsed && compared && invalid && parsed.firstChild.tagName === "EM" &&
            parsed.firstChild.textContent === "part" && r.commonAncestorContainer === p.parentNode &&
            copy.collapsed && copy.startContainer === p.firstChild && copy.startOffset === 5 &&
            new Range().endContainer === document
    "#, true);
}
fn js(runtime: &mut ScriptRuntime, source: &str, expected: impl Into<Value>) {
    assert_eq!(
        runtime
            .eval(source)
            .unwrap_or_else(|e| panic!("{e}\n{source}")),
        expected.into()
    );
}
fn arena(document: &Document) {
    let mut inbound = vec![0; document.nodes.len()];
    for (index, node) in document.nodes.iter().enumerate() {
        for &child in &node.children {
            assert_eq!(document.nodes[child.0].parent.map(|id| id.0), Some(index));
            inbound[child.0] += 1;
            assert_eq!(inbound[child.0], 1);
        }
    }
    for (index, node) in document.nodes.iter().enumerate() {
        assert_eq!(node.parent.is_some(), inbound[index] == 1);
    }
    let mut pending = vec![document.root];
    let mut seen = vec![false; document.nodes.len()];
    while let Some(id) = pending.pop() {
        assert!(!seen[id.0], "cycle or duplicate ownership");
        seen[id.0] = true;
        pending.extend(document.nodes[id.0].children.iter().copied());
        if let Some(&content) = document.template_contents.get(&id) {
            assert!(document.nodes[content.0].parent.is_none());
            assert!(matches!(document.nodes[content.0].kind, NodeKind::Document));
            pending.push(content);
        }
    }
}

#[test]
fn html_mutations_use_real_context_and_preserve_detached_identity() {
    let (mut page, mut runtime) =
        realm("<!doctype html><main id=host><b id=old>old</b></main><table id=table></table>");
    let old = page.document.elements("b").next().unwrap();
    runtime.eval("var host=document.getElementById('host');var old=document.getElementById('old');host.innerHTML='<p>A<p>B<b><i>C</b>D</i>';var table=document.getElementById('table');table.innerHTML='<tr><td>A<td>B';").unwrap();
    js(
        &mut runtime,
        "host.innerHTML",
        "<p>A</p><p>B<b><i>C</i></b><i>D</i></p>",
    );
    js(
        &mut runtime,
        "table.innerHTML",
        "<tbody><tr><td>A</td><td>B</td></tr></tbody>",
    );
    js(
        &mut runtime,
        "old.parentNode===null && old.parentElement===null && old.textContent==='old'",
        true,
    );
    runtime.eval("var first=host.firstChild;first.outerHTML='<span>X</span><!--c--><em>Y</em>';host.insertAdjacentHTML('afterbegin','<u>start</u>');host.insertAdjacentHTML('beforeend','<u>end</u>');").unwrap();
    js(&mut runtime, "first.parentNode===null", true);
    js(
        &mut runtime,
        "host.firstChild.outerHTML + ':' + host.lastChild.outerHTML",
        "<u>start</u>:<u>end</u>",
    );
    js(
        &mut runtime,
        "old===old.ownerDocument.getElementById('old')",
        false,
    );
    assert!(runtime.snapshot(&mut page));
    assert_eq!(page.document.text(old), "old");
    assert!(page.document.nodes[old.0].parent.is_none());
    arena(&page.document);
}

#[test]
fn fragments_flatten_without_losing_wrapper_identity_or_order() {
    let (mut page, mut runtime) =
        realm("<!doctype html><main id=host><b id=a>A</b><b id=b>B</b><b id=c>C</b></main>");
    runtime.eval("var host=document.getElementById('host');var a=document.getElementById('a');var b=document.getElementById('b');var c=document.getElementById('c');var f=document.createDocumentFragment();f.appendChild(a);f.appendChild(c);").unwrap();
    js(
        &mut runtime,
        "a.parentNode===f && a.parentElement===null && document.documentElement.parentElement===null",
        true,
    );
    runtime
        .eval("var returned=host.insertBefore(f,b);")
        .unwrap();
    js(
        &mut runtime,
        "returned===f && a.parentElement===host && f.firstChild===null && f.nodeType===11 && f.nodeName==='#document-fragment' && f.ownerDocument===document",
        true,
    );
    js(
        &mut runtime,
        "host.innerHTML",
        "<b id=\"a\">A</b><b id=\"c\">C</b><b id=\"b\">B</b>",
    );
    runtime.eval("host.insertBefore(a,a);host.insertBefore(b,a);var same=host.replaceChild(c,c);var empty=document.createDocumentFragment();var removed=host.replaceChild(empty,a);").unwrap();
    js(
        &mut runtime,
        "same===c && removed===a && a.parentNode===null && b.nextSibling===c && host.lastChild===c",
        true,
    );
    runtime
        .eval("f.textContent='fragment';host.appendChild(f);")
        .unwrap();
    js(
        &mut runtime,
        "host.lastChild.textContent + ':' + f.textContent",
        "fragment:",
    );
    runtime.snapshot(&mut page);
    arena(&page.document);
}

#[test]
fn variadic_parent_methods_convert_strings_and_move_duplicate_and_nested_arguments() {
    let (mut page, mut runtime) =
        realm("<!doctype html><main id=host><div id=a><b id=x>X</b></div><i id=b>B</i></main>");
    runtime.eval("var host=document.getElementById('host');var a=document.getElementById('a');var x=document.getElementById('x');var b=document.getElementById('b');host.append(a,x,'tail');host.prepend(b,'start',b);").unwrap();
    js(
        &mut runtime,
        "host.innerHTML",
        "start<i id=\"b\">B</i><div id=\"a\"></div><b id=\"x\">X</b>tail",
    );
    runtime.eval("var f=document.createDocumentFragment();f.append('one',a,'two');host.append(f);x.remove();").unwrap();
    js(
        &mut runtime,
        "host.innerHTML",
        "start<i id=\"b\">B</i>tailone<div id=\"a\"></div>two",
    );
    js(
        &mut runtime,
        "x.parentNode===null && f.firstChild===null",
        true,
    );
    let before = runtime.eval("host.innerHTML").unwrap();
    js(
        &mut runtime,
        "var result='';try{a.append('before',host,'after');}catch(e){result=e.name;}result",
        "HierarchyRequestError",
    );
    assert_eq!(runtime.eval("host.innerHTML").unwrap(), before);
    runtime.snapshot(&mut page);
    arena(&page.document);
}

#[test]
fn replace_children_converts_variadic_strings_and_fragments_and_preserves_identity() {
    let (mut page, mut runtime) =
        realm("<!doctype html><main id=host><div id=a><b id=x>X</b></div><em id=b>B</em></main>");
    runtime.eval("var host=document.getElementById('host');var a=document.getElementById('a');var x=document.getElementById('x');var b=document.getElementById('b');var fragment=document.createDocumentFragment();var italic=document.createElement('i');italic.textContent='I';fragment.append('fragment',italic);var result=host.replaceChildren(a,x,'tail',fragment,b,a);").unwrap();
    js(&mut runtime, "typeof result", "undefined");
    js(
        &mut runtime,
        "host.innerHTML",
        "<b id=\"x\">X</b>tailfragment<i>I</i><em id=\"b\">B</em><div id=\"a\"></div>",
    );
    js(
        &mut runtime,
        "fragment.firstChild===null && host.firstChild===x && host.lastChild===a && italic.parentNode===host && b.parentNode===host",
        true,
    );
    runtime
        .eval("var former=host.childNodes;var formerChild=former[1];host.replaceChildren();")
        .unwrap();
    js(
        &mut runtime,
        "host.childNodes===former && former.length===0 && former[1]===undefined && a.parentNode===null && b.parentNode===null && x.parentNode===null && italic.parentNode===null && formerChild.parentNode===null",
        true,
    );
    runtime
        .eval("fragment.replaceChildren('new',a);host.replaceChildren(fragment);")
        .unwrap();
    js(&mut runtime, "host.innerHTML", "new<div id=\"a\"></div>");
    js(
        &mut runtime,
        "fragment.firstChild===null && host.lastChild===a",
        true,
    );
    runtime.snapshot(&mut page);
    arena(&page.document);
}

#[test]
fn replace_children_validates_document_fragment_hierarchy_before_mutation() {
    let (mut page, mut runtime) = realm("<!doctype html><main id=host>old</main>");
    runtime.eval("var original=document.documentElement;var doctype=document.firstChild;var host=document.getElementById('host');var fragment=document.createDocumentFragment();var first=document.createElement('first');var second=document.createElement('second');fragment.append(first,second);var result='';").unwrap();
    js(
        &mut runtime,
        "try{document.replaceChildren(fragment);}catch(e){result=e.name;}result",
        "HierarchyRequestError",
    );
    js(
        &mut runtime,
        "document.documentElement===original && original.parentNode===document && doctype.parentNode===document && fragment.firstChild===first && fragment.lastChild===second",
        true,
    );
    js(
        &mut runtime,
        "try{fragment.replaceChildren(doctype);}catch(e){result=e.name;}result",
        "HierarchyRequestError",
    );
    js(
        &mut runtime,
        "doctype.parentNode===document && fragment.firstChild===first && fragment.lastChild===second",
        true,
    );
    js(
        &mut runtime,
        "try{host.replaceChildren('before',original,'after');}catch(e){result=e.name;}result",
        "HierarchyRequestError",
    );
    js(
        &mut runtime,
        "host.textContent==='old' && document.documentElement===original",
        true,
    );
    runtime.eval("document.replaceChildren();").unwrap();
    js(
        &mut runtime,
        "document.childNodes.length===0 && original.parentNode===null && doctype.parentNode===null",
        true,
    );
    runtime.eval("document.replaceChildren(first);").unwrap();
    js(
        &mut runtime,
        "document.firstChild===first && document.childNodes.length===1 && fragment.firstChild===second",
        true,
    );
    runtime.snapshot(&mut page);
    arena(&page.document);
}

#[test]
fn replace_children_depth_failure_keeps_old_children_and_does_not_import_strings() {
    let (mut page, mut runtime) = realm("<!doctype html><main id=host></main>");
    runtime.eval("var host=document.getElementById('host');var branch=document.createElement('b');var leaf=branch;for(var i=0;i<120;i++){var child=document.createElement('b');leaf.appendChild(child);leaf=child;}host.appendChild(branch);var target=document.createElement('div');var tip=target;for(var i=0;i<140;i++){var child=document.createElement('div');tip.appendChild(child);tip=child;}host.appendChild(target);tip.textContent='old';var old=tip.firstChild;").unwrap();
    runtime.snapshot(&mut page);
    let count = page.document.nodes.len();
    let before = page.document.serialize();
    let error = runtime
        .eval("tip.replaceChildren('new',branch)")
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::HeapLimit);
    js(
        &mut runtime,
        "branch.parentNode===host && tip.firstChild===old && tip.textContent==='old'",
        true,
    );
    assert!(!runtime.snapshot(&mut page));
    assert_eq!(page.document.nodes.len(), count);
    assert_eq!(page.document.serialize(), before);
    arena(&page.document);
}

#[test]
fn detached_templates_import_and_clone_inert_contents() {
    let (mut page, mut runtime) = realm(
        "<!doctype html><main id=host></main><template id=parsed><b id=inside>hidden</b><template><i>nested</i></template></template>",
    );
    runtime.eval("var host=document.getElementById('host');var t=document.createElement('template');t.innerHTML='<tr><td>X</td></tr><script>throw new Error(\"must stay inert\")</script>';var content=t.content;var deep=t.cloneNode(true);var shallow=t.cloneNode(false);host.appendChild(t);host.appendChild(deep);var parsed=document.getElementById('parsed');var pc=parsed.cloneNode(true);host.appendChild(pc);").unwrap();
    js(&mut runtime, r#"
        var proto=HTMLTemplateElement.prototype, descriptor=Object.getOwnPropertyDescriptor(proto,'content');
        var brand=false, illegal=false, wrong=document.createElement('div');
        try { descriptor.get.call(wrong); } catch(e) { brand=e instanceof TypeError; }
        try { new HTMLTemplateElement(); } catch(e) { illegal=e instanceof TypeError; }
        Object.getPrototypeOf(HTMLTemplateElement)===HTMLElement &&
        Object.getPrototypeOf(proto)===HTMLElement.prototype &&
        [parsed,t,deep,shallow,pc].every(n=>Object.getPrototypeOf(n)===proto) &&
        descriptor.get.call(t)===t.content && t.content instanceof DocumentFragment &&
        Object.prototype.toString.call(t)==='[object HTMLTemplateElement]' &&
        !('content' in wrong) && brand && illegal;
    "#, true);
    js(
        &mut runtime,
        "content===t.content && content.nodeType===11 && content.parentNode===null && t.firstChild===null",
        true,
    );
    js(
        &mut runtime,
        "shallow.content.firstChild===null && deep.content!==content && deep.innerHTML===t.innerHTML",
        true,
    );
    js(
        &mut runtime,
        "document.getElementById('inside')===null && parsed.content.querySelector('#inside').textContent==='hidden'",
        true,
    );
    js(
        &mut runtime,
        "pc.content.querySelector('template').content.querySelector('i').textContent",
        "nested",
    );
    js(&mut runtime, "host.textContent", "");
    assert!(runtime.diagnostics().is_empty());
    runtime.snapshot(&mut page);
    assert_eq!(page.document.template_contents.len(), 7);
    arena(&page.document);
}

#[test]
fn cloning_copies_attributes_but_not_listeners_or_property_handlers() {
    let (mut page, mut runtime) =
        realm("<!doctype html><main><button id=b onclick='calls+=10'>B</button></main>");
    let original = page.document.elements("button").next().unwrap();
    runtime.eval("var calls=0;var b=document.getElementById('b');b.addEventListener('click',function(){calls+=100;});b.onclick=function(){calls+=1000;};var copy=b.cloneNode(true);copy.id='copy';b.parentNode.appendChild(copy);").unwrap();
    runtime.snapshot(&mut page);
    let copy = page
        .document
        .elements("button")
        .find(|&id| page.document.attr(id, "id") == Some("copy"))
        .unwrap();
    runtime.click(copy);
    js(&mut runtime, "calls", 10.);
    runtime.click(original);
    js(&mut runtime, "calls", 1110.);
    arena(&page.document);
}

#[test]
fn hierarchy_notfound_and_adjacent_errors_are_domexceptions_and_atomic() {
    let (mut page, mut runtime) =
        realm("<!doctype html><main id=host><b id=a>A</b><i id=b>B</i></main>");
    runtime.eval("var host=document.getElementById('host');var a=document.getElementById('a');var b=document.getElementById('b');var before=host.innerHTML;").unwrap();
    js(
        &mut runtime,
        "var result='';try{a.appendChild(host);}catch(e){result=e.name+':'+e.code+':'+(e instanceof DOMException)+':'+(e instanceof Error);}result",
        "HierarchyRequestError:3:true:true",
    );
    js(
        &mut runtime,
        "try{host.insertBefore(a,document.body);}catch(e){result=e.name;}result",
        "NotFoundError",
    );
    js(
        &mut runtime,
        "try{host.removeChild(document.body);}catch(e){result=e.name;}result",
        "NotFoundError",
    );
    js(
        &mut runtime,
        "try{host.insertAdjacentHTML('bad','<p>x');}catch(e){result=e.name+':'+e.code;}result",
        "SyntaxError:12",
    );
    js(
        &mut runtime,
        "try{document.documentElement.outerHTML='<div></div>';}catch(e){result=e.name;}result",
        "NoModificationAllowedError",
    );
    js(
        &mut runtime,
        "try{document.appendChild(document.createTextNode('text'));}catch(e){result=e.name;}result",
        "HierarchyRequestError",
    );
    js(
        &mut runtime,
        "try{document.appendChild(document.createElement('second'));}catch(e){result=e.name;}result",
        "HierarchyRequestError",
    );
    js(
        &mut runtime,
        "try{a.appendChild(document);}catch(e){result=e.name;}result",
        "HierarchyRequestError",
    );
    js(
        &mut runtime,
        "host.innerHTML===before && a.parentNode===host && b.parentNode===host",
        true,
    );
    runtime
        .eval("var Saved=DOMException;DOMException=function(){};")
        .unwrap();
    js(
        &mut runtime,
        "try{a.appendChild(host);}catch(e){result=e instanceof Saved;}result",
        true,
    );
    runtime.snapshot(&mut page);
    arena(&page.document);
}

#[test]
fn imported_html_keeps_scripts_inert_and_form_and_raw_context_correct() {
    let (mut page, mut runtime) = realm(
        "<!doctype html><form id=form><div id=host></div></form><textarea id=raw></textarea>",
    );
    runtime.eval("var host=document.getElementById('host');host.innerHTML='<form id=ignored><input></form><script>globalThis.bad=true</script>';var raw=document.getElementById('raw');raw.innerHTML='&amp;</textarea><b>X';").unwrap();
    js(
        &mut runtime,
        "document.getElementById('ignored')===null && typeof bad==='undefined'",
        true,
    );
    js(&mut runtime, "raw.textContent", "&</textarea><b>X");
    js(
        &mut runtime,
        "var detached=document.createElement('div');detached.innerHTML='<p>old';detached.outerHTML='<i>new';detached.innerHTML",
        "<p>old</p>",
    );
    runtime.eval("host.innerHTML=null;").unwrap();
    js(&mut runtime, "host.innerHTML", "");
    runtime.snapshot(&mut page);
    arena(&page.document);
}

#[test]
fn moving_deep_subtrees_and_template_ownership_fails_before_detach() {
    let (mut page, mut runtime) = realm("<!doctype html><main id=host></main>");
    runtime.eval("var host=document.getElementById('host');var branch=document.createElement('b');var leaf=branch;for(var i=0;i<120;i++){var child=document.createElement('b');leaf.appendChild(child);leaf=child;}host.appendChild(branch);var target=document.createElement('div');var tip=target;for(var i=0;i<140;i++){var child=document.createElement('div');tip.appendChild(child);tip=child;}host.appendChild(target);").unwrap();
    let error = runtime.eval("tip.appendChild(branch)").unwrap_err();
    assert_eq!(error.kind, ErrorKind::HeapLimit);
    js(
        &mut runtime,
        "branch.parentNode===host && target.parentNode===host && leaf.parentNode!==null && tip.firstChild===null",
        true,
    );
    runtime.eval("var t=document.createElement('template');t.innerHTML='<template><span>X</span></template>';host.appendChild(t);").unwrap();
    js(
        &mut runtime,
        "var result='';try{t.content.appendChild(t);}catch(e){result=e.name;}result",
        "HierarchyRequestError",
    );
    js(
        &mut runtime,
        "t.parentNode===host && t.content.firstChild.tagName==='TEMPLATE'",
        true,
    );
    runtime.snapshot(&mut page);
    arena(&page.document);
}

#[test]
fn native_dom_traversal_and_copying_share_the_script_task_budget() {
    let source = format!(
        "<!doctype html><main id=host>{}</main>",
        format!("<b>{}</b>", "x".repeat(64)).repeat(100)
    );
    let (_, mut runtime) = realm(&source);
    runtime
        .eval("var host=document.getElementById('host');var copies=0;")
        .unwrap();
    let error = runtime
        .eval("for(var i=0;i<2000;i++){host.cloneNode(true);copies++;}")
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
    let copies = runtime.eval("copies").unwrap().as_number().unwrap();
    assert!(copies > 0. && copies < 500.);
    let error = runtime
        .eval("for(var i=0;i<10000;i++)host.innerHTML;")
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
    js(&mut runtime, "1+2", 3.);
}

#[test]
fn domexception_construction_required_arguments_and_xml_name_validation() {
    let (_, mut runtime) = realm("<!doctype html><main id=host></main>");
    js(
        &mut runtime,
        "var e=new DOMException('stopped','AbortError');e.name+':'+e.code+':'+(e instanceof DOMException)+':'+(e instanceof Error)",
        "AbortError:20:true:true",
    );
    js(
        &mut runtime,
        r"document.createElement('\u{1f980}').tagName",
        "🦀",
    );
    js(
        &mut runtime,
        r"document.createElement('a\u0301').tagName",
        "Á",
    );
    for source in [
        r"document.createElement('\ud800')",
        r"document.createElement('\u0301')",
        "document.createElement('1bad')",
    ] {
        js(
            &mut runtime,
            &format!("var result='';try{{{source};}}catch(e){{result=e.name+':'+e.code;}}result"),
            "InvalidCharacterError:5",
        );
    }
    for source in [
        "document.createElement()",
        "document.body.insertBefore(document.createElement('b'))",
        "document.body.insertAdjacentHTML('beforeend')",
    ] {
        js(
            &mut runtime,
            &format!("try{{{source};}}catch(e){{result=e instanceof TypeError;}}result"),
            true,
        );
    }
}

#[test]
fn class_list_tracks_attribute_edits_and_validates_before_mutating() {
    let (_, mut runtime) = realm("<!doctype html><div id=target class='  a   b a  '></div>");
    js(
        &mut runtime,
        r#"
        const element = document.getElementById('target');
        const list = element.classList;
        list instanceof DOMTokenList && element.classList === list &&
            list.length === 2 && list.value === '  a   b a  ' && String(list) === list.value &&
            list[0] === 'a' && list.item(1) === 'b' && list.item(2) === null &&
            list[2] === undefined && Array.from(list).join(',') === 'a,b' &&
            !list.contains('a b')
    "#,
        true,
    );
    js(
        &mut runtime,
        r#"
        const observer = new MutationObserver(() => {});
        observer.observe(element, { attributes: true, attributeOldValue: true });
        list.add('b', 'c', 'd');
        const changes = observer.takeRecords();
        changes.length === 1 && changes[0].oldValue === '  a   b a  ' &&
            element.className === 'a b c d' && document.querySelector('.d') === element &&
            list.replace('a', 'c') && list.value === 'c b d' && list.replace('d', 'c') &&
            !list.replace('missing', 'x') && list.value === 'c b'
    "#,
        true,
    );
    js(
        &mut runtime,
        r#"
        list.remove('c', 'missing');
        list.toggle('b', true) && !list.toggle('absent', false) &&
            !list.toggle('b') && list.toggle('x', undefined) && element.className === 'x'
    "#,
        true,
    );
    js(
        &mut runtime,
        r#"
        const errors = [];
        for (const run of [
            () => list.add('premature', 'bad token'),
            () => list.remove('x', ''),
            () => list.replace('bad token', ''),
            () => list.toggle('bad\ttoken')
        ]) {
            try { run(); } catch (err) { errors.push(err instanceof DOMException && err.name); }
        }
        errors.join() === 'InvalidCharacterError,SyntaxError,SyntaxError,InvalidCharacterError' &&
            list.value === 'x'
    "#,
        true,
    );
    js(
        &mut runtime,
        r#"
        element.classList = 'first  first second';
        const forwarded = list.value === 'first  first second' && list.item(1) === 'second';
        list.value = 'from-list';
        const reflected = element.getAttribute('class') === 'from-list';
        element.removeAttribute('class');
        list.remove('absent');
        forwarded && reflected && list.length === 0 && !element.hasAttribute('class')
    "#,
        true,
    );
}

#[test]
fn document_fragment_constructor_brands_real_fragments_and_supports_subclasses() {
    let (_, mut runtime) = realm("<!doctype html><template id=tmpl><p>template</p></template>");
    js(
        &mut runtime,
        r#"
        const proto = DocumentFragment.prototype;
        const a = new DocumentFragment(), b = document.createDocumentFragment();
        class Section extends DocumentFragment {}
        const section = new Section();
        const elem = document.createElement("article");
        section.append(elem);
        const copy = section.cloneNode(true);
        document.body.append(section);
        let needsNew = false;
        try { DocumentFragment(); } catch (err) { needsNew = err instanceof TypeError; }
        function F() {}
        F.prototype = null;
        const fallback = Reflect.construct(DocumentFragment, [], F);
        needsNew && Object.getPrototypeOf(DocumentFragment) === Node &&
        Object.getPrototypeOf(proto) === Node.prototype && proto.constructor === DocumentFragment &&
        document.createElement('template').content.cloneNode() instanceof DocumentFragment &&
        a !== b && [a,b,document.getElementById('tmpl').content,copy].every(
            f => Object.getPrototypeOf(f) === proto && f instanceof Node &&
                 f.nodeType === 11 && f.ownerDocument === document) &&
        Object.prototype.toString.call(a) === '[object DocumentFragment]' &&
        !(document instanceof DocumentFragment) && !(elem instanceof DocumentFragment) &&
        section instanceof Section && section instanceof DocumentFragment &&
        section.firstChild === null && document.body.lastChild === elem &&
        copy.firstChild.tagName === 'ARTICLE' && Object.getPrototypeOf(fallback) === proto
    "#,
        true,
    );
}

#[test]
fn import_node_copies_template_contents_into_detached_target_nodes() {
    let (mut page, mut runtime) = realm(
        "<!doctype html><main></main><template id=t><b>Hi</b><!--marker--><template><i>nested</i></template></template>",
    );
    js(
        &mut runtime,
        r#"
        const source = document.getElementById('t').content;
        const copy = document.importNode(source, true);
        const section = document.querySelector('main');
        const bold = copy.firstChild;
        const nested = copy.lastChild.content.firstChild;
        const owned = copy.ownerDocument === document && bold.ownerDocument === document;
        section.insertBefore(copy, null);
        owned && copy.nodeType === 11 && copy.parentNode === null && copy.firstChild === null &&
        bold === section.firstChild && bold !== source.firstChild && bold.textContent === 'Hi' &&
        nested.textContent === 'nested' && nested !== source.lastChild.content.firstChild &&
        section.firstChild.nextSibling.nodeType === 8 && source.firstChild.parentNode === source
    "#,
        true,
    );
    js(
        &mut runtime,
        r#"
        const name = f => {try { f(); return ''; } catch(e) { return e.name; }};
        document.importNode.length === 1 &&
        document.importNode(source).firstChild === null &&
        document.importNode(source, {selfOnly:true}).firstChild === null &&
        document.importNode(source, {}).firstChild.textContent === 'Hi' &&
        name(() => document.importNode(document)) === 'NotSupportedError' &&
        name(() => document.importNode({nodeType:11})) === 'TypeError' &&
        name(() => document.importNode.call(source, source)) === 'TypeError'
    "#,
        true,
    );
    runtime.snapshot(&mut page);
    arena(&page.document);
}

#[test]
fn import_node_reads_the_source_frame_arena_and_keeps_executed_scripts_inert() {
    let (mut page, mut runtime) =
        realm("<!doctype html><main><u>different tree</u></main><iframe></iframe>");
    js(
        &mut runtime,
        r#"
        const child = document.querySelector('iframe').contentDocument;
        child.write("<b id='book'>Frame<!--marker--><i>text</i><script>parent.count++<" + "/script></b>");
        child.close();
        const original = child.getElementById('book');
        const imported = document.importNode(original, true);
        const target = document.querySelector('main');
        var count = 0;
        target.replaceChildren(imported);
        child.open(); child.write("<p>New frame document</p>"); child.close();
        imported.tagName === 'B' && imported.ownerDocument === document &&
        imported.firstChild.nodeValue === 'Frame' && imported.querySelector('i').textContent === 'text' &&
        count === 0 && document.importNode(original, true).innerHTML === imported.innerHTML &&
        document.importNode(original, false).firstChild === null
    "#,
        true,
    );
    js(
        &mut runtime,
        "try { document.importNode(child); false } catch(e) { e instanceof DOMException && e.name==='NotSupportedError' }",
        true,
    );
    runtime.snapshot(&mut page);
    arena(&page.document);
}

#[test]
fn attribute_names_follow_source_and_mutation_order_across_template_imports() {
    let (_, mut runtime) = realm("<!doctype html><main></main>");
    let actual = runtime.eval(r##"
        (() => {
            const template = document.createElement('template');
            template.innerHTML = '<a Z=first a=1 z=duplicate b=2></a><svg><g viewBox="1" xlink:href="#x"/></svg>';
            const f = document.importNode(template.content, true);
            const a = f.firstChild, empty = document.createElement('aside'), g = f.lastChild.firstChild;
            const names = a.getAttributeNames();
            a.setAttribute('a', 'updated');
            const afterSet = a.getAttributeNames();
            a.removeAttribute('z'); a.setAttribute('Z', 'last');
            let brand = '';
            try { Element.prototype.hasAttributes.call(template.content); } catch (e) { brand = e.name; }
            return [names, afterSet, a.getAttributeNames(), a.cloneNode().getAttributeNames(),
                    template.content.firstChild.getAttribute('z'), g.getAttributeNames(),
                    a.hasAttributes(), empty.hasAttributes(), empty.getAttributeNames().length, brand].join('|');
        })()
    "##).unwrap();
    assert_eq!(
        actual,
        Value::from("z,a,b|z,a,b|a,b,z|a,b,z|first|viewBox,xlink:href|true|false|0|TypeError")
    );
}

#[test]
fn toggle_attribute_returns_presence_and_only_delivers_actual_mutations() {
    let (_, mut runtime) = realm("<!doctype html><body></body>");
    let actual = runtime.eval(r#"
        (() => {
            const changes = [];
            class Panel extends HTMLElement {
                static get observedAttributes() { return ['visible']; }
                attributeChangedCallback(name, before, after) { changes.push([name,before,after]); }
            }
            customElements.define('x-toggle-test', Panel);
            const p = document.createElement('x-toggle-test');
            const added = p.toggleAttribute('VISIBLE', true);
            p.setAttribute('visible','kept');
            const kept = p.toggleAttribute('visible', true) && p.getAttribute('visible') === 'kept';
            const removed = p.toggleAttribute('visible');
            const missing = p.toggleAttribute('visible', false);
            const again = p.toggleAttribute('visible', undefined);
            const xml = new Document().createElement('Item');
            xml.toggleAttribute('MixedCase');
            let invalid = '';
            try { p.toggleAttribute('bad name'); } catch (e) { invalid = e.name; }
            return [added,kept,removed,missing,again,p.getAttribute('visible') === '',
                    JSON.stringify(changes),xml.hasAttribute('MixedCase'),xml.hasAttribute('mixedcase'),
                    p.toggleAttribute('@click'),invalid].join('|');
        })()
    "#).unwrap();
    assert_eq!(
        actual,
        Value::from(
            r#"true|true|false|false|true|true|[["visible",null,""],["visible","","kept"],["visible","kept",null],["visible",null,""]]|true|false|true|InvalidCharacterError"#
        )
    );
}

#[test]
fn range_delete_contents_removes_selected_nodes_and_collapses() {
    let (_, mut runtime) = realm("<main><div id='frame'><p>old</p><!--marker--><span>stale</span></div><p id='after'>keep</p></main>");
    js(&mut runtime, r#"
        const frame = document.getElementById("frame");
        const old = frame.firstChild;
        const observer = new MutationObserver(() => {});
        observer.observe(frame, {childList: true});
        const range = document.createRange();
        range.selectNodeContents(frame);
        const returned = range.deleteContents();
        const records = observer.takeRecords();
        returned === undefined && frame.childNodes.length === 0 &&
            old.parentNode === null && old.textContent === "old" &&
            range.collapsed && range.startContainer === frame && range.startOffset === 0 &&
            document.getElementById("after").textContent === "keep" &&
            records.length === 3 && records[0].removedNodes[0] === old
    "#, true);
}

#[test]
fn range_delete_contents_preserves_partial_ancestors_and_utf16_boundaries() {
    let (_, mut runtime) = realm("<main id='root'><p id='a'>A😀B</p><i>remove</i><p id='b'>CDE</p></main>");
    js(&mut runtime, r#"
        const root = document.getElementById("root");
        const a = document.getElementById("a"), b = document.getElementById("b");
        const r = document.createRange();
        r.setStart(a.firstChild, 1);
        r.setEnd(b.firstChild, 2);
        r.deleteContents();
        const cross = root.children.length === 2 && a.textContent === "A" &&
            b.textContent === "E" && r.collapsed && r.startContainer === root && r.startOffset === 1;
        const t = document.createTextNode("A😀B");
        r.setStart(t, 1);
        r.setEnd(t, 3);
        r.deleteContents();
        const same = t.data === "AB" && r.collapsed && r.startContainer === t && r.startOffset === 1;
        r.deleteContents();
        cross && same && t.data === "AB"
    "#, true);
}

#[test]
fn range_extract_contents_moves_nodes_and_copies_partial_ancestors() {
    let (mut page, mut runtime) = realm("<main id='root'><p id='a'>A😀B</p><i>move</i><p id='b'>CDE</p></main>");
    js(&mut runtime, r#"
        const root = document.getElementById('root');
        const a = document.getElementById('a'), b = document.getElementById('b');
        const middle = root.children[1];
        const r = document.createRange();
        r.setStart(a.firstChild, 1);
        r.setEnd(b.firstChild, 2);
        const f = r.extractContents();
        f instanceof DocumentFragment && f.ownerDocument === document &&
            f.firstChild !== a && f.firstChild.textContent === '😀B' &&
            f.children[1] === middle && middle.parentNode === f &&
            f.lastChild !== b && f.lastChild.textContent === 'CD' &&
            root.children.length === 2 && a.textContent === 'A' && b.textContent === 'E' &&
            r.collapsed && r.startContainer === root && r.startOffset === 1
    "#, true);
    runtime.snapshot(&mut page);
    arena(&page.document);
}

#[test]
fn range_extract_contents_handles_text_empty_and_other_documents() {
    let (_, mut runtime) = realm("<!doctype html><main></main>");
    js(&mut runtime, r#"
        const r = document.createRange();
        const t = document.createTextNode('A😀B');
        r.setStart(t, 1);
        r.setEnd(t, 3);
        const f = r.extractContents();
        const text = f.firstChild !== t && f.textContent === '😀' && t.data === 'AB' &&
            r.startContainer === t && r.startOffset === 1 && r.collapsed &&
            r.extractContents().childNodes.length === 0;
        r.selectNode(document.doctype);
        let invalid = false;
        try { r.extractContents(); } catch (e) { invalid = e.name === 'HierarchyRequestError'; }
        const doctype = invalid && document.doctype !== null && !r.collapsed;
        const d = document.implementation.createHTMLDocument('other');
        const item = d.createElement('p');
        d.body.append(item);
        r.selectNodeContents(d.body);
        const df = r.extractContents();
        text && doctype && df.ownerDocument === d && df.firstChild === item &&
            item.ownerDocument === d && d.body.childNodes.length === 0
    "#, true);
}

#[test]
fn range_extract_contents_transfers_incoming_frame_children() {
    let (_, mut runtime) = realm("<main id='frame'><p>old</p></main>");
    js(&mut runtime, r#"
        const source = new DOMParser().parseFromString('<main><a href="/new">new</a><b>second</b></main>', 'text/html');
        const incoming = document.importNode(source.querySelector('main'), true);
        const target = document.getElementById('frame');
        const link = incoming.firstChild;
        const r = document.createRange();
        r.selectNodeContents(target);
        r.deleteContents();
        r.selectNodeContents(incoming);
        const f = r.extractContents();
        target.appendChild(f);
        target.firstChild === link && target.textContent === 'newsecond' &&
            incoming.childNodes.length === 0 && f.childNodes.length === 0 && r.collapsed
    "#, true);
}
