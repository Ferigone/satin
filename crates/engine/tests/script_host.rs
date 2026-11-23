//! Integration regressions for the browser host, independent of the VM unit tests.
use rust_browser_engine::{LoadedPage, dom::NodeKind, script::ScriptRuntime};
use rust_browser_javascript::{ErrorKind, Value};

fn realm(source: &str) -> (LoadedPage, ScriptRuntime) {
    let page = LoadedPage::from_html("https://host.test/base/index.html", source);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.run_initial(&page);
    (page, runtime)
}

#[test]
fn inline_classic_async_attribute_is_ignored_without_src() {
    let (_, mut runtime) = realm("<!doctype html><script async>var inlineRan = 1;</script>");
    assert_eq!(runtime.eval("inlineRan").unwrap(), Value::Number(1.));
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
}

#[test]
fn clearing_onclick_property_disables_original_content_handler() {
    let (page, mut runtime) = realm(
        "<!doctype html><a id='link' onclick='window.clicked=1;return false' href='next'>Link</a>",
    );
    let link = page.document.elements("a").next().unwrap();
    runtime
        .eval("var link=document.getElementById('link');link.onclick=null;")
        .unwrap();
    assert!(
        runtime.click(link),
        "null onclick must allow default navigation"
    );
    assert_eq!(
        runtime.eval("typeof clicked").unwrap(),
        Value::from("undefined")
    );
}

#[test]
fn content_handler_mutations_replace_and_remove_property_handler() {
    let (page, mut runtime) = realm("<!doctype html><a id='link' href='next'>Link</a>");
    let link = page.document.elements("a").next().unwrap();
    runtime.eval("var link=document.getElementById('link');var order='';link.onclick=function(){order+='p';return false;};link.setAttribute('onclick',\"order+='a';return true;\");").unwrap();
    assert!(runtime.click(link));
    assert_eq!(runtime.eval("order").unwrap(), Value::from("a"));
    runtime.eval("link.removeAttribute('onclick');").unwrap();
    assert!(runtime.click(link));
    assert_eq!(runtime.eval("order").unwrap(), Value::from("a"));
}

#[test]
fn click_handler_href_changes_are_visible_to_default_action_snapshot() {
    let (mut page, mut runtime) = realm(
        "<!doctype html><a id='link' href='old' onclick=\"this.href='new?x=1#target'\">Link</a>",
    );
    let link = page.document.elements("a").next().unwrap();
    assert_eq!(runtime.click_link(link), Some("new?x=1#target".into()));
    assert!(runtime.snapshot(&mut page));
    assert_eq!(page.document.attr(link, "href"), Some("new?x=1#target"));
    assert_eq!(
        page.resolve(page.document.attr(link, "href").unwrap())
            .unwrap(),
        "https://host.test/base/new?x=1#target"
    );
}

#[test]
fn listener_exceptions_are_reported_without_aborting_other_listeners() {
    let (page, mut runtime) = realm("<!doctype html><button id='button'>Go</button>");
    runtime.eval("var count=0;var saved;var b=document.getElementById('button');b.addEventListener('click',function(event){saved=event;throw new TypeError('listener failed');});b.addEventListener('click',function(){count++;});").unwrap();
    assert!(runtime.click(page.document.elements("button").next().unwrap()));
    assert_eq!(runtime.eval("count").unwrap(), Value::Number(1.));
    assert_eq!(
        runtime
            .eval("saved.currentTarget===null && saved.eventPhase===0")
            .unwrap(),
        Value::Bool(true)
    );
    assert!(
        runtime
            .diagnostics()
            .iter()
            .any(|line| line.contains("listener failed"))
    );
}

