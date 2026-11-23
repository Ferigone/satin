use ring::digest::{SHA256, digest};
use rust_browser_engine::{LoadedPage, layout::Viewport, script::ScriptRuntime};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/wpt-intersection-observer/upstream"
);
const REVISION: &str = "9e1907a13cbbc338975302097a4bafcbc4d12dc5";
const BRIDGE: &str = include_str!("fixtures/wpt-intersection-observer/bridge.js");
const OBSERVER_UTIL: &str = include_str!(
    "fixtures/wpt-intersection-observer/upstream/intersection-observer/resources/intersection-observer-test-utils.js"
);

#[derive(Deserialize)]
struct Manifest {
    revision: String,
    files: Vec<Fixture>,
    support: Vec<Source>,
}

#[derive(Deserialize)]
struct Source {
    path: String,
    sha256: String,
}

impl Source {
    fn read(&self) -> String {
        let bytes = fs::read(Path::new(FIXTURES).join(&self.path)).unwrap();
        let hash: String = digest(&SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(hash, self.sha256, "{}", self.path);
        String::from_utf8(bytes).unwrap()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    #[serde(flatten)]
    source: Source,
    subtests: usize,
    #[serde(default)]
    window_ids: Vec<String>,
}

#[derive(Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum Status {
    Pass,
    Fail,
    Running,
}

#[derive(Deserialize, Serialize)]
struct Subtest {
    name: String,
    status: Status,
    message: Option<String>,
}

fn evaluate(fixture: &Fixture) -> (Vec<Subtest>, Vec<String>) {
    let url = format!("https://wpt.example/{}", fixture.source.path);
    let mut page = LoadedPage::from_html(&url, &fixture.source.read());
    let mut bridge = BRIDGE.to_owned();
    for id in &fixture.window_ids {
        let key = serde_json::to_string(id).unwrap();
        bridge.push_str(&format!(
            "\nglobalThis[{key}] = document.getElementById({key});"
        ));
    }
    for script in Arc::make_mut(&mut page.scripts) {
        match script.name.as_str() {
            "/resources/testharness.js" => script.source = Some(bridge.clone()),
            "./resources/intersection-observer-test-utils.js" => {
                script.source = Some(OBSERVER_UTIL.to_owned());
            }
            _ => {}
        }
    }

    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.set_viewport(Viewport {
        width: 800.,
        height: 600.,
        ..Viewport::default()
    });
    runtime.run_initial(&page);
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        runtime.pump();
        let result = runtime
            .eval("JSON.stringify(wptIntersectionTests)")
            .unwrap();
        let json = result.as_string().unwrap().to_string_lossy();
        let tests: Vec<Subtest> = serde_json::from_str(&json).unwrap();
        let diagnostics = runtime.diagnostics();
        if !diagnostics.is_empty()
            || tests.len() >= fixture.subtests
                && tests.iter().all(|test| test.status != Status::Running)
            || Instant::now() >= deadline
        {
            return (tests, diagnostics);
        }
        let Some(next) = runtime.next_deadline() else {
            return (tests, diagnostics);
        };
        std::thread::sleep(next.min(deadline).saturating_duration_since(Instant::now()));
    }
}

#[test]
fn pinned_intersection_observer_wpt() {
    let manifest: Manifest = serde_json::from_str(include_str!(
        "fixtures/wpt-intersection-observer/manifest.json"
    ))
    .unwrap();
    assert_eq!(manifest.revision, REVISION);
    assert!(!manifest.files.is_empty());
    for source in manifest.support {
        source.read();
    }
    let total: usize = manifest.files.iter().map(|fixture| fixture.subtests).sum();
    let mut failures = Vec::new();
    let mut passed = 0;
    for fixture in manifest.files {
        let (tests, diagnostics) = evaluate(&fixture);
        let fixture_passed = tests
            .iter()
            .filter(|test| test.status == Status::Pass)
            .count();
        passed += fixture_passed;
        if tests.len() != fixture.subtests
            || fixture_passed != fixture.subtests
            || !diagnostics.is_empty()
        {
            failures.push(fixture.source.path.clone());
        }
        println!(
            "WPT IntersectionObserver {}",
            serde_json::json!({
                "file": fixture.source.path,
                "expectedSubtests": fixture.subtests,
                "tests": tests,
                "diagnostics": diagnostics,
            })
        );
    }
    println!("WPT IntersectionObserver {passed}/{total} passed");
    assert!(failures.is_empty(), "WPT failures: {failures:?}");
}
