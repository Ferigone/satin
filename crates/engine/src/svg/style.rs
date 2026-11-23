use crate::css::{self, Color};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SvgPaint {
    None,
    Color(Color),
    CurrentColor,
    /// Only same-document references are supported. Fallback is a solid color
    /// or currentColor; external paint resources are never fetched here.
    Url {
        id: String,
        fallback: Option<Box<SvgPaint>>,
    },
}

/// Optional post-cascade values. None means presentation/inherited value.
/// Supplying computed values must include presentation-attribute precedence;
/// explicit per-node overrides are applied after attributes and inline style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SvgStyle {
    pub fill: Option<SvgPaint>,
    pub stroke: Option<SvgPaint>,
    pub color: Option<Color>,
    pub fill_opacity: Option<f32>,
    pub stroke_opacity: Option<f32>,
    pub opacity: Option<f32>,
    pub fill_rule: Option<FillRule>,
    pub clip_rule: Option<FillRule>,
    pub stroke_width: Option<f32>,
    pub stroke_linecap: Option<LineCap>,
    pub stroke_linejoin: Option<LineJoin>,
    pub stroke_miterlimit: Option<f32>,
    pub stroke_dasharray: Option<Vec<f32>>,
    pub stroke_dashoffset: Option<f32>,
    /// Some(false) means display:none / visibility:hidden respectively.
    pub display: Option<bool>,
    pub visibility: Option<bool>,
    /// Same-document clip-path URL, or empty string for none.
    pub clip_path: Option<String>,
    pub stop_color: Option<Color>,
    /// Keep currentColor unresolved until the stop's own inherited color is known.
    pub stop_current_color: Option<bool>,
    pub stop_opacity: Option<f32>,
    /// Explicit CSS inherit/unset requests, applied after presentation attrs.
    pub inherit: BTreeSet<String>,
    /// SVG2 geometry cascade winners, resolved against the current viewBox.
    pub geometry: BTreeMap<String, String>,
}

