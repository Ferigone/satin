//! Unmodified, pinned html5lib fixtures. This is a partial conformance measurement,
//! not an assertion of universal HTML5 support. See data/html5lib/README.md.
use rust_browser_engine::{
    dom::{AttributeNamespace, Document, Namespace, NodeId, NodeKind},
    html,
};
use std::{collections::BTreeSet, path::Path};

fn dump(document: &Document) -> String {
    let mut out = String::new();
    let mut pending: Vec<_> = document.nodes[document.root.0]
        .children
        .iter()
        .rev()
        .map(|&id| (Some(id), 0))
        .collect();
    while let Some((id, depth)) = pending.pop() {
        let prefix = format!("| {}", "  ".repeat(depth));
        out.push_str(&prefix);
        let Some(id) = id else {
            out.push_str("content\n");
            continue;
        };
        match &document.nodes[id.0].kind {
            NodeKind::Document => unreachable!(),
            NodeKind::Element { tag, attrs } => {
                let namespace = match document.namespace(id) {
                    Namespace::Html => "",
                    Namespace::Null => {
                        unreachable!("HTML parsing never creates null-namespace elements")
                    }
                    Namespace::Svg => "svg ",
                    Namespace::MathMl => "math ",
                };
                out.push_str(&format!("<{namespace}{tag}>\n"));
                let mut attrs: Vec<_> = attrs
                    .iter()
                    .map(|(name, value)| {
                        let prefix = match document.attribute_namespace(id, name) {
                            Some(AttributeNamespace::XLink) => "xlink ",
                            Some(AttributeNamespace::Xml) => "xml ",
                            Some(AttributeNamespace::Xmlns) => "xmlns ",
                            None => "",
                        };
                        (
                            format!("{prefix}{}", document.attribute_local_name(id, name)),
                            value,
                        )
                    })
                    .collect();
                attrs.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
                for (name, value) in attrs {
                    out.push_str(&format!("{prefix}  {name}=\"{value}\"\n"));
                }
            }
            NodeKind::Text(text) => out.push_str(&format!("\"{text}\"\n")),
            NodeKind::Cdata(_) | NodeKind::ProcessingInstruction { .. } => {
                unreachable!("HTML parsing only creates Text and Comment nodes for this markup")
            }
            NodeKind::Comment(text) => out.push_str(&format!("<!-- {text} -->\n")),
            NodeKind::Doctype {
                name,
                public_id,
                system_id,
            } => {
                out.push_str(&format!("<!DOCTYPE {name}"));
                if public_id.as_ref().is_some_and(|s| !s.is_empty())
                    || system_id.as_ref().is_some_and(|s| !s.is_empty())
                {
                    out.push_str(&format!(
                        " \"{}\" \"{}\"",
                        public_id.as_deref().unwrap_or(""),
                        system_id.as_deref().unwrap_or("")
                    ));
                }
                out.push_str(">\n");
            }
        }
        let content = document.template_contents.get(&id).copied();
        pending.extend(
            document.nodes[content.unwrap_or(id).0]
                .children
                .iter()
                .rev()
                .map(|&child| (Some(child), depth + if content.is_some() { 2 } else { 1 })),
        );
        if content.is_some() {
            pending.push((None, depth + 1));
        }
    }
    out.trim_end_matches('\n').into()
}

fn check_invariants(document: &Document) {
    let mut seen = BTreeSet::new();
    let mut pending = vec![document.root];
    for (&template, &content) in &document.template_contents {
        assert_eq!(document.tag(template), "template");
        assert_eq!(document.namespace(template), Namespace::Html);
        assert!(document.nodes[template.0].children.is_empty());
        assert!(matches!(document.nodes[content.0].kind, NodeKind::Document));
        assert!(document.nodes[content.0].parent.is_none());
        assert_ne!(content, document.root);
        pending.push(content);
    }
    for &id in document.namespaces.keys() {
        assert!(matches!(
            document.nodes[id.0].kind,
            NodeKind::Element { .. }
        ));
    }
    for (id, name) in document.attribute_namespaces.keys() {
        assert!(document.attr(*id, name).is_some());
        assert_ne!(document.namespace(*id), Namespace::Html);
    }
    assert!(document.nodes[document.root.0].parent.is_none());
    while let Some(id) = pending.pop() {
        assert!(seen.insert(id.0), "cycle or multiply-parented node {id:?}");
        for &child in &document.nodes[id.0].children {
            assert!(child.0 < document.nodes.len());
            assert_eq!(document.nodes[child.0].parent, Some(id));
            pending.push(child);
        }
    }
}

