//! Read top-level style rules without flattening a comma-separated selector list.
use super::*;

pub(crate) fn constructed_rules(input: &str) -> (Vec<String>, bool) {
    let source = without_comments(input);
    let mut rules = Vec::new();
    let mut tail = source.as_str();
    let mut top_level_count = 0usize;
    while let Some((open, delimiter)) = first_top_delimiter(tail, b"{;") {
        top_level_count += 1;
        if rules.len() >= 8192 {
            break;
        }
        let header = trim_boundary(&tail[..open]);
        if delimiter == b';' {
            if layers::header(header).and_then(layers::order).is_some() {
                rules.push(format!("{};", serialized_tokens(header)));
            }
            tail = &tail[open + 1..];
            continue;
        }
        let end = stylesheet_block_end(tail, open).unwrap_or(tail.len());
        let body = &tail[open + 1..end];
        if !header.starts_with('@') {
            let selectors = split_top(header, b',');
            if !selectors.is_empty() && selectors.iter().all(|s| compile_selector(s, 0).is_some()) {
                let mut rule = selectors
                    .into_iter()
                    .map(serialized_tokens)
                    .collect::<Vec<_>>()
                    .join(", ");
                rule.push_str(" {");
                for d in clean_declarations(body) {
                    rule.push(' ');
                    rule.push_str(&d.name);
                    rule.push_str(": ");
                    rule.push_str(&d.value);
                    if d.important {
                        rule.push_str(" !important");
                    }
                    rule.push(';');
                }
                rule.push(' ');
                rule.push('}');
                rules.push(rule);
            }
        } else if layers::header(header).is_some_and(layers::valid_block)
            || header.split_ascii_whitespace().next().is_some_and(|k| {
                [
                    "@media",
                    "@supports",
                    "@font-face",
                    "@keyframes",
                    "@container",
                ]
                .iter()
                .any(|t| k.eq_ignore_ascii_case(t))
            })
        {
            rules.push(format!(
                "{} {{ {} }}",
                serialized_tokens(header),
                body.trim()
            ));
        }
        if end == tail.len() {
            tail = "";
            break;
        }
        if rules.len() == 8192 {
            tail = &tail[end + 1..];
            break;
        }
        tail = &tail[end + 1..];
    }
    (rules, top_level_count == 1 && tail.trim().is_empty())
}
