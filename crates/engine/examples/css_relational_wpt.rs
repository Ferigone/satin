//! Run the original synchronous selector subtests against a local WPT checkout.
//! cargo run -p rust-browser-engine --example css_relational_wpt -- /path/to/wpt
use ring::digest::{SHA256, digest};
use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use serde::Deserialize;
use std::{error::Error, path::Path};

const REVISION: &str = "9b8b1beec730060783726b7c538b21e25e63b1f4";
const FILES: &[(&str, usize, &str)] = &[
    (
        "has-argument-with-explicit-scope.html",
        13,
        "81af070754f0051d38874b04bc8a6048218d6c0b5f4c4965c0ce640a70fe4ec2",
    ),
    (
        "has-basic.html",
        18,
        "26d16dec7d291eea910dbfd873e7a347497a0b34a6a55beefaaf473377171c84",
    ),
    (
        "has-relative-argument.html",
        35,
        "636427bd870b0a7b85778582c45095bfe94ac3fb4cfcc8243ac5bb2a97fc63f2",
    ),
    (
        "has-specificity.html",
        8,
        "e117be61b35faf276c1465a39a7cb1914ac16d49fcd55f61d36ce875d1e5a24b",
    ),
    (
        "has-where-computed.html",
        1,
        "3dc71a5f99fa753ac6fccf3026366e15a7b50e3096269a2c4cf32eede9e90fe0",
    ),
    (
        "has-matches-to-uninserted-elements.html",
        12,
        "1198912c925ce51de32d4770919160294bd86d246cbdd92475354ccd0c8ad442",
    ),
];

const ADAPTER: &str = r#"
var passed = 0, failures = [];
function assert_equals(actual, expected, description) {
    if (!Object.is(actual, expected)) {
        throw new Error((description || "") + " expected " + String(expected) + ", got " + String(actual));
    }
}
function test(run, name) {
    try { run(); passed++; }
    catch (error) { failures.push(name + ": " + error.message); }
}
"#;

#[derive(Deserialize)]
struct Results {
    passed: usize,
    failures: Vec<String>,
}

fn run_page(root: &Path, name: &str, sha256: &str) -> Result<Results, Box<dyn Error>> {
    let path = format!("css/selectors/{name}");
    let html = std::fs::read_to_string(root.join(&path))?;
    let hash: String = digest(&SHA256, html.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if hash != sha256 {
        return Err(format!("{path}: content differs from WPT {REVISION}").into());
    }
    let page = LoadedPage::from_html(&format!("https://wpt.test/{path}"), &html);
    let mut runtime = ScriptRuntime::new(&page)?;
    // The upstream scripts use named Window access for their test elements.
    for id in page
        .document
        .connected_nodes()
        .filter_map(|n| page.document.attr(n, "id"))
    {
        let name = serde_json::to_string(id)?;
        runtime.eval(&format!(
            "globalThis[{name}] = document.getElementById({name})"
        ))?;
    }
    runtime.eval(ADAPTER)?;
    for script in page.scripts.iter() {
        if let Some(source) = &script.source {
            runtime.eval(source)?;
        } else if !(script.name.ends_with("/resources/testharness.js")
            || script.name.ends_with("/resources/testharnessreport.js"))
        {
            return Err(format!("Unimplemented script dependency {}", script.name).into());
        }
    }
    let json = runtime.eval("JSON.stringify({passed: passed, failures: failures})")?;
    Ok(serde_json::from_str(
        &json
            .as_string()
            .ok_or("missing adapter result")?
            .to_string_lossy(),
    )?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or("expected a path to a WPT checkout")?;
    let mut count = 0;
    let mut expected_total = 0;
    for &(name, expected, sha256) in FILES {
        let result = run_page(Path::new(&root), name, sha256)?;
        println!("{name}: {}/{} passed", result.passed, expected);
        for message in &result.failures {
            eprintln!("  FAIL {message}");
        }
        if result.passed + result.failures.len() != expected {
            return Err(format!("{name}: expected {expected} subtests").into());
        }
        count += result.passed;
        expected_total += expected;
    }
    println!("WPT {REVISION}: {count}/{expected_total} passed");
    if count != expected_total {
        return Err("selector subtest failure".into());
    }
    Ok(())
}
