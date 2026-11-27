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
    /// The actual main-document classic script node active at the host boundary.
    pub script_node: Option<crate::dom::NodeId>,
    /// Synchronous interpreted functions (innermost first) when observed.
    pub calls: Option<std::rc::Rc<Vec<String>>>,
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
            script_node: None,
            calls: None,
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

fn preview_args(args: &[Value], _vm: Option<&Vm>) -> (String, bool) {
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
        #[cfg(feature = "media-startup-diagnostics")]
        if let Some(error) = _vm.and_then(|vm| vm.diagnostic_native_error(value)) {
            text.push_str(&error[..error.floor_char_boundary(remaining.min(error.len()))]);
            truncated |= error.len() > remaining;
            continue;
        }
        let (value, clipped) = preview(value, remaining);
        text.push_str(&value);
        truncated |= clipped;
    }
    (text, truncated)
}

fn record_args(state: &Shared, level: ConsoleLevel, origin: ConsoleOrigin, args: &[Value]) {
    record_call(state, level, origin, args, None);
}

fn record_call(
    state: &Shared,
    level: ConsoleLevel,
    origin: ConsoleOrigin,
    args: &[Value],
    vm: Option<&Vm>,
) {
    let mut state = state.borrow_mut();
    let legacy_preview = if state.diagnostics.len() < DIAGNOSTIC_LIMIT {
        let preview = preview_args(args, vm);
        state
            .diagnostics
            .push(preview.0.chars().take(DIAGNOSTIC_CHAR_LIMIT).collect());
        Some(preview)
    } else {
        None
    };
    let active_node = state.current_script;
    state.devtools_console.push(|| {
        let (text, truncated) = legacy_preview.unwrap_or_else(|| preview_args(args, vm));
        let mut record = ConsoleRecord::new(level, origin, &text);
        if let Some(node) = active_node {
            record.script_node = Some(node);
            record.source = Some(format!("script #{}", node.0));
        }
        record.calls = vm.and_then(Vm::inspect_call_stack);
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
    method(vm, console, name, move |vm, _, args| {
        record_call(&state, level, origin, args, Some(vm));
        Ok(Value::Undefined)
    })
}

const LABEL_LIMIT: usize = 256;
const TABLE_LIMIT: usize = 128;

#[derive(Default)]
struct ToolsState {
    counters: std::collections::HashMap<String, u64>,
    timers: std::collections::HashMap<String, std::time::Instant>,
}

fn install_utilities(vm: &mut Vm, state: &Shared, console: &Value) -> Result<(), JsError> {
    let shared = state.clone();
    method(vm, console.clone(), "assert", move |_, _, args| {
        if !args.first().is_some_and(Value::is_truthy) {
            let mut data = vec![Value::from("Assertion failed:")];
            data.extend(args.iter().skip(1).take(ARG_LIMIT).cloned());
            record_args(
                &shared,
                ConsoleLevel::Error,
                ConsoleOrigin::ConsoleApi,
                &data,
            );
        }
        Ok(Value::Undefined)
    })?;
    let shared = state.clone();
    method(vm, console.clone(), "dir", move |vm, _, args| {
        let preview = vm.inspect_value(args.first().unwrap_or(&Value::Undefined));
        record_args(
            &shared,
            ConsoleLevel::Log,
            ConsoleOrigin::ConsoleApi,
            &[Value::from(preview.as_str())],
        );
        Ok(Value::Undefined)
    })?;
    let storage = std::rc::Rc::new(std::cell::RefCell::new(ToolsState::default()));
    for name in ["count", "countReset", "time", "timeLog", "timeEnd"] {
        let shared = state.clone();
        let storage = storage.clone();
        method(vm, console.clone(), name, move |vm, _, args| {
            let label = match args.first() {
                None | Some(Value::Undefined) => "default".to_owned(),
                Some(value) => {
                    let text = vm.to_string(value)?;
                    if text.len() > LABEL_LIMIT {
                        record_args(
                            &shared,
                            ConsoleLevel::Warn,
                            ConsoleOrigin::ConsoleApi,
                            &[Value::from("Console label is too long")],
                        );
                        return Ok(Value::Undefined);
                    }
                    text.to_string_lossy()
                }
            };
            let mut tools = storage.borrow_mut();
            let mut level = ConsoleLevel::Log;
            let message = match name {
                "count" => {
                    if tools.counters.len() == TABLE_LIMIT && !tools.counters.contains_key(&label) {
                        level = ConsoleLevel::Warn;
                        "Console counter limit reached".to_owned()
                    } else {
                        let count = tools.counters.entry(label.clone()).or_default();
                        *count = count.saturating_add(1);
                        format!("{label}: {count}")
                    }
                }
                "countReset" => {
                    if tools.counters.remove(&label).is_some() {
                        return Ok(Value::Undefined);
                    }
                    level = ConsoleLevel::Warn;
                    format!("Unknown counter: {label}")
                }
                "time" => {
                    if tools.timers.contains_key(&label) {
                        level = ConsoleLevel::Warn;
                        format!("Timer already running: {label}")
                    } else if tools.timers.len() == TABLE_LIMIT {
                        level = ConsoleLevel::Warn;
                        "Console timer limit reached".to_owned()
                    } else {
                        tools.timers.insert(label, std::time::Instant::now());
                        return Ok(Value::Undefined);
                    }
                }
                _ => {
                    let start = if name == "timeEnd" {
                        tools.timers.remove(&label)
                    } else {
                        tools.timers.get(&label).copied()
                    };
                    if let Some(start) = start {
                        format!("{label}: {:.3} ms", start.elapsed().as_secs_f64() * 1000.)
                    } else {
                        level = ConsoleLevel::Warn;
                        format!("Unknown timer: {label}")
                    }
                }
            };
            drop(tools);
            let mut data = vec![Value::from(message.as_str())];
            if name == "timeLog" && level == ConsoleLevel::Log {
                data.extend(args.iter().skip(1).take(ARG_LIMIT).cloned());
            }
            record_args(&shared, level, ConsoleOrigin::ConsoleApi, &data);
            Ok(Value::Undefined)
        })?;
    }
    Ok(())
}

const LOG_METHODS: [(&str, ConsoleLevel); 6] = [
    ("log", ConsoleLevel::Log),
    ("info", ConsoleLevel::Info),
    ("warn", ConsoleLevel::Warn),
    ("error", ConsoleLevel::Error),
    ("debug", ConsoleLevel::Debug),
    ("trace", ConsoleLevel::Debug),
];

pub(super) fn install(vm: &mut Vm, state: &Shared) -> Result<Value, JsError> {
    vm.enable_inspection();
    let console = vm.new_object();
    for (name, level) in LOG_METHODS {
        let origin = if name == "error" {
            // EventTarget bootstrap roots this callback as its legacy reporter.
            ConsoleOrigin::BrowserReported(ConsolePhase::EventTargetListener)
        } else {
            ConsoleOrigin::ConsoleApi
        };
        install_method(vm, state, console.clone(), name, level, origin)?;
    }
    install_utilities(vm, state, &console)?;
    vm.set_global("console", console.clone())?;
    Ok(console)
}

pub(super) fn install_worker(
    vm: &mut Vm,
    send: impl Fn(ConsoleLevel, String, bool) + 'static,
) -> Result<(), JsError> {
    let console = vm.new_object();
    let send = std::rc::Rc::new(send);
    for (name, level) in LOG_METHODS {
        let send = send.clone();
        method(vm, console.clone(), name, move |vm, _, args| {
            let (text, truncated) = preview_args(args, Some(vm));
            send(level, text, truncated);
            Ok(Value::Undefined)
        })?;
    }
    vm.set_global("console", console)
}

pub(super) fn record_worker(state: &Shared, level: ConsoleLevel, text: &str, truncated: bool) {
    let mut state = state.borrow_mut();
    if state.diagnostics.len() < DIAGNOSTIC_LIMIT {
        state.diagnostics.push(text.chars().take(DIAGNOSTIC_CHAR_LIMIT).collect());
    }
    state.devtools_console.push(|| {
        let mut record = ConsoleRecord::new(level, ConsoleOrigin::ConsoleApi, text);
        record.source = Some("Worker".into());
        record.truncated |= truncated;
        record
    });
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
    record_error(state, phase, source, error);
    if state.borrow().diagnostics.len() < DIAGNOSTIC_LIMIT {
        diagnostic(state, legacy());
    }
}

pub(super) fn record_error(
    state: &Shared,
    phase: ConsolePhase,
    source: Option<&str>,
    error: &JsError,
) {
    record_error_at_node(state, phase, source, error, None)
}

pub(super) fn report_classic_error(
    state: &Shared,
    node: crate::dom::NodeId,
    error: &JsError,
    legacy: impl FnOnce() -> String,
) {
    record_error_at_node(
        state,
        ConsolePhase::ClassicScript,
        Some(&format!("script #{}", node.0)),
        error,
        Some(node),
    );
    if state.borrow().diagnostics.len() < DIAGNOSTIC_LIMIT {
        diagnostic(state, legacy());
    }
}

fn record_error_at_node(
    state: &Shared,
    phase: ConsolePhase,
    source: Option<&str>,
    error: &JsError,
    script_node: Option<crate::dom::NodeId>,
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
        record.script_node = script_node;
        record.calls = error.inspected_calls.clone();
        record.error_kind = Some(error.kind);
        record.line = error.line;
        record.column = error.column;
        record
    });
}