impl SvgStyle {
    /// Parse a cascade winner after CSS variable substitution. Returns false for
    /// unsupported/invalid declarations without replacing the previous value.
    pub fn parse_property(&mut self, name: &str, value: &str) -> bool {
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        let inherited = !geometry_property(&name)
            && !matches!(
                name.as_str(),
                "opacity" | "display" | "clip-path" | "stop-color" | "stop-opacity"
            );
        if value.eq_ignore_ascii_case("inherit") || value.eq_ignore_ascii_case("unset") && inherited
        {
            if !known(&name) {
                return false;
            }
            self.inherit.insert(name);
            return true;
        }
        let value =
            if value.eq_ignore_ascii_case("initial") || value.eq_ignore_ascii_case("unset") {
                match name.as_str() {
                    "fill" | "color" | "stop-color" => "black",
                    "stroke" | "clip-path" | "stroke-dasharray" => "none",
                    "opacity" | "fill-opacity" | "stroke-opacity" | "stop-opacity"
                    | "stroke-width" => "1",
                    "stroke-miterlimit" => "4",
                    "stroke-dashoffset" => "0",
                    "fill-rule" | "clip-rule" => "nonzero",
                    "stroke-linecap" => "butt",
                    "stroke-linejoin" => "miter",
                    "display" => "inline",
                    "visibility" => "visible",
                    "rx" | "ry" | "width" | "height" => "auto",
                    "x" | "y" | "cx" | "cy" | "r" => "0",
                    _ => return false,
                }
            } else {
                value
            };
        if geometry_property(&name) {
            let valid = value.len() <= 128
                && (super::scene::length(value, 100.).is_some_and(|length| {
                    length >= 0. || matches!(name.as_str(), "x" | "y" | "cx" | "cy")
                }) || value == "auto"
                    && matches!(name.as_str(), "rx" | "ry" | "width" | "height"));
            if valid {
                self.geometry.insert(name.clone(), value.into());
                self.inherit.remove(&name);
            }
            return valid;
        }
        let okay = match name.as_str() {
            "fill" => assign(&mut self.fill, parse_paint(value)),
            "stroke" => assign(&mut self.stroke, parse_paint(value)),
            "color" => assign(&mut self.color, css::color(value)),
            "stop-color" => {
                if value.eq_ignore_ascii_case("currentcolor") {
                    self.stop_current_color = Some(true);
                    self.stop_color = None;
                    true
                } else if let Some(color) = css::color(value) {
                    self.stop_current_color = Some(false);
                    self.stop_color = Some(color);
                    true
                } else {
                    false
                }
            }
            "opacity" => assign(&mut self.opacity, opacity(value)),
            "fill-opacity" => assign(&mut self.fill_opacity, opacity(value)),
            "stroke-opacity" => assign(&mut self.stroke_opacity, opacity(value)),
            "stop-opacity" => assign(&mut self.stop_opacity, opacity(value)),
            "fill-rule" => assign(&mut self.fill_rule, rule(&value.to_ascii_lowercase())),
            "clip-rule" => assign(&mut self.clip_rule, rule(&value.to_ascii_lowercase())),
            "stroke-width" => assign(&mut self.stroke_width, scalar(value).filter(|v| *v >= 0.)),
            "stroke-miterlimit" => assign(
                &mut self.stroke_miterlimit,
                scalar(value).filter(|v| *v >= 1.),
            ),
            "stroke-dashoffset" => assign(&mut self.stroke_dashoffset, scalar(value)),
            "stroke-linecap" => assign(
                &mut self.stroke_linecap,
                match value.to_ascii_lowercase().as_str() {
                    "butt" => Some(LineCap::Butt),
                    "round" => Some(LineCap::Round),
                    "square" => Some(LineCap::Square),
                    _ => None,
                },
            ),
            "stroke-linejoin" => assign(
                &mut self.stroke_linejoin,
                match value.to_ascii_lowercase().as_str() {
                    "miter" => Some(LineJoin::Miter),
                    "round" => Some(LineJoin::Round),
                    "bevel" => Some(LineJoin::Bevel),
                    _ => None,
                },
            ),
            "stroke-dasharray" => {
                let parsed = if value.eq_ignore_ascii_case("none") {
                    Some(vec![])
                } else {
                    value
                        .split(|c: char| c == ',' || c.is_ascii_whitespace())
                        .filter(|s| !s.is_empty())
                        .take(257)
                        .map(scalar)
                        .collect::<Option<Vec<_>>>()
                        .filter(|v| !v.is_empty() && v.len() <= 256 && v.iter().all(|n| *n >= 0.))
                };
                assign(&mut self.stroke_dasharray, parsed)
            }
            "display" => assign(
                &mut self.display,
                match value.to_ascii_lowercase().as_str() {
                    "none" => Some(false),
                    "inline" | "block" | "inline-block" | "list-item" | "table"
                    | "inline-table" | "table-row" | "table-row-group" | "table-column"
                    | "table-column-group" | "table-header-group" | "table-footer-group"
                    | "table-cell" | "table-caption" | "flex" | "inline-flex" | "grid"
                    | "inline-grid" | "flow-root" | "contents" => Some(true),
                    _ => None,
                },
            ),
            "visibility" => assign(
                &mut self.visibility,
                match value.to_ascii_lowercase().as_str() {
                    "visible" => Some(true),
                    "hidden" | "collapse" => Some(false),
                    _ => None,
                },
            ),
            "clip-path" => {
                let parsed = if value.eq_ignore_ascii_case("none") {
                    Some(String::new())
                } else {
                    value
                        .get(..4)
                        .filter(|s| s.eq_ignore_ascii_case("url("))
                        .map(|_| &value[4..])
                        .and_then(|v| v.strip_suffix(')'))
                        .map(str::trim)
                        .map(|v| v.trim_matches(['\'', '"']))
                        .and_then(|v| v.strip_prefix('#'))
                        .filter(|v| !v.is_empty() && v.len() <= 1024)
                        .map(str::to_owned)
                };
                assign(&mut self.clip_path, parsed)
            }
            _ => false,
        };
        if okay {
            self.inherit.remove(&name);
        }
        okay
    }
}
fn known(name: &str) -> bool {
    geometry_property(name)
        || matches!(
            name,
            "fill"
                | "stroke"
                | "color"
                | "stop-color"
                | "opacity"
                | "fill-opacity"
                | "stroke-opacity"
                | "stop-opacity"
                | "fill-rule"
                | "clip-rule"
                | "stroke-width"
                | "stroke-miterlimit"
                | "stroke-dashoffset"
                | "stroke-linecap"
                | "stroke-linejoin"
                | "stroke-dasharray"
                | "display"
                | "visibility"
                | "clip-path"
        )
}
pub(crate) fn geometry_property(name: &str) -> bool {
    matches!(
        name,
        "x" | "y" | "width" | "height" | "cx" | "cy" | "r" | "rx" | "ry"
    )
}
fn assign<T>(field: &mut Option<T>, value: Option<T>) -> bool {
    if let Some(value) = value {
        *field = Some(value);
        true
    } else {
        false
    }
}
pub(crate) fn scalar(value: &str) -> Option<f32> {
    value
        .strip_suffix("px")
        .unwrap_or(value)
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && v.abs() <= 1_000_000.)
}
fn opacity(value: &str) -> Option<f32> {
    let n = if let Some(value) = value.strip_suffix('%') {
        scalar(value)? / 100.
    } else {
        scalar(value)?
    };
    Some(n.clamp(0., 1.))
}
fn rule(value: &str) -> Option<FillRule> {
    match value {
        "nonzero" => Some(FillRule::NonZero),
        "evenodd" => Some(FillRule::EvenOdd),
        _ => None,
    }
}

pub fn parse_paint(value: &str) -> Option<SvgPaint> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Some(SvgPaint::None);
    }
    if value.eq_ignore_ascii_case("currentcolor") {
        return Some(SvgPaint::CurrentColor);
    }
    if value
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case("url("))
    {
        let end = value.find(')')?;
        let reference = value[4..end].trim().trim_matches(['\'', '"']);
        let id = reference.strip_prefix('#')?;
        if id.is_empty() || id.len() > 1024 {
            return None;
        }
        let fallback = value[end + 1..].trim();
        let fallback = if fallback.is_empty() {
            None
        } else if fallback.eq_ignore_ascii_case("none") {
            Some(Box::new(SvgPaint::None))
        } else if fallback.eq_ignore_ascii_case("currentcolor") {
            Some(Box::new(SvgPaint::CurrentColor))
        } else {
            Some(Box::new(SvgPaint::Color(css::color(fallback)?)))
        };
        return Some(SvgPaint::Url {
            id: id.into(),
            fallback,
        });
    }
    css::color(value).map(SvgPaint::Color)
}
