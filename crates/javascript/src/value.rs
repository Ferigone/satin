use std::{
    cell::OnceCell,
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    rc::Rc,
};

const INLINE_UNITS: usize = 8;
const MAX_APPEND_DEPTH: u8 = 48;

enum StringStorage {
    Inline {
        units: [u16; INLINE_UNITS],
        len: u8,
    },
    Flat(Box<[u16]>),
    Appended {
        prefix: JsString,
        suffix: [u16; INLINE_UNITS],
        suffix_len: u8,
        len: usize,
        depth: u8,
        flattened: OnceCell<Box<[u16]>>,
    },
}

/// JavaScript text is a sequence of UTF-16 code units, not Unicode scalar values.
/// In particular, this representation round-trips isolated surrogate code units.
#[derive(Clone)]
pub struct JsString(Rc<StringStorage>);

impl JsString {
    pub fn from_utf16(units: impl Into<Vec<u16>>) -> Self {
        let units = units.into();
        if units.len() <= INLINE_UNITS {
            return Self::from_utf16_slice(&units);
        }
        Self(Rc::new(StringStorage::Flat(units.into_boxed_slice())))
    }
    pub(crate) fn from_utf16_slice(units: &[u16]) -> Self {
        if units.len() <= INLINE_UNITS {
            let mut inline = [0; INLINE_UNITS];
            inline[..units.len()].copy_from_slice(units);
            Self(Rc::new(StringStorage::Inline {
                units: inline,
                len: units.len() as u8,
            }))
        } else {
            Self(Rc::new(StringStorage::Flat(units.into())))
        }
    }
    pub fn as_utf16(&self) -> &[u16] {
        match self.0.as_ref() {
            StringStorage::Inline { units, len } => &units[..*len as usize],
            StringStorage::Flat(units) => units,
            StringStorage::Appended { len, flattened, .. } => flattened.get_or_init(|| {
                let mut units = Vec::with_capacity(*len);
                self.copy_units(&mut units);
                units.into_boxed_slice()
            }),
        }
    }
    pub(crate) fn storage_id(&self) -> usize {
        Rc::as_ptr(&self.0) as usize
    }
    pub(crate) fn allocation_bytes(&self) -> usize {
        // Appended strings reserve their eventual contiguous representation.
        // Native code may borrow the UTF-16 slice without a fallible allocation API.
        self.len()
            .saturating_mul(2)
            .saturating_add(std::mem::size_of::<StringStorage>() + 16)
    }
    pub fn len(&self) -> usize {
        match self.0.as_ref() {
            StringStorage::Inline { len, .. } => *len as usize,
            StringStorage::Flat(units) => units.len(),
            StringStorage::Appended { len, .. } => *len,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Lossy conversion is for display/host APIs only. VM string operations use code units.
    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(self.as_utf16())
    }
    pub(crate) fn concat(&self, other: &Self) -> Self {
        let mut units = Vec::with_capacity(self.len() + other.len());
        self.copy_units(&mut units);
        other.copy_units(&mut units);
        Self::from_utf16(units)
    }
    pub(crate) fn can_append(&self, other: &Self) -> bool {
        other.len() <= INLINE_UNITS && self.depth() < MAX_APPEND_DEPTH
    }
    pub(crate) fn append(self, other: &Self) -> Self {
        let len = self.len() + other.len();
        let depth = self.depth() + 1;
        let suffix_len = other.len();
        let mut suffix = [0; INLINE_UNITS];
        suffix[..suffix_len].copy_from_slice(other.as_utf16());
        Self(Rc::new(StringStorage::Appended {
            prefix: self,
            suffix,
            suffix_len: suffix_len as u8,
            len,
            depth,
            flattened: OnceCell::new(),
        }))
    }
    pub(crate) fn prefix(&self) -> Option<&Self> {
        match self.0.as_ref() {
            StringStorage::Appended { prefix, .. } => Some(prefix),
            _ => None,
        }
    }
    fn depth(&self) -> u8 {
        match self.0.as_ref() {
            StringStorage::Appended { depth, .. } => *depth,
            _ => 0,
        }
    }
    fn copy_units(&self, out: &mut Vec<u16>) {
        if let StringStorage::Appended {
            prefix,
            suffix,
            suffix_len,
            flattened,
            ..
        } = self.0.as_ref()
        {
            if let Some(units) = flattened.get() {
                out.extend_from_slice(units);
            } else {
                prefix.copy_units(out);
                out.extend_from_slice(&suffix[..*suffix_len as usize]);
            }
        } else {
            out.extend_from_slice(self.as_utf16());
        }
    }
}
impl Default for JsString {
    fn default() -> Self {
        Self::from_utf16_slice(&[])
    }
}
impl PartialEq for JsString {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
            || self.len() == other.len() && self.as_utf16() == other.as_utf16()
    }
}
impl Eq for JsString {}
impl PartialOrd for JsString {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for JsString {
    fn cmp(&self, other: &Self) -> Ordering {
        if Rc::ptr_eq(&self.0, &other.0) {
            Ordering::Equal
        } else {
            self.as_utf16().cmp(other.as_utf16())
        }
    }
}
impl Hash for JsString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_utf16().hash(state);
    }
}
impl From<&str> for JsString {
    fn from(s: &str) -> Self {
        Self::from_utf16(s.encode_utf16().collect::<Vec<_>>())
    }
}
impl From<String> for JsString {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}
impl fmt::Debug for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"")?;
        for unit in self.as_utf16() {
            match *unit {
                0x20..=0x21 | 0x23..=0x5b | 0x5d..=0x7e => {
                    write!(f, "{}", char::from_u32(*unit as u32).unwrap())?
                }
                _ => write!(f, "\\u{:04x}", unit)?,
            }
        }
        f.write_str("\"")
    }
}
impl fmt::Display for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_lossy())
    }
}

