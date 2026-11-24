//! The matcher uses an explicit backtrack stack and reversible captures.
pub(super) mod prefix;

use super::{
    Flags, case, limit,
    pattern::{Assertion, BackReference, Expr, Member, Pattern, Shorthand, decode_pair},
};
use crate::{JsError, JsString, Vm};
use std::{mem::size_of, ops::Range};

#[derive(Debug)]
pub(crate) struct Program {
    pub(super) source: JsString,
    pub(super) flags: Flags,
    code: Vec<Op>,
    captures: usize,
    repeats: usize,
    pub(super) first_ascii: u128,
    bytes: usize,
    pub(super) names: Vec<(JsString, Vec<usize>)>,
}

#[derive(Debug)]
enum Op {
    Match,
    Character(Member, Flags),
    Class(Vec<Member>, bool, Flags, Option<Box<u128>>),
    Any(Flags),
    Assert(Assertion, Flags),
    Split(usize),
    Jump(usize),
    OpenCapture(usize),
    CloseCapture(usize),
    BackReference(Reference, Flags),
    InitRepeat(usize),
    ChooseRepeat {
        slot: usize,
        minimum: usize,
        maximum: Option<usize>,
        greedy: bool,
        exit: usize,
    },
    DisjointClassRepeat {
        minimum: usize,
        maximum: Option<usize>,
        exit: usize,
    },
    BeginRepeat(usize, Range<usize>),
    EndRepeat(usize, usize, usize),
    Look {
        behind: bool,
        negative: bool,
        after: usize,
    },
    EndLook,
}

#[derive(Debug)]
enum Reference {
    Number(usize),
    Name(usize),
}

struct Compiler<'a> {
    code: Vec<Op>,
    repeats: usize,
    names: &'a [(JsString, Vec<usize>)],
}

pub(super) fn compile(source: JsString, flags: Flags, pattern: Pattern) -> Program {
    let first_ascii = prefix::first_ascii(&pattern.body, flags);
    let mut compiler = Compiler {
        code: Vec::new(),
        repeats: 0,
        names: &pattern.names,
    };
    compiler.emit(pattern.body, flags, false);
    compiler.code.push(Op::Match);
    specialize_disjoint_class_repeats(&mut compiler.code);
    let mut program = Program {
        source,
        flags,
        code: compiler.code,
        captures: pattern.captures,
        repeats: compiler.repeats,
        first_ascii,
        names: pattern.names,
        bytes: 0,
    };
    program.bytes = program.storage_size();
    program
}

fn specialize_disjoint_class_repeats(code: &mut [Op]) {
    let mut pc = 0;
    while pc < code.len() {
        if let Op::Look { after, .. } = &code[pc] {
            // Do not specialize forward or backward assertion bodies.
            pc = *after;
            continue;
        }
        if let Some(op) = disjoint_class_repeat_at(code, pc) {
            code[pc] = op;
        }
        pc += 1;
    }
}

fn disjoint_class_repeat_at(code: &[Op], pc: usize) -> Option<Op> {
    let Op::ChooseRepeat {
        minimum,
        maximum,
        greedy: true,
        exit,
        ..
    } = &code[pc]
    else {
        return None;
    };
    let [
        Op::BeginRepeat(_, captures),
        Op::Class(members, negative, flags, ..),
        Op::EndRepeat(..),
    ] = code.get(pc + 1..*exit)?
    else {
        return None;
    };
    let Op::Character(Member::Point(delimiter), delimiter_flags) = code.get(*exit)? else {
        return None;
    };
    let unsupported = Flags::IGNORE_CASE | Flags::UNICODE_SETS;
    if !captures.is_empty()
        || members.is_empty()
        || *delimiter >= 128
        || flags.has(unsupported)
        || delimiter_flags.has(unsupported)
        || flags.unicode() != delimiter_flags.unicode()
    {
        return None;
    }
    // For every shorter endpoint, the class accepted the next point and
    // the mandatory literal would reject it. Unknown members prevent proof.
    let class = members.iter().try_fold(0, |mask, &member| {
        Some(mask | prefix::ascii_member(member)?)
    })?;
    let contains = class & (1u128 << *delimiter) != 0;
    if contains != *negative {
        return None;
    }
    Some(Op::DisjointClassRepeat {
        minimum: *minimum,
        maximum: *maximum,
        exit: *exit,
    })
}

