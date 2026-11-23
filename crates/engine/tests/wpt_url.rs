//! Unchanged synchronous URL WPT files executed in separate browser realms.
use ring::digest::{SHA256, digest};
use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::JsError;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wpt-url");
const REVISION: &str = "9e1907a13cbbc338975302097a4bafcbc4d12dc5";
const BRIDGE: &str = include_str!("fixtures/wpt-url/bridge.js");

#[derive(Deserialize)]
struct Manifest {
    revision: String,
    files: Vec<Fixture>,
}

#[derive(Deserialize)]
struct Fixture {
    file: String,
    bytes: usize,
    sha256: String,
    subtests: Option<usize>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Subtest {
    name: String,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
enum Status {
    #[serde(rename = "PASS")]
    Pass,
    #[serde(rename = "FAIL")]
    Fail,
}

fn realm() -> ScriptRuntime {
    let page = LoadedPage::from_html(
        "https://wpt.example/url/",
        "<!doctype html><title>WPT URL</title>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime
        .eval(BRIDGE)
        .expect("install synchronous testharness bridge");
    runtime
}

fn evaluate(source: &str) -> (Vec<Subtest>, Option<JsError>) {
    let mut runtime = realm();
    let fault = runtime.eval(source).err();
    let result = runtime
        .eval("JSON.stringify(wptResults)")
        .expect("collect testharness results");
    let json = result
        .as_string()
        .expect("result JSON string")
        .to_string_lossy();
    (
        serde_json::from_str(&json).expect("valid testharness result schema"),
        fault,
    )
}

#[test]
fn bridge_reports_assertion_mismatches_and_requires_exact_exception_type() {
    let (results, fault) = evaluate(
        r#"
        test(() => assert_true(true), "boolean pass");
        test(() => assert_true(1), "strict boolean");
        test(() => assert_equals(NaN, NaN), "SameValue NaN");
        test(() => assert_equals(0, -0), "SameValue signed zero");
        test(() => assert_array_equals(new Array(1), [undefined]), "holes differ");
        test(() => assert_throws_js(TypeError, () => { throw new TypeError(); }), "exact TypeError");
        test(() => assert_throws_js(TypeError, () => { throw { name: "TypeError" }; }), "fake TypeError");
        test(() => assert_throws_js(TypeError, () => {}), "missing throw");
        test(() => assert_unreached(), "unreachable");
    "#,
    );
    assert!(fault.is_none(), "{fault:?}");
    assert_eq!(
        results
            .iter()
            .map(|entry| &entry.status)
            .collect::<Vec<_>>(),
        [
            &Status::Pass,
            &Status::Fail,
            &Status::Pass,
            &Status::Fail,
            &Status::Fail,
            &Status::Pass,
            &Status::Fail,
            &Status::Fail,
            &Status::Fail,
        ],
        "{results:#?}"
    );
}

#[test]
fn pinned_url_wpt() {
    let manifest: Manifest =
        serde_json::from_str(include_str!("fixtures/wpt-url/manifest.json")).unwrap();
    assert_eq!(manifest.revision, REVISION);
    assert_eq!(manifest.files.len(), 15);
    assert_eq!(
        manifest
            .files
            .iter()
            .filter_map(|file| file.subtests)
            .sum::<usize>(),
        110
    );
    let mut failures = Vec::new();
    for fixture in manifest.files {
        let bytes = fs::read(Path::new(FIXTURES).join(&fixture.file)).unwrap();
        assert_eq!(bytes.len(), fixture.bytes, "{}", fixture.file);
        let hash = digest(&SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(hash, fixture.sha256, "{}", fixture.file);
        let Some(expected) = fixture.subtests else {
            assert_eq!(fixture.file, "LICENSE.md");
            continue;
        };

        let source = std::str::from_utf8(&bytes).unwrap();
        let (subtests, fault) = evaluate(source);
        let passed = subtests
            .iter()
            .filter(|test| test.status == Status::Pass)
            .count();
        let failed = subtests
            .iter()
            .filter(|test| test.status == Status::Fail)
            .collect::<Vec<_>>();
        println!(
            "WPT URL {}",
            serde_json::json!({
                "file": fixture.file,
                "expected": expected,
                "passed": passed,
                "failed": failed,
                "not_run": expected.saturating_sub(subtests.len()),
                "execution_error": fault.as_ref().map(ToString::to_string),
            })
        );
        if passed != expected || !failed.is_empty() || fault.is_some() {
            failures.push(fixture.file);
        }
    }
    assert!(failures.is_empty(), "WPT URL failures: {failures:?}");
}
