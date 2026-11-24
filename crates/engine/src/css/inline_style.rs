use super::{
    Declaration, calc, contains_variable, declaration_tokens, identifier, number,
    serialized_tokens, supports_declaration, trim_boundary, valid_components, whitespace_tokens,
    without_comments,
};

/// Specified-value reflection, independent of computed style and layout.
#[derive(Clone, Copy)]
pub(crate) enum ReflectedLonghand {
    Float,
    Opacity,
}

impl ReflectedLonghand {
    pub(crate) fn named(name: &str) -> Option<Self> {
        match name {
            "float" => Some(Self::Float),
            "opacity" => Some(Self::Opacity),
            _ => None,
        }
    }

    pub(crate) fn serialize(self, source: &str) -> Option<String> {
        let clean = without_comments(source);
        if !valid_components(&clean, 0) {
            return None;
        }
        let value = trim_boundary(&clean);
        if contains_variable(value) {
            return Some(serialized_tokens(value));
        }
        let mut end = 0;
        if value
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'\\')
            && let Some(mut keyword) = identifier(value, &mut end)
            && end == value.len()
        {
            keyword.make_ascii_lowercase();
            return (matches!(
                keyword.as_str(),
                "initial" | "inherit" | "unset" | "revert" | "revert-layer"
            ) || matches!(self, Self::Float)
                && matches!(
                    keyword.as_str(),
                    "none" | "left" | "right" | "inline-start" | "inline-end"
                ))
            .then_some(keyword);
        }
        if matches!(self, Self::Float) {
            return None;
        }
        // A specified value retains its range; computed style clamps to [0, 1].
        opacity_number(value).map(|alpha| (alpha + 0.).to_string())
    }
}

pub(super) fn opacity_number(value: &str) -> Option<f32> {
    let end = calc::number_end(value)?;
    let scale = match &value[end..] {
        "" => 1.,
        "%" => 100.,
        _ => return None,
    };
    let alpha = (value[..end].parse::<f64>().ok()? / scale) as f32;
    alpha.is_finite().then_some(alpha)
}

#[derive(Clone, Copy)]
struct Axis {
    shorthand: &'static str,
    edges: [&'static str; 2],
}

#[derive(Clone, Copy)]
enum Part {
    Both,
    Edge(usize),
}

const AXES: [Axis; 6] = [
    Axis {
        shorthand: "margin-block",
        edges: ["margin-block-start", "margin-block-end"],
    },
    Axis {
        shorthand: "margin-inline",
        edges: ["margin-inline-start", "margin-inline-end"],
    },
    Axis {
        shorthand: "padding-block",
        edges: ["padding-block-start", "padding-block-end"],
    },
    Axis {
        shorthand: "padding-inline",
        edges: ["padding-inline-start", "padding-inline-end"],
    },
    Axis {
        shorthand: "inset-block",
        edges: ["inset-block-start", "inset-block-end"],
    },
    Axis {
        shorthand: "inset-inline",
        edges: ["inset-inline-start", "inset-inline-end"],
    },
];

impl Axis {
    fn part(self, name: &str) -> Option<Part> {
        if name == self.shorthand {
            Some(Part::Both)
        } else {
            self.edges
                .iter()
                .position(|edge| *edge == name)
                .map(Part::Edge)
        }
    }

    fn find(name: &str) -> Option<(Self, Part)> {
        AXES.into_iter()
            .find_map(|axis| Some((axis, axis.part(name)?)))
    }
}

enum EdgeValue<'a> {
    Plain(String),
    Variable(&'a str),
    PendingShorthand,
}

enum ShorthandValue {
    Edges([String; 2]),
    Pending,
}

struct Edge<'a> {
    declaration: &'a Declaration,
    value: EdgeValue<'a>,
}

fn choose<'a>(target: &mut Option<Edge<'a>>, declaration: &'a Declaration, value: EdgeValue<'a>) {
    if target
        .as_ref()
        .is_none_or(|old| !old.declaration.important || declaration.important)
    {
        *target = Some(Edge { declaration, value });
    }
}

fn is_wide_keyword(value: &str) -> bool {
    matches!(value, "initial" | "inherit" | "unset")
}

fn serialize_value(token: &str) -> String {
    if token.contains('(') {
        // Preserve valid math and its token boundaries for this CSSOM subset.
        return serialized_tokens(token);
    }
    let lower = token.to_ascii_lowercase();
    let units = [
        "rem", "px", "em", "vw", "vh", "pt", "pc", "in", "cm", "mm", "q", "%",
    ];
    for unit in units {
        if let Some(n) = lower.strip_suffix(unit).and_then(number) {
            return format!("{}{unit}", n + 0.);
        }
    }
    if number(&lower) == Some(0.) {
        "0px".into()
    } else {
        lower
    }
}

fn serialize_pair(start: String, end: String) -> String {
    if start == end {
        start
    } else if is_wide_keyword(&start) || is_wide_keyword(&end) {
        String::new()
    } else {
        format!("{start} {end}")
    }
}

fn declaration_value(declaration: &Declaration) -> Option<String> {
    supports_declaration(&declaration.name, &declaration.value)
        .then(|| declaration_tokens(&declaration.value))
}

