//! Run the offline, property-by-property CSSOM comparison. See tools/css-properties/README.md.
use rust_browser_engine::{LoadedPage, layout::Viewport, script::ScriptRuntime};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{self, Write},
    process::ExitCode,
};

const FIXTURE: &str = include_str!("../../../tools/css-properties/probe.html");
const CASES: &str = include_str!("../../../tools/css-properties/properties.jsonl");

#[derive(Debug, Deserialize, Serialize)]
struct Measurement {
    before: String,
    after: String,
    specified: String,
    edited: bool,
}

type Case = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn measure(runtime: &mut ScriptRuntime, name: &str, value: &str) -> Measurement {
    let code = format!("JSON.stringify(measure({}, {}))", json!(name), json!(value));
    let result = runtime
        .eval(&code)
        .unwrap_or_else(|err| panic!("{name}: {err}"));
    serde_json::from_str(&result.as_string().expect("Expected JSON").to_string_lossy()).unwrap()
}

fn computed_status(
    result: &Measurement,
    before: Option<&str>,
    after: Option<&str>,
) -> &'static str {
    let (Some(before), Some(after)) = (before, after) else {
        return "no-reference";
    };
    if after.is_empty() || before == after {
        return "reference-not-distinguishing";
    }
    if result.after.is_empty() {
        "missing-readback"
    } else if result.before == result.after {
        "unchanged"
    } else if result.after != after {
        "different-value"
    } else {
        "matched-change"
    }
}

fn main() -> ExitCode {
    let strict = match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => false,
        [flag] if flag == "--strict" => true,
        _ => {
            eprintln!("Usage: css_property_inventory [--strict] > report.jsonl");
            return ExitCode::FAILURE;
        }
    };
    let mut page = LoadedPage::from_html("https://example.test/css-property-inventory", FIXTURE);
    let mut runtime = ScriptRuntime::for_page_with_viewport(
        &mut page,
        Viewport {
            width: 800.,
            height: 600.,
            ..Viewport::default()
        },
    )
    .unwrap()
    .unwrap();
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
    let positive = measure(&mut runtime, "opacity", "0.25");
    assert!(
        positive.edited && positive.before == "1" && positive.after == "0.25",
        "Positive control failed: {positive:?}"
    );
    let negative = measure(&mut runtime, "satin-unknown-control", "initial");
    assert!(
        !negative.edited && negative.specified.is_empty() && negative.after.is_empty(),
        "Unknown property control failed: {negative:?}"
    );

    let mut inline = BTreeMap::new();
    let mut computed = BTreeMap::new();
    let mut total = 0;
    let mut gaps = 0;
    let mut out = io::BufWriter::new(io::stdout().lock());
    for line in CASES.lines() {
        let (name, value, specified, before, after): Case = serde_json::from_str(line).unwrap();
        let result = measure(&mut runtime, &name, &value);
        let editing = if result.specified.is_empty() {
            "rejected"
        } else if !result.edited {
            "edit-failure"
        } else if specified.as_ref().is_some_and(|s| s != &result.specified) {
            "different-serialization"
        } else if specified.is_none() {
            "accepted-no-reference"
        } else {
            "matched"
        };
        let readback = computed_status(&result, before.as_deref(), after.as_deref());
        *inline.entry(editing).or_insert(0) += 1;
        *computed.entry(readback).or_insert(0) += 1;
        total += 1;
        if !matches!(editing, "matched" | "accepted-no-reference")
            || matches!(
                readback,
                "missing-readback" | "unchanged" | "different-value"
            )
        {
            gaps += 1;
        }
        let row = json!({
            "property": name, "input": value, "inline": editing, "computed": readback,
            "actual": result,
            "reference": {"specified": specified, "before": before, "after": after},
        });
        serde_json::to_writer(&mut out, &row).unwrap();
        writeln!(out).unwrap();
    }
    out.flush().unwrap();
    eprintln!(
        "Properties: {total}\nInline: {inline:?}\nComputed: {computed:?}\nRows with a failed check: {gaps}"
    );
    if strict && gaps > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
