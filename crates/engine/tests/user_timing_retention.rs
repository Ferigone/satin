use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use serde_json::{Value, json};

#[test]
fn user_timing_retains_entries_until_cleared() {
    let page = LoadedPage::from_html(
        "https://example.test/user-timing-retention.html",
        include_str!("../../../tools/fixtures/user-timing-retention.html"),
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.run_initial(&page);
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
    let result = runtime
        .eval("document.getElementById('result').textContent")
        .unwrap();
    let result: Value =
        serde_json::from_str(&result.as_string().unwrap().to_string_lossy()).unwrap();
    assert_eq!(
        result,
        json!({
            "rowCount": 1025,
            "firstRetained": true,
            "lastStart": 1028,
            "anchorRetained": true,
            "batchDuration": 1027,
            "afterClear": ["render-batch"],
            "missingAfterClear": true
        })
    );
}
