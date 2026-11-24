use super::sets::{Operation, Set};
use super::{Flags, MAX_CAPTURES, MAX_PATTERN_DEPTH, limit, syntax};
use crate::{JsError, JsString};
use std::ops::Range;

#[derive(Clone, Copy, Debug)]
pub(super) enum Shorthand {
    Digit,
    Space,
    Word,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Member {
    Point(u32),
    Range(u32, u32),
    Shorthand(Shorthand, bool),
    Property(super::property::Property, bool),
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Assertion {
    Start,
    End,
    WordBoundary(bool),
}

#[derive(Debug)]
pub(super) enum BackReference {
    Number(usize),
    Name(JsString),
}

#[derive(Debug)]
pub(super) enum Expr {
    Sequence(Vec<Expr>),
    Choice(Vec<Expr>),
    Character(Member),
    UnicodeSet(Set),
    Class {
        members: Vec<Member>,
        negated: bool,
    },
    Any,
    Assert(Assertion),
    Capture(usize, Box<Expr>),
    BackReference(BackReference),
    Repeat {
        body: Box<Expr>,
        minimum: usize,
        maximum: Option<usize>,
        greedy: bool,
        captures: Range<usize>,
    },
    Look {
        body: Box<Expr>,
        behind: bool,
        negative: bool,
    },
    Modifiers {
        body: Box<Expr>,
        add: u8,
        remove: u8,
    },
}

pub(super) struct Pattern {
    pub(super) body: Expr,
    pub(super) captures: usize,
    pub(super) wrapping_captures: usize,
    pub(super) names: Vec<(JsString, Vec<usize>)>,
}

struct Named {
    name: JsString,
    sites: Vec<(usize, Vec<(usize, usize)>)>,
}

struct Parser<'a> {
    units: &'a [u16],
    at: usize,
    flags: Flags,
    total_captures: usize,
    next_capture: usize,
    has_named: bool,
    names: Vec<Named>,
    references: Vec<JsString>,
    disjunctions: usize,
    alternatives: Vec<(usize, usize)>,
}

#[derive(Clone, Debug)]
enum Escape {
    Emoji(u8),
    Character(Member),
    Assertion(Assertion),
    Number(usize),
    Name(JsString),
}
struct Quantity {
    minimum: usize,
    maximum: Option<usize>,
}
enum Group {
    Plain,
    Capture(Option<JsString>),
    Look { behind: bool, negative: bool },
    Modifiers { add: u8, remove: u8 },
}

pub(super) fn parse(units: &[u16], flags: Flags) -> Result<Pattern, JsError> {
    let Groups {
        total_captures,
        has_named,
        body,
        wrapping_captures,
    } = scan_groups(units, flags.has(Flags::UNICODE_SETS));
    if total_captures > MAX_CAPTURES {
        return Err(limit("Regular expression capture limit exceeded"));
    }
    let mut parser = Parser {
        units: &units[body],
        at: 0,
        flags,
        total_captures,
        next_capture: wrapping_captures,
        has_named,
        names: Vec::new(),
        references: Vec::new(),
        disjunctions: 0,
        alternatives: Vec::new(),
    };
    let body = parser.disjunction(0)?;
    if parser.peek().is_some() {
        return Err(syntax("Unmatched ')' in regular expression"));
    }
    for name in &parser.references {
        if !parser.names.iter().any(|named| named.name == *name) {
            return Err(syntax("Unknown regular expression capture name"));
        }
    }
    Ok(Pattern {
        body,
        captures: total_captures,
        wrapping_captures,
        names: parser
            .names
            .into_iter()
            .map(|name| {
                (
                    name.name,
                    name.sites.into_iter().map(|(slot, _)| slot).collect(),
                )
            })
            .collect(),
    })
}

struct Groups {
    total_captures: usize,
    has_named: bool,
    body: Range<usize>,
    wrapping_captures: usize,
}

// Count forward references while recognizing groups that enclose the entire
// pattern. Those groups share one span, so compiling their open/close ops in
// a loop avoids building or recursively dropping a chain of identical nodes.
fn scan_groups(units: &[u16], nested_classes: bool) -> Groups {
    let mut at = 0;
    let mut brackets = 0usize;
    let mut depth = 0usize;
    let mut wrappers: Vec<(usize, usize)> = Vec::new();
    let mut captures = 0;
    let mut named = false;
    while let Some(&unit) = units.get(at) {
        match ascii(unit) {
            Some(b'\\') => at += 1,
            Some(b'[') if nested_classes || brackets == 0 => brackets += 1,
            Some(b']') => brackets = brackets.saturating_sub(1),
            Some(b'(') if brackets == 0 => {
                depth += 1;
                let plain_capture = units.get(at + 1).copied().and_then(ascii) != Some(b'?');
                let (previous_end, previous_captures) = wrappers.last().copied().unwrap_or((0, 0));
                if at == previous_end {
                    if plain_capture {
                        wrappers.push((at + 1, previous_captures + 1));
                    } else if units.get(at + 2) == Some(&u16::from(b':')) {
                        wrappers.push((at + 3, previous_captures));
                    }
                }
                if plain_capture {
                    captures += 1;
                } else if units.get(at + 2).copied().and_then(ascii) == Some(b'<')
                    && !matches!(
                        units.get(at + 3).copied().and_then(ascii),
                        Some(b'=' | b'!')
                    )
                {
                    captures += 1;
                    named = true;
                }
            }
            Some(b')') if brackets == 0 => {
                if depth == 0 {
                    wrappers.clear();
                }
                depth = depth.saturating_sub(1);
                if at + depth + 1 != units.len() {
                    wrappers.truncate(depth);
                }
            }
            _ => {}
        }
        at += 1;
    }
    if depth != 0 {
        wrappers.clear();
    }
    let (start, wrapping_captures) = wrappers.last().copied().unwrap_or((0, 0));
    Groups {
        total_captures: captures,
        has_named: named,
        body: start..units.len() - wrappers.len(),
        wrapping_captures,
    }
}

fn ascii(unit: u16) -> Option<u8> {
    u8::try_from(unit).ok().filter(u8::is_ascii)
}

impl Parser<'_> {
    fn peek(&self) -> Option<u16> {
        self.units.get(self.at).copied()
    }
    fn byte(&self) -> Option<u8> {
        self.peek().and_then(ascii)
    }
    fn eat(&mut self, byte: u8) -> bool {
        if self.byte() == Some(byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn point(&mut self, unicode: bool) -> Option<u32> {
        let first = self.peek()?;
        self.at += 1;
        if unicode
            && let Some(second) = self.peek()
            && let Some(point) = decode_pair(first.into(), second.into())
        {
            self.at += 1;
            return Some(point);
        }
        Some(first.into())
    }

    fn disjunction(&mut self, depth: usize) -> Result<Expr, JsError> {
        if depth > MAX_PATTERN_DEPTH {
            return Err(limit("Regular expression nesting limit exceeded"));
        }
        let id = self.disjunctions;
        self.disjunctions += 1;
        self.alternatives.push((id, 0));
        let mut alternatives = Vec::new();
        loop {
            let mut terms = Vec::new();
            while self.peek().is_some() && !matches!(self.byte(), Some(b'|' | b')')) {
                terms.push(self.term(depth)?);
            }
            alternatives.push(sequence(terms));
            if !self.eat(b'|') {
                break;
            }
            self.alternatives.last_mut().unwrap().1 += 1;
        }
        self.alternatives.pop();
        Ok(if alternatives.len() == 1 {
            alternatives.pop().unwrap()
        } else {
            Expr::Choice(alternatives)
        })
    }

    fn term(&mut self, depth: usize) -> Result<Expr, JsError> {
        let first_capture = self.next_capture;
        let atom = self.atom(depth)?;
        let Some(quantity) = self.quantity()? else {
            return Ok(atom);
        };
        let allowed = match &atom {
            Expr::Assert(_) => false,
            Expr::Look { behind, .. } => !*behind && !self.flags.unicode(),
            _ => true,
        };
        if !allowed {
            return Err(syntax("Assertion cannot be quantified"));
        }
        Ok(Expr::Repeat {
            body: Box::new(atom),
            minimum: quantity.minimum,
            maximum: quantity.maximum,
            greedy: !self.eat(b'?'),
            captures: first_capture..self.next_capture,
        })
    }

    fn atom(&mut self, depth: usize) -> Result<Expr, JsError> {
        match self.byte() {
            Some(b'^') => {
                self.at += 1;
                Ok(Expr::Assert(Assertion::Start))
            }
            Some(b'$') => {
                self.at += 1;
                Ok(Expr::Assert(Assertion::End))
            }
            Some(b'.') => {
                self.at += 1;
                Ok(Expr::Any)
            }
            Some(b'(') => self.group(depth),
            Some(b'[') if self.flags.has(Flags::UNICODE_SETS) => {
                Ok(Expr::UnicodeSet(self.unicode_set(depth + 1)?))
            }
            Some(b'[') => self.class(),
            Some(b'\\') => {
                self.at += 1;
                Ok(match self.escape(false)? {
                    Escape::Emoji(mask) => Expr::UnicodeSet(Set::Emoji(mask)),
                    Escape::Character(member) => Expr::Character(member),
                    Escape::Assertion(assertion) => Expr::Assert(assertion),
                    Escape::Number(number) => Expr::BackReference(BackReference::Number(number)),
                    Escape::Name(name) => Expr::BackReference(BackReference::Name(name)),
                })
            }
            Some(b'*' | b'+' | b'?') => Err(syntax("Nothing to repeat in regular expression")),
            Some(b'{') => {
                if self.quantity()?.is_some() {
                    return Err(syntax("Nothing to repeat in regular expression"));
                }
                self.at += 1;
                Ok(Expr::Character(Member::Point(u32::from(b'{'))))
            }
            Some(b'}' | b']') if self.flags.unicode() => {
                Err(syntax("Unexpected regular expression delimiter"))
            }
            _ => Ok(Expr::Character(Member::Point(
                self.point(self.flags.unicode()).unwrap(),
            ))),
        }
    }

    fn group(&mut self, depth: usize) -> Result<Expr, JsError> {
        self.at += 1;
        let kind = if self.eat(b'?') {
            match self.byte() {
                Some(b':') => {
                    self.at += 1;
                    Group::Plain
                }
                Some(b'=' | b'!') => {
                    let negative = self.eat(b'!');
                    if !negative {
                        self.at += 1;
                    }
                    Group::Look {
                        behind: false,
                        negative,
                    }
                }
                Some(b'<') => {
                    self.at += 1;
                    match self.byte() {
                        Some(b'=' | b'!') => {
                            let negative = self.eat(b'!');
                            if !negative {
                                self.at += 1;
                            }
                            Group::Look {
                                behind: true,
                                negative,
                            }
                        }
                        _ => Group::Capture(Some(self.name()?)),
                    }
                }
                Some(b'i' | b'm' | b's' | b'-') => {
                    let (add, remove) = self.modifiers()?;
                    Group::Modifiers { add, remove }
                }
                _ => return Err(syntax("Invalid regular expression group")),
            }
        } else {
            Group::Capture(None)
        };
        let index = self.next_capture;
        if let Group::Capture(name) = &kind {
            self.next_capture += 1;
            if let Some(name) = name {
                self.add_name(name, index)?;
            }
        }
        let body = self.disjunction(depth + 1)?;
        if !self.eat(b')') {
            return Err(syntax("Unterminated regular expression group"));
        }
        Ok(match kind {
            Group::Plain => Expr::Sequence(vec![body]),
            Group::Capture(_) => Expr::Capture(index, Box::new(body)),
            Group::Look { behind, negative } => Expr::Look {
                body: Box::new(body),
                behind,
                negative,
            },
            Group::Modifiers { add, remove } => Expr::Modifiers {
                body: Box::new(body),
                add,
                remove,
            },
        })
    }

    fn modifiers(&mut self) -> Result<(u8, u8), JsError> {
        let mut add = 0;
        let mut remove = 0;
        let mut subtract = false;
        loop {
            match self.byte() {
                Some(b':') => break,
                Some(b'-') if !subtract => subtract = true,
                Some(byte @ (b'i' | b'm' | b's')) => {
                    let flag = Flags::bit(u16::from(byte)).unwrap();
                    if (add | remove) & flag != 0 {
                        return Err(syntax("Duplicate regular expression modifier"));
                    }
                    if subtract {
                        remove |= flag;
                    } else {
                        add |= flag;
                    }
                }
                _ => return Err(syntax("Invalid regular expression modifier")),
            }
            self.at += 1;
        }
        if (add | remove) == 0 {
            return Err(syntax("Empty regular expression modifier"));
        }
        self.at += 1;
        Ok((add, remove))
    }

    fn add_name(&mut self, name: &JsString, index: usize) -> Result<(), JsError> {
        let path = self.alternatives.clone();
        if let Some(existing) = self.names.iter_mut().find(|entry| entry.name == *name) {
            for (_, previous) in &existing.sites {
                let disjoint =
                    previous
                        .iter()
                        .zip(&path)
                        .any(|((id, branch), (current, current_branch))| {
                            id == current && branch != current_branch
                        });
                if !disjoint {
                    return Err(syntax("Duplicate regular expression capture name"));
                }
            }
            existing.sites.push((index, path));
        } else {
            self.names.push(Named {
                name: name.clone(),
                sites: vec![(index, path)],
            });
        }
        Ok(())
    }

    fn name(&mut self) -> Result<JsString, JsError> {
        let mut name = Vec::new();
        while !self.eat(b'>') {
            let point = if self.eat(b'\\') {
                if !self.eat(b'u') {
                    return Err(syntax("Invalid capture name escape"));
                }
                self.unicode_escape(true)
                    .ok_or_else(|| syntax("Invalid capture name escape"))?
            } else {
                self.point(true)
                    .ok_or_else(|| syntax("Unterminated regular expression capture name"))?
            };
            let ch =
                char::from_u32(point).ok_or_else(|| syntax("Invalid capture name character"))?;
            let identifier = if name.is_empty() {
                identifier_start(ch)
            } else {
                identifier_continue(ch)
            };
            if !identifier {
                return Err(syntax("Invalid regular expression capture name"));
            }
            let mut storage = [0; 2];
            name.extend_from_slice(ch.encode_utf16(&mut storage));
        }
        if name.is_empty() {
            return Err(syntax("Empty regular expression capture name"));
        }
        Ok(JsString::from_utf16(name))
    }

    fn digits(&mut self) -> &[u16] {
        let start = self.at;
        while matches!(self.byte(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        &self.units[start..self.at]
    }

    fn quantity(&mut self) -> Result<Option<Quantity>, JsError> {
        let quantity = match self.byte() {
            Some(b'*') => Quantity {
                minimum: 0,
                maximum: None,
            },
            Some(b'+') => Quantity {
                minimum: 1,
                maximum: None,
            },
            Some(b'?') => Quantity {
                minimum: 0,
                maximum: Some(1),
            },
            Some(b'{') => {
                let start = self.at;
                self.at += 1;
                let left = self.at;
                let minimum = decimal(self.digits());
                let left_end = self.at;
                let maximum = if self.eat(b',') {
                    let right = self.at;
                    let maximum = decimal(self.digits());
                    let right_end = self.at;
                    maximum.map(|n| (n, right, right_end))
                } else {
                    minimum.map(|n| (n, left, left_end))
                };
                if let Some(minimum) = minimum
                    && self.eat(b'}')
                {
                    if let Some((_, right, right_end)) = maximum {
                        let left = significant(&self.units[left..left_end]);
                        let right = significant(&self.units[right..right_end]);
                        if (left.len(), left) > (right.len(), right) {
                            return Err(syntax("Regular expression quantifier is out of order"));
                        }
                    }
                    return Ok(Some(Quantity {
                        minimum,
                        maximum: maximum.map(|(count, _, _)| count),
                    }));
                }
                self.at = start;
                if self.flags.unicode() {
                    return Err(syntax("Invalid regular expression quantifier"));
                }
                return Ok(None);
            }
            _ => return Ok(None),
        };
        self.at += 1;
        Ok(Some(quantity))
    }

    fn set_operator(&self) -> Option<Operation> {
        let pair = self.units.get(self.at..self.at + 2)?;
        match pair {
            [38, 38] => Some(Operation::Intersection),
            [45, 45] => Some(Operation::Difference),
            _ => None,
        }
    }

    fn unicode_set(&mut self, depth: usize) -> Result<Set, JsError> {
        if depth > MAX_PATTERN_DEPTH {
            return Err(limit("Regular expression nesting limit exceeded"));
        }
        self.at += 1;
        let complement = self.eat(b'^');
        let mut sets = Vec::new();
        let mut operator = None;
        let mut allow_operation = true;
        while !self.eat(b']') {
            let (mut operand, point) = self.set_operand(depth)?;
            if self.byte() == Some(b'-') && self.set_operator().is_none() {
                self.at += 1;
                let first =
                    point.ok_or_else(|| syntax("A range begins with a class-set character"))?;
                let last = self.set_character()?;
                if first > last || operator.is_some() {
                    return Err(syntax("Invalid Unicode set range"));
                }
                operand = Set::Character(Member::Range(first, last));
                allow_operation = false;
            }
            sets.push(operand);
            if let Some(next) = self.set_operator() {
                if !allow_operation
                    || (operator.is_none() && sets.len() != 1)
                    || operator.is_some_and(|op| op != next)
                {
                    return Err(syntax(
                        "Mixed Unicode set operations require nested classes",
                    ));
                }
                self.at += 2;
                if self.byte() == Some(b']')
                    || matches!(next, Operation::Intersection) && self.byte() == Some(b'&')
                {
                    return Err(syntax("Expected a Unicode set operand"));
                }
                operator = Some(next);
            } else if operator.is_some() && self.byte() != Some(b']') {
                return Err(syntax("Expected a Unicode set operator"));
            }
        }
        let set = if sets.len() == 1 {
            sets.pop().unwrap()
        } else {
            Set::Operation(operator.unwrap_or(Operation::Union), sets)
        };
        if complement {
            if set.may_contain_strings() {
                return Err(syntax("Cannot complement a Unicode set of strings"));
            }
            Ok(Set::Complement(Box::new(set)))
        } else {
            Ok(set)
        }
    }

    fn set_operand(&mut self, depth: usize) -> Result<(Set, Option<u32>), JsError> {
        if self.byte() == Some(b'[') {
            return Ok((self.unicode_set(depth + 1)?, None));
        }
        if self.eat(b'\\') {
            if self.eat(b'q') {
                if !self.eat(b'{') {
                    return Err(syntax("Expected string disjunction"));
                }
                let mut strings = Vec::new();
                loop {
                    let mut string = Vec::new();
                    while !matches!(self.byte(), Some(b'|' | b'}')) {
                        string.push(self.set_character()?);
                    }
                    strings.push(string);
                    if self.eat(b'}') {
                        break;
                    }
                    self.at += 1;
                }
                return Ok((Set::Strings(strings), None));
            }
            return match self.escape(true)? {
                Escape::Character(Member::Point(c)) => {
                    Ok((Set::Character(Member::Point(c)), Some(c)))
                }
                Escape::Character(member) => Ok((Set::Character(member), None)),
                Escape::Emoji(mask) => Ok((Set::Emoji(mask), None)),
                _ => Err(syntax("Invalid Unicode set operand")),
            };
        }
        let point = self.set_character()?;
        Ok((Set::Character(Member::Point(point)), Some(point)))
    }

    fn set_character(&mut self) -> Result<u32, JsError> {
        if self.eat(b'\\') {
            return match self.escape(true)? {
                Escape::Character(Member::Point(point)) => Ok(point),
                _ => Err(syntax("Expected a Unicode set character")),
            };
        }
        if let Some(ch) = self.byte()
            && (b"()[]{}/-|".contains(&ch)
                || set_punctuator(ch) && self.units.get(self.at + 1) == Some(&u16::from(ch)))
        {
            return Err(syntax("Reserved punctuation in Unicode set"));
        }
        self.point(true)
            .ok_or_else(|| syntax("Unterminated Unicode set"))
    }

    fn class(&mut self) -> Result<Expr, JsError> {
        self.at += 1;
        let negated = self.eat(b'^');
        let mut members = Vec::new();
        while !self.eat(b']') {
            let left = self.class_member()?;
            if self.byte() == Some(b'-')
                && self.units.get(self.at + 1).copied().and_then(ascii) != Some(b']')
            {
                self.at += 1;
                let right = self.class_member()?;
                match (left, right) {
                    (Member::Point(first), Member::Point(last)) if first <= last => {
                        members.push(Member::Range(first, last));
                    }
                    (Member::Point(_), Member::Point(_)) => {
                        return Err(syntax("Regular expression class range is out of order"));
                    }
                    _ if self.flags.unicode() => {
                        return Err(syntax(
                            "Class range endpoints must be individual characters",
                        ));
                    }
                    _ => members.extend([left, Member::Point(u32::from(b'-')), right]),
                }
            } else {
                members.push(left);
            }
        }
        Ok(Expr::Class { members, negated })
    }

    fn class_member(&mut self) -> Result<Member, JsError> {
        if self.eat(b'\\') {
            if let Escape::Character(member) = self.escape(true)? {
                return Ok(member);
            }
            return Err(syntax("Invalid regular expression class escape"));
        }
        self.point(self.flags.unicode())
            .map(Member::Point)
            .ok_or_else(|| syntax("Unterminated regular expression character class"))
    }

    fn hex(&mut self, count: usize) -> Option<u32> {
        let units = self.units.get(self.at..self.at + count)?;
        let mut value = 0;
        for &unit in units {
            value = value * 16 + hex_digit(unit)?;
        }
        self.at += count;
        Some(value)
    }

    fn unicode_escape(&mut self, unicode: bool) -> Option<u32> {
        let start = self.at;
        if unicode && self.eat(b'{') {
            let result = (|| {
                let mut value = hex_digit(self.peek()?)?;
                self.at += 1;
                while let Some(digit) = self.peek().and_then(hex_digit) {
                    value = value * 16 + digit;
                    if value > 0x10ffff {
                        return None;
                    }
                    self.at += 1;
                }
                self.eat(b'}').then_some(value)
            })();
            if result.is_none() {
                self.at = start;
            }
            return result;
        }
        let first = self.hex(4)?;
        if unicode && (0xd800..=0xdbff).contains(&first) {
            let after = self.at;
            if self.eat(b'\\')
                && self.eat(b'u')
                && let Some(second) = self.hex(4)
                && let Some(point) = decode_pair(first, second)
            {
                return Some(point);
            }
            self.at = after;
        }
        Some(first)
    }

    fn escape(&mut self, inside_class: bool) -> Result<Escape, JsError> {
        let start = self.at;
        let unicode = self.flags.unicode();
        let point = self
            .point(unicode)
            .ok_or_else(|| syntax("Trailing regular expression escape"))?;
        let ch = u8::try_from(point).ok().filter(u8::is_ascii);
        let character = |point| Ok(Escape::Character(Member::Point(point)));
        match ch {
            Some(b'd' | b'D' | b's' | b'S' | b'w' | b'W') => {
                let ch = ch.unwrap();
                let kind = match ch.to_ascii_lowercase() {
                    b'd' => Shorthand::Digit,
                    b's' => Shorthand::Space,
                    _ => Shorthand::Word,
                };
                Ok(Escape::Character(Member::Shorthand(
                    kind,
                    ch.is_ascii_uppercase(),
                )))
            }
            Some(b'b' | b'B') if !inside_class => {
                Ok(Escape::Assertion(Assertion::WordBoundary(ch == Some(b'b'))))
            }
            Some(b'b') => character(8),
            Some(b'f') => character(12),
            Some(b'n') => character(10),
            Some(b'r') => character(13),
            Some(b't') => character(9),
            Some(b'v') => character(11),
            Some(b'c') => {
                if let Some(ch) = self.byte()
                    && (ch.is_ascii_alphabetic()
                        || inside_class && !unicode && (ch.is_ascii_digit() || ch == b'_'))
                {
                    self.at += 1;
                    character(u32::from(ch % 32))
                } else if unicode {
                    Err(syntax("Invalid regular expression control escape"))
                } else {
                    self.at = start;
                    character(u32::from(b'\\'))
                }
            }
            Some(b'x' | b'u') => {
                let decoded = if ch == Some(b'x') {
                    self.hex(2)
                } else {
                    self.unicode_escape(unicode)
                };
                match decoded {
                    Some(decoded) => character(decoded),
                    None if !unicode => character(point),
                    None => Err(syntax("Invalid regular expression hexadecimal escape")),
                }
            }
            Some(b'k') if unicode || self.has_named => {
                if inside_class || !self.eat(b'<') {
                    return Err(syntax("Invalid regular expression named reference"));
                }
                let name = self.name()?;
                self.references.push(name.clone());
                Ok(Escape::Name(name))
            }
            Some(b'p' | b'P') if unicode => {
                if !self.eat(b'{') {
                    return Err(syntax("Expected a Unicode property name in braces"));
                }
                let start = self.at;
                while self
                    .byte()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'='))
                {
                    self.at += 1;
                }
                let end = self.at;
                if !self.eat(b'}') {
                    return Err(syntax("Invalid Unicode property escape"));
                }
                let name =
                    String::from_utf16(&self.units[start..end]).expect("ASCII property name");
                if self.flags.has(Flags::UNICODE_SETS)
                    && let Some(prop) = super::sets::emoji_property(&name)
                {
                    if ch == Some(b'P') {
                        return Err(syntax("Cannot complement a Unicode property of strings"));
                    }
                    return Ok(Escape::Emoji(prop));
                }
                let property = super::property::Property::parse(&name)
                    .ok_or_else(|| syntax("Unknown Unicode property name or value"))?;
                Ok(Escape::Character(Member::Property(
                    property,
                    ch == Some(b'P'),
                )))
            }
            Some(b'0'..=b'9') => {
                self.at = start;
                self.numeric_escape(inside_class)
            }
            Some(
                b'^' | b'$' | b'\\' | b'.' | b'*' | b'+' | b'?' | b'(' | b')' | b'[' | b']' | b'{'
                | b'}' | b'|' | b'/',
            ) => character(point),
            Some(b'-') if inside_class => character(point),
            Some(ch)
                if inside_class && self.flags.has(Flags::UNICODE_SETS) && set_punctuator(ch) =>
            {
                character(point)
            }
            _ if !unicode => character(point),
            _ => Err(syntax("Invalid regular expression identity escape")),
        }
    }

    fn numeric_escape(&mut self, inside_class: bool) -> Result<Escape, JsError> {
        let start = self.at;
        let first = self.byte().unwrap();
        let reference = decimal(self.digits()).unwrap();
        let single_zero = first == b'0' && self.at == start + 1;
        if first != b'0' && !inside_class && reference <= self.total_captures {
            return Ok(Escape::Number(reference - 1));
        }
        if single_zero {
            return Ok(Escape::Character(Member::Point(0)));
        }
        if self.flags.unicode() {
            return Err(syntax("Invalid regular expression decimal escape"));
        }
        self.at = start + 1;
        if first >= b'8' {
            return Ok(Escape::Character(Member::Point(u32::from(first))));
        }
        let mut value = u32::from(first - b'0');
        for _ in 0..if first <= b'3' { 2 } else { 1 } {
            let Some(ch @ b'0'..=b'7') = self.byte() else {
                break;
            };
            self.at += 1;
            value = value * 8 + u32::from(ch - b'0');
        }
        Ok(Escape::Character(Member::Point(value)))
    }
}

fn set_punctuator(ch: u8) -> bool {
    b"!#$%&*+,.:;<=>?@^`~".contains(&ch)
}

fn decimal(digits: &[u16]) -> Option<usize> {
    if digits.is_empty() {
        return None;
    }
    Some(digits.iter().fold(0usize, |n, &unit| {
        n.saturating_mul(10).saturating_add(usize::from(unit - 48))
    }))
}

fn significant(digits: &[u16]) -> &[u16] {
    let n = digits.iter().take_while(|&&unit| unit == 48).count();
    &digits[n..]
}

fn hex_digit(unit: u16) -> Option<u32> {
    let digit = ascii(unit)?;
    Some(u32::from(match digit {
        b'0'..=b'9' => digit - b'0',
        b'a'..=b'f' => digit - b'a' + 10,
        b'A'..=b'F' => digit - b'A' + 10,
        _ => return None,
    }))
}

pub(super) fn decode_pair(first: u32, second: u32) -> Option<u32> {
    if (0xd800..=0xdbff).contains(&first) && (0xdc00..=0xdfff).contains(&second) {
        Some(0x10000 + ((first - 0xd800) << 10) + second - 0xdc00)
    } else {
        None
    }
}

fn sequence(mut terms: Vec<Expr>) -> Expr {
    if terms.len() == 1 {
        terms.pop().unwrap()
    } else {
        Expr::Sequence(terms)
    }
}

fn identifier_start(ch: char) -> bool {
    matches!(ch, '$' | '_') || super::case::identifier_start(ch as u32)
}

fn identifier_continue(ch: char) -> bool {
    matches!(ch, '$' | '_' | '\u{200c}' | '\u{200d}') || super::case::identifier_continue(ch as u32)
}
