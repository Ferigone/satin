//! Foreground `color-mix(in srgb, …)` expressions awaiting the inherited color.
use super::{
    Color, color, color_number, functional_color_components, matching_close, split_top,
    whitespace_tokens,
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
    Absolute([f64; 4]),
    Current,
    Mix(Mix),
}

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

fn from_color(Color(r, g, b, a): Color) -> [f64; 4] {
    [r, g, b, a].map(|c| f64::from(c) / 255.)
}

fn fraction(token: &str, scale: f64) -> Option<f64> {
    if let Some(percent) = token.strip_suffix('%') {
        Some(color_number(percent)? / 100.)
    } else {
        Some(color_number(token)? / scale)
    }
}

fn absolute(source: &str) -> Option<[f64; 4]> {
    // Keep in-range sRGB literals at full precision through nesting.
    let srgb = |body: &str, rgb_function: bool| -> Option<[f64; 4]> {
        let ([r, g, b], alpha) = functional_color_components(body, rgb_function)?;
        let scale = if rgb_function { 255. } else { 1. };
        let [r, g, b] = [r, g, b].map(|c| fraction(c, scale));
        let a = fraction(alpha.unwrap_or("1"), 1.)?.clamp(0., 1.);
        Some([r?, g?, b?, a])
    };
    if let Some(body) = source
        .strip_prefix("rgb(")
        .or_else(|| source.strip_prefix("rgba("))
        .and_then(|body| body.strip_suffix(')'))
    {
        return srgb(body, true).map(|rgba| rgba.map(|c| c.clamp(0., 1.)));
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
        let rgba = srgb(channels, false)?;
        if rgba.iter().all(|c| (0. ..=1.).contains(c)) {
            return Some(rgba);
        }
    }
    // Other supported absolute syntaxes (including out-of-gamut profiles)
    // go through the renderer's existing sRGB conversion.
    color(source).map(from_color)
}

fn premultiply([r, g, b, a]: [f64; 4]) -> [f64; 4] {
    [r * a, g * a, b * a, a]
}

impl Operand {
    fn premultiplied(&self, inherited: [f64; 4]) -> [f64; 4] {
        match self {
            Self::Absolute(rgba) => premultiply(*rgba),
            Self::Current => inherited,
            Self::Mix(mix) => mix.premultiplied(inherited),
        }
    }
}

impl Mix {
    fn premultiplied(&self, inherited: [f64; 4]) -> [f64; 4] {
        let [left, right] = self.operands.each_ref().map(|c| c.premultiplied(inherited));
        let [p, q] = self.weights;
        // Scaling alpha after interpolation also scales premultiplied channels.
        std::array::from_fn(|i| (left[i] * p + right[i] * q) * self.alpha_multiplier)
    }

    pub(super) fn resolve(&self, inherited: Color) -> Color {
        let [r, g, b, a] = self.premultiplied(premultiply(from_color(inherited)));
        let byte = |c: f64| (c.clamp(0., 1.) * 255. + 1e-12).round() as u8;
        if a == 0. {
            return Color(0, 0, 0, 0);
        }
        Color(byte(r / a), byte(g / a), byte(b / a), byte(a))
    }
}
