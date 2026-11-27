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
        Expr::Character(member) => {
            Head::consuming(member_for_flags(*member, flags).unwrap_or(ALL))
        }
        Expr::Class { members, negated } => {
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
        // Resource-safe fallback for backreferences and any
        // future op whose first point or empty-match behavior is unknown.
        _ => Head::UNKNOWN,
    }
}

/// Exact membership for ASCII code points, or no table if the class requires
/// Unicode sets or an unknown member predicate.
pub(in crate::regexp) fn class_ascii(
    members: &[Member],
    negated: bool,
    flags: Flags,
) -> Option<u128> {
    if flags.has(Flags::UNICODE_SETS) {
        return None;
    }
    let mask = members
        .iter()
        .try_fold(0, |mask, &member| Some(mask | member_for_flags(member, flags)?))?;
    Some(if negated { !mask } else { mask })
}

fn member_for_flags(member: Member, flags: Flags) -> Option<u128> {
    let mask = ascii_member(member)?;
    if !flags.has(Flags::IGNORE_CASE) { return Some(mask); }
    // Under /iu a non-ASCII literal or range can fold back into ASCII.
    // Leave those atoms to the full matcher. Shorthands are already exact
    // for ASCII inputs. Non-ASCII input positions always remain candidates.
    match member {
        Member::Point(p) if p > 127 => return None,
        Member::Range(_, last) if last > 127 => return None,
        _ => {}
    }
    const UPPER: u128 = ((1 << 26) - 1) << 65;
    const LOWER: u128 = UPPER << 32;
    Some(mask | ((mask & UPPER) << 32) | ((mask & LOWER) >> 32))
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
    // Charge every rejected start. Checking the deadline once per bounded
    // native scan avoids a clock read for every UTF-16 unit of a long miss.
    for chunk in input[index..].chunks(64) {
        let skipped = chunk
            .iter()
            .position(|&unit| unit >= 128 || ascii & (1 << unit) != 0)
            .unwrap_or(chunk.len());
        if skipped != 0 {
            vm.consume_work(skipped)?;
            index += skipped;
        }
        // Always leave a non-ASCII start, including a surrogate pair, to
        // the matcher. It charges the accepted candidate separately.
        if skipped < chunk.len() {
            return Ok(Some(index));
        }
    }
    // The prefix filter is enabled only where an empty match is impossible.
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_scan_charges_only_the_rejected_units() {
        let mut vm = Vm::new();
        for skipped in [0, 1, 63, 64, 65, 128, 129] {
            for candidate in [Some(b'x' as u16), Some(0xd83d), None] {
                let mut input = vec![b'-' as u16; skipped + 3];
                input.extend(candidate);
                let start = 3;
                let expected = candidate.map(|_| start + skipped);
                vm.with_task_limits(skipped as u64 + 1, None, |vm| {
                    assert_eq!(seek(vm, &input, start, 1 << b'x')?, expected);
                    vm.charge_work(1)?;
                    assert_eq!(
                        vm.charge_work(1).unwrap_err().kind,
                        crate::ErrorKind::ExecutionLimit
                    );
                    Ok(())
                })
                .unwrap();
            }
        }
    }
}
