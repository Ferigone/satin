use ring::digest::{SHA256, digest};
use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use serde::Deserialize;
use serde_json::Value;
use std::{fs, path::Path};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wpt-canvas");
const REVISION: &str = "9e1907a13cbbc338975302097a4bafcbc4d12dc5";

#[derive(Deserialize)]
struct Manifest {
    revision: String,
    files: Vec<Fixture>,
}

#[derive(Deserialize)]
struct Fixture {
    file: String,
    sha256: String,
    subtests: usize,
}

#[test]
fn pinned_canvas_wpt() {
    let manifest: Manifest =
        serde_json::from_str(include_str!("fixtures/wpt-canvas/manifest.json")).unwrap();
    assert_eq!(manifest.revision, REVISION);
    assert_eq!(manifest.files.len(), 31);
    assert_eq!(
        manifest
            .files
            .iter()
            .map(|fixture| fixture.subtests)
            .sum::<usize>(),
        45
    );
    let mut failures = Vec::new();
    for fixture in manifest.files {
        let bytes = fs::read(Path::new(FIXTURES).join(&fixture.file)).unwrap();
        let actual_hash: String = digest(&SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(actual_hash, fixture.sha256, "{}", fixture.file);
        let page = LoadedPage::from_html(
            "https://wpt.example/html/canvas/element/",
            std::str::from_utf8(&bytes).unwrap(),
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        runtime
            .eval(include_str!("fixtures/wpt-url/bridge.js"))
            .unwrap();
        runtime
            .eval(include_str!("fixtures/wpt-canvas/bridge.js"))
            .unwrap();
        let fault = page
            .scripts
            .iter()
            .filter_map(|script| script.source.as_ref())
            .find_map(|script| runtime.eval(script).err())
            .or_else(|| runtime.eval("runCanvasWpt()").err());
        let result = runtime.eval("JSON.stringify(canvasWptResults)").unwrap();
        let tests: Vec<Value> =
            serde_json::from_str(&result.as_string().unwrap().to_string_lossy()).unwrap();
        println!(
            "WPT Canvas {}",
            serde_json::json!({
                "file": fixture.file,
                "tests": tests,
                "error": fault.as_ref().map(ToString::to_string),
            })
        );
        if fault.is_some()
            || tests.len() != fixture.subtests
            || tests.iter().any(|test| test["status"] != "PASS")
        {
            failures.push(fixture.file);
        }
    }
    assert!(failures.is_empty(), "WPT Canvas failures: {failures:?}");
}