fn shorthand_value(declaration: &Declaration) -> Option<ShorthandValue> {
    let source = declaration_value(declaration)?;
    let value = trim_boundary(&source);
    if contains_variable(value) {
        return Some(ShorthandValue::Pending);
    }
    let tokens = whitespace_tokens(value);
    let first = *tokens.first()?;
    let second = tokens.get(1).copied().unwrap_or(first);
    Some(ShorthandValue::Edges([first, second].map(serialize_value)))
}

fn longhand_value(declaration: &Declaration) -> Option<EdgeValue<'_>> {
    let source = declaration_value(declaration)?;
    let value = trim_boundary(&source);
    Some(if contains_variable(value) {
        EdgeValue::Variable(&declaration.value)
    } else {
        EdgeValue::Plain(serialize_value(value))
    })
}

/// Read the declared value and important flag, independently of computed style.
pub(crate) fn specified(declarations: &[Declaration], name: &str) -> (String, bool) {
    if let Some(property) = ReflectedLonghand::named(name) {
        return declarations
            .iter()
            .filter(|declaration| declaration.name == name)
            .filter_map(|declaration| {
                let value = property.serialize(&declaration.value)?;
                Some((value, declaration.important))
            })
            .max_by_key(|(_, important)| *important)
            .unwrap_or_default();
    }
    let Some((axis, target)) = Axis::find(name) else {
        return declarations
            .iter()
            .filter(|declaration| declaration.name == name)
            .max_by_key(|declaration| declaration.important)
            .map(|declaration| (declaration.value.clone(), declaration.important))
            .unwrap_or_default();
    };
    let mut edges = [None, None];
    for declaration in declarations {
        let Some(part) = axis.part(&declaration.name) else {
            continue;
        };
        match part {
            Part::Both => {
                let [first, second] = match shorthand_value(declaration) {
                    Some(ShorthandValue::Edges(values)) => values.map(EdgeValue::Plain),
                    Some(ShorthandValue::Pending) => {
                        [EdgeValue::PendingShorthand, EdgeValue::PendingShorthand]
                    }
                    None => continue,
                };
                choose(&mut edges[0], declaration, first);
                choose(&mut edges[1], declaration, second);
            }
            Part::Edge(index) => {
                if let Some(value) = longhand_value(declaration) {
                    choose(&mut edges[index], declaration, value);
                }
            }
        }
    }
    if let Part::Edge(index) = target {
        let Some(edge) = edges[index].take() else {
            return (String::new(), false);
        };
        let value = match edge.value {
            EdgeValue::Plain(value) => value,
            EdgeValue::Variable(value) => value.to_owned(),
            EdgeValue::PendingShorthand => String::new(),
        };
        return (value, edge.declaration.important);
    }
    let [Some(start), Some(end)] = edges else {
        return (String::new(), false);
    };
    let important = start.declaration.important;
    if important != end.declaration.important {
        return (String::new(), false);
    }
    let value = match (start.value, end.value) {
        (EdgeValue::Plain(start), EdgeValue::Plain(end)) => serialize_pair(start, end),
        (EdgeValue::PendingShorthand, EdgeValue::PendingShorthand)
            if std::ptr::eq(start.declaration, end.declaration) =>
        {
            start.declaration.value.clone()
        }
        _ => String::new(),
    };
    (value, important)
}

/// Replace or remove each declared edge. A surviving ordinary edge stays at its
/// former source position relative to physical declarations in the same group.
pub(crate) fn edit(
    declarations: &mut Vec<Declaration>,
    name: &str,
    replacement: Option<Declaration>,
) -> bool {
    let target = Axis::find(name);
    // If the same declaration is already last, moving it cannot change the
    // cascade. Only skip standalone, unique declarations: an edit can still
    // replace a stronger duplicate or alter other edges of a logical axis.
    if target.is_none()
        && let Some((last, earlier)) = declarations.split_last()
        && let Some(new) = replacement.as_ref()
        && last.name == name
        && last.value == new.value
        && last.important == new.important
        && !earlier.iter().any(|declaration| declaration.name == name)
    {
        return false;
    }
    let mut changed = replacement.is_some();
    declarations.retain_mut(|declaration| {
        if declaration.name == name {
            changed = true;
            return false;
        }
        let Some((axis, part)) = target else {
            return true;
        };
        let Some(other) = axis.part(&declaration.name) else {
            return true;
        };
        match (part, other) {
            (Part::Both, _) => {
                changed = true;
                false
            }
            (Part::Edge(index), Part::Both) => {
                changed = true;
                match shorthand_value(declaration) {
                    Some(ShorthandValue::Edges([start, end])) => {
                        declaration.name = axis.edges[1 - index].to_owned();
                        declaration.value = if index == 0 { end } else { start };
                        true
                    }
                    // A shorthand with unresolved substitutions has no independent
                    // edge text. Keep it only if the new edge can override it.
                    Some(ShorthandValue::Pending) => replacement
                        .as_ref()
                        .is_some_and(|new| !declaration.important || new.important),
                    None => false,
                }
            }
            (Part::Edge(_), Part::Edge(_)) => true,
        }
    });
    if let Some(declaration) = replacement {
        declarations.push(declaration);
    }
    changed
}