#[cfg(test)]
mod tests {
    use super::Op;
    use crate::JsString;

    #[test]
    fn disjoint_specialization_guards() {
        let cases = [
            (r"[^!]*!", "", true),
            (r"[a-z]{2,9}!(?=z)", "u", true),
            (r"(?<body>[^!]*!)", "", true),
            (r"(?<=x)[^!]*!", "u", true),
            (r"(?=[^!]*!)", "", false),
            (r"(?![^!]*!)", "", false),
            (r"(?<=![^!]+)", "u", false),
            (r"(?<last>[ab])+!", "", false),
            (r"([ab]?)*!", "", false),
            (r"(?:[ab]|x?)*!", "", false),
            (r"[^!]*!?", "", false),
            (r"[ab]+a", "", false),
            (r"(?i:x(?-i:[^a])*a)", "", false),
            (r"[^!]*!", "i", false),
            (r"[]*!", "", false),
            (r".*!", "", false),
            (r"[^!]*?!", "", false),
            (r"[^é]*é", "u", false),
            (r"[^!]*$", "", false),
        ];
        for (pattern, flags, expected) in cases {
            let program =
                super::super::compile(JsString::from(pattern), JsString::from(flags)).unwrap();
            assert_eq!(
                program
                    .code
                    .iter()
                    .any(|op| matches!(op, Op::DisjointClassRepeat { .. })),
                expected,
                "/{pattern}/{flags}"
            );
        }
    }
}

impl Compiler<'_> {
    fn emit(&mut self, expr: Expr, flags: Flags, behind: bool) {
        match expr {
            Expr::Sequence(parts) => {
                if behind {
                    for part in parts.into_iter().rev() {
                        self.emit(part, flags, behind);
                    }
                } else {
                    for part in parts {
                        self.emit(part, flags, behind);
                    }
                }
            }
            Expr::Choice(parts) => {
                let mut parts = parts.into_iter().peekable();
                let mut jumps = Vec::new();
                while let Some(part) = parts.next() {
                    if parts.peek().is_none() {
                        self.emit(part, flags, behind);
                        break;
                    }
                    let selection = self.code.len();
                    self.code.push(Op::Split(0));
                    self.emit(part, flags, behind);
                    jumps.push(self.code.len());
                    self.code.push(Op::Jump(0));
                    self.code[selection] = Op::Split(self.code.len());
                }
                let end = self.code.len();
                for jump in jumps {
                    self.code[jump] = Op::Jump(end);
                }
            }
            Expr::Character(ch) => self.code.push(Op::Character(ch, flags)),
            Expr::Class { members, negated } => {
                // A single member already needs at most one test.
                let ascii = if members.len() > 1 {
                    prefix::class_ascii(&members, negated, flags).map(Box::new)
                } else {
                    None
                };
                self.code.push(Op::Class(members, negated, flags, ascii));
            }
            Expr::Any => self.code.push(Op::Any(flags)),
            Expr::Assert(assertion) => self.code.push(Op::Assert(assertion, flags)),
            Expr::Capture(slot, body) => {
                self.code.push(Op::OpenCapture(slot));
                self.emit(*body, flags, behind);
                self.code.push(Op::CloseCapture(slot));
            }
            Expr::BackReference(reference) => {
                let reference = match reference {
                    BackReference::Number(slot) => Reference::Number(slot),
                    BackReference::Name(name) => Reference::Name(
                        self.names.iter().position(|(key, _)| key == &name).unwrap(),
                    ),
                };
                self.code.push(Op::BackReference(reference, flags));
            }
            Expr::Repeat {
                body,
                minimum,
                maximum,
                greedy,
                captures,
            } => {
                let slot = self.repeats;
                self.repeats += 1;
                self.code.push(Op::InitRepeat(slot));
                let choice = self.code.len();
                self.code.push(Op::Match); // Patched after emitting the body.
                self.code.push(Op::BeginRepeat(slot, captures));
                self.emit(*body, flags, behind);
                self.code.push(Op::EndRepeat(slot, choice, minimum));
                self.code[choice] = Op::ChooseRepeat {
                    slot,
                    minimum,
                    maximum,
                    greedy,
                    exit: self.code.len(),
                };
            }
            Expr::Look {
                body,
                behind,
                negative,
            } => {
                let enter = self.code.len();
                self.code.push(Op::Match);
                self.emit(*body, flags, behind);
                self.code.push(Op::EndLook);
                self.code[enter] = Op::Look {
                    behind,
                    negative,
                    after: self.code.len(),
                };
            }
            Expr::Modifiers { body, add, remove } => {
                self.emit(*body, flags.with_modifiers(add, remove), behind);
            }
        }
    }
}

