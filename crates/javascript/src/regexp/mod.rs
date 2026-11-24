//! Independent ECMAScript regular-expression compiler and matcher.
mod bulk;
mod case;
mod operations;
mod pattern;
mod program;
mod property;
mod runtime;

use crate::{ErrorKind, JsError, JsString};
use std::rc::Rc;

pub(crate) use case::{identifier_continue, identifier_start};
pub(crate) use operations::{StringIterator, install as install_protocols, string_hook};
pub(crate) use program::Program;
pub(crate) use runtime::{install, literal, reject_regexp};

const MAX_PATTERN_UNITS: usize = 16 * 1024;
const MAX_CAPTURES: usize = 256;
const MAX_PATTERN_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Flags(u8);
impl Flags {
    const INDICES: u8 = 1;
    const GLOBAL: u8 = 2;
    const IGNORE_CASE: u8 = 4;
    const MULTILINE: u8 = 8;
    const DOT_ALL: u8 = 16;
    const UNICODE: u8 = 32;
    const STICKY: u8 = 64;
    const UNICODE_SETS: u8 = 128;

    fn bit(ch: u16) -> Option<u8> {
        Some(match ch {
            100 => Self::INDICES,
            103 => Self::GLOBAL,
            105 => Self::IGNORE_CASE,
            109 => Self::MULTILINE,
            115 => Self::DOT_ALL,
            117 => Self::UNICODE,
            121 => Self::STICKY,
            118 => Self::UNICODE_SETS,
            _ => return None,
        })
    }
    fn parse(text: &JsString) -> Result<Self, JsError> {
        let mut flags = Self::default();
        for &ch in text.as_utf16() {
            let bit = Self::bit(ch).ok_or_else(|| syntax("Invalid regular expression flag"))?;
            if flags.has(bit) {
                return Err(syntax("Duplicate regular expression flag"));
            }
            flags.0 |= bit;
        }
        if flags.has(Self::UNICODE_SETS) {
            return Err(if flags.has(Self::UNICODE) {
                syntax("Regular expression flags u and v are mutually exclusive")
            } else {
                unsupported("Unicode-set regular expressions (flag v) are unsupported")
            });
        }
        Ok(flags)
    }
    fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
    fn unicode(self) -> bool {
        self.has(Self::UNICODE)
    }
    fn with_modifiers(self, add: u8, remove: u8) -> Self {
        Self((self.0 | add) & !remove)
    }
}

pub(crate) fn compile(source: JsString, flags: JsString) -> Result<Rc<Program>, JsError> {
    if source.len() > MAX_PATTERN_UNITS {
        return Err(limit("Regular expression source limit exceeded"));
    }
    let flags = Flags::parse(&flags)?;
    let pattern = pattern::parse(source.as_utf16(), flags)?;
    Ok(Rc::new(program::compile(source, flags, pattern)))
}

pub(crate) fn literal_flag_character(ch: char) -> bool {
    matches!(ch, '\\' | '$' | '_' | '\u{200c}' | '\u{200d}') || case::identifier_continue(ch as u32)
}

fn syntax(message: impl Into<String>) -> JsError {
    JsError::new(ErrorKind::SyntaxError, message)
}
fn unsupported(message: impl Into<String>) -> JsError {
    JsError::new(ErrorKind::UnsupportedFeature, message)
}
fn limit(message: impl Into<String>) -> JsError {
    JsError::new(ErrorKind::ExecutionLimit, message)
}
