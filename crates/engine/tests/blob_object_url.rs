use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use serde_json::{Value, json};

#[test]
fn object_url_revocation_preserves_started_blob_reads() {
    const FIXTURE: &str = include_str!("fixtures/blob_object_url.html");
    for (url, origin) in [
        (
            "https://example.test:9443/editor",
            "https://example.test:9443",
        ),
        ("data:text/html,opaque", "null"),
    ] {
        let mut page = LoadedPage::from_html(url, FIXTURE);
        let mut runtime = ScriptRuntime::for_page(&mut page).unwrap().unwrap();
        let mut output = String::new();
        for _ in 0..8 {
            runtime.pump();
            output = runtime
                .eval("document.getElementById('result').textContent")
                .unwrap()
                .as_string()
                .unwrap()
                .to_string_lossy();
            if output != "pending" {
                break;
            }
        }
        let actual: Value = serde_json::from_str(&output)
            .unwrap_or_else(|error| panic!("{url}: {output}: {error}"));
        assert_eq!(
            actual,
            json!({
                "origin": origin,
                "serialized": true,
                "distinct": true,
                "fresh": "TypeError",
                "preview": [true, "basic", 200, "text/plain", [97, 98, 99]],
                "download": "abc",
                "direct": "abc",
            }),
            "{url}",
        );
        assert!(runtime.take_network_jobs().is_empty(), "{url}");
        assert!(runtime.diagnostics().is_empty(), "{url}");
    }
}