pub(super) type Span = (usize, usize);
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Capture {
    first: Option<usize>,
    last: Option<usize>,
}
impl Capture {
    fn span(self) -> Option<Span> {
        let first = self.first?;
        let last = self.last?;
        Some((first.min(last), first.max(last)))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Repeat {
    count: usize,
    start: usize,
}

#[derive(Clone, Copy)]
enum Undo {
    Capture(usize, Capture),
    Repeat(usize, Repeat),
}

enum Frame {
    Choice {
        target: usize,
        cursor: usize,
        checkpoint: usize,
    },
    Look {
        after: usize,
        cursor: usize,
        checkpoint: usize,
        behind: bool,
        negative: bool,
    },
}

struct Matcher<'a> {
    program: &'a Program,
    input: &'a [u16],
    pc: usize,
    cursor: usize,
    behind: bool,
    captures: Vec<Capture>,
    repeats: Vec<Repeat>,
    stack: Vec<Frame>,
    undo: Vec<Undo>,
    bytes: usize,
}

pub(super) struct Match {
    pub(super) whole: Span,
    pub(super) captures: Vec<Option<Span>>,
}

impl Program {
    pub(crate) fn retained_bytes(&self) -> usize {
        self.bytes
    }

    fn storage_size(&self) -> usize {
        size_of::<Program>()
            + self.source.len() * 2
            + self.code.capacity() * size_of::<Op>()
            + self
                .code
                .iter()
                .map(|op| match op {
                    Op::Class(members, _, _, ascii) => {
                        members.capacity() * size_of::<Member>()
                            + ascii.as_ref().map_or(0, |_| size_of::<u128>())
                    }
                    _ => 0,
                })
                .sum::<usize>()
            + self.names.capacity() * size_of::<(JsString, Vec<usize>)>()
            + self
                .names
                .iter()
                .map(|(name, slots)| name.len() * 2 + 32 + slots.capacity() * size_of::<usize>())
                .sum::<usize>()
    }

