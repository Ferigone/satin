use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

#[test]
fn dispatches_the_same_event_through_window_document_and_element_targets() {
    let fixture = include_str!("../../../tools/fixtures/dom-events.html");
    let page = LoadedPage::from_html("https://events.test/", fixture);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.run_initial(&page);
    runtime
        .eval("document.getElementById('run-events').click()")
        .unwrap();
    let title = runtime.eval("document.title").unwrap();
    let report = runtime
        .eval("document.getElementById('events-report').textContent")
        .unwrap();
    assert_eq!(title, Value::from("EventTarget OK"), "{report:?}");
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
}