pub(super) fn report_rejection(vm: &Vm, state: &Shared, value: &Value) {
    let make_preview = || {
        #[cfg(feature = "media-startup-diagnostics")]
        if let Some(error) = vm.diagnostic_native_error(value) {
            let end = error.floor_char_boundary(DIAGNOSTIC_CHAR_LIMIT.min(error.len()));
            return (error[..end].to_owned(), end < error.len());
        }
        if matches!(value, Value::Object(_)) {
            (
                vm.inspect_rejection_reason(value).unwrap_or_else(|| "object".into()),
                false,
            )
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

// Run in the actual document realm. Format the completion before draining jobs:
// the checkpoint may release JS values, and must also deliver mutations/logs.
impl super::ScriptRuntime {
    pub fn evaluate_console(&mut self, source: &str) -> Result<String, String> {
        self.evaluate_console_with_selection(source, None)
    }

    /// The dock supplies a selection only from this navigation's Elements tree.
    pub fn evaluate_console_with_selection(
        &mut self,
        source: &str,
        selection: Option<crate::dom::NodeId>,
    ) -> Result<String, String> {
        let selection = selection.filter(|id| self.state.borrow().document.is_connected(*id));
        let value = match selection {
            Some(node) => super::wrap(&mut self.vm, &self.state, Some(node)),
            None => Ok(Value::Undefined),
        };
        let completion = value.and_then(|node| self.vm.eval_with_inspected_node(source, node));
        let result = match completion {
            Ok(value) => Ok(self.vm.inspect_value(&value)),
            Err(error) => {
                let mut label = error.kind.name().to_owned();
                if let Some(line) = error.line {
                    label.push_str(&format!(" at {line}"));
                    if let Some(column) = error.column {
                        label.push_str(&format!(":{column}"));
                    }
                }
                let (message, trimmed) = clipped(&error.message);
                let mut diagnostic =
                    format!("{label}: {message}{}", if trimmed { "…" } else { "" });
                if let Some(calls) = &error.inspected_calls {
                    for name in calls.iter() {
                        diagnostic.push_str("\n  at ");
                        diagnostic.push_str(name);
                    }
                }
                Err(diagnostic)
            }
        };
        self.drain_microtasks();
        result
    }
}
