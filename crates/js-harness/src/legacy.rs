//! Script-by-script adapter for the March 2016 browser runner.
//! The original website's ed.js and gs.js handle exceptions and grade each case.
use super::{Request, error_json};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm, VmLimits};
use serde_json::{Value as Json, json};
use std::{cell::RefCell, rc::Rc};

pub(super) fn execute(request: Request, limits: VmLimits) -> Json {
    let id = &request.id;
    let mut vm = Vm::with_limits(limits);
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
    let evaluation = vm.eval_with_identifier(&source, id);
    if let Err(error) = &evaluation
        && matches!(error.kind, ErrorKind::ExecutionLimit | ErrorKind::HeapLimit)
    {
        return json!({"id":id,"status":"fail","message":"Satin exhausted its resource budget before the website reported a result","evaluation_error":error_json(error),"resource_limit":true});
    }
    let mut initial_error = None;
    let result = (|| -> Result<(), JsError> {
        if let Err(error) = evaluation {
            initial_error = Some(error_json(&error));
            let thrown = match error.thrown {
                Some(ref value) => value.clone(),
                None => vm.error_value(&error)?,
            };
            let message = Value::from(format!("{}: {}", error.kind.name(), error.message));
            let handler = vm.get_global("onerror")?;
            vm.call(
                handler,
                vm.global(),
                &[
                    message,
                    Value::from(id.as_str()),
                    Value::from(error.line.unwrap_or(0)),
                    Value::from(error.column.unwrap_or(0)),
                    thrown,
                ],
            )?;
        }
        // A browser performs a microtask checkpoint when each classic script returns.
        for _ in 0..10_000 {
            if observation.borrow().is_some() || !vm.has_pending_jobs() {
                break;
            }
            vm.drain_jobs(1)?;
        }
        if observation.borrow().is_none() && !request.async_test {
            vm.eval("if (typeof $DONE === 'function') $DONE()")?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        return json!({"id":id,"status":"harness_error","error":error_json(&error),"evaluation_error":initial_error});
    }
    match observation.borrow_mut().take() {
        Some((status, message)) => {
            json!({"id":id,"status":status,"message":message,"evaluation_error":initial_error})
        }
        None => {
            json!({"id":id,"status":"fail","message":"No completion callback after the job queue drained","evaluation_error":initial_error})
        }
    }
}
