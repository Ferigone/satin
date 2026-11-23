//! Typed arithmetic for finite CSS length-percentage calculations.
use super::{CalcLength, absolute_length_scale, identifier};

const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_TERMS: usize = 256;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy)]
enum Unit {
    Px,
    Percent,
    Em,
    Rem,
    Vw,
    Vh,
}

impl Unit {
    fn dimension(value: &str) -> Option<(Self, f64)> {
        let value = value.to_ascii_lowercase();
        let unit = match value.as_str() {
            "em" => (Self::Em, 1.),
            "rem" => (Self::Rem, 1.),
            "vw" => (Self::Vw, 1.),
            "vh" => (Self::Vh, 1.),
            _ => (Self::Px, absolute_length_scale(&value)?),
        };
        Some(unit)
    }
}

#[derive(Clone, Copy)]
struct Terms {
    coefficients: [f64; 6],
    has_percent: bool,
}

impl Terms {
    fn new(unit: Unit, value: f64) -> Self {
        let mut coefficients = [0.; 6];
        coefficients[unit as usize] = value;
        Self {
            coefficients,
            has_percent: matches!(unit, Unit::Percent),
        }
    }

    fn map(self, f: impl FnMut(f64) -> f64) -> Self {
        Self {
            coefficients: self.coefficients.map(f),
            ..self
        }
    }

    fn add(mut self, rhs: Self, sign: f64) -> Self {
        for (left, right) in self.coefficients.iter_mut().zip(rhs.coefficients) {
            *left += right * sign;
        }
        // A canceled or zero percentage still has a percentage type.
        self.has_percent |= rhs.has_percent;
        self
    }

    fn finish(self) -> Option<CalcLength> {
        let coefficients = self.coefficients.map(|n| n as f32);
        if !coefficients.iter().all(|n| n.is_finite()) {
            return None;
        }
        let [px, percent, em, rem, vw, vh] = coefficients;
        Some(CalcLength {
            px,
            percent,
            em,
            rem,
            vw,
            vh,
            has_percent: self.has_percent,
            nonnegative: false,
        })
    }
}

enum Numeric {
    Number(f64),
    Length(Terms),
}

impl Numeric {
    fn number(n: f64) -> Option<Self> {
        n.is_finite().then_some(Self::Number(n))
    }

    fn add(self, rhs: Self, operation: u8) -> Option<Self> {
        let sign = if operation == b'+' { 1. } else { -1. };
        match (self, rhs) {
            (Self::Number(a), Self::Number(b)) => Self::number(a + b * sign),
            (Self::Length(a), Self::Length(b)) => Some(Self::Length(a.add(b, sign))),
            _ => None,
        }
    }

    fn multiply(self, rhs: Self, operation: u8) -> Option<Self> {
        match (operation, self, rhs) {
            (b'*', Self::Number(a), Self::Number(b)) => Self::number(a * b),
            (b'*', Self::Length(length), Self::Number(n))
            | (b'*', Self::Number(n), Self::Length(length)) => {
                Some(Self::Length(length.map(|v| v * n)))
            }
            (b'/', Self::Number(a), Self::Number(b)) if b != 0. => Self::number(a / b),
            (b'/', Self::Length(length), Self::Number(n)) if n != 0. => {
                Some(Self::Length(length.map(|v| v / n)))
            }
            _ => None,
        }
    }
}

pub(super) fn parse(input: &str) -> Option<CalcLength> {
    if input.len() > MAX_INPUT_BYTES {
        return None;
    }
    // A CRLF pair is one code point after CSS preprocessing. In particular
    // it cannot both terminate a hex escape and separate adjacent tokens.
    let normalized = input.contains("\r\n").then(|| input.replace("\r\n", "\n"));
    let mut parser = Parser {
        input: normalized.as_deref().unwrap_or(input),
        terms_left: MAX_TERMS,
    };
    parser.skip_trivia();
    let Numeric::Length(length) = parser.function(0)? else {
        return None;
    };
    parser.skip_trivia();
    if !parser.input.is_empty() {
        return None;
    }
    length.finish()
}

struct Parser<'a> {
    input: &'a str,
    terms_left: usize,
}

