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
