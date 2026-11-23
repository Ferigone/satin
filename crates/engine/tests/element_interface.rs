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
