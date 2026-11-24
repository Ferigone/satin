//! Find unanchored ASCII starts without dispatching the backtracking matcher
//! at positions where its first consuming atom cannot succeed.
use super::super::{
    Flags,
    pattern::{Expr, Member, Shorthand},
};
use crate::{JsError, Vm};

const ALL: u128 = u128::MAX;

#[derive(Clone, Copy)]
struct Head {
    ascii: u128,
    nullable: bool,
}

impl Head {
    const EMPTY: Self = Self {
        ascii: 0,
        nullable: true,
    };
    const UNKNOWN: Self = Self {
        ascii: ALL,
        nullable: true,
    };

    const fn consuming(ascii: u128) -> Self {
        Self {
            ascii,
            nullable: false,
        }
    }
}

pub(in crate::regexp) fn first_ascii(expr: &Expr, flags: Flags) -> u128 {
    // Unicode-set classes may match strings, including the empty string.
    if flags.has(Flags::UNICODE_SETS) {
        return ALL;
    }
    let head = head(expr, flags);
    if head.nullable { ALL } else { head.ascii }
}

fn head(expr: &Expr, flags: Flags) -> Head {
    match expr {
        Expr::Sequence(parts) => {
            let mut out = Head::EMPTY;
            for part in parts {
                if !out.nullable {
                    break;
                }
                let next = head(part, flags);
                out.ascii |= next.ascii;
                out.nullable = next.nullable;
            }
            out
        }
        Expr::Choice(parts) => {
            let mut out = Head::consuming(0);
            for part in parts {
                let branch = head(part, flags);
                out.ascii |= branch.ascii;
                out.nullable |= branch.nullable;
            }
            out
        }
        Expr::Character(member) if !flags.has(Flags::IGNORE_CASE) => {
            Head::consuming(ascii_member(*member).unwrap_or(ALL))
        }
        Expr::Class { members, negated } if !flags.has(Flags::IGNORE_CASE) => {
            Head::consuming(class_ascii(members, *negated, flags).unwrap_or(ALL))
        }
        Expr::Assert(_) | Expr::Look { .. } => Head::EMPTY,
        Expr::Capture(_, body) => head(body, flags),
        Expr::Modifiers { body, add, remove } => head(body, flags.with_modifiers(*add, *remove)),
        Expr::Repeat {
            body,
            minimum,
            maximum,
            ..
        } => {
            if *maximum == Some(0) {
                Head::EMPTY
            } else {
                let mut out = head(body, flags);
                out.nullable |= *minimum == 0;
                out
            }
        }
        // Resource-safe fallback for case folding, backreferences, and any
        // future op whose first point or empty-match behavior is unknown.
        _ => Head::UNKNOWN,
    }
}

/// Exact membership for ASCII code points, or no table if the class requires
/// case folding, Unicode sets, or an unknown member predicate.
pub(in crate::regexp) fn class_ascii(
    members: &[Member],
    negated: bool,
    flags: Flags,
) -> Option<u128> {
    if flags.has(Flags::IGNORE_CASE | Flags::UNICODE_SETS) {
        return None;
    }
    let mask = members
        .iter()
        .try_fold(0, |mask, &member| Some(mask | ascii_member(member)?))?;
    Some(if negated { !mask } else { mask })
}

pub(in crate::regexp) fn ascii_member(member: Member) -> Option<u128> {
    let mask = match member {
        Member::Point(point) => range(point, point),
        Member::Range(first, last) => range(first, last),
        Member::Shorthand(kind, negated) => {
            let digits = range(b'0' as u32, b'9' as u32);
            let mask = match kind {
                Shorthand::Digit => digits,
                Shorthand::Space => range(0x09, 0x0d) | range(0x20, 0x20),
                Shorthand::Word => {
                    digits | range(0x41, 0x5a) | range(0x61, 0x7a) | range(0x5f, 0x5f)
                }
            };
            if negated { !mask } else { mask }
        }
        // A complement is safe only if all members are understood exactly.
        #[allow(unreachable_patterns)]
        _ => return None,
    };
    Some(mask)
}

fn range(first: u32, last: u32) -> u128 {
    if first > 127 || last < first {
        0
    } else {
        (ALL << first) & (ALL >> (127 - last.min(127)))
    }
}

pub(super) fn seek(
    vm: &mut Vm,
    input: &[u16],
    mut index: usize,
    ascii: u128,
) -> Result<Option<usize>, JsError> {
    if ascii == ALL {
        return Ok(Some(index));
    }
    while let Some(&unit) = input.get(index) {
        // Always leave non-ASCII starts to the matcher, especially the
        // interior of a surrogate pair with u. Every accepted start pays the
        // ordinary matcher charge for processing that candidate.
        if unit >= 128 || ascii & (1 << unit) != 0 {
            return Ok(Some(index));
        }
        vm.consume_work(1)?;
        index += 1;
    }
    // The prefix filter is enabled only where an empty match is impossible.
    Ok(None)
}
