//! Script-by-script adapter for the March 2016 browser runner.
//! The original website's ed.js and gs.js handle exceptions and grade each case.
use super::{Request, error_json};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm, VmLimits};
use serde_json::{Value as Json, json};
use std::{cell::RefCell, rc::Rc};

/// The versioned profile copies document VM settings from alpha.37's
/// engine/src/script.rs. It runs isolated scripts without a browser or timer.
#[derive(Clone, Copy, serde::Deserialize)]
pub(super) enum DiagnosticProfile {
    #[serde(rename = "web-vm-alpha37")]
    WebVmAlpha37,
}

impl DiagnosticProfile {
    fn limits(self, ordinary: VmLimits) -> VmLimits {
        match self {
            Self::WebVmAlpha37 => VmLimits {
                shared_array_buffer: false,
                max_string_units: 64 * 1024 * 1024,
                max_source_bytes: 12 * 1024 * 1024,
                max_heap_objects: 512 * 1024,
                max_heap_bytes: 512 * 1024 * 1024,
                max_bytecode_depth: 96,
                buffer_units_per_instruction: 1024,
                ..ordinary
            },
        }
    }

    fn settings(self, limits: &VmLimits) -> Json {
        json!({
            "name": "web-vm-alpha37",
            "source_commit": "014e7754608bb500ba2c32b80af1094793ae2e0b",
            "max_instructions": limits.max_instructions,
            "buffer_units_per_instruction": limits.buffer_units_per_instruction,
            "max_heap_bytes": limits.max_heap_bytes,
            "max_heap_objects": limits.max_heap_objects,
            "max_call_depth": limits.max_call_depth,
            "max_bytecode_depth": limits.max_bytecode_depth,
            "max_source_bytes": limits.max_source_bytes,
            "max_string_units": limits.max_string_units,
            "max_array_length": limits.max_array_length,
            "max_pending_jobs": limits.max_pending_jobs,
            "shared_array_buffer": limits.shared_array_buffer,
        })
    }
}

pub(super) fn execute(request: Request, limits: VmLimits) -> Json {
    let vm = Vm::with_limits(match request.diagnostic_profile {
        Some(profile) => profile.limits(limits),
        None => limits,
    });
    let settings = request.diagnostic_profile.map(|profile| profile.settings(vm.limits()));
    let mut result = execute_in_vm(request, vm);
    if let Some(settings) = settings {
        result["diagnostic_profile"] = settings;
    }
    result
}

fn execute_in_vm(request: Request, mut vm: Vm) -> Json {
    let id = &request.id;
    let observation = Rc::new(RefCell::new(None));
    let report = observation.clone();
    let record = vm.new_native_function("testRun", move |vm, _, args| {
        if args.len() != 6 {
            return Err(JsError::type_error("Invalid testRun callback"));
        }
        let result = vm.to_string(&args[4])?.to_string_lossy();
        let error = if result == "fail" {
            Some(vm.to_string(&args[5])?.to_string_lossy())
        } else {
            None
        };
        *report.borrow_mut() = Some((result, error));
        Ok(Value::Undefined)
    });
    let finish = vm.new_native_function("testFinished", |_, _, _| Ok(Value::Undefined));
    let global = vm.global();
    for (name, value) in [
        ("window", global),
        ("testRun", record),
        ("testFinished", finish),
    ] {
        if let Err(e) = vm.set_global(name, value) {
            return json!({"id":id,"status":"harness_error","error":error_json(&e)});
        }
    }
    for script in request.setup {
        if let Err(e) = vm.eval_with_identifier(&script.source, &script.name) {
            return json!({"id":id,"status":"harness_error","script":script.name,"error":error_json(&e)});
        }
    }
    let source = if request.strict {
        format!("'use strict';\n{}", request.source)
    } else {
        request.source
    };
    let mut evaluation_error = None;
    let mut phase = "script";
    let run = vm.with_task(|vm| {
        if let Err(error) = vm.eval_with_identifier(&source, id) {
            evaluation_error = Some(error_json(&error));
            report_uncaught(vm, error)?;
        }
        phase = "microtask";
        for _ in 0..10_000 {
            if observation.borrow().is_some() || !vm.has_pending_jobs() {
                break;
            }
            if let Err(error) = vm.drain_jobs(1) {
                evaluation_error = Some(error_json(&error));
                report_uncaught(vm, error)?;
            }
        }
        if observation.borrow().is_none() && vm.has_pending_jobs() {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Legacy microtask checkpoint exceeds 10000 jobs",
            ));
        }
        if observation.borrow().is_none() && !request.async_test {
            phase = "completion";
            vm.eval("if (typeof $DONE === 'function') $DONE()")?;
        }
        Ok(())
    });
    if let Err(error) = run {
        let resource = matches!(error.kind, ErrorKind::ExecutionLimit | ErrorKind::HeapLimit);
        let execution_stop = resource || error.kind == ErrorKind::UnsupportedFeature;
        let detail = error_json(&error);
        return json!({
            "id":id, "status": if execution_stop {"fail"} else {"harness_error"},
            "verdict_source":"vm", "phase":phase, "resource_limit":resource,
            "error":detail, "evaluation_error":evaluation_error.unwrap_or(detail),
            "message":"The VM stopped before the website returned a verdict",
        });
    }
    match observation.borrow_mut().take() {
        Some((status, message)) => json!({"id":id,"status":status,"message":message,
            "verdict_source":"website-callback","evaluation_error":evaluation_error}),
        None => json!({"id":id,"status":"fail","verdict_source":"vm",
            "message":"No website completion callback after the job queue drained","evaluation_error":evaluation_error}),
    }
}