#[test]
fn native_string_conversion_can_reenter_dom_without_borrow_panics() {
    let (mut page, mut runtime) = realm("<!doctype html><p id='target'>Text</p>");
    runtime.eval("var p=document.getElementById('target');var name={toString:function(){document.body.setAttribute('data-reentrant','yes');return 'class';}};p.setAttribute(name,'changed');").unwrap();
    runtime.snapshot(&mut page);
    let p = page.document.elements("p").next().unwrap();
    let body = page.document.elements("body").next().unwrap();
    assert_eq!(page.document.attr(p, "class"), Some("changed"));
    assert_eq!(page.document.attr(body, "data-reentrant"), Some("yes"));
}

#[test]
fn microtask_callback_uses_undefined_this_for_strict_function() {
    let (_, mut runtime) = realm(
        "<!doctype html><script>var strictMicrotaskThis='pending';queueMicrotask(function(){'use strict';strictMicrotaskThis=typeof this;});</script>",
    );
    assert_eq!(
        runtime.eval("strictMicrotaskThis").unwrap(),
        Value::from("undefined")
    );
}

#[test]
fn body_load_content_handler_is_a_window_load_handler() {
    let (_, mut runtime) = realm("<!doctype html><body onload='window.bodyLoaded=1'></body>");
    assert_eq!(runtime.eval("bodyLoaded").unwrap(), Value::Number(1.));
}

#[test]
fn style_value_cannot_inject_additional_declarations() {
    let (_, mut runtime) =
        realm("<!doctype html><p id='target' style='width:10px;color:green'>Text</p>");
    runtime
        .eval("var style=document.getElementById('target').style;style.width='20px;color:red';")
        .unwrap();
    assert_eq!(runtime.eval("style.width").unwrap(), Value::from("10px"));
    assert_eq!(runtime.eval("style.color").unwrap(), Value::from("green"));
}

#[test]
fn content_handler_getter_exposes_callable_and_property_reset_preserves_attribute() {
    let (_, mut runtime) =
        realm("<!doctype html><button id='button' onclick='return false'>Go</button>");
    assert_eq!(
        runtime
            .eval("var b=document.getElementById('button');typeof b.onclick")
            .unwrap(),
        Value::from("function")
    );
    runtime.eval("b.onclick=null;").unwrap();
    assert_eq!(
        runtime
            .eval("b.onclick===null && b.getAttribute('onclick')==='return false'")
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn script_dom_depth_is_bounded_before_recursive_layout() {
    let (mut page, mut runtime) = realm("<!doctype html><body></body>");
    let error = runtime.eval("var parent=document.body;for(var i=0;i<300;i++){var next=document.createElement('div');parent.appendChild(next);parent=next;}").unwrap_err();
    assert_eq!(error.kind, ErrorKind::HeapLimit);
    runtime.snapshot(&mut page);
    let mut stack = vec![(page.document.root, 0)];
    while let Some((id, depth)) = stack.pop() {
        let depth = depth
            + usize::from(matches!(
                page.document.nodes[id.0].kind,
                NodeKind::Element { .. }
            ));
        assert!(depth <= 256);
        stack.extend(
            page.document.nodes[id.0]
                .children
                .iter()
                .map(|&child| (child, depth)),
        );
    }
}

#[test]
fn inserting_a_deep_existing_subtree_checks_resulting_depth_before_reparenting() {
    let (mut page, mut runtime) = realm("<!doctype html><body></body>");
    let error = runtime.eval("var parent=document.body;for(var i=0;i<20;i++){var n=document.createElement('div');parent.appendChild(n);parent=n;}var detached=document.createElement('div');detached.id='detached';var tip=detached;for(var i=0;i<240;i++){var n=document.createElement('div');tip.appendChild(n);tip=n;}parent.appendChild(detached);").unwrap_err();
    assert_eq!(error.kind, ErrorKind::HeapLimit);
    assert_eq!(
        runtime
            .eval("detached.parentNode===null && document.getElementById('detached')===null")
            .unwrap(),
        Value::Bool(true)
    );
    runtime.snapshot(&mut page);
    assert!(!page.document.serialize().contains("id=\"detached\""));
}
