//! Feature queries use the same property and selector parsers as the cascade.
//! Font probes use the formats and shaping technologies available to @font-face.
//! Other conditional functions default to unsupported.
use super::{
    CssByte, FontFormat, compile_selector_in, font_format, font_tech_supported, identifier,
    matching_close, separators, serialized_tokens, structural_bytes, supports_declaration,
    trim_boundary, without_comments,
};
use std::borrow::Cow;

/// CSSOM's two-string overload takes a literal property name, without tokenization.
pub(crate) fn property(name: &str, value: &str) -> bool {
    name == name.trim() && !value.is_empty() && supports_declaration(name, value)
}

/// CSSOM additionally allows a single declaration without its parentheses.
pub(crate) fn query(source: &str) -> bool {
    if source.len() > 65536 {
        return false;
    }
    let clean = without_comments(source);
    let Some(clean) = close_at_eof(&clean) else {
        return false;
    };
    let source = trim_boundary(&clean);
    condition(source, 0)
        .or_else(|| declaration(source))
        .unwrap_or(false)
}

// Unclosed component-value blocks close at EOF in CSS Syntax. An unexpected
// closing delimiter or bad string cannot turn an invalid condition into true.
fn close_at_eof(source: &str) -> Option<Cow<'_, str>> {
    let mut closes = Vec::new();
    for (_, token) in structural_bytes(source) {
        match token {
            CssByte::BadString => return None,
            CssByte::Unquoted(b'(') => closes.push(b')'),
            CssByte::Unquoted(b'[') => closes.push(b']'),
            CssByte::Unquoted(b'{') => closes.push(b'}'),
            CssByte::Unquoted(b @ (b')' | b']' | b'}')) if closes.pop() != Some(b) => return None,
            _ => {}
        }
        if closes.len() > 32 {
            return None;
        }
    }
    if closes.is_empty() {
        return Some(Cow::Borrowed(source));
    }
    let mut closed = source.to_owned();
    closed.extend(closes.into_iter().rev().map(char::from));
    Some(Cow::Owned(closed))
}

fn declaration(source: &str) -> Option<bool> {
    let colon = *separators(source, b':').first()?;
    let name = trim_boundary(&source[..colon]);
    let mut consumed = 0;
    let name = identifier(name, &mut consumed).filter(|_| consumed == name.len())?;
    let mut value = &source[colon + 1..];
    if !separators(value, b';').is_empty() {
        return Some(false);
    }
    if let Some(&bang) = separators(value, b'!').last() {
        if !trim_boundary(&value[bang + 1..]).eq_ignore_ascii_case("important") {
            return Some(false);
        }
        value = &value[..bang];
    }
    Some(supports_declaration(&name, &serialized_tokens(value)))
}

/// Stylesheet @supports requires the normal condition grammar.
pub(super) fn group(source: &str) -> bool {
    source.len() <= 65536 && condition(trim_boundary(source), 0) == Some(true)
}

fn keyword<'a>(source: &'a str, word: &str) -> Option<&'a str> {
    let mut end = 0;
    let token = identifier(source, &mut end)?;
    let rest = &source[end..];
    (token.eq_ignore_ascii_case(word) && !rest.starts_with('(')).then(|| trim_boundary(rest))
}

fn condition(source: &str, depth: usize) -> Option<bool> {
    if depth > 32 {
        return None;
    }
    if let Some(rest) = keyword(source, "not") {
        let (value, tail) = term(rest, depth)?;
        return tail.is_empty().then_some(!value);
    }
    let (mut value, mut tail) = term(source, depth)?;
    if tail.is_empty() {
        return Some(value);
    }
    let operator = if keyword(tail, "and").is_some() {
        "and"
    } else {
        "or"
    };
    while !tail.is_empty() {
        let rest = keyword(tail, operator)?;
        let (next, after) = term(rest, depth)?;
        value = if operator == "and" {
            value && next
        } else {
            value || next
        };
        tail = after;
    }
    Some(value)
}

fn term(source: &str, depth: usize) -> Option<(bool, &str)> {
    let mut open = 0;
    let function = if source.starts_with('(') {
        None
    } else {
        // A number followed by parentheses is not a CSS function token.
        let name_start = source
            .strip_prefix('-')
            .unwrap_or(source)
            .as_bytes()
            .first()?;
        if !(name_start.is_ascii_alphabetic()
            || matches!(name_start, b'-' | b'_' | b'\\')
            || *name_start >= 128)
        {
            return None;
        }
        let ident = identifier(source, &mut open)?;
        if !source[open..].starts_with('(') {
            return None;
        }
        Some(ident)
    };
    let close = matching_close(source, open, b'(', b')')?;
    let body = trim_boundary(&source[open + 1..close]);
    let value = match function {
        None => condition(body, depth + 1)
            .or_else(|| declaration(body))
            .unwrap_or(false),
        Some(name) if name.eq_ignore_ascii_case("selector") => {
            compile_selector_in(body, 0, false, true).is_some()
        }
        Some(name)
            if name.eq_ignore_ascii_case("font-format")
                || name.eq_ignore_ascii_case("font-tech") =>
        {
            let mut end = 0;
            identifier(body, &mut end).is_some_and(|id| {
                end == body.len()
                    && if name.eq_ignore_ascii_case("font-format") {
                        font_format(&id.to_ascii_lowercase()) != FontFormat::Unsupported
                    } else {
                        font_tech_supported(&id)
                    }
            })
        }
        Some(_) => false,
    };
    Some((value, trim_boundary(&source[close + 1..])))
}
