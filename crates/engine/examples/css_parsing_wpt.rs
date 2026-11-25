//! Run a local WPT HTML document through Satin's JS, CSSOM and layout engine.
//! Use tools/css-parsing-wpt.py to select synchronous cases and enforce process timeouts.
use rust_browser_engine::{LoadedPage, layout::Viewport, script::ScriptRuntime};
use serde_json::{Value, json};
use std::{env, fs, path::Path, sync::Arc, time::Duration};

const REPORTER: &str = r#"
setup({output: false, explicit_timeout: true, message_events: []});
globalThis.__satinWpt = {tests: [], harness: null};
add_result_callback(t => __satinWpt.tests.push([t.name, t.status, t.message]));
add_completion_callback((tests, status) => {
    __satinWpt.harness = [status.status, status.message, tests.length];
});
"#;

fn run(root: &Path, file: &str) -> Result<Value, String> {
    let html = fs::read_to_string(root.join(file)).map_err(|e| e.to_string())?;
    let mut page = LoadedPage::from_html(&format!("https://web-platform.test/{file}"), &html);
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut has_reporter = false;
    for script in Arc::make_mut(&mut page.scripts) {
        if let Some(reason) = &script.unsupported {
            return Err(format!("Unsupported script: {reason}"));
        }
        let Some(src) = page.document.attr(script.node, "src") else {
            continue;
        };
        let url = page.base.join(src).map_err(|e| e.to_string())?;
        if url.origin() != page.base.origin() || url.query().is_some() || url.fragment().is_some() {
            return Err(format!("Not a local static script: {src}"));
        }
        let local = root
            .join(url.path().trim_start_matches('/'))
            .canonicalize()
            .map_err(|e| format!("{src}: {e}"))?;
        if !local.starts_with(&root) {
            return Err(format!("Script outside WPT checkout: {src}"));
        }
        let mut source = fs::read_to_string(&local).map_err(|e| format!("{src}: {e}"))?;
        if url.path() == "/resources/testharnessreport.js" {
            // WPT explicitly designates this file as the vendor reporting hook.
            // All upstream code, including its default reporter, runs unmodified.
            source.push('\n');
            source.push_str(REPORTER);
            has_reporter = true;
        }
        script.source = Some(source);
    }
    if !has_reporter {
        return Err("Missing WPT report hook".into());
    }
    let mut runtime = ScriptRuntime::for_page_with_viewport(
        &mut page,
        Viewport {
            width: 800.,
            height: 600.,
            ..Viewport::default()
        },
    )
    .map_err(|e| e.to_string())?
    .ok_or("No scripts")?;
    for _ in 0..8 {
        runtime.pump();
        if runtime
            .eval("Boolean(globalThis.__satinWpt?.harness)")
            .map_err(|e| e.to_string())?
            .as_bool()
            == Some(true)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let raw = runtime
        .eval("JSON.stringify(globalThis.__satinWpt)")
        .map_err(|e| e.to_string())?;
    let json = raw
        .as_string()
        .ok_or_else(|| format!("Reporter did not start: {:?}", runtime.diagnostics()))?
        .to_string_lossy();
    let mut results: Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    results["diagnostics"] = json!(runtime.diagnostics());
    Ok(results)
}

fn main() {
    let mut args = env::args().skip(1);
    let root = args.next().expect("expected WPT checkout path");
    let file = args
        .next()
        .expect("expected WPT document path relative to checkout");
    let result = run(Path::new(&root), &file).unwrap_or_else(|error| json!({"error": error}));
    println!("{result}");
}