fn report_uncaught(vm: &mut Vm, error: JsError) -> Result<(), JsError> {
    if matches!(
        error.kind,
        ErrorKind::ExecutionLimit
            | ErrorKind::HeapLimit
            | ErrorKind::UnsupportedFeature
            | ErrorKind::InternalError
    ) {
        return Err(error);
    }
    let message = if error.kind == ErrorKind::Thrown {
        format!("Uncaught {}", error.message)
    } else {
        format!("Uncaught {}: {}", error.kind.name(), error.message)
    };
    let thrown = match error.thrown {
        Some(ref value) => value.clone(),
        None => vm.error_value(&error)?,
    };
    let handler = vm.get_global("onerror")?;
    vm.call(
        handler,
        vm.global(),
        &[
            message.into(),
            "about:blank".into(),
            error.line.unwrap_or(0).into(),
            error.column.unwrap_or(0).into(),
            thrown,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observe(source: &str, asynchronous: bool) -> Json {
        let request = serde_json::from_value(json!({
            "id":"case.js", "source":source, "async_test":asynchronous,
            "setup":[{"name":"callbacks", "source":"
                function $DONE(error) {
                    testRun('case','case.js','','',error ? 'fail' : 'pass',error);
                    testFinished();
                }
                window.onerror = function(message, url, line, col, error) { $DONE(error); };
            "}]
        }))
        .unwrap();
        execute(request, VmLimits::default())
    }

    #[test]
    fn async_completion_needs_a_real_callback() {
        let completed = observe("Promise.resolve().then(function(){ $DONE() });", true);
        assert_eq!(completed["status"], "pass");
        assert_eq!(completed["verdict_source"], "website-callback");

        let silent = observe("Promise.resolve(1);", true);
        assert_eq!(silent["status"], "fail");
        assert_eq!(silent["verdict_source"], "vm");

        let thrown = observe("throw new TypeError('err');", true);
        assert_eq!(thrown["status"], "fail");
        assert_eq!(thrown["verdict_source"], "website-callback");
        assert_eq!(thrown["message"], "TypeError: err");
    }

    #[test]
    fn exhausted_job_queue_budget_cannot_synthesize_a_passing_sync_callback() {
        let result = observe(
            "function spin(){ Promise.resolve().then(spin); } spin();",
            false,
        );
        assert_eq!(result["status"], "fail");
        assert_eq!(result["verdict_source"], "vm");
        assert_eq!(result["phase"], "microtask");
        assert_eq!(result["resource_limit"], true);
    }
}
