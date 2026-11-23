//! Independent HTML tokenizer and tree constructor.
//!
//! This implements a measured subset of WHATWG HTML document and fragment
//! parsing, including inert template contents and SVG/MathML foreign content.
//! See data/html5lib/README.md for pinned conformance measurements and gaps.
#[path = "html/foreign.rs"]
mod foreign;
#[path = "html/incremental.rs"]
mod incremental;
#[path = "html/stream.rs"]
mod stream;
pub use stream::{
    HtmlStream, HtmlStreamError, HtmlStreamReport, MAX_SCRIPT_NESTING, MAX_STREAM_INPUT_PIECES,
    MAX_STREAM_WORK, MAX_WRITE_CALLS, ParserEvent, ParserProgress, ParserScript, ParserScriptKind,
    STREAM_ADVANCE_BYTES, WriteId,
};
#[path = "html/tokenizer.rs"]
mod tokenizer;
#[cfg(test)]
#[path = "html/tokenizer_tests.rs"]
mod tokenizer_tests;
#[path = "html/tree_builder.rs"]
mod tree_builder;
use crate::dom::{Document, Namespace, QuirksMode};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

/// Resource budgets for this alpha. Reaching any budget sets Document::truncated.
pub const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NODES: usize = 100_000;
pub const MAX_OPEN_ELEMENTS: usize = 256;
pub const MAX_DOM_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_FORMATTING_ENTRIES: usize = MAX_OPEN_ELEMENTS * 2;
pub const MAX_FORMATTING_BYTES: usize = MAX_INPUT_BYTES;

pub fn parse(source: &str) -> Document {
    parse_with_scripting(source, false)
}

/// Parse a complete document with the requested HTML scripting flag, including
/// the corresponding noscript rules. For parser scripts and document.write,
/// use HtmlStream with a script execution driver.
pub fn parse_with_scripting(source: &str, enabled: bool) -> Document {
    parse_input(source, |normalized| {
        tree_builder::parse(normalized, enabled)
    })
}

/// Fragment context supplied by a DOM embedding. The owning document's quirks mode and nearest form
/// ancestor affect recovery; they cannot be inferred from a tag name alone.
#[derive(Clone, Copy, Debug)]
pub struct FragmentContext<'a> {
    pub tag: &'a str,
    pub namespace: Namespace,
    /// Context attributes are needed for MathML annotation-xml integration.
    pub attrs: Option<&'a BTreeMap<String, String>>,
    pub scripting_enabled: bool,
    pub quirks_mode: QuirksMode,
    pub form_ancestor: bool,
}

impl<'a> FragmentContext<'a> {
    pub fn new(tag: &'a str, scripting_enabled: bool) -> Self {
        Self {
            tag,
            namespace: Namespace::Html,
            attrs: None,
            scripting_enabled,
            quirks_mode: QuirksMode::NoQuirks,
            form_ancestor: false,
        }
    }
}

/// Parse innerHTML-style input. The returned root's children are the fragment
/// roots; no synthetic html/head/body wrappers are connected to the result.
/// Scripts are represented in the DOM and are not executed by this operation.
pub fn parse_fragment(source: &str, context_tag: &str, scripting: bool) -> Document {
    parse_fragment_with_context(source, FragmentContext::new(context_tag, scripting))
}

pub fn parse_fragment_with_context(source: &str, context: FragmentContext<'_>) -> Document {
    parse_input(source, |normalized| {
        tree_builder::parse_fragment(normalized, context)
    })
}

fn parse_input(source: &str, parse: impl FnOnce(&str) -> Document) -> Document {
    let mut end = source.len().min(MAX_INPUT_BYTES);
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    // The HTML input-stream preprocessing step precedes tokenization.
    let normalized = source[..end].replace("\r\n", "\n").replace('\r', "\n");
    let mut document = parse(&normalized);
    document.truncated |= end < source.len();
    document
}
fn entity_values() -> &'static HashMap<String, String> {
    static ENTITIES: OnceLock<HashMap<String, String>> = OnceLock::new();
    ENTITIES.get_or_init(|| {
        let values: HashMap<String, serde_json::Value> =
            serde_json::from_str(include_str!("../data/entities.json"))
                .expect("WHATWG entity data");
        values
            .into_iter()
            .filter_map(|(k, v)| v["characters"].as_str().map(|s| (k, s.to_owned())))
            .collect()
    })
}

