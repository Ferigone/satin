use crate::{
    css::Length,
    dom::{NodeId, NodeKind},
    script::{Shared, abort, node, webidl},
};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};

const MAX_THRESHOLDS: usize = 1024;

pub(super) struct Options {
    pub root: Option<NodeId>,
    pub root_element: Option<NodeId>,
    pub margin: [Length; 4],
    pub thresholds: Vec<f64>,
}

impl Options {
    pub fn convert(
        vm: &mut Vm,
        state: &Shared,
        value: Value,
        exception: &Value,
    ) -> Result<Self, JsError> {
        if !value.is_nullish() && value.as_object().is_none() {
            return Err(JsError::type_error("Expected IntersectionObserver options"));
        }
        let root = member(vm, &value, "root")?;
        let (root, root_element) = if root.is_nullish() {
            (None, None)
        } else {
            let id = node(state, &root)?;
            match state.borrow().document.nodes[id.0].kind {
                NodeKind::Document => (Some(id), None),
                NodeKind::Element { .. } => (Some(id), Some(id)),
                _ => return Err(JsError::type_error("Expected an Element or Document root")),
            }
        };
        let margin = member(vm, &value, "rootMargin")?;
        let margin = if margin.is_undefined() {
            String::new()
        } else {
            webidl::usv_string(vm, &margin, 64 * 1024)?
        };
        let threshold = member(vm, &value, "threshold")?;
        let mut thresholds = convert_thresholds(vm, threshold)?;

        let Some(margin) = parse_margin(&margin) else {
            return Err(abort::dom_exception_error(
                vm,
                exception.clone(),
                "SyntaxError",
                "rootMargin must have one to four absolute lengths or percentages",
            )?);
        };
        if thresholds.iter().any(|n| !(0. ..=1.).contains(n)) {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "IntersectionObserver thresholds must be between 0 and 1",
            ));
        }
        if thresholds.is_empty() {
            thresholds.push(0.);
        }
        thresholds.sort_by(f64::total_cmp);
        Ok(Self {
            root,
            root_element,
            margin,
            thresholds,
        })
    }

    pub fn serialized_margin(&self) -> String {
        self.margin
            .map(|length| match length {
                Length::Percent(n) => format!("{}%", n + 0.),
                Length::Px(n) => format!("{}px", n + 0.),
                _ => unreachable!("observer margins contain only absolute lengths and percentages"),
            })
            .join(" ")
    }
}

fn member(vm: &mut Vm, value: &Value, key: &str) -> Result<Value, JsError> {
    if value.is_nullish() {
        Ok(Value::Undefined)
    } else {
        vm.get_property(value.clone(), key)
    }
}

fn convert_thresholds(vm: &mut Vm, value: Value) -> Result<Vec<f64>, JsError> {
    if value.is_undefined() {
        return Ok(Vec::new());
    }
    if value.as_object().is_some() {
        let iterator = webidl::iterator_method(vm, &value)?;
        if !iterator.is_nullish() {
            return webidl::sequence(vm, value, iterator, MAX_THRESHOLDS, finite_double);
        }
    }
    Ok(vec![finite_double(vm, value)?])
}

fn finite_double(vm: &mut Vm, value: Value) -> Result<f64, JsError> {
    let value = vm.to_number(&value)?;
    if !value.is_finite() {
        return Err(JsError::type_error(
            "IntersectionObserver threshold must be finite",
        ));
    }
    Ok(value)
}

fn parse_margin(mut input: &str) -> Option<[Length; 4]> {
    use Length::Px;
    let mut margins = [Px(0.); 4];
    let mut count = 0;
    loop {
        input = input.trim_start_matches(|c: char| c.is_ascii_whitespace());
        if let Some(comment) = input.strip_prefix("/*") {
            input = comment.split_once("*/").map_or("", |(_, rest)| rest);
            continue;
        }
        if input.is_empty() {
            break;
        }
        let number = number_prefix(input);
        let value = input[..number].parse::<f64>().ok()?;
        let suffix = &input[number..];
        let (unit, remaining) = if let Some(rest) = suffix.strip_prefix('%') {
            ("%", rest)
        } else {
            let end = suffix
                .bytes()
                .take_while(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') || !byte.is_ascii()
                })
                .count();
            suffix.split_at(end)
        };
        let scale = match unit.to_ascii_lowercase().as_str() {
            "%" | "px" => 1.,
            "in" => 96.,
            "cm" => 96. / 2.54,
            "mm" => 96. / 25.4,
            "q" => 96. / 101.6,
            "pt" => 96. / 72.,
            "pc" => 16.,
            _ => return None,
        };
        let value = (value * scale).clamp(i32::MIN as f64, i32::MAX as f64);
        *margins.get_mut(count)? = if unit == "%" {
            Length::Percent(value as f32)
        } else {
            Px(value.floor() as f32)
        };
        count += 1;
        input = remaining;
    }
    let [top, right, bottom, _] = margins;
    Some(match count {
        0 | 4 => margins,
        1 => [top; 4],
        2 => [top, right, top, right],
        3 => [top, right, bottom, right],
        _ => unreachable!(),
    })
}

fn number_prefix(input: &str) -> usize {
    let bytes = input.as_bytes();
    let skip_sign = |start| start + usize::from(matches!(bytes.get(start), Some(b'+' | b'-')));
    let digits = |start| {
        start
            + bytes[start..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count()
    };
    let mut end = digits(skip_sign(0));
    if bytes.get(end) == Some(&b'.') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
        end = digits(end + 1);
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let start = skip_sign(end + 1);
        let exponent = digits(start);
        if exponent != start {
            end = exponent;
        }
    }
    end
}