    pub(super) fn find(
        &self,
        vm: &mut Vm,
        input: &[u16],
        start: usize,
        sticky: bool,
    ) -> Result<Option<Match>, JsError> {
        if start > input.len() {
            return Ok(None);
        }
        let first_ascii = if sticky { u128::MAX } else { self.first_ascii };
        let Some(mut start) = prefix::seek(vm, input, start, first_ascii)? else {
            return Ok(None);
        };
        let bytes = self.captures * size_of::<Capture>() + self.repeats * size_of::<Repeat>();
        vm.ensure_allocation(bytes + self.captures * size_of::<Option<Span>>())?;
        let mut state = Matcher {
            program: self,
            input,
            pc: 0,
            cursor: 0,
            behind: false,
            captures: vec![Capture::default(); self.captures],
            repeats: vec![Repeat::default(); self.repeats],
            stack: Vec::new(),
            undo: Vec::new(),
            bytes,
        };
        loop {
            vm.consume_work(self.captures + self.repeats + 1)?;
            // A code unit in the middle of a surrogate pair names the same character.
            let offset = if self.flags.unicode()
                && start > 0
                && start < input.len()
                && decode_pair(input[start - 1].into(), input[start].into()).is_some()
            {
                start - 1
            } else {
                start
            };
            state.pc = 0;
            state.cursor = offset;
            state.behind = false;
            state.captures.fill(Capture::default());
            state.repeats.fill(Repeat::default());
            state.stack.clear();
            state.undo.clear();
            if state.run(vm)? {
                return Ok(Some(Match {
                    whole: (offset, state.cursor),
                    captures: state
                        .captures
                        .iter()
                        .map(|capture| capture.span())
                        .collect(),
                }));
            }
            if sticky || start == input.len() {
                return Ok(None);
            }
            let next = advance(input, start, self.flags.unicode());
            let Some(next) = prefix::seek(vm, input, next, first_ascii)? else {
                return Ok(None);
            };
            start = next;
        }
    }
}

impl Matcher<'_> {
    fn run(&mut self, vm: &mut Vm) -> Result<bool, JsError> {
        loop {
            vm.consume_work(1)?;
            let op = &self.program.code[self.pc];
            self.pc += 1;
            let success = match op {
                Op::Match => return Ok(true),
                Op::Character(member, flags) => {
                    self.read(*flags, |ch| Ok(member_matches(*member, ch, *flags)))?
                }
                Op::Class(..) => self.read_class(vm, op)?,
                Op::Any(flags) => self.read(*flags, |ch| {
                    Ok(flags.has(Flags::DOT_ALL) || !line_terminator(ch))
                })?,
                Op::Assert(assertion, flags) => self.assert(*assertion, *flags),
                Op::Split(target) => {
                    self.choice(vm, *target)?;
                    true
                }
                Op::Jump(target) => {
                    self.pc = *target;
                    true
                }
                Op::OpenCapture(slot) => {
                    self.capture(
                        vm,
                        *slot,
                        Capture {
                            first: Some(self.cursor),
                            last: None,
                        },
                    )?;
                    true
                }
                Op::CloseCapture(slot) => {
                    let capture = Capture {
                        last: Some(self.cursor),
                        ..self.captures[*slot]
                    };
                    self.capture(vm, *slot, capture)?;
                    true
                }
                Op::BackReference(reference, flags) => {
                    let capture = match reference {
                        Reference::Number(slot) => self.captures[*slot].span(),
                        Reference::Name(name) => {
                            let slots = &self.program.names[*name].1;
                            vm.consume_work(slots.len())?;
                            slots.iter().find_map(|slot| self.captures[*slot].span())
                        }
                    };
                    self.back_reference(vm, capture, *flags)?
                }
                Op::InitRepeat(slot) => {
                    self.repeat(vm, *slot, Repeat::default())?;
                    true
                }
                Op::ChooseRepeat {
                    slot,
                    minimum,
                    maximum,
                    greedy,
                    exit,
                } => {
                    let count = self.repeats[*slot].count;
                    if maximum.is_some_and(|maximum| count >= maximum) {
                        self.pc = *exit;
                    } else if count >= *minimum {
                        if *greedy {
                            self.choice(vm, *exit)?;
                        } else {
                            self.choice(vm, self.pc)?;
                            self.pc = *exit;
                        }
                    }
                    true
                }
                Op::DisjointClassRepeat {
                    minimum,
                    maximum,
                    exit,
                } => {
                    // pc is at BeginRepeat. No choices or capture/repeat
                    // state are needed within this single consuming atom.
                    let class = &self.program.code[self.pc + 1];
                    let mut count = 0;
                    while maximum.is_none_or(|maximum| count < maximum) {
                        // Keep the ordinary atom-dispatch and member charges.
                        vm.consume_work(1)?;
                        if !self.read_class(vm, class)? {
                            break;
                        }
                        count += 1;
                    }
                    self.pc = *exit;
                    count >= *minimum
                }
                Op::BeginRepeat(slot, captures) => {
                    vm.consume_work(captures.len())?;
                    for index in captures.clone() {
                        self.capture(vm, index, Capture::default())?;
                    }
                    let repeat = Repeat {
                        start: self.cursor,
                        ..self.repeats[*slot]
                    };
                    self.repeat(vm, *slot, repeat)?;
                    true
                }
                Op::EndRepeat(slot, loop_pc, minimum) => {
                    let previous = self.repeats[*slot];
                    if previous.count >= *minimum && previous.start == self.cursor {
                        false
                    } else {
                        self.repeat(
                            vm,
                            *slot,
                            Repeat {
                                count: previous.count + 1,
                                ..previous
                            },
                        )?;
                        self.pc = *loop_pc;
                        true
                    }
                }
                Op::Look {
                    behind,
                    negative,
                    after,
                } => {
                    let frame = Frame::Look {
                        after: *after,
                        cursor: self.cursor,
                        checkpoint: self.undo.len(),
                        behind: self.behind,
                        negative: *negative,
                    };
                    push(vm, &mut self.stack, &mut self.bytes, frame)?;
                    self.behind = *behind;
                    true
                }
                Op::EndLook => self.end_look(vm)?,
            };
            if !success && !self.backtrack(vm)? {
                return Ok(false);
            }
        }
    }