/// Can this incomplete reference still consume another input character?
fn named_entity_prefix(reference: &str) -> bool {
    static PREFIXES: OnceLock<HashSet<&'static str>> = OnceLock::new();
    PREFIXES
        .get_or_init(|| {
            entity_values()
                .keys()
                .flat_map(|name| (1..name.len()).map(|end| &name[..end]))
                .collect()
        })
        .contains(reference)
}

pub fn decode_entities(source: &str, attribute: bool) -> String {
    if !source.contains('&') {
        return source.to_owned();
    }
    let entities = entity_values();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < source.len() {
        if source.as_bytes()[i] != b'&' {
            let c = source[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        if source.as_bytes().get(i + 1) == Some(&b'#') {
            let mut cursor = i + 2;
            let hex = matches!(source.as_bytes().get(cursor), Some(b'x') | Some(b'X'));
            if hex {
                cursor += 1;
            }
            let start = cursor;
            while cursor < source.len()
                && if hex {
                    source.as_bytes()[cursor].is_ascii_hexdigit()
                } else {
                    source.as_bytes()[cursor].is_ascii_digit()
                }
            {
                cursor += 1;
            }
            if cursor > start {
                let mut value =
                    u32::from_str_radix(&source[start..cursor], if hex { 16 } else { 10 })
                        .unwrap_or(0xfffd);
                const C1: [u32; 32] = [
                    0x20ac, 0x81, 0x201a, 0x192, 0x201e, 0x2026, 0x2020, 0x2021, 0x2c6, 0x2030,
                    0x160, 0x2039, 0x152, 0x8d, 0x17d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d,
                    0x2022, 0x2013, 0x2014, 0x2dc, 0x2122, 0x161, 0x203a, 0x153, 0x9d, 0x17e,
                    0x178,
                ];
                if (0x80..=0x9f).contains(&value) {
                    value = C1[(value - 0x80) as usize];
                }
                out.push(if value == 0 {
                    '\u{fffd}'
                } else {
                    char::from_u32(value).unwrap_or('\u{fffd}')
                });
                if source.as_bytes().get(cursor) == Some(&b';') {
                    cursor += 1;
                }
                i = cursor;
                continue;
            }
        }
        let mut matched = None;
        for size in (2..=34.min(source.len() - i)).rev() {
            if !source.is_char_boundary(i + size) {
                continue;
            }
            let key = &source[i..i + size];
            if let Some(value) = entities.get(key) {
                let next = source.as_bytes().get(i + size);
                if attribute
                    && !key.ends_with(';')
                    && next.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'=')
                {
                    continue;
                }
                matched = Some((size, value));
                break;
            }
        }
        if let Some((len, value)) = matched {
            out.push_str(value);
            i += len;
        } else {
            out.push('&');
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_text_and_quoted_angles() {
        let d =
            parse("<title>A &amp; B</title><p title='a>b'>x<script>if(a<b){x='&amp;'}</script>z");
        assert_eq!(d.title(), "A & B");
        let p = d.elements("p").next().unwrap();
        assert_eq!(d.attr(p, "title"), Some("a>b"));
        assert!(d.serialize().contains("if(a<b){x='&amp;'}"));
    }
    #[test]
    fn optional_end_tags_and_attributes() {
        let d = parse("<ul><li id=a>one<li id=b>two</ul>");
        let li = d.elements("li").collect::<Vec<_>>();
        assert_eq!(li.len(), 2);
        assert_eq!(d.nodes[li[0].0].parent, d.nodes[li[1].0].parent);
        assert_eq!(d.attr(li[1], "id"), Some("b"));
    }
    #[test]
    fn references_and_invalid_scalars() {
        assert_eq!(
            decode_entities("&copy; &NotEqualTilde; &#x1f980; &#0; &#x80;", false),
            "© ≂̸ 🦀 � €"
        );
        assert_eq!(decode_entities("&notit;", true), "&notit;");
        assert_eq!(decode_entities("&notit;", false), "¬it;");
    }
}
