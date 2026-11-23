//! Direct token-output comparison with unchanged html5lib tokenizer JSON.
use super::incremental::IncrementalTokenizer;
use super::tokenizer::{Content, Token, Tokenizer};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn unescape(source: &str) -> Option<String> {
    let mut output = String::new();
    let mut pos = 0;
    while pos < source.len() {
        if source[pos..].starts_with("\\u") && pos + 6 <= source.len() {
            let value = u16::from_str_radix(&source[pos + 2..pos + 6], 16).ok()?;
            pos += 6;
            if (0xd800..=0xdbff).contains(&value)
                && source[pos..].starts_with("\\u")
                && pos + 6 <= source.len()
            {
                let low = u16::from_str_radix(&source[pos + 2..pos + 6], 16).ok()?;
                if !(0xdc00..=0xdfff).contains(&low) {
                    return None;
                }
                output.push(char::from_u32(
                    0x10000 + (u32::from(value) - 0xd800) * 0x400 + u32::from(low) - 0xdc00,
                )?);
                pos += 6;
            } else {
                output.push(char::from_u32(u32::from(value))?);
            }
        } else {
            let ch = source[pos..].chars().next()?;
            output.push(ch);
            pos += ch.len_utf8();
        }
    }
    Some(output)
}

fn unescape_value(value: &mut Value) -> Option<()> {
    match value {
        Value::String(s) => *s = unescape(s)?,
        Value::Array(array) => {
            for item in array {
                unescape_value(item)?;
            }
        }
        Value::Object(map) => {
            let mut result = serde_json::Map::new();
            for (key, mut value) in std::mem::take(map) {
                unescape_value(&mut value)?;
                result.insert(unescape(&key)?, value);
            }
            *map = result;
        }
        _ => {}
    }
    Some(())
}

fn initial_content(state: &str, last_tag: &str) -> Option<Content> {
    Some(match state {
        "Data state" => Content::Data,
        "RCDATA state" => Content::Rcdata(last_tag.into()),
        "RAWTEXT state" => Content::Rawtext(last_tag.into()),
        "Script data state" => Content::Script(last_tag.into()),
        "PLAINTEXT state" => Content::Plaintext,
        "CDATA section state" => Content::Cdata,
        _ => return None,
    })
}

fn tokens(source: &str, state: &str, last_tag: &str, streamed: bool) -> Option<Value> {
    let source = source.replace("\r\n", "\n").replace('\r', "\n");
    let content = initial_content(state, last_tag)?;
    if streamed {
        let mut tokenizer = IncrementalTokenizer::new();
        tokenizer.content = content;
        let mut output = Vec::new();
        for c in source.chars() {
            output.extend(tokenizer.push(c));
            while let Some(token) = tokenizer.partial() {
                output.push(token);
            }
        }
        tokenizer.finish();
        output.extend(std::iter::from_fn(|| tokenizer.pop()));
        Some(serialize_tokens(output))
    } else {
        let mut tokenizer = Tokenizer::new(&source);
        tokenizer.content = content;
        Some(serialize_tokens(std::iter::repeat_with(|| {
            tokenizer.next()
        })))
    }
}

fn serialize_tokens(tokens: impl IntoIterator<Item = Token>) -> Value {
    let mut result: Vec<Value> = vec![];
    for token in tokens {
        let token = match token {
            Token::Eof => break,
            Token::Characters(text) => {
                if let Some(last) = result.last_mut()
                    && last[0] == "Character"
                {
                    last[1] = json!(format!("{}{text}", last[1].as_str().unwrap()));
                    continue;
                }
                json!(["Character", text])
            }
            Token::Start(tag) => {
                if tag.self_closing {
                    json!(["StartTag", tag.name, tag.attrs, true])
                } else {
                    json!(["StartTag", tag.name, tag.attrs])
                }
            }
            Token::End(name) => json!(["EndTag", name]),
            Token::Comment(data) => json!(["Comment", data]),
            Token::Doctype(d) => json!([
                "DOCTYPE",
                if d.name.is_empty() {
                    None
                } else {
                    Some(d.name)
                },
                d.public_id,
                d.system_id,
                !d.force_quirks
            ]),
        };
        result.push(token);
    }
    json!(result)
}