    fn read<F>(&mut self, flags: Flags, test: F) -> Result<bool, JsError>
    where
        F: FnOnce(u32) -> Result<bool, JsError>,
    {
        if let Some((point, cursor)) = point(self.input, self.cursor, self.behind, flags.unicode())
            && test(point)?
        {
            self.cursor = cursor;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn read_class(&mut self, vm: &mut Vm, class: &Op) -> Result<bool, JsError> {
        let Op::Class(members, negative, flags, ascii) = class else {
            unreachable!("class read requires a class opcode");
        };
        self.read(*flags, |ch| {
            if ch < 128
                && let Some(mask) = ascii
            {
                // One exact lookup replaces the member tests below.
                vm.consume_work(1)?;
                return Ok(**mask & (1u128 << ch) != 0);
            }
            for member in members {
                vm.consume_work(1)?;
                if member_matches(*member, ch, *flags) {
                    return Ok(!*negative);
                }
            }
            Ok(*negative)
        })
    }

    fn assert(&self, assertion: Assertion, flags: Flags) -> bool {
        let previous = point(self.input, self.cursor, true, flags.unicode()).map(|(ch, _)| ch);
        let next = point(self.input, self.cursor, false, flags.unicode()).map(|(ch, _)| ch);
        match assertion {
            Assertion::Start => {
                previous.is_none()
                    || flags.has(Flags::MULTILINE) && previous.is_some_and(line_terminator)
            }
            Assertion::End => {
                next.is_none() || flags.has(Flags::MULTILINE) && next.is_some_and(line_terminator)
            }
            Assertion::WordBoundary(positive) => {
                (previous.is_some_and(|ch| word(ch, flags))
                    != next.is_some_and(|ch| word(ch, flags)))
                    == positive
            }
        }
    }

    fn back_reference(
        &mut self,
        vm: &mut Vm,
        capture: Option<Span>,
        flags: Flags,
    ) -> Result<bool, JsError> {
        let Some((first, last)) = capture else {
            return Ok(true);
        };
        let mut from = if self.behind { last } else { first };
        let sentinel = if self.behind { first } else { last };
        let mut cursor = self.cursor;
        while from != sentinel {
            vm.consume_work(1)?;
            let (expected, next) = point(self.input, from, self.behind, flags.unicode()).unwrap();
            let Some((actual, after)) = point(self.input, cursor, self.behind, flags.unicode())
            else {
                return Ok(false);
            };
            if !member_matches(Member::Point(expected), actual, flags) {
                return Ok(false);
            }
            from = next;
            cursor = after;
        }
        self.cursor = cursor;
        Ok(true)
    }

    fn choice(&mut self, vm: &mut Vm, target: usize) -> Result<(), JsError> {
        let frame = Frame::Choice {
            target,
            cursor: self.cursor,
            checkpoint: self.undo.len(),
        };
        push(vm, &mut self.stack, &mut self.bytes, frame)
    }

    fn capture(&mut self, vm: &mut Vm, slot: usize, capture: Capture) -> Result<(), JsError> {
        if self.captures[slot] != capture {
            if !self.stack.is_empty() {
                push(
                    vm,
                    &mut self.undo,
                    &mut self.bytes,
                    Undo::Capture(slot, self.captures[slot]),
                )?;
            }
            self.captures[slot] = capture;
        }
        Ok(())
    }

    fn repeat(&mut self, vm: &mut Vm, slot: usize, repeat: Repeat) -> Result<(), JsError> {
        if self.repeats[slot] != repeat {
            if !self.stack.is_empty() {
                push(
                    vm,
                    &mut self.undo,
                    &mut self.bytes,
                    Undo::Repeat(slot, self.repeats[slot]),
                )?;
            }
            self.repeats[slot] = repeat;
        }
        Ok(())
    }

    fn restore(&mut self, vm: &mut Vm, checkpoint: usize) -> Result<(), JsError> {
        vm.consume_work(self.undo.len() - checkpoint)?;
        for action in self.undo.drain(checkpoint..).rev() {
            match action {
                Undo::Capture(slot, old) => self.captures[slot] = old,
                Undo::Repeat(slot, old) => self.repeats[slot] = old,
            }
        }
        Ok(())
    }

    fn backtrack(&mut self, vm: &mut Vm) -> Result<bool, JsError> {
        while let Some(frame) = self.stack.pop() {
            vm.consume_work(1)?;
            match frame {
                Frame::Choice {
                    target,
                    cursor,
                    checkpoint,
                } => {
                    self.restore(vm, checkpoint)?;
                    self.cursor = cursor;
                    self.pc = target;
                    return Ok(true);
                }
                Frame::Look {
                    after,
                    cursor,
                    checkpoint,
                    behind,
                    negative,
                } => {
                    self.restore(vm, checkpoint)?;
                    self.cursor = cursor;
                    self.behind = behind;
                    if negative {
                        self.pc = after;
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    fn end_look(&mut self, vm: &mut Vm) -> Result<bool, JsError> {
        // Assertions are atomic. Discard their internal alternatives on success.
        loop {
            vm.consume_work(1)?;
            if let Frame::Look {
                after,
                cursor,
                checkpoint,
                behind,
                negative,
            } = self.stack.pop().unwrap()
            {
                self.cursor = cursor;
                self.behind = behind;
                if negative {
                    self.restore(vm, checkpoint)?;
                    return Ok(false);
                }
                if self.stack.is_empty() {
                    self.undo.clear();
                }
                self.pc = after;
                return Ok(true);
            }
        }
    }
}

fn push<T>(vm: &Vm, values: &mut Vec<T>, bytes: &mut usize, value: T) -> Result<(), JsError> {
    if values.len() == values.capacity() {
        let capacity = (values.capacity() * 2).max(8);
        let increase = (capacity - values.capacity()) * size_of::<T>();
        if *bytes + increase > 8 * 1024 * 1024 {
            return Err(limit(
                "Regular expression backtracking storage limit exceeded",
            ));
        }
        // During reallocation, both buffers can be resident.
        vm.ensure_allocation(*bytes + capacity * size_of::<T>())?;
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(|_| limit("Regular expression backtracking allocation failed"))?;
        *bytes += increase;
    }
    values.push(value);
    Ok(())
}

fn point(input: &[u16], at: usize, behind: bool, unicode: bool) -> Option<(u32, usize)> {
    if behind {
        let at = at.checked_sub(1)?;
        let last = u32::from(*input.get(at)?);
        if unicode
            && at > 0
            && let Some(point) = decode_pair(input[at - 1].into(), last)
        {
            return Some((point, at - 1));
        }
        Some((last, at))
    } else {
        let first = u32::from(*input.get(at)?);
        if unicode
            && let Some(&second) = input.get(at + 1)
            && let Some(point) = decode_pair(first, second.into())
        {
            return Some((point, at + 2));
        }
        Some((first, at + 1))
    }
}

pub(super) fn advance(input: &[u16], at: usize, unicode: bool) -> usize {
    point(input, at, false, unicode).map_or(at + 1, |(_, end)| end)
}

pub(super) fn line_terminator(ch: u32) -> bool {
    matches!(ch, 0x0a | 0x0d | 0x2028 | 0x2029)
}

pub(super) fn space(ch: u32) -> bool {
    matches!(ch, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a
        | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

fn ascii_word(ch: u32) -> bool {
    matches!(ch, 48..=57 | 65..=90 | 95 | 97..=122)
}

fn word(ch: u32, flags: Flags) -> bool {
    ascii_word(ch)
        || flags.unicode()
            && flags.has(Flags::IGNORE_CASE)
            && case::equivalents(ch, true).any(ascii_word)
}

fn member_matches(member: Member, actual: u32, flags: Flags) -> bool {
    let (first, last) = match member {
        Member::Point(point) => (point, point),
        Member::Range(first, last) => (first, last),
        Member::Shorthand(shorthand, negated) => {
            let matches = match shorthand {
                Shorthand::Digit => (48..=57).contains(&actual),
                Shorthand::Space => space(actual),
                Shorthand::Word => word(actual, flags),
            };
            return matches != negated;
        }
    };
    (first..=last).contains(&actual)
        || flags.has(Flags::IGNORE_CASE)
            && case::equivalents(actual, flags.unicode())
                .any(|point| (first..=last).contains(&point))
}
