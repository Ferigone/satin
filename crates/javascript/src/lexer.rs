use crate::{ErrorKind, JsBigInt, JsError, JsString};

const BIGINT_SOURCE_WORK: usize = 8_388_608;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TokenKind {
    Word(String),
    EscapedWord(String),
    PrivateIdentifier(JsString),
    Number(f64),
    BigInt(JsBigInt),
    String(JsString),
    Template {
        raw: JsString,
        cooked: Option<JsString>,
        head: bool,
        tail: bool,
    },
    Punct(&'static str),
    Invalid,
    Eof,
}
#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub line: usize,
    pub column: usize,
    pub newline: bool,
    pub error: Option<Box<JsError>>,
    slash: Option<Box<SlashPosition>>,
}
#[derive(Clone, Debug)]
struct SlashPosition {
    pos: usize,
    templates: Vec<usize>,
}

pub(crate) struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    column: usize,
    newline: bool,
    templates: Vec<usize>,
    bigint_work: usize,
}
impl Lexer {
    pub(crate) fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            column: 1,
            newline: false,
            templates: Vec::new(),
            bigint_work: BIGINT_SOURCE_WORK,
        }
    }

    fn peek(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek(0)?;
        self.pos += 1;
        if c == '\n' || c == '\u{2028}' || c == '\u{2029}' {
            self.line += 1;
            self.column = 1;
            self.newline = true;
        } else if c == '\r' {
            self.line += 1;
            self.column = 1;
            self.newline = true;
            if self.peek(0) == Some('\n') {
                self.pos += 1;
            }
        } else {
            self.column += 1;
        }
        Some(c)
    }
    fn error(&self, message: impl Into<String>) -> JsError {
        let message = message.into();
        let mut error = JsError::syntax(&message, self.line, self.column);
        if message.starts_with("Unsupported") || message.contains("unsupported") {
            error.kind = ErrorKind::UnsupportedFeature;
        }
        error
    }
    pub(crate) fn next(&mut self) -> Token {
        let trivia = self.skip_trivia();
        let line = self.line;
        let column = self.column;
        let newline = std::mem::take(&mut self.newline);
        let slash = (self.peek(0) == Some('/')).then(|| {
            Box::new(SlashPosition {
                pos: self.pos,
                templates: self.templates.clone(),
            })
        });
        let (kind, error) = match trivia.and_then(|()| self.kind()) {
            Ok(kind) => (kind, None),
            Err(error) => (TokenKind::Invalid, Some(Box::new(error))),
        };
        Token {
            kind,
            line,
            column,
            newline,
            error,
            slash,
        }
    }

    fn skip_trivia(&mut self) -> Result<(), JsError> {
        while let Some(c) = self.peek(0) {
            if c.is_whitespace() || c == '\u{feff}' {
                self.bump();
                continue;
            }
            if c == '/' && self.peek(1) == Some('/') {
                while self
                    .peek(0)
                    .is_some_and(|x| !matches!(x, '\r' | '\n' | '\u{2028}' | '\u{2029}'))
                {
                    self.bump();
                }
                continue;
            }
            if c == '/' && self.peek(1) == Some('*') {
                self.bump();
                self.bump();
                loop {
                    match (self.peek(0), self.peek(1)) {
                        (Some('*'), Some('/')) => {
                            self.bump();
                            self.bump();
                            break;
                        }
                        (None, _) => return Err(self.error("Unterminated comment")),
                        _ => {
                            self.bump();
                        }
                    }
                }
                continue;
            }
            break;
        }
        Ok(())
    }

    fn kind(&mut self) -> Result<TokenKind, JsError> {
        let Some(c) = self.peek(0) else {
            return if self.templates.is_empty() {
                Ok(TokenKind::Eof)
            } else {
                Err(self.error("Unterminated template substitution"))
            };
        };
        if c == '#' {
            self.bump();
            let (name, _) = self.identifier(String::from("#"))?;
            return Ok(TokenKind::PrivateIdentifier(name.into()));
        }
        Ok(
            if c == '`' || (c == '}' && self.templates.last() == Some(&0)) {
                let head = c == '`';
                if !head {
                    self.templates.pop();
                }
                self.template(head)?
            } else if c == '\'' || c == '"' {
                TokenKind::String(self.string(c)?)
            } else if c.is_ascii_digit()
                || (c == '.' && self.peek(1).is_some_and(|x| x.is_ascii_digit()))
            {
                self.number()?
            } else if c == '\\' || identifier_start(c) {
                let (name, escaped) = self.identifier(String::new())?;
                if escaped {
                    TokenKind::EscapedWord(name)
                } else {
                    TokenKind::Word(name)
                }
            } else {
                let mut found = None;
                for punct in [
                    ">>>=", "&&=", "||=", "??=", "===", "!==", ">>>", "**=", "<<=", ">>=", "?.",
                    "=>", "==", "!=", "<=", ">=", "++", "--", "&&", "||", "??", "+=", "-=", "*=",
                    "/=", "%=", "**", "<<", ">>", "&=", "|=", "^=", "...", "{", "}", "(", ")", "[",
                    "]", ".", ";", ",", ":", "?", "+", "-", "*", "/", "%", "!", "~", "<", ">", "=",
                    "&", "|", "^",
                ] {
                    if punct == "?." && self.peek(2).is_some_and(|c| c.is_ascii_digit()) {
                        continue;
                    }
                    if punct
                        .chars()
                        .enumerate()
                        .all(|(i, ch)| self.peek(i) == Some(ch))
                    {
                        found = Some(punct);
                        break;
                    }
                }
                let punct = found.ok_or_else(|| self.error(format!("Unsupported token {c:?}")))?;
                for _ in punct.chars() {
                    self.bump();
                }
                if let Some(depth) = self.templates.last_mut() {
                    if punct == "{" {
                        *depth += 1;
                    } else if punct == "}" {
                        *depth -= 1;
                    }
                }
                TokenKind::Punct(punct)
            },
        )
    }

    fn identifier(&mut self, mut name: String) -> Result<(String, bool), JsError> {
        let prefix_len = name.len();
        let mut escaped = false;
        while let Some(next) = self.peek(0) {
            let allowed = if name.len() == prefix_len {
                identifier_start
            } else {
                identifier_continue
            };
            let ch = if next == '\\' {
                escaped = true;
                self.bump();
                if self.bump() != Some('u') {
                    return Err(self.error("Expected a Unicode escape in an identifier"));
                }
                let point = if self.peek(0) == Some('{') {
                    self.bump();
                    let mut point = self.hex(1)?;
                    while self.peek(0) != Some('}') {
                        point = point * 16 + self.hex(1)?;
                        if point > 0x10ffff {
                            return Err(self.error("Invalid Unicode code point"));
                        }
                    }
                    self.bump();
                    point
                } else {
                    self.hex(4)?
                };
                let ch = char::from_u32(point)
                    .ok_or_else(|| self.error("Invalid Unicode code point"))?;
                if !allowed(ch) {
                    return Err(self.error("Invalid escape in an identifier"));
                }
                ch
            } else if allowed(next) {
                self.bump().unwrap()
            } else {
                break;
            };
            name.push(ch);
        }
        if name.len() == prefix_len {
            return Err(self.error("Expected a private identifier after #"));
        }
        Ok((name, escaped))
    }

    pub(crate) fn read_regexp(&mut self, slash: &Token) -> Result<(JsString, JsString), JsError> {
        let checkpoint = slash
            .slash
            .as_ref()
            .expect("RegExp starts at a slash token");
        self.pos = checkpoint.pos;
        self.line = slash.line;
        self.column = slash.column;
        self.newline = false;
        self.templates.clone_from(&checkpoint.templates);
        self.bump();
        let pattern_start = self.pos;
        let mut in_class = false;
        loop {
            match self.peek(0) {
                None | Some('\r' | '\n' | '\u{2028}' | '\u{2029}') => {
                    return Err(self.error("Unterminated regular expression literal"));
                }
                Some('/') if !in_class => break,
                Some('\\') => {
                    self.bump();
                    if self
                        .peek(0)
                        .is_none_or(|c| matches!(c, '\r' | '\n' | '\u{2028}' | '\u{2029}'))
                    {
                        return Err(self.error("Unterminated regular expression escape"));
                    }
                }
                Some('[') => in_class = true,
                Some(']') => in_class = false,
                _ => {}
            }
            self.bump();
        }
        let pattern_end = self.pos;
        self.bump();
        let flag_start = self.pos;
        while self
            .peek(0)
            .is_some_and(crate::regexp::literal_flag_character)
        {
            self.bump();
        }
        let utf16 = |slice: &[char]| {
            let mut units = Vec::with_capacity(slice.len());
            for ch in slice {
                let mut pair = [0; 2];
                units.extend_from_slice(ch.encode_utf16(&mut pair));
            }
            JsString::from_utf16(units)
        };
        Ok((
            utf16(&self.chars[pattern_start..pattern_end]),
            utf16(&self.chars[flag_start..self.pos]),
        ))
    }
    fn template(&mut self, head: bool) -> Result<TokenKind, JsError> {
        self.bump(); // opening backtick, or substitution's closing brace
        let mut raw = Vec::new();
        let tail = loop {
            match self.peek(0) {
                None => return Err(self.error("Unterminated template literal")),
                Some('`') => {
                    self.bump();
                    break true;
                }
                Some('$') if self.peek(1) == Some('{') => {
                    self.bump();
                    self.bump();
                    self.templates.push(0);
                    break false;
                }
                Some('\\') => {
                    raw.push(92);
                    self.bump();
                    let c = self
                        .bump()
                        .ok_or_else(|| self.error("Unterminated template escape"))?;
                    let mut buf = [0; 2];
                    if c == '\r' {
                        raw.push(10);
                    } else {
                        raw.extend_from_slice(c.encode_utf16(&mut buf));
                    }
                }
                Some(_) => {
                    let c = self.bump().unwrap();
                    let mut buf = [0; 2];
                    if c == '\r' {
                        raw.push(10);
                    } else {
                        raw.extend_from_slice(c.encode_utf16(&mut buf));
                    }
                }
            }
        };
        let cooked = cook_template(&raw);
        // Line terminators inside a template are part of its token, not ASI trivia.
        self.newline = false;
        Ok(TokenKind::Template {
            raw: JsString::from_utf16(raw),
            cooked,
            head,
            tail,
        })
    }
    fn hex(&mut self, n: usize) -> Result<u32, JsError> {
        let mut value = 0;
        for _ in 0..n {
            let c = self.bump().ok_or_else(|| self.error("Incomplete escape"))?;
            value = value * 16
                + c.to_digit(16)
                    .ok_or_else(|| self.error("Invalid hexadecimal escape"))?;
        }
        Ok(value)
    }
    fn string(&mut self, quote: char) -> Result<JsString, JsError> {
        self.bump();
        let mut units = Vec::new();
        loop {
            let c = self
                .bump()
                .ok_or_else(|| self.error("Unterminated string literal"))?;
            if c == quote {
                return Ok(JsString::from_utf16(units));
            }
            if matches!(c, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
                return Err(self.error("Line terminator in string literal"));
            }
            if c != '\\' {
                let mut buf = [0; 2];
                units.extend_from_slice(c.encode_utf16(&mut buf));
                continue;
            }
            let escape = self
                .bump()
                .ok_or_else(|| self.error("Unterminated string escape"))?;
            match escape {
                'n' => units.push(10),
                'r' => units.push(13),
                't' => units.push(9),
                'b' => units.push(8),
                'f' => units.push(12),
                'v' => units.push(11),
                '0' => {
                    if self.peek(0).is_some_and(|x| x.is_ascii_digit()) {
                        return Err(self.error("Legacy octal escapes are unsupported"));
                    }
                    units.push(0);
                }
                'x' => units.push(self.hex(2)? as u16),
                'u' => {
                    if self.peek(0) == Some('{') {
                        self.bump();
                        let mut value = 0u32;
                        let mut count = 0;
                        while self.peek(0) != Some('}') {
                            let c = self
                                .bump()
                                .ok_or_else(|| self.error("Unterminated Unicode escape"))?;
                            value = value
                                .checked_mul(16)
                                .and_then(|v| c.to_digit(16).and_then(|d| v.checked_add(d)))
                                .ok_or_else(|| self.error("Invalid Unicode escape"))?;
                            count += 1;
                            if count > 6 {
                                return Err(self.error("Invalid Unicode code point"));
                            }
                        }
                        self.bump();
                        if count == 0 || value > 0x10ffff {
                            return Err(self.error("Invalid Unicode code point"));
                        }
                        if value <= 0xffff {
                            units.push(value as u16)
                        } else {
                            let n = value - 0x10000;
                            units.push((0xd800 + (n >> 10)) as u16);
                            units.push((0xdc00 + (n & 0x3ff)) as u16);
                        }
                    } else {
                        units.push(self.hex(4)? as u16);
                    }
                }
                '\n' | '\r' | '\u{2028}' | '\u{2029}' => {}
                '1'..='9' => return Err(self.error("Legacy octal escapes are unsupported")),
                other => {
                    let mut buf = [0; 2];
                    units.extend_from_slice(other.encode_utf16(&mut buf));
                }
            }
        }
    }
    fn number(&mut self) -> Result<TokenKind, JsError> {
        let start = self.pos;
        if self.peek(0) == Some('0')
            && let Some(radix) = match self.peek(1) {
                Some('x' | 'X') => Some(16),
                Some('b' | 'B') => Some(2),
                Some('o' | 'O') => Some(8),
                _ => None,
            }
        {
            self.bump();
            self.bump();
            let digits = self.pos;
            if self.digits(radix)? == 0 {
                return Err(self.error("Invalid numeric literal"));
            }
            if self.peek(0) == Some('n') {
                return self.bigint(digits, radix);
            }
            self.number_suffix()?;
            let value = self.chars[digits..self.pos]
                .iter()
                .filter_map(|c| c.to_digit(radix))
                .fold(0f64, |value, digit| {
                    value * f64::from(radix) + f64::from(digit)
                });
            return Ok(TokenKind::Number(value));
        }
        let integers = self.digits(10)?;
        let leading_zero = integers > 1 && self.chars[start] == '0';
        if self.peek(0) == Some('n') && integers != 0 {
            if leading_zero {
                return Err(self.error("Decimal BigInt literals cannot have leading zeroes"));
            }
            return self.bigint(start, 10);
        }
        if self.peek(0) == Some('.') {
            self.bump();
            self.digits(10)?;
        }
        if matches!(self.peek(0), Some('e' | 'E')) {
            self.bump();
            if matches!(self.peek(0), Some('+' | '-')) {
                self.bump();
            }
            if self.digits(10)? == 0 {
                return Err(self.error("Missing exponent digits"));
            }
        }
        self.number_suffix()?;
        let raw = &self.chars[start..self.pos];
        if leading_zero && raw.contains(&'_') {
            return Err(self.error("Numeric separator after a leading zero"));
        }
        let text: String = raw.iter().filter(|c| **c != '_').collect();
        text.parse()
            .map(TokenKind::Number)
            .map_err(|_| self.error("Invalid numeric literal"))
    }

    fn digits(&mut self, radix: u32) -> Result<usize, JsError> {
        let mut count = 0;
        while let Some(next) = self.peek(0) {
            if next.is_digit(radix) {
                count += 1;
            } else if next == '_' {
                if count == 0 || !self.peek(1).is_some_and(|c| c.is_digit(radix)) {
                    return Err(self.error("Numeric separators must occur between digits"));
                }
            } else {
                break;
            }
            self.bump();
        }
        Ok(count)
    }

    fn number_suffix(&self) -> Result<(), JsError> {
        if self
            .peek(0)
            .is_some_and(|c| c == '\\' || identifier_continue(c))
        {
            return Err(self.error("Invalid numeric literal suffix"));
        }
        Ok(())
    }

    fn bigint(&mut self, digits: usize, radix: u32) -> Result<TokenKind, JsError> {
        let end = self.pos;
        self.bump();
        self.number_suffix()?;
        let digits: Vec<_> = self.chars[digits..end]
            .iter()
            .filter(|c| **c != '_')
            .map(|c| *c as u8)
            .collect();
        self.charge_bigint_source(crate::bigint::literal_work(&digits, radix))?;
        crate::bigint::literal(&digits, radix).map(TokenKind::BigInt)
    }

    pub(crate) fn bigint_property_name(&mut self, value: &JsBigInt) -> Result<JsString, JsError> {
        let words = (value.bit_length() as usize).div_ceil(32).max(1);
        self.charge_bigint_source(words.saturating_mul(words))?;
        Ok(value.to_string().into())
    }

    fn charge_bigint_source(&mut self, work: usize) -> Result<(), JsError> {
        self.bigint_work = self.bigint_work.checked_sub(work).ok_or_else(|| {
            JsError::new(
                ErrorKind::ExecutionLimit,
                "BigInt source work budget exhausted",
            )
        })?;
        Ok(())
    }
}
fn identifier_start(c: char) -> bool {
    if c.is_ascii() {
        matches!(c, '$' | '_') || c.is_ascii_alphabetic()
    } else {
        crate::regexp::identifier_start(c as u32)
    }
}
fn identifier_continue(c: char) -> bool {
    if c.is_ascii() {
        matches!(c, '$' | '_') || c.is_ascii_alphanumeric()
    } else {
        matches!(c, '\u{200c}' | '\u{200d}') || crate::regexp::identifier_continue(c as u32)
    }
}
fn cook_template(raw: &[u16]) -> Option<JsString> {
    let mut out = Vec::new();
    let mut i = 0;
    fn hex(raw: &[u16], i: &mut usize, count: usize) -> Option<u32> {
        let mut value = 0;
        for _ in 0..count {
            value = value * 16 + char::from_u32(*raw.get(*i)? as u32)?.to_digit(16)?;
            *i += 1;
        }
        Some(value)
    }
    while let Some(&unit) = raw.get(i) {
        i += 1;
        if unit != 92 {
            out.push(unit);
            continue;
        }
        let escape = *raw.get(i)?;
        i += 1;
        match escape {
            110 => out.push(10),
            114 => out.push(13),
            116 => out.push(9),
            98 => out.push(8),
            102 => out.push(12),
            118 => out.push(11),
            10 | 13 | 0x2028 | 0x2029 => {}
            48 if raw.get(i).is_none_or(|u| !(48..=57).contains(u)) => out.push(0),
            48..=57 => return None,
            120 => out.push(hex(raw, &mut i, 2)? as u16),
            117 => {
                let value = if raw.get(i) == Some(&123) {
                    i += 1;
                    let start = i;
                    let mut value = 0u32;
                    while raw.get(i) != Some(&125) {
                        let digit = char::from_u32(*raw.get(i)? as u32)?.to_digit(16)?;
                        value = value.checked_mul(16)?.checked_add(digit)?;
                        i += 1;
                        if value > 0x10ffff {
                            return None;
                        }
                    }
                    if i == start {
                        return None;
                    }
                    i += 1;
                    value
                } else {
                    hex(raw, &mut i, 4)?
                };
                if value <= 0xffff {
                    out.push(value as u16);
                } else {
                    out.push(0xd800 + ((value - 0x10000) >> 10) as u16);
                    out.push(0xdc00 + ((value - 0x10000) & 1023) as u16);
                }
            }
            other => out.push(other),
        }
    }
    Some(JsString::from_utf16(out))
}