fn unsupported(_metadata: &str, expected: &str) -> Option<&'static str> {
    if expected.contains("<selectedcontent>") {
        return Some("form-control DOM algorithms");
    }
    None
}

fn fragment_context(metadata: &str) -> Option<&str> {
    metadata
        .split_once("\n#document-fragment\n")
        .and_then(|(_, rest)| rest.lines().next())
}

#[derive(Default)]
struct Counts {
    fixtures: usize,
    pass: usize,
    fail: usize,
    unsupported: usize,
    streamed_documents: usize,
}

fn stream_document(input: &str, scripting: bool) -> Document {
    let mut parser = html::HtmlStream::new(scripting);
    for (start, c) in input.char_indices() {
        parser.feed(&input[start..start + c.len_utf8()]).unwrap();
        loop {
            match parser.next_event().unwrap() {
                html::ParserEvent::NeedInput => break,
                html::ParserEvent::Progress(_) => {}
                html::ParserEvent::Script(script) => parser.skip_script(script.node).unwrap(),
                event => panic!("unexpected streaming corpus event {event:?}"),
            }
        }
    }
    parser.finish().unwrap();
    loop {
        match parser.next_event().unwrap() {
            html::ParserEvent::Complete => break,
            html::ParserEvent::Progress(_) => {}
            html::ParserEvent::Script(script) => parser.skip_script(script.node).unwrap(),
            event => panic!("unexpected streaming corpus event after EOF {event:?}"),
        }
    }
    parser.take_document().unwrap()
}

fn audit(
    directory: &Path,
    files: &[String],
    verbose: bool,
    streamed: bool,
) -> (BTreeSet<String>, Counts) {
    let mut failures = BTreeSet::new();
    let mut total = Counts::default();
    let mut unsupported_counts = std::collections::BTreeMap::<&str, usize>::new();
    for file in files {
        let content = std::fs::read_to_string(directory.join(file)).unwrap();
        let content = format!("\n{content}");
        let mut counts = Counts::default();
        for (index, record) in content.split("\n#data\n").skip(1).enumerate() {
            counts.fixtures += 1;
            let (input, rest) = record
                .split_once("\n#errors")
                .or_else(|| record.strip_prefix("#errors").map(|rest| ("", rest)))
                .expect("fixture #errors");
            let (metadata, expected) = rest.split_once("\n#document\n").expect("fixture #document");
            let expected = expected.trim_end_matches('\n');
            let flags: &[bool] = if metadata.contains("\n#script-on") {
                &[true]
            } else if metadata.contains("\n#script-off") {
                &[false]
            } else {
                &[false, true]
            };
            for &enabled in flags {
                let id = format!(
                    "{file}:{}:scripting-{}",
                    index + 1,
                    if enabled { "on" } else { "off" }
                );
                if let Some(reason) = unsupported(metadata, expected) {
                    counts.unsupported += 1;
                    *unsupported_counts.entry(reason).or_default() += 1;
                    if verbose {
                        eprintln!("UNSUPPORTED {id}: {reason}");
                    }
                    continue;
                }
                let document = if let Some(context) = fragment_context(metadata) {
                    let (namespace, tag) = if let Some(tag) = context.strip_prefix("svg ") {
                        (Namespace::Svg, tag)
                    } else if let Some(tag) = context.strip_prefix("math ") {
                        (Namespace::MathMl, tag)
                    } else {
                        (Namespace::Html, context)
                    };
                    let mut context = html::FragmentContext::new(tag, enabled);
                    context.namespace = namespace;
                    html::parse_fragment_with_context(input, context)
                } else if streamed {
                    counts.streamed_documents += 1;
                    stream_document(input, enabled)
                } else {
                    html::parse_with_scripting(input, enabled)
                };
                check_invariants(&document);
                assert!(
                    !document.truncated,
                    "small corpus fixture reached a budget: {id}"
                );
                let actual = dump(&document);
                if actual == expected {
                    counts.pass += 1;
                } else {
                    counts.fail += 1;
                    failures.insert(id.clone());
                    if verbose {
                        eprintln!(
                            "FAIL {id}\ninput: {input:?}\nexpected:\n{expected}\nactual:\n{actual}\n"
                        );
                    }
                }
            }
        }
        eprintln!(
            "{file}: PASS {} FAIL {} UNSUPPORTED {}",
            counts.pass, counts.fail, counts.unsupported
        );
        total.fixtures += counts.fixtures;
        total.pass += counts.pass;
        total.fail += counts.fail;
        total.unsupported += counts.unsupported;
        total.streamed_documents += counts.streamed_documents;
    }
    eprintln!(
        "TOTAL: FIXTURES {} PASS {} FAIL {} UNSUPPORTED {}",
        total.fixtures, total.pass, total.fail, total.unsupported
    );
    for (reason, count) in unsupported_counts {
        eprintln!("TREE UNSUPPORTED {reason}: {count}");
    }
    if streamed {
        eprintln!(
            "STREAMED DOCUMENT RUNS: {} (one Unicode code point per feed)",
            total.streamed_documents
        );
    }
    (failures, total)
}

