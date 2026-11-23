//! All input/MIME/body tuples from one unchanged pinned WPT fixture. This tests
//! resource decoding directly; it is not a claim of full Fetch conformance.
use rust_browser_engine::data_url;
use url::Url;
#[test]
fn pinned_wpt_data_urls_match_every_case_without_exclusions() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_slice(include_bytes!("../data/data-urls/data-urls.json")).unwrap();
    assert_eq!(cases.len(), 72);
    let mut failures = vec![];
    for (index, case) in cases.iter().enumerate() {
        let input = case[0].as_str().unwrap();
        let result = Url::parse(input)
            .map_err(|e| e.to_string())
            .and_then(|url| data_url::process(&url, 1024 * 1024).map_err(|e| e.to_string()));
        if case[1].is_null() {
            if result.is_ok() {
                failures.push(format!("#{index} expected failure: {input:?}"));
            }
            continue;
        }
        let expected = case[1].as_str().unwrap();
        let body = case[2]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| u8::try_from(n.as_u64().unwrap()).unwrap())
            .collect::<Vec<_>>();
        match result {
            Ok(actual) if actual.mime_type.serialize() == expected && actual.body == body => {}
            actual => failures.push(format!(
                "#{index} {input:?}: expected {expected:?}/{body:?}, got {actual:?}"
            )),
        }
    }
    eprintln!(
        "Pinned WPT data URLs: {} passed / {} failed / 72 total; 0 skipped",
        cases.len() - failures.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
