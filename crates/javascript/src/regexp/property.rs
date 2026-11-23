//! Exact ECMAScript Unicode property names and values for `u` patterns.
use super::unsupported;
use crate::JsError;

include!("data/property-aliases.rs");
include!("data/property-ranges.rs");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Property {
    AsciiHexDigit,
    HexDigit,
    SoftDotted,
    Unsupported,
}

impl Property {
    pub(super) fn contains(self, point: u32) -> Result<bool, JsError> {
        let ranges = match self {
            Self::AsciiHexDigit => ASCII_HEX_DIGIT,
            Self::HexDigit => HEX_DIGIT,
            Self::SoftDotted => SOFT_DOTTED,
            Self::Unsupported => return Err(unsupported_escape()),
        };
        let index = ranges.partition_point(|&(_, last)| last < point);
        Ok(ranges.get(index).is_some_and(|&(first, _)| first <= point))
    }
}

pub(super) fn unsupported_escape() -> JsError {
    unsupported("This Unicode property escape is unsupported")
}

pub(super) fn resolve(expression: &[u16]) -> Option<Property> {
    let mut parts = expression.split(|&unit| unit == u16::from(b'='));
    let name = parts.next().unwrap();
    let Some(value) = parts.next() else {
        if let Ok(index) = MATCHED_BINARY
            .binary_search_by(|(alias, _)| alias.encode_utf16().cmp(name.iter().copied()))
        {
            return Some(MATCHED_BINARY[index].1);
        }
        return (contains(CATEGORIES, name) || contains(BINARY, name))
            .then_some(Property::Unsupported);
    };
    if parts.next().is_some() {
        return None;
    }
    let valid = if ["gc", "General_Category"]
        .iter()
        .any(|alias| alias.encode_utf16().eq(name.iter().copied()))
    {
        contains(CATEGORIES, value)
    } else if ["sc", "Script", "scx", "Script_Extensions"]
        .iter()
        .any(|alias| alias.encode_utf16().eq(name.iter().copied()))
    {
        contains(SCRIPTS, value)
    } else {
        false
    };
    valid.then_some(Property::Unsupported)
}

fn contains(aliases: &[&str], input: &[u16]) -> bool {
    aliases
        .binary_search_by(|alias| alias.encode_utf16().cmp(input.iter().copied()))
        .is_ok()
}
