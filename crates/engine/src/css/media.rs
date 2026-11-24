use super::{Length, length, matching_close, split_top, without_comments};
use std::borrow::Cow;

pub(crate) fn stylesheet_media_applies(source: &str, viewport: (f32, f32)) -> bool {
    source.trim().is_empty()
        || source.len() <= 4096 && media_allows(&without_comments(source), viewport)
}

// Stylesheet callers have already removed comments. Their boundary must still
// separate an identifier from the next token.
pub(super) fn media_allows(source: &str, viewport: (f32, f32)) -> bool {
    let source = source.replace('\0', " ");
    split_top(&source, b',')
        .into_iter()
        .any(|item| Query::parse(item).is_some_and(|query| query.matches(viewport)))
}
pub(crate) fn serialize_media_query_list(source: &str) -> String {
    let source = without_comments(source).replace('\0', " ");
    if source.trim().is_empty() {
        return String::new();
    }
    split_top(&source, b',')
        .into_iter()
        .map(|item| Query::parse(item).map_or_else(|| "not all".to_owned(), |q| q.serialize()))
        .collect::<Vec<_>>()
        .join(", ")
}

struct Query<'a> {
    modifier: &'static str,
    media_type: Option<String>,
    conditions: Vec<&'a str>,
    disjunction: bool,
}

fn word<'a>(input: &mut &'a str) -> Option<&'a str> {
    let s = input.trim_start();
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(s.len());
    if end == 0 || s.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    *input = &s[end..];
    Some(&s[..end])
}
impl<'a> Query<'a> {
    fn parse(mut source: &'a str) -> Option<Self> {
        source = source.trim();
        if source.is_empty() {
            return None;
        }
        let mut result = Self {
            modifier: "",
            media_type: None,
            conditions: Vec::new(),
            disjunction: false,
        };
        if !source.starts_with('(') {
            let mut head = word(&mut source)?;
            if head.eq_ignore_ascii_case("not") || head.eq_ignore_ascii_case("only") {
                result.modifier = if head.eq_ignore_ascii_case("not") {
                    "not"
                } else {
                    "only"
                };
                if !source.starts_with(char::is_whitespace) {
                    return None;
                }
                if result.modifier == "not" && source.trim_start().starts_with('(') {
                    head = "";
                } else {
                    head = word(&mut source)?;
                }
            }
            if !head.is_empty() {
                let name = head.to_ascii_lowercase();
                if matches!(name.as_str(), "and" | "or" | "not" | "only") {
                    return None;
                }
                result.media_type = Some(name);
            }
        }
        while !source.trim().is_empty() {
            if result.media_type.is_some() || !result.conditions.is_empty() {
                let conjunction = word(&mut source)?;
                let or = conjunction.eq_ignore_ascii_case("or");
                if (!or && !conjunction.eq_ignore_ascii_case("and"))
                    || !source.starts_with(char::is_whitespace)
                    || or && result.media_type.is_some()
                    || !result.conditions.is_empty()
                        && result.modifier == "not"
                        && result.media_type.is_none()
                    || result.conditions.len() >= 2 && result.disjunction != or
                {
                    return None;
                }
                result.disjunction = or;
            }
            source = source.trim_start();
            if !source.starts_with('(') {
                return None;
            }
            let end = matching_close(source, 0, b'(', b')')?;
            result.conditions.push(source[1..end].trim());
            source = &source[end + 1..];
        }
        Some(result)
    }
    fn matches(&self, viewport: (f32, f32)) -> bool {
        let mut value = if self.disjunction {
            Some(false)
        } else {
            Some(
                self.media_type
                    .as_deref()
                    .is_none_or(|name| matches!(name, "screen" | "all")),
            )
        };
        for expr in &self.conditions {
            let next = feature(expr, viewport).1;
            value = if self.disjunction {
                if value == Some(true) || next == Some(true) {
                    Some(true)
                } else {
                    value.and(next)
                }
            } else if value == Some(false) || next == Some(false) {
                Some(false)
            } else {
                value.and(next)
            };
        }
        value.is_some_and(|matched| matched != (self.modifier == "not"))
    }
    fn serialize(&self) -> String {
        let mut out = String::new();
        if !self.modifier.is_empty() {
            out.push_str(self.modifier);
            out.push(' ');
        }
        let explicit_type = self.media_type.as_deref().filter(|name| {
            *name != "all" || !self.modifier.is_empty() || self.conditions.is_empty()
        });
        if let Some(name) = explicit_type {
            out.push_str(name);
        }
        for (i, source) in self.conditions.iter().enumerate() {
            if i > 0 || explicit_type.is_some() {
                out.push_str(if self.disjunction { " or " } else { " and " });
            }
            out.push('(');
            out.push_str(&feature(source, (1., 1.)).0);
            out.push(')');
        }
        out
    }
}

