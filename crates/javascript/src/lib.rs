//! The browser's independently implemented JavaScript interpreter.
//!
//! This is an explicitly incomplete ECMAScript implementation. See
//! `docs/JAVASCRIPT.md` for the supported language and embedding contract.
mod async_iterator;
mod bigint;
mod binary;
mod builtins;
mod bytecode;
mod classes;
mod collections;
mod date;
mod dynamic_import;
mod execution;
mod for_in;
mod generator;
mod heap;
mod host_properties;
mod iterator;
mod json;
mod lexer;
mod modules;
mod parser;
mod patterns;
mod private;
mod promise;
mod regexp;
mod structured_clone;
mod symbol;
mod syntax;
mod uri;
mod value;
mod vm;
mod wasm;

pub use bigint::JsBigInt;
pub use binary::{ArrayBufferView, ArrayBufferViewKind};
pub use dynamic_import::DynamicImportRequest;
pub use host_properties::{HostIndexedProperties, HostNamedProperties};
pub use iterator::IteratorRecord;
pub use modules::{ModuleId, ModuleRequest, ModuleStatus};
pub use promise::PromiseCapability;
pub use structured_clone::{
    MAX_STRUCTURED_CLONE_TRANSFERS, PlatformClone, PlatformTransfer, StructuredCloneError,
    StructuredCloneOutput, StructuredClonePacket,
};
pub use value::{ErrorKind, JsError, JsString, ObjectId, PropertyKey, SymbolId, Value};
pub use vm::realm::RealmId;
pub use vm::{GcStats, RootId, Vm, VmLimits};
