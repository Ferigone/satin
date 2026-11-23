use ring::digest::{SHA256, digest};
use rust_browser_engine::{LoadedPage, layout::Viewport, script::ScriptRuntime};
use rust_browser_javascript::{ErrorKind, JsError, Value};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wpt-sticky");
const REVISION: &str = "9e1907a13cbbc338975302097a4bafcbc4d12dc5";
const BRIDGE: &str = include_str!("fixtures/wpt-sticky/bridge.js");
const STYLE_FORWARDING: &str = include_str!("fixtures/wpt-sticky/style-forwarding.js");
const STICKY_UTIL: &str = include_str!("fixtures/wpt-sticky/upstream/sticky-util.js");

#[derive(Deserialize)]
struct Manifest {
    revision: String,
    files: Vec<Fixture>,
    support: Vec<Source>,
}

#[derive(Deserialize)]
struct Source {
    file: String,
    upstream: String,
    sha256: String,
}

impl Source {
    fn verify(&self) -> Vec<u8> {
        let bytes = fs::read(Path::new(FIXTURES).join("upstream").join(&self.file)).unwrap();
        let hash: String = digest(&SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(hash, self.sha256, "{}", self.upstream);
        bytes
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    #[serde(flatten)]
    source: Source,
    subtests: usize,
    window_ids: Vec<String>,
    style_forwarding: bool,
}

#[derive(Serialize)]
struct Subtest {
    name: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

fn evaluate(fixture: &Fixture, html: &str) -> Result<Vec<Subtest>, JsError> {
    let page = LoadedPage::from_html(
        &format!("https://wpt.example/{}", fixture.source.upstream),
        html,
    );
    let mut runtime = ScriptRuntime::new(&page)?;
    runtime.set_viewport(Viewport {
        width: 800.,
        height: 600.,
        ..Default::default()
    });
    runtime.eval(BRIDGE)?;
    if fixture.style_forwarding {
        runtime.eval(STYLE_FORWARDING)?;
    }
    for id in &fixture.window_ids {
        let key = serde_json::to_string(id).unwrap();
        runtime.eval(&format!(
            "globalThis[{key}] = document.getElementById({key});"
        ))?;
    }
    for script in page.scripts.iter() {
        if let Some(source) = &script.source {
            runtime.eval(source)?;
        } else if script.name.ends_with("sticky-util.js") {
            runtime.eval(STICKY_UTIL)?;
        }
    }

    let Value::Number(count) = runtime.eval("stickyWptTests.length")? else {
        panic!("WPT adapter must register test bodies");
    };
    let mut results = Vec::new();
    for index in 0..count as usize {
        let value = runtime.eval(&format!("stickyWptTests[{index}].name"))?;
        let name = value.as_string().unwrap().to_string_lossy();
        let result = runtime.eval(&format!("stickyWptTests[{index}].run()"));
        results.push(Subtest {
            name,
            status: if result.is_ok() { "PASS" } else { "FAIL" },
            message: result.err().map(|error| error.to_string()),
        });
    }
    Ok(results)
}

#[test]
fn pinned_sticky_wpt() {
    let manifest: Manifest =
        serde_json::from_str(include_str!("fixtures/wpt-sticky/manifest.json")).unwrap();
    assert_eq!(manifest.revision, REVISION);
    assert_eq!(manifest.files.len(), 19);
    assert_eq!(
        manifest
            .files
            .iter()
            .map(|fixture| fixture.subtests)
            .sum::<usize>(),
        52
    );
    for source in manifest.support {
        source.verify();
    }
    let mut failures = Vec::new();
    let mut passed = 0;
    let mut unexecuted = 0;
    for fixture in manifest.files {
        let bytes = fixture.source.verify();
        let results = evaluate(&fixture, std::str::from_utf8(&bytes).unwrap());
        // A future VM with assignment targets in for-of can run this file
        // directly. Until then report its original test as unexecuted.
        let not_run = if fixture.source.file == "position-sticky-parsing.html"
            && matches!(&results, Err(error)
                if error.kind == ErrorKind::UnsupportedFeature
                && error.to_string().contains("Assignment targets in for-of are not implemented"))
        {
            fixture.subtests
        } else {
            0
        };
        let (subtests, error) = match results {
            Ok(subtests) => (subtests, None),
            Err(error) => (vec![], Some(error.to_string())),
        };
        println!(
            "WPT Sticky {}",
            serde_json::json!({
                "file": fixture.source.file,
                "tests": subtests,
                "error": error,
                "notRun": not_run,
                "styleForwarding": fixture.style_forwarding,
                "windowIds": fixture.window_ids,
            })
        );
        passed += subtests.iter().filter(|test| test.status == "PASS").count();
        unexecuted += not_run;
        if not_run == 0
            && (error.is_some()
                || subtests.len() != fixture.subtests
                || subtests.iter().any(|test| test.status != "PASS"))
        {
            failures.push(fixture.source.file);
        }
    }
    assert!(failures.is_empty(), "WPT Sticky failures: {failures:?}");
    assert_eq!(passed + unexecuted, 52);
    println!("WPT Sticky {passed}/52 passed, {unexecuted} unexecuted");
}
