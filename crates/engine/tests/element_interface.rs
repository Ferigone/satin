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