impl Parser<'_> {
    fn skip_trivia(&mut self) -> bool {
        let mut whitespace = false;
        loop {
            let rest = self
                .input
                .trim_start_matches(|c: char| c.is_ascii_whitespace());
            whitespace |= rest.len() != self.input.len();
            self.input = rest;
            if let Some(rest) = self.input.strip_prefix('\0') {
                // CSS declaration parsing records comment boundaries as NUL.
                self.input = rest;
            } else if let Some(comment) = self.input.strip_prefix("/*") {
                self.input = comment.split_once("*/").map_or("", |(_, rest)| rest);
            } else {
                return whitespace;
            }
        }
    }

    fn function(&mut self, depth: usize) -> Option<Numeric> {
        if !self.ident()?.eq_ignore_ascii_case("calc") {
            return None;
        }
        self.group(depth)
    }

    fn group(&mut self, depth: usize) -> Option<Numeric> {
        if depth == MAX_DEPTH {
            return None;
        }
        self.input = self.input.strip_prefix('(')?;
        let number = self.sum(depth + 1)?;
        self.skip_trivia();
        self.input = self.input.strip_prefix(')')?;
        Some(number)
    }

    fn sum(&mut self, depth: usize) -> Option<Numeric> {
        let mut value = self.product(depth)?;
        loop {
            let left_space = self.skip_trivia();
            let Some(&operation @ (b'+' | b'-')) = self.input.as_bytes().first() else {
                return Some(value);
            };
            self.input = &self.input[1..];
            if !left_space || !self.skip_trivia() {
                return None;
            }
            value = value.add(self.product(depth)?, operation)?;
        }
    }

    fn product(&mut self, depth: usize) -> Option<Numeric> {
        let mut value = self.atom(depth)?;
        loop {
            let before_trivia = self.input;
            self.skip_trivia();
            let Some(&operation @ (b'*' | b'/')) = self.input.as_bytes().first() else {
                // The sum parser needs to know if a following + or - has whitespace.
                self.input = before_trivia;
                return Some(value);
            };
            self.input = &self.input[1..];
            value = value.multiply(self.atom(depth)?, operation)?;
        }
    }

    fn atom(&mut self, depth: usize) -> Option<Numeric> {
        self.skip_trivia();
        match self.input.as_bytes().first()? {
            b'(' => self.group(depth),
            b'c' | b'C' | b'\\' => self.function(depth),
            _ => self.quantity(),
        }
    }

    fn quantity(&mut self) -> Option<Numeric> {
        self.terms_left = self.terms_left.checked_sub(1)?;
        let end = number_end(self.input)?;
        let n = self.input[..end].parse::<f64>().ok()?;
        if !n.is_finite() {
            return None;
        }
        self.input = &self.input[end..];
        if let Some(rest) = self.input.strip_prefix('%') {
            self.input = rest;
            return Some(Numeric::Length(Terms::new(Unit::Percent, n)));
        }
        if let Some(unit) = self.ident() {
            let (unit, factor) = Unit::dimension(&unit)?;
            return Some(Numeric::Length(Terms::new(unit, n * factor)));
        }
        Some(Numeric::Number(n))
    }

    fn ident(&mut self) -> Option<String> {
        // The shared CSS helper skips a leading comment boundary. A function
        // name or unit must be adjacent to its opening parenthesis or number.
        if self.input.starts_with('\0') {
            return None;
        }
        let mut end = 0;
        let name = identifier(self.input, &mut end)?;
        self.input = &self.input[end..];
        Some(name)
    }
}

pub(super) fn number_end(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    let past_sign = |i| i + usize::from(matches!(bytes.get(i), Some(b'+' | b'-')));
    let past_digits = |i| i + bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
    let start = past_sign(0);
    let mut end = past_digits(start);
    if bytes.get(end) == Some(&b'.') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
        end = past_digits(end + 1);
    }
    if end == start {
        return None;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let start = past_sign(end + 1);
        let exponent = past_digits(start);
        if exponent != start {
            end = exponent;
        }
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn calculations_preserve_units_and_precedence() {
        let result = parse("CaLc(100vh + 2 * (20px + calc(50% - .5em)) + +1e1vw - 2rem)").unwrap();
        assert_eq!(result.px, 40.);
        assert_eq!(result.percent, 100.);
        assert_eq!(result.em, -1.);
        assert_eq!(result.rem, -2.);
        assert_eq!(result.vw, 10.);
        assert_eq!(result.vh, 100.);
        assert!(result.has_percent);
        assert!(!result.nonnegative);

        let result = parse("calc((1in + 2.54cm + 25.4mm + 101.6q + 72pt + 6pc) / 2 / 3)").unwrap();
        assert_eq!(result.px, 96.);
        assert!(!result.has_percent);
        let result = parse("calc(10% - 10% + 20px)").unwrap();
        assert_eq!(result.percent, 0.);
        assert!(result.has_percent);
    }

    #[test]
    fn calculation_requires_complete_compatible_tokens() {
        for value in [
            "calc(1px+ 2px)",
            "calc(1px +2px)",
            "calc(1px/**/+ 2px)",
            "calc(1px +/**/2px)",
            "calc(1px\0+ 2px)",
            "calc(1\0px + 2px)",
            "calc/**/(1px)",
            "calc(1/**/px)",
            "calc(1px - - 2px)",
            "calc(1.px)",
            "calc(1e+px)",
            "calc(2px * 3px)",
            "calc(4px / 2px * 1px)",
            "calc(3px / -0.0)",
            "calc(3px + 0)",
            "calc(2 - 2)",
            "calc(1e200px)",
            "calc(1px))",
            "calc(min(3px, 4px))",
        ] {
            assert!(parse(value).is_none(), "{value}");
        }
        assert_eq!(parse("calc(1px \0+/* */ 2px)").unwrap().px, 3.);
        assert_eq!(parse(r"calc(1p\78  + 2px)").unwrap().px, 3.);
        assert!(parse("calc(1p\\78\r\n+ 2px)").is_none());
        assert_eq!(parse("calc(-.5px*-6)").unwrap().px, 3.);
    }

    #[test]
    fn calculation_limits_cover_required_depth_and_term_count() {
        let nested = format!("calc({}1px{})", "(".repeat(32), ")".repeat(32));
        assert_eq!(parse(&nested).unwrap().px, 1.);
        let expression = format!("calc({})", vec!["1px"; 32].join(" + "));
        assert_eq!(parse(&expression).unwrap().px, 32.);
        let too_deep = format!("calc({}1px{})", "(".repeat(64), ")".repeat(64));
        assert!(parse(&too_deep).is_none());
        let too_many = format!("calc({})", vec!["1px"; 257].join(" + "));
        assert!(parse(&too_many).is_none());
    }
}