#[test]
fn pinned_html5lib_token_outputs() {
    audit(false);
}

#[test]
fn pinned_html5lib_tokens_one_code_point_at_a_time() {
    audit(true);
}

fn audit(streamed: bool) {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/html5lib/tokenizer");
    let mut files: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|s| s == "test"))
        .collect();
    files.sort();
    let mut failures = BTreeSet::new();
    let mut unsupported = std::collections::BTreeMap::<&str, usize>::new();
    let mut total = (0, 0, 0);
    let verbose = std::env::var_os("HTML_CONFORMANCE_VERBOSE").is_some();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy();
        let data: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let mut counts = (0, 0, 0);
        if let Some(tests) = data["xmlViolationTests"].as_array() {
            *unsupported.entry("XML infoset coercion").or_default() += tests.len();
            total.2 += tests.len();
            eprintln!(
                "tokenizer/{name}: PASS 0 FAIL 0 UNSUPPORTED {}",
                tests.len()
            );
            continue;
        }
        for (index, test) in data["tests"].as_array().unwrap().iter().enumerate() {
            let initial = vec![json!("Data state")];
            let states = test["initialStates"].as_array().unwrap_or(&initial);
            for state in states {
                let state = state.as_str().unwrap();
                let id = format!("{name}:{}:{state}", index + 1);
                let mut source = test["input"].as_str().unwrap().to_owned();
                let mut expected = test["output"].clone();
                if test["doubleEscaped"].as_bool() == Some(true) {
                    let Some(unescaped) = unescape(&source) else {
                        counts.2 += 1;
                        *unsupported.entry("isolated UTF-16 surrogate").or_default() += 1;
                        continue;
                    };
                    source = unescaped;
                    if unescape_value(&mut expected).is_none() {
                        counts.2 += 1;
                        *unsupported.entry("isolated UTF-16 surrogate").or_default() += 1;
                        continue;
                    }
                }
                let Some(actual) = tokens(
                    &source,
                    state,
                    test["lastStartTag"].as_str().unwrap_or(""),
                    streamed,
                ) else {
                    counts.2 += 1;
                    *unsupported.entry("foreign CDATA state").or_default() += 1;
                    continue;
                };
                if actual == expected {
                    counts.0 += 1;
                } else {
                    counts.1 += 1;
                    failures.insert(id.clone());
                    if verbose {
                        eprintln!(
                            "FAIL {id} {}\ninput: {source:?}\nexpected: {expected}\nactual: {actual}\n",
                            test["description"]
                        );
                    }
                }
            }
        }
        eprintln!(
            "tokenizer/{name}: PASS {} FAIL {} UNSUPPORTED {}",
            counts.0, counts.1, counts.2
        );
        total.0 += counts.0;
        total.1 += counts.1;
        total.2 += counts.2;
    }
    eprintln!(
        "{} TOKENIZER TOTAL: PASS {} FAIL {} UNSUPPORTED {}",
        if streamed { "STREAMED" } else { "CONTIGUOUS" },
        total.0,
        total.1,
        total.2
    );
    for (reason, count) in unsupported {
        eprintln!("TOKENIZER UNSUPPORTED {reason}: {count}");
    }
    assert_eq!(
        total.0 + total.1 + total.2,
        7036,
        "Pinned corpus case/state count changed"
    );
    assert_eq!(
        total.2, 8,
        "Only isolated surrogates and XML infoset coercion remain excluded"
    );
    let expected: BTreeSet<_> = std::fs::read_to_string(directory.join("known-failures.txt"))
        .expect("pinned tokenizer failure baseline")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    assert_eq!(
        failures, expected,
        "Token-output conformance changed. Use HTML_CONFORMANCE_VERBOSE=1 to inspect."
    );
}
