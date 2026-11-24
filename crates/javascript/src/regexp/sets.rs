//! ECMAScript Unicode Sets. Set operations apply equally to whole strings
//! and to individual code points, including unpaired UTF-16 surrogates.
use super::{Flags, case, pattern::Member, program::member_matches};
use crate::{JsError, Vm};
use std::mem::size_of;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    Union,
    Intersection,
    Difference,
}

#[derive(Debug)]
pub(super) enum Set {
    Character(Member),
    Strings(Vec<Vec<u32>>),
    Emoji(u8),
    Operation(Operation, Vec<Set>),
    Complement(Box<Set>),
}

impl Set {
    pub(super) fn max_length(&self) -> usize {
        match self {
            Self::Character(_) | Self::Complement(_) => 1,
            Self::Strings(strings) => strings.iter().map(Vec::len).max().unwrap_or(0),
            Self::Emoji(_) => word(4),
            Self::Operation(op, sets) => match op {
                Operation::Union => sets.iter().map(Self::max_length).max().unwrap_or(0),
                Operation::Intersection => sets.iter().map(Self::max_length).min().unwrap_or(0),
                Operation::Difference => sets[0].max_length(),
            },
        }
    }

    pub(super) fn may_contain_strings(&self) -> bool {
        match self {
            Self::Character(_) | Self::Complement(_) => false,
            Self::Strings(strings) => strings.iter().any(|s| s.len() != 1),
            Self::Emoji(_) => true,
            Self::Operation(op, sets) => match op {
                Operation::Union => sets.iter().any(Self::may_contain_strings),
                Operation::Intersection => sets.iter().all(Self::may_contain_strings),
                Operation::Difference => sets[0].may_contain_strings(),
            },
        }
    }

    pub(super) fn storage(&self) -> usize {
        match self {
            Self::Character(_) | Self::Emoji(_) => 0,
            Self::Strings(strings) => {
                strings.capacity() * size_of::<Vec<u32>>()
                    + strings.iter().map(|s| s.capacity() * 4).sum::<usize>()
            }
            Self::Operation(_, sets) => {
                sets.capacity() * size_of::<Self>() + sets.iter().map(Self::storage).sum::<usize>()
            }
            Self::Complement(set) => size_of::<Self>() + set.storage(),
        }
    }

    pub(super) fn contains(
        &self,
        vm: &mut Vm,
        points: &[u32],
        flags: Flags,
    ) -> Result<bool, JsError> {
        vm.consume_work(1)?;
        Ok(match self {
            Self::Character(member) => matches!(points, [p] if member_matches(*member,*p,flags)),
            Self::Complement(set) => points.len() == 1 && !set.contains(vm, points, flags)?,
            Self::Emoji(mask) => emoji_contains(points, *mask),
            Self::Strings(strings) => {
                for string in strings {
                    vm.consume_work(1)?;
                    if points.len() == string.len() {
                        vm.consume_work(points.len())?;
                        if points.iter().zip(string).all(|(&a, &b)| {
                            a == b
                                || flags.has(Flags::IGNORE_CASE)
                                    && case::equivalents(a, true).any(|x| x == b)
                        }) {
                            return Ok(true);
                        }
                    }
                }
                false
            }
            Self::Operation(op, sets) => {
                for (i, set) in sets.iter().enumerate() {
                    let yes = set.contains(vm, points, flags)?;
                    match op {
                        Operation::Union if yes => return Ok(true),
                        Operation::Intersection if !yes => return Ok(false),
                        Operation::Difference if yes != (i == 0) => return Ok(false),
                        _ => {}
                    }
                }
                !matches!(op, Operation::Union)
            }
        })
    }
}

pub(super) fn emoji_property(name: &str) -> Option<u8> {
    Some(match name {
        "Basic_Emoji" => 1,
        "Emoji_Keycap_Sequence" => 2,
        "RGI_Emoji_Flag_Sequence" => 4,
        "RGI_Emoji_Modifier_Sequence" => 8,
        "RGI_Emoji_Tag_Sequence" => 16,
        "RGI_Emoji_ZWJ_Sequence" => 32,
        "RGI_Emoji" => 63,
        _ => return None,
    })
}

const EMOJI: &[u8] = include_bytes!("data/emoji-sequences.bin");
fn word(at: usize) -> usize {
    u32::from_le_bytes(EMOJI[at..at + 4].try_into().unwrap()) as usize
}
fn emoji_contains(points: &[u32], mask: u8) -> bool {
    let count = word(0);
    let base = 8 + 4 * (count + 1);
    let (mut lo, mut hi) = (0, count);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let offset = 8 + 4 * mid;
        let record = &EMOJI[base + word(offset)..base + word(offset + 4)];
        let values = record[1..]
            .chunks_exact(3)
            .map(|p| u32::from_be_bytes([0, p[0], p[1], p[2]]));
        match values.cmp(points.iter().copied()) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return record[0] & mask != 0,
        }
    }
    false
}
