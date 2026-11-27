//! Bridge supported Stylo box declarations to Satin's layout-facing properties.
use super::{Property, Value, parse_assignment};
use std::sync::OnceLock;
use style::{
    context::QuirksMode,
    properties::{PropertyDeclarationId, parse_style_attribute},
    stylesheets::{CssRuleType, UrlExtraData},
};

pub(super) fn box_assignments(name: &str, value: &str) -> Option<Vec<(Property, Value)>> {
    // These declarations do not require resource URL resolution or DOM state.
    // The cascade, variable resolution and layout use Satin data.
    if !matches!(
        name,
        "display" | "position" | "width" | "height" | "min-width" | "min-height"
            | "max-width" | "max-height" | "overflow" | "overflow-x" | "overflow-y"
    ) || value.contains(['\\', '(', ';'])
    {
        return None;
    }
    static BASE: OnceLock<UrlExtraData> = OnceLock::new();
    let base = BASE.get_or_init(|| UrlExtraData::from(url::Url::parse("about:blank").unwrap()));
    let declarations = parse_style_attribute(
        &format!("{name}:{value}"),
        base,
        None,
        QuirksMode::NoQuirks,
        CssRuleType::Style,
    );
    let mut assignments = Vec::new();
    for declaration in declarations.declarations() {
        let PropertyDeclarationId::Longhand(id) = declaration.id() else {
            return None;
        };
        let mut value = String::new();
        declaration.to_css(&mut value).ok()?;
        let properties = parse_assignment(id.name(), &value);
        if properties.is_empty() {
            return None;
        }
        assignments.extend(properties);
    }
    (!assignments.is_empty()).then_some(assignments)
}

#[cfg(test)]
mod tests {
    use crate::{css, html};

    #[test]
    fn stylo_box_syntax_reaches_author_and_inline_layout_styles() {
        let document = html::parse(
            "<!doctype html><p id='author'>author</p><p id='inline' style='display:inline flow-root'>inline</p>",
        );
        let rules = css::parse("#author { display: inline flow-root }", 800.);
        let stylesheet = css::Stylesheet::new(rules, 800.);
        let parent = css::Style::default();
        for id in document.elements("p") {
            assert_eq!(
                stylesheet.compute(&document, id, &parent, 16.).display,
                css::Display::InlineBlock,
            );
        }
    }
}
