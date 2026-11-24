//! Document-realm console capture. This channel does not alter legacy diagnostics.
use super::{DIAGNOSTIC_CHAR_LIMIT, DIAGNOSTIC_LIMIT, Shared, diagnostic, method};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};

/// Maximum pending records in a runtime, independent of its diagnostics budget.
pub const CONSOLE_RECORD_LIMIT: usize = 64;
/// Byte limit for each display string (text or source), at a UTF-8 boundary.
pub const CONSOLE_TEXT_BYTES: usize = 2048;
const ARG_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleLevel {
    Log,
    Info,
    Warn,
    Error,
    Debug,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsolePhase {
    /// A classic script task, including a fatal failure in its checkpoint.
    ClassicScript,
    DomListener,
    EventHandlerCompile,
    EventHandler,
    EventTargetListener,
    Microtask,
    UnhandledPromiseRejection,
    HostTask,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleOrigin {
    ConsoleApi,
    /// Reported by the host, not merely thrown and caught by page code.
    BrowserReported(ConsolePhase),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleRecord {
    pub level: ConsoleLevel,
    pub origin: ConsoleOrigin,
    pub text: String,
    /// A known runtime label (e.g. an event name or script node), not a stack frame.
    pub source: Option<String>,
    /// Present when the reporting boundary had a typed VM error.
    pub error_kind: Option<ErrorKind>,
    /// VM-provided coordinates, when available.
    pub line: Option<usize>,
    pub column: Option<usize>,
    /// Capture clipped the text, source, or argument list.
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsoleSnapshot {
    /// Capture order. Once full, the buffer drops new records until drained.
    pub records: Vec<ConsoleRecord>,
    /// Number dropped since this runtime was created or the last drain.
    pub dropped: u64,
}

#[derive(Default)]
pub(super) struct ConsoleBuffer(ConsoleSnapshot);

impl ConsoleBuffer {
    fn push(&mut self, record: impl FnOnce() -> ConsoleRecord) {
        if self.0.records.len() >= CONSOLE_RECORD_LIMIT {
            self.0.dropped = self.0.dropped.saturating_add(1);
        } else {
            self.0.records.push(record());
        }
    }

    pub(super) fn snapshot(&self) -> ConsoleSnapshot {
        self.0.clone()
    }

    pub(super) fn take_snapshot(&mut self) -> ConsoleSnapshot {
        std::mem::take(&mut self.0)
    }
}

impl ConsoleRecord {
    fn new(level: ConsoleLevel, origin: ConsoleOrigin, text: &str) -> Self {
        let (text, truncated) = clipped(text);
        Self {
            level,
            origin,
            text: text.into(),
            source: None,
            error_kind: None,
            line: None,
            column: None,
            truncated,
        }
    }
}

fn clipped(text: &str) -> (&str, bool) {
    let end = text.floor_char_boundary(CONSOLE_TEXT_BYTES.min(text.len()));
    (&text[..end], end < text.len())
}

// Read only VM value variants and Rust-owned text. In particular do not inspect
// properties, traverse prototypes, or call the VM's coercion operations here.
fn preview(value: &Value, units: usize) -> (String, bool) {
    let text = match value {
        Value::String(value) => {
            let units = &value.as_utf16()[..value.len().min(units)];
            return (String::from_utf16_lossy(units), units.len() < value.len());
        }
        Value::Number(value) => value.to_string(),
        // JsBigInt's Rust Debug uses a fixed-size summary above 512 bits.
        Value::BigInt(value) => format!("{value:?}"),
        Value::Bool(value) => value.to_string(),
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Object(_) => "[object]".into(),
        Value::Symbol(_) => "Symbol".into(),
    };
    (text, false)
}

fn preview_args(args: &[Value]) -> (String, bool) {
    // Keep the legacy spacing and UTF-16 preview behavior. The structured
    // record has an additional byte cap applied after this step.
    let mut text = String::new();
    let mut truncated = args.len() > ARG_LIMIT;
    for value in args.iter().take(ARG_LIMIT) {
        let remaining = DIAGNOSTIC_CHAR_LIMIT.saturating_sub(text.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        let (value, clipped) = preview(value, remaining);
        text.push_str(&value);
        truncated |= clipped;
    }
    (text, truncated)
}

fn record_args(state: &Shared, level: ConsoleLevel, origin: ConsoleOrigin, args: &[Value]) {
    let mut state = state.borrow_mut();
    let legacy_preview = if state.diagnostics.len() < DIAGNOSTIC_LIMIT {
        let preview = preview_args(args);
        state
            .diagnostics
            .push(preview.0.chars().take(DIAGNOSTIC_CHAR_LIMIT).collect());
        Some(preview)
    } else {
        None
    };
    state.devtools_console.push(|| {
        let (text, truncated) = legacy_preview.unwrap_or_else(|| preview_args(args));
        let mut record = ConsoleRecord::new(level, origin, &text);
        record.truncated |= truncated;
        record
    });
}

fn install_method(
    vm: &mut Vm,
    state: &Shared,
    console: Value,
    name: &str,
    level: ConsoleLevel,
    origin: ConsoleOrigin,
) -> Result<(), JsError> {
    let state = state.clone();
    method(vm, console, name, move |_, _, args| {
        record_args(&state, level, origin, args);
        Ok(Value::Undefined)
    })
}

pub(super) fn install(vm: &mut Vm, state: &Shared) -> Result<Value, JsError> {
    let console = vm.new_object();
    for (name, level) in [
        ("log", ConsoleLevel::Log),
        ("info", ConsoleLevel::Info),
        ("warn", ConsoleLevel::Warn),
        ("error", ConsoleLevel::Error),
        ("debug", ConsoleLevel::Debug),
    ] {
        let origin = if name == "error" {
            // EventTarget bootstrap roots this callback as its legacy reporter.
            ConsoleOrigin::BrowserReported(ConsolePhase::EventTargetListener)
        } else {
            ConsoleOrigin::ConsoleApi
        };
        install_method(vm, state, console.clone(), name, level, origin)?;
    }
    vm.set_global("console", console.clone())?;
    Ok(console)
}

/// Called after the host has rooted the internal EventTarget reporter, before
/// any page code runs. Public console.error calls now have their own origin.
pub(super) fn install_public_error(
    vm: &mut Vm,
    state: &Shared,
    console: Value,
) -> Result<(), JsError> {
    install_method(
        vm,
        state,
        console,
        "error",
        ConsoleLevel::Error,
        ConsoleOrigin::ConsoleApi,
    )
}

pub(super) fn report_error(
    state: &Shared,
    phase: ConsolePhase,
    source: Option<&str>,
    error: &JsError,
    legacy: impl FnOnce() -> String,
) {
    state.borrow_mut().devtools_console.push(|| {
        // JsError already contains a VM-produced message. Never inspect `thrown`.
        let mut record = ConsoleRecord::new(
            ConsoleLevel::Error,
            ConsoleOrigin::BrowserReported(phase),
            &error.message,
        );
        if let Some(source) = source {
            let (source, truncated) = clipped(source);
            record.source = Some(source.into());
            record.truncated |= truncated;
        }
        record.error_kind = Some(error.kind);
        record.line = error.line;
        record.column = error.column;
        record
    });
    if state.borrow().diagnostics.len() < DIAGNOSTIC_LIMIT {
        diagnostic(state, legacy());
    }
}

pub(super) fn report_rejection(state: &Shared, value: &Value) {
    let make_preview = || {
        if matches!(value, Value::Object(_)) {
            ("object".into(), false)
        } else {
            preview(value, DIAGNOSTIC_CHAR_LIMIT)
        }
    };
    let legacy_preview = if state.borrow().diagnostics.len() < DIAGNOSTIC_LIMIT {
        let preview = make_preview();
        diagnostic(state, format!("Unhandled promise rejection: {}", preview.0));
        Some(preview)
    } else {
        None
    };
    state.borrow_mut().devtools_console.push(|| {
        let (text, truncated) = legacy_preview.unwrap_or_else(make_preview);
        let mut record = ConsoleRecord::new(
            ConsoleLevel::Error,
            ConsoleOrigin::BrowserReported(ConsolePhase::UnhandledPromiseRejection),
            &text,
        );
        record.truncated |= truncated;
        record
    });
}