#[test]
fn pinned_html5lib_tree_construction() {
    assert_pinned_tree_construction(false);
}

#[test]
fn pinned_html5lib_tree_construction_with_streamed_documents() {
    assert_pinned_tree_construction(true);
}

fn assert_pinned_tree_construction(streamed: bool) {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/html5lib");
    let files = fixture_files(&directory);
    let verbose = std::env::var_os("HTML_CONFORMANCE_VERBOSE").is_some();
    let (failures, counts) = audit(&directory, &files, verbose, streamed);
    let expected_path = directory.join("known-failures.txt");
    let expected: BTreeSet<_> = std::fs::read_to_string(expected_path)
        .expect("pinned tree failure baseline")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    assert!(counts.pass > 0);
    assert_eq!(
        counts.unsupported, 8,
        "Only selectedcontent algorithms remain excluded"
    );
    assert_eq!(counts.fixtures, 1796, "Pinned corpus fixture count changed");
    if streamed {
        assert_eq!(
            counts.streamed_documents, 3161,
            "Streamed document case count changed"
        );
    }
    assert_eq!(
        counts.pass + counts.fail + counts.unsupported,
        3553,
        "Pinned corpus case/mode count changed"
    );
    assert_eq!(
        failures, expected,
        "Conformance changed. Fix new failures or update the measured baseline for resolved failures. Use HTML_CONFORMANCE_VERBOSE=1 to inspect mismatches."
    );
}

fn fixture_files(directory: &Path) -> Vec<String> {
    let mut files: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|s| s.ends_with(".dat"))
        .collect();
    let scripted = directory.join("scripted");
    if scripted.is_dir() {
        files.extend(
            std::fs::read_dir(scripted)
                .unwrap()
                .map(|e| format!("scripted/{}", e.unwrap().file_name().to_string_lossy()))
                .filter(|s| s.ends_with(".dat")),
        );
    }
    files.sort();
    files
}

/// Opt-in audit of every upstream .dat file, without an expected-pass claim.
/// HTML5LIB_TREE_DIR must point at the pinned tree-construction checkout.
#[test]
#[ignore = "external complete corpus audit; set HTML5LIB_TREE_DIR"]
fn complete_html5lib_tree_audit() {
    let directory = std::env::var("HTML5LIB_TREE_DIR").expect("set HTML5LIB_TREE_DIR");
    let directory = Path::new(&directory);
    let files = fixture_files(directory);
    audit(
        directory,
        &files,
        std::env::var_os("HTML_CONFORMANCE_VERBOSE").is_some(),
        false,
    );
}

#[test]
fn adoption_foster_parenting_and_arena_identity() {
    let document =
        html::parse("<b>bold<i>nested</b>tail</i><table>before<tr><td>cell<td>next</table>after");
    check_invariants(&document);
    assert_eq!(
        document.serialize(),
        "<html><head></head><body><b>bold<i>nested</i></b><i>tail</i>before<table><tbody><tr><td>cell</td><td>next</td></tr></tbody></table>after</body></html>"
    );
    let elements: Vec<NodeId> = document.elements("i").collect();
    assert_eq!(elements.len(), 2);
    assert_eq!(document.text(elements[0]), "nested");
    assert_eq!(document.text(elements[1]), "tail");
}
