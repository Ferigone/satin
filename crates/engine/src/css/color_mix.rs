//! Foreground `color-mix(in srgb, …)` expressions awaiting the inherited color.
use super::{
    Color, color, color_number, functional_color_components, matching_close,
    oklab::encoded_srgb_to_color, split_top, whitespace_tokens,
};

const MAX_BYTES: usize = 16 * 1024;
const MAX_DEPTH: usize = 16;
const MAX_NODES: usize = 64;

#[derive(Debug)]
pub(super) struct Mix {
    operands: Box<[Operand; 2]>,
    weights: [f64; 2],
    alpha_multiplier: f64,
}

#[derive(Debug)]
enum Operand {
    Absolute(Components),
    Current,
    Mix(Mix),
}

// Straight, encoded RGB and alpha. A missing channel survives until interpolation
// with a counterpart supplies it, including across nested mixes.
type Components = [Option<f64>; 4];

pub(super) fn parse(source: &str) -> Option<Mix> {
    if source.len() > MAX_BYTES {
        return None;
    }
    let mut remaining = MAX_NODES;
    match operand(source, 0, &mut remaining)? {
        Operand::Mix(mix) => Some(mix),
        _ => None,
    }
}

fn operand(source: &str, depth: usize, remaining: &mut usize) -> Option<Operand> {
    *remaining = remaining.checked_sub(1)?;
    if source == "currentcolor" {
        return Some(Operand::Current);
    }
    // Require exactly one complete function wherever an operand uses one.
    if let Some(open) = source.find('(')
        && matching_close(source, open, b'(', b')')? != source.len() - 1
    {
        return None;
    }
    let Some(body) = source
        .strip_prefix("color-mix(")
        .and_then(|body| body.strip_suffix(')'))
    else {
        return absolute(source).map(Operand::Absolute);
    };
    if depth >= MAX_DEPTH {
        return None;
    }
    let parts = split_top(body, b',');
    let [method, left, right] = parts.as_slice() else {
        return None;
    };
    if whitespace_tokens(method).as_slice() != ["in", "srgb"] {
        return None;
    }
    let (left, p) = weighted(left)?;
    let (right, q) = weighted(right)?;
    let (p, q) = match (p, q) {
        (None, None) => (50., 50.),
        (Some(p), None) => (p, 100. - p),
        (None, Some(q)) => (100. - q, q),
        (Some(p), Some(q)) => (p, q),
    };
    let sum = p + q;
    if sum == 0. {
        return None;
    }
    Some(Operand::Mix(Mix {
        operands: Box::new([
            operand(left, depth + 1, remaining)?,
            operand(right, depth + 1, remaining)?,
        ]),
        weights: [p / sum, q / sum],
        alpha_multiplier: (sum / 100.).min(1.),
    }))
}

fn weighted(source: &str) -> Option<(&str, Option<f64>)> {
    fn weight(token: &str) -> Option<f64> {
        let value = color_number(token.strip_suffix('%')?)?;
        (0. ..=100.).contains(&value).then_some(value)
    }
    match whitespace_tokens(source).as_slice() {
        [color] => Some((color, None)),
        [p, color] if p.ends_with('%') => Some((color, Some(weight(p)?))),
        [color, p] => Some((color, Some(weight(p)?))),
        _ => None,
    }
}

fn from_color(Color(r, g, b, a): Color) -> Components {
    [r, g, b, a].map(|c| Some(f64::from(c) / 255.))
}

fn fraction(token: &str, scale: f64) -> Option<f64> {
    if let Some(percent) = token.strip_suffix('%') {
        Some(color_number(percent)? / 100.)
    } else {
        Some(color_number(token)? / scale)
    }
}

fn absolute(source: &str) -> Option<Components> {
    let srgb = |body: &str, rgb_function: bool| -> Option<Components> {
        let legacy = body.contains(',');
        let ([r, g, b], alpha) = functional_color_components(body, rgb_function)?;
        let scale = if rgb_function { 255. } else { 1. };
        let component = |token, scale| {
            if token == "none" && !legacy {
                return Some(None);
            }
            fraction(token, scale).map(Some)
        };
        let [r, g, b] = [r, g, b].map(|c| {
            component(c, scale).map(|v| v.map(|v| if rgb_function { v.clamp(0., 1.) } else { v }))
        });
        let a = component(alpha.unwrap_or("1"), 1.)?.map(|v| v.clamp(0., 1.));
        Some([r?, g?, b?, a])
    };
    if let Some(body) = source
        .strip_prefix("rgb(")
        .or_else(|| source.strip_prefix("rgba("))
        .and_then(|body| body.strip_suffix(')'))
    {
        return srgb(body, true);
    }
    if let Some(body) = source
        .strip_prefix("color(")
        .and_then(|body| body.strip_suffix(')'))
        && let Some((profile, channels)) = body
            .trim_ascii_start()
            .split_once(|c: char| c.is_ascii_whitespace())
        && profile == "srgb"
    {
        if channels.contains(',') {
            return None;
        }
        return srgb(channels, false);
    }
    // Other supported absolute syntaxes still use the existing sRGB fallback.
    color(source).map(from_color)
}

fn interpolate(left: f64, right: f64, p: f64, q: f64) -> f64 {
    if left == right {
        return left;
    }
    let (min, max) = (left.min(right), left.max(right));
    if left.abs().max(right.abs()) > f64::MAX / 2. {
        // Room for rounding at the largest finite magnitude, without
        // subtracting opposite-sign endpoints or discarding tiny weights.
        (left * 0.5 * p + right * 0.5 * q).clamp(min * 0.5, max * 0.5) * 2.
    } else {
        (left * p + right * q).clamp(min, max)
    }
}

impl Operand {
    fn components(&self, inherited: Components) -> Components {
        match self {
            Self::Absolute(rgba) => *rgba,
            Self::Current => inherited,
            Self::Mix(mix) => mix.components(inherited),
        }
    }
}

impl Mix {
    fn components(&self, inherited: Components) -> Components {
        let [left, right] = self.operands.each_ref().map(|c| c.components(inherited));
        let [p, q] = self.weights;
        let alpha_missing = left[3].is_none() && right[3].is_none();
        // Borrow alpha first. If neither side has it, interpolate the raw
        // channels and keep the result's alpha missing.
        let a = left[3].or(right[3]).unwrap_or(1.);
        let b = right[3].or(left[3]).unwrap_or(1.);
        // Normalize the alpha contributions, equivalent to premultiplying and
        // then undoing premultiplication, without dividing large RGB values.
        let peak = a.max(b);
        let (p, q) = if peak == 0. {
            (0., 0.)
        } else {
            (a / peak * p, b / peak * q)
        };
        let sum = p + q;
        let channel = |i: usize| match (left[i], right[i]) {
            (None, None) => None,
            _ if sum == 0. => Some(0.),
            (Some(l), Some(r)) => Some(interpolate(l, r, p / sum, q / sum)),
            // With a component missing on one side, both sides use the
            // other side's value before premultiplication.
            (l, r) => l.or(r),
        };
        [
            channel(0),
            channel(1),
            channel(2),
            (!alpha_missing).then_some((sum * peak * self.alpha_multiplier).min(1.)),
        ]
    }

    pub(super) fn resolve(&self, inherited: Color) -> Color {
        let [r, g, b, a] = self
            .components(from_color(inherited))
            .map(|v| v.unwrap_or(0.));
        if a == 0. {
            return Color(0, 0, 0, 0);
        }
        encoded_srgb_to_color([r, g, b], (a * 255. + 1e-12).round() as u8)
    }
}