/// An opaque, generation-checked handle valid only in its allocating heap.
/// Keep externally held object values rooted in that heap.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ObjectId {
    pub(crate) is_html_dda: bool,
    pub(crate) heap_id: u64,
    pub(crate) index: u32,
    pub(crate) generation: u32,
}
#[derive(Debug)]
pub(crate) struct SymbolData {
    pub description: Option<JsString>,
    pub registry_key: Option<JsString>,
}
/// A primitive symbol's opaque identity. Descriptions are diagnostic text, never identity.
#[derive(Clone, Debug)]
pub struct SymbolId(pub(crate) Rc<SymbolData>);
impl SymbolId {
    pub fn description(&self) -> Option<&JsString> {
        self.0.description.as_ref()
    }
    pub(crate) fn id(&self) -> usize {
        Rc::as_ptr(&self.0) as usize
    }
}
impl PartialEq for SymbolId {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for SymbolId {}
impl PartialOrd for SymbolId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for SymbolId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id().cmp(&other.id())
    }
}
impl std::hash::Hash for SymbolId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id().hash(state)
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum PropertyKey {
    String(JsString),
    Symbol(SymbolId),
}
impl From<JsString> for PropertyKey {
    fn from(s: JsString) -> Self {
        Self::String(s)
    }
}
impl From<&str> for PropertyKey {
    fn from(s: &str) -> Self {
        Self::String(s.into())
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    BigInt(crate::JsBigInt),
    String(JsString),
    Symbol(SymbolId),
    Object(ObjectId),
}
impl Value {
    pub fn as_bigint(&self) -> Option<&crate::JsBigInt> {
        if let Self::BigInt(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub fn as_number(&self) -> Option<f64> {
        if let Self::Number(n) = self {
            Some(*n)
        } else {
            None
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(b) = self {
            Some(*b)
        } else {
            None
        }
    }
    pub fn as_string(&self) -> Option<&JsString> {
        if let Self::String(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_object(&self) -> Option<ObjectId> {
        if let Self::Object(id) = self {
            Some(*id)
        } else {
            None
        }
    }
    pub fn as_symbol(&self) -> Option<&SymbolId> {
        if let Self::Symbol(id) = self {
            Some(id)
        } else {
            None
        }
    }
    pub fn is_undefined(&self) -> bool {
        matches!(self, Self::Undefined)
    }
    pub fn is_nullish(&self) -> bool {
        matches!(self, Self::Undefined | Self::Null)
    }
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => *n != 0.0 && !n.is_nan(),
            Self::BigInt(value) => !value.is_zero(),
            Self::String(s) => !s.is_empty(),
            Self::Object(id) => !id.is_html_dda,
            Self::Symbol(_) => true,
        }
    }
}
impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::String(s.into())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::String(s.into())
    }
}
impl From<JsString> for Value {
    fn from(s: JsString) -> Self {
        Self::String(s)
    }
}
impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Self::Number(n)
    }
}
impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Self::Number(n as f64)
    }
}
impl From<u32> for Value {
    fn from(n: u32) -> Self {
        Self::Number(n as f64)
    }
}
impl From<usize> for Value {
    fn from(n: usize) -> Self {
        Self::Number(n as f64)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ErrorKind {
    Error,
    SyntaxError,
    ReferenceError,
    TypeError,
    RangeError,
    EvalError,
    UriError,
    AggregateError,
    CompileError,
    LinkError,
    RuntimeError,
    Thrown,
    UnsupportedFeature,
    ExecutionLimit,
    HeapLimit,
    InternalError,
}
impl ErrorKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::SyntaxError => "SyntaxError",
            Self::ReferenceError => "ReferenceError",
            Self::TypeError => "TypeError",
            Self::RangeError => "RangeError",
            Self::EvalError => "EvalError",
            Self::UriError => "URIError",
            Self::AggregateError => "AggregateError",
            Self::CompileError => "CompileError",
            Self::LinkError => "LinkError",
            Self::RuntimeError => "RuntimeError",
            Self::Thrown => "Thrown",
            Self::UnsupportedFeature => "UnsupportedFeature",
            Self::ExecutionLimit => "ExecutionLimit",
            Self::HeapLimit => "HeapLimit",
            Self::InternalError => "InternalError",
        }
    }
}
#[derive(Clone, Debug)]
pub struct JsError {
    pub kind: ErrorKind,
    pub message: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub thrown: Option<Value>,
    /// Actual synchronous interpreted functions at the error boundary, innermost first.
    pub inspected_calls: Option<Rc<Vec<String>>>,
}
impl JsError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            line: None,
            column: None,
            thrown: None,
            inspected_calls: None,
        }
    }
    pub fn type_error(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::TypeError, message)
    }
    pub(crate) fn syntax(message: impl Into<String>, line: usize, column: usize) -> Self {
        Self {
            kind: ErrorKind::SyntaxError,
            message: message.into(),
            line: Some(line),
            column: Some(column),
            thrown: None,
            inspected_calls: None,
        }
    }
    pub(crate) fn uncatchable(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::UnsupportedFeature
                | ErrorKind::ExecutionLimit
                | ErrorKind::HeapLimit
                | ErrorKind::InternalError
        )
    }
}
impl fmt::Display for JsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.name(), self.message)?;
        if let (Some(line), Some(column)) = (self.line, self.column) {
            write!(f, " ({line}:{column})")?;
        }
        Ok(())
    }
}
impl std::error::Error for JsError {}
