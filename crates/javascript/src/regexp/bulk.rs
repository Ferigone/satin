//! Capture-free, byte-oriented matching for large ASCII strings. Convert the
//! parsed ECMAScript tree, so Rust and JavaScript escape syntax never mix.
use super::{
    Flags, limit,
    pattern::{Assertion, Expr},
    program::prefix::{ascii_member, class_ascii},
};
use crate::{JsError, JsString, Value, Vm};
use regex_automata::{
    Anchored,
    dfa::{Automaton, StartKind, dense},
    nfa::thompson,
    util::{start, syntax},
};
use std::fmt::Write;

const DFA_BYTES: usize = 96 * 1024;
const SCRATCH_BYTES: usize = 4 * DFA_BYTES;
const MIN_INPUT: usize = 4096;

fn class(mask: u128, output: &mut String) {
    if mask == 0 {
        output.push_str("[a&&b]");
        return;
    }
    output.push('[');
    let mut start = 0;
    while start < 128 {
        if mask & (1 << start) == 0 {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < 127 && mask & (1 << (end + 1)) != 0 {
            end += 1;
        }
        write!(output, "\\x{start:02X}").unwrap();
        if end != start {
            write!(output, "-\\x{end:02X}").unwrap();
        }
        start = end + 1;
    }
    output.push(']');
}

// Returns whether the expression might match an empty string. A repeated
// nullable body has special ECMAScript empty-iteration rules, so defer it.
fn emit(expr: &Expr, output: &mut String) -> Option<bool> {
    if output.len() > 4096 {
        return None;
    }
    Some(match expr {
        Expr::Sequence(parts) => {
            let mut empty = true;
            for part in parts {
                empty &= emit(part, output)?;
            }
            empty
        }
        Expr::Choice(parts) => {
            output.push_str("(?:");
            let mut empty = false;
            for (n, part) in parts.iter().enumerate() {
                if n != 0 {
                    output.push('|');
                }
                empty |= emit(part, output)?;
            }
            output.push(')');
            empty
        }
        Expr::Character(member) => {
            class(ascii_member(*member)?, output);
            false
        }
        Expr::Class { members, negated } => {
            class(class_ascii(members, *negated, Flags::default())?, output);
            false
        }
        Expr::Any => {
            class(u128::MAX ^ ((1 << 10) | (1 << 13)), output);
            false
        }
        Expr::Assert(Assertion::Start) => {
            output.push_str("\\A");
            true
        }
        Expr::Assert(Assertion::End) => {
            output.push_str("\\z");
            true
        }
        Expr::Capture(_, body) => emit(body, output)?,
        Expr::Repeat {
            body,
            minimum,
            maximum,
            greedy,
            ..
        } => {
            if *minimum > 32 || maximum.is_some_and(|n| n > 32) {
                return None;
            }
            output.push_str("(?:");
            if emit(body, output)? {
                return None;
            }
            output.push(')');
            match maximum {
                Some(max) => write!(output, "{{{minimum},{max}}}").unwrap(),
                None => write!(output, "{{{minimum},}}").unwrap(),
            }
            if !greedy {
                output.push('?');
            }
            *minimum == 0
        }
        _ => return None,
    })
}

fn anchored_edges(expr: &Expr) -> Option<(u128, u128)> {
    // Only reject a candidate when every alternative is anchored and its
    // adjacent mandatory atom excludes the actual first/last code unit.
    match expr {
        Expr::Choice(parts) => {
            let mut edges = (0, 0);
            for part in parts {
                let (first, last) = anchored_edges(part)?;
                edges.0 |= first;
                edges.1 |= last;
            }
            Some(edges)
        }
        Expr::Sequence(parts) => match parts.as_slice() {
            [Expr::Assert(Assertion::Start), next, ..] => Some((
                super::program::prefix::first_ascii(next, Flags::default()),
                0,
            )),
            [.., last, Expr::Assert(Assertion::End)] => Some((0, last_atom(last)?)),
            _ => None,
        },
        _ => None,
    }
}

fn last_atom(expr: &Expr) -> Option<u128> {
    match expr {
        Expr::Capture(_, body) => last_atom(body),
        Expr::Choice(parts) => parts
            .iter()
            .try_fold(0, |mask, expr| Some(mask | last_atom(expr)?)),
        Expr::Sequence(parts) => last_atom(parts.last()?),
        Expr::Repeat { body, minimum, .. } if *minimum > 0 => last_atom(body),
        Expr::Character(member) => ascii_member(*member),
        Expr::Class { members, negated } => class_ascii(members, *negated, Flags::default()),
        _ => None,
    }
}

pub(super) struct Search {
    dfa: dense::DFA<Vec<u32>>,
    first_ascii: u128,
    edges: Option<(u128, u128)>,
    work: usize,
    non_ascii: bool,
}

impl Search {
    pub(super) fn eligible(vm: &Vm, input: &JsString) -> bool {
        input.len() >= MIN_INPUT && vm.limits().buffer_units_per_instruction > 1
    }

    pub(super) fn new(vm: &mut Vm, program: &super::Program) -> Result<Option<Self>, JsError> {
        if program.source.len() > 256
            || program.flags.0 & !Flags::GLOBAL != 0
            || vm.ensure_allocation(SCRATCH_BYTES).is_err()
        {
            return Ok(None);
        }
        // Build the accelerated form on demand. Small strings and ordinary
        // standalone VM contexts incur no additional compiled-pattern storage.
        vm.consume_work(program.source.len().saturating_mul(2))?;
        let tree = super::pattern::parse(program.source.as_utf16(), program.flags)?;
        let mut pattern = String::new();
        if emit(&tree.body, &mut pattern).is_none() {
            return Ok(None);
        }
        let edges = anchored_edges(&tree.body);
        vm.consume_work(pattern.len().saturating_mul(8))?;
        let dfa = dense::Builder::new()
            .syntax(syntax::Config::new().unicode(false).utf8(false))
            .thompson(
                thompson::Config::new()
                    .utf8(false)
                    .nfa_size_limit(Some(DFA_BYTES)),
            )
            .configure(
                dense::Config::new()
                    .start_kind(StartKind::Anchored)
                    .dfa_size_limit(Some(DFA_BYTES))
                    .determinize_size_limit(Some(DFA_BYTES)),
            )
            .build(&pattern);
        Ok(dfa.ok().map(|dfa| Self {
            dfa,
            first_ascii: program.first_ascii,
            edges,
            work: 0,
            non_ascii: false,
        }))
    }

    fn bill(vm: &mut Vm, work: &mut usize, n: usize) -> Result<(), JsError> {
        *work = work.saturating_add(n);
        if *work >= 4096 {
            vm.charge_buffer_work(std::mem::take(work))?;
        }
        Ok(())
    }

    fn end(&mut self, vm: &mut Vm, text: &[u16], from: usize) -> Result<Option<usize>, JsError> {
        let dfa = &self.dfa;
        // The accepted patterns use only absolute start/end assertions.
        let config = start::Config::new()
            .anchored(Anchored::Yes)
            .look_behind((from != 0).then_some(b'a'));
        let mut state = dfa
            .start_state(&config)
            .map_err(|_| limit("Invalid DFA start state"))?;
        let mut matched = None;
        for (i, &unit) in text[from..].iter().enumerate() {
            Self::bill(vm, &mut self.work, 1)?;
            if unit >= 128 {
                self.non_ascii = true;
                return Ok(None);
            }
            state = dfa.next_state(state, unit as u8);
            if dfa.is_match_state(state) {
                matched = Some(from + i);
            } else if dfa.is_dead_state(state) {
                return Ok(matched);
            }
        }
        if dfa.is_match_state(dfa.next_eoi_state(state)) {
            matched = Some(text.len());
        }
        Ok(matched)
    }

    fn find(
        &mut self,
        vm: &mut Vm,
        text: &[u16],
        from: usize,
    ) -> Result<Option<(usize, usize)>, JsError> {
        if let Some((first, last)) = self.edges
            && let (Some(&a), Some(&b)) = (text.first(), text.last())
            && a < 128
            && b < 128
            && first & (1 << a) == 0
            && last & (1 << b) == 0
        {
            return Ok(None);
        }
        for start in from..=text.len() {
            Self::bill(vm, &mut self.work, 1)?;
            if let Some(&unit) = text.get(start) {
                if unit >= 128 {
                    self.non_ascii = true;
                    return Ok(None);
                }
                if self.first_ascii & (1 << unit) == 0 {
                    continue;
                }
            }
            if let Some(end) = self.end(vm, text, start)? {
                return Ok(Some((start, end)));
            }
            if self.non_ascii {
                break;
            }
        }
        Ok(None)
    }

    pub(super) fn first_match(
        mut self,
        vm: &mut Vm,
        text: &[u16],
    ) -> Result<Option<Option<super::program::Span>>, JsError> {
        let found = self.find(vm, text, 0)?;
        vm.charge_buffer_work(self.work)?;
        Ok((!self.non_ascii).then_some(found))
    }

    pub(super) fn replace(
        mut self,
        vm: &mut Vm,
        input: &JsString,
        replacement: &JsString,
        global: bool,
    ) -> Result<Option<(Value, Option<super::program::Span>)>, JsError> {
        let units = input.as_utf16();
        let mut from = 0;
        let mut copied = 0;
        let mut matched = None;
        let mut output = Vec::new();
        while from <= units.len() {
            let Some((start, end)) = self.find(vm, units, from)? else {
                break;
            };
            let length = output
                .len()
                .saturating_add(start - copied)
                .saturating_add(replacement.len());
            Self::bill(vm, &mut self.work, length - output.len())?;
            Self::reserve_output(vm, &mut output, length)?;
            output.extend_from_slice(&units[copied..start]);
            output.extend_from_slice(replacement.as_utf16());
            matched = Some((start, end));
            copied = end;
            if !global {
                break;
            }
            from = end + usize::from(start == end);
        }
        vm.charge_buffer_work(self.work)?;
        if self.non_ascii {
            return Ok(None);
        }
        if matched.is_none() {
            return Ok(Some((Value::String(input.clone()), None)));
        }
        let length = output.len().saturating_add(units.len() - copied);
        vm.charge_buffer_work(units.len() - copied)?;
        Self::reserve_output(vm, &mut output, length)?;
        output.extend_from_slice(&units[copied..]);
        vm.finish_buffer_string(output)
            .map(|value| Some((value, matched)))
    }

    fn reserve_output(vm: &Vm, output: &mut Vec<u16>, length: usize) -> Result<(), JsError> {
        vm.ensure_string_allocation(length)?;
        if length > output.capacity() {
            let capacity = length
                .max(output.capacity().saturating_mul(2))
                .min(vm.limits().max_string_units);
            vm.ensure_allocation(SCRATCH_BYTES.saturating_add(capacity.saturating_mul(2)))?;
            output
                .try_reserve_exact(capacity - output.len())
                .map_err(|_| limit("RegExp output allocation failed"))?;
        }
        Ok(())
    }
}