fn feature<'a>(source: &'a str, viewport: (f32, f32)) -> (Cow<'a, str>, Option<bool>) {
    let unknown = || (Cow::Borrowed(source), None);
    if let Some((name, raw)) = source.split_once(':') {
        let name = name.trim().to_ascii_lowercase();
        let value = raw.trim().to_ascii_lowercase();
        let matched = match name.as_str() {
            "min-width" | "max-width" | "width" | "min-height" | "max-height" | "height" => {
                let Some(n) = media_length(&value, viewport) else {
                    return unknown();
                };
                let size = if name.ends_with("width") {
                    viewport.0
                } else {
                    viewport.1
                };
                if name.starts_with("min-") {
                    size >= n
                } else if name.starts_with("max-") {
                    size <= n
                } else {
                    size == n
                }
            }
            "orientation" if matches!(value.as_str(), "portrait" | "landscape") => {
                viewport.1.is_finite() && (viewport.0 > viewport.1) == (value == "landscape")
            }
            "prefers-color-scheme" if matches!(value.as_str(), "light" | "dark") => {
                value == "light"
            }
            "prefers-reduced-motion" if matches!(value.as_str(), "reduce" | "no-preference") => {
                value == "no-preference"
            }
            "hover" if matches!(value.as_str(), "hover" | "none") => value == "hover",
            _ => return unknown(),
        };
        return (Cow::Owned(format!("{name}: {value}")), Some(matched));
    }
    let expr = source.to_ascii_lowercase();
    let matched = match expr.as_str() {
        "width" => viewport.0 > 0.,
        "height" => viewport.1 > 0.,
        "color" | "hover" | "orientation" | "prefers-color-scheme" => true,
        "prefers-reduced-motion" => false,
        _ => {
            let Some((canonical, result)) = dimension_range(&expr, viewport) else {
                return unknown();
            };
            return (Cow::Owned(canonical), Some(result));
        }
    };
    (Cow::Owned(expr), Some(matched))
}
fn media_length(value: &str, viewport: (f32, f32)) -> Option<f32> {
    let length = length(value.trim())?;
    if length.has_percent() {
        return None;
    }
    length
        .resolve(
            if matches!(length, Length::Vh(_)) {
                viewport.1
            } else {
                viewport.0
            },
            16.,
        )
        .filter(|value| value.is_finite())
}

fn dimension_range(expr: &str, viewport: (f32, f32)) -> Option<(String, bool)> {
    fn split_comparison(expr: &str) -> Option<(&str, &str, &str)> {
        let i = expr.find(['<', '>', '='])?;
        let rest = &expr[i..];
        let size = if rest.starts_with("<=") || rest.starts_with(">=") {
            2
        } else {
            1
        };
        Some((expr[..i].trim(), &rest[..size], rest[size..].trim()))
    }
    fn compare(a: f32, op: &str, b: f32) -> bool {
        match op {
            "<" => a < b,
            "<=" => a <= b,
            "=" => a == b,
            ">=" => a >= b,
            ">" => a > b,
            _ => false,
        }
    }

    let (left, op, rest) = split_comparison(expr)?;
    let dimension = |name| match name {
        "width" => Some(viewport.0),
        "height" => Some(viewport.1),
        _ => None,
    };
    if let Some((middle, next_op, right)) = split_comparison(rest) {
        let size = dimension(middle)?;
        if op == "=" || op.as_bytes()[0] != next_op.as_bytes()[0] {
            return None;
        }
        let lo = media_length(left, viewport)?;
        let hi = media_length(right, viewport)?;
        return Some((
            format!("{left} {op} {middle} {next_op} {right}"),
            compare(lo, op, size) && compare(size, next_op, hi),
        ));
    }
    let matched = if let Some(size) = dimension(left) {
        compare(size, op, media_length(rest, viewport)?)
    } else {
        compare(media_length(left, viewport)?, op, dimension(rest)?)
    };
    Some((format!("{left} {op} {rest}"), matched))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_query_serialization_and_matching_share_the_same_grammar() {
        let size = (1280., 800.);
        for (query, serialized, matched) in [
            ("not screen junk", "not all", false),
            ("screen (min-width:20px)", "not all", false),
            ("all and (MIN-WIDTH:10PX)", "(min-width: 10px)", true),
            (
                "screen/**/and (width:2px)",
                "screen and (width: 2px)",
                false,
            ),
            ("screen,", "screen, not all", true),
            ("(made-UP:MiXeD)", "(made-UP:MiXeD)", false),
            ("not (foo:Bar)", "not (foo:Bar)", false),
            ("(WIDTH>=800px)", "(width >= 800px)", true),
            ("(orientation:landscape)", "(orientation: landscape)", true),
            ("(orientation:SQUARE)", "(orientation:SQUARE)", false),
            (
                "(height < 20px) or (orientation: landscape)",
                "(height < 20px) or (orientation: landscape)",
                true,
            ),
            (
                "screen and (orientation:landscape) or (color)",
                "not all",
                false,
            ),
        ] {
            assert_eq!(serialize_media_query_list(query), serialized, "{query}");
            assert_eq!(stylesheet_media_applies(query, size), matched, "{query}");
        }
        assert!(stylesheet_media_applies(
            "(orientation: portrait)",
            (400., 400.)
        ));
    }
}
