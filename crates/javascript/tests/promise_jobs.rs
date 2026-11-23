use rust_browser_javascript::{ErrorKind, JsError, Value, Vm, VmLimits};
use std::{cell::Cell, rc::Rc};

fn vm() -> Vm {
    let mut vm = Vm::new();
    let callback = vm.new_native_function("queueMicrotask", |vm, _, args| {
        vm.enqueue_microtask(args.first().cloned().unwrap_or_default())?;
        Ok(Value::Undefined)
    });
    vm.set_global("queueMicrotask", callback).unwrap();
    vm
}
fn flush(vm: &mut Vm) {
    vm.with_task(|vm| {
        while vm.has_pending_jobs() {
            vm.drain_jobs(1)?;
        }
        Ok(())
    })
    .unwrap();
}
fn text(vm: &mut Vm, source: &str, expected: &str) {
    assert_eq!(vm.eval(source).unwrap(), Value::from(expected));
}

#[test]
fn reactions_are_async_and_share_fifo_with_microtasks() {
    let mut vm = vm();
    vm.eval("var log=[];Promise.resolve(1).then(v=>log.push('p'+v)).then(()=>log.push('chain'));queueMicrotask(()=>log.push('q'));log.push('sync');").unwrap();
    text(&mut vm, "log.join(',')", "sync");
    flush(&mut vm);
    text(&mut vm, "log.join(',')", "sync,p1,q,chain");
}
#[test]
fn executor_is_synchronous_and_only_first_settlement_wins() {
    let mut vm = vm();
    vm.eval("var log=[];var p=new Promise((resolve,reject)=>{log.push('executor');resolve(3);reject(4);throw 5;});p.then(v=>log.push(v),e=>log.push('bad'));log.push('end');").unwrap();
    flush(&mut vm);
    text(&mut vm, "log.join(',')", "executor,end,3");
}
#[test]
fn executor_exceptions_reject_authentic_errors() {
    let mut vm = vm();
    vm.eval("var result;new Promise(()=>{throw new TypeError('bad');}).catch(e=>{result=e instanceof TypeError && e.message==='bad';});").unwrap();
    flush(&mut vm);
    assert_eq!(vm.get_global("result").unwrap(), Value::Bool(true));
}
#[test]
fn thenable_getter_runs_now_and_call_runs_as_a_job() {
    let mut vm = vm();
    vm.eval("var log=[];var t={};Object.defineProperty(t,'then',{get:function(){log.push('get');return function(resolve,reject){log.push('call');resolve(9);reject(1);};}});Promise.resolve(t).then(v=>log.push(v));queueMicrotask(()=>log.push('q'));log.push('sync');").unwrap();
    flush(&mut vm);
    text(&mut vm, "log.join(',')", "get,sync,call,q,9");
}
#[test]
fn throwing_then_getter_rejects_without_throwing_from_resolve() {
    let mut vm = vm();
    vm.eval("var result;var t={};Object.defineProperty(t,'then',{get:()=>{throw 17;}});Promise.resolve(t).catch(e=>result=e);").unwrap();
    flush(&mut vm);
    assert_eq!(vm.get_global("result").unwrap(), Value::from(17));
}
#[test]
fn chained_handlers_propagate_values_and_exceptions() {
    let mut vm = vm();
    vm.eval("var result;Promise.resolve(2).then(v=>v+3).then(()=>{throw 'rejected';}).then(42).catch(e=>e+'!').then(v=>result=v);").unwrap();
    flush(&mut vm);
    text(&mut vm, "result", "rejected!");
}
#[test]
fn promise_self_resolution_and_chain_cycles_reject() {
    let mut vm = vm();
    vm.eval("var resolve;var p=new Promise(r=>resolve=r);var a;resolve(p);p.catch(e=>a=e instanceof TypeError);var q=Promise.resolve().then(()=>q);var b;q.catch(e=>b=e instanceof TypeError);").unwrap();
    flush(&mut vm);
    assert_eq!(vm.eval("a&&b").unwrap(), Value::Bool(true));
}
#[test]
fn native_promise_resolution_adopts_pending_and_fulfilled_promises() {
    let mut vm = vm();
    vm.eval("var resolve;var inner=new Promise(r=>resolve=r);var result;new Promise(r=>r(inner)).then(v=>result=v);resolve(8);").unwrap();
    flush(&mut vm);
    assert_eq!(vm.get_global("result").unwrap(), Value::from(8));
    assert_eq!(
        vm.eval("Promise.resolve(inner)===inner").unwrap(),
        Value::Bool(true)
    );
}
#[test]
fn finally_waits_and_preserves_or_overrides_completion() {
    let mut vm = vm();
    vm.eval("var log=[];Promise.resolve(7).finally(()=>{log.push('finally');return Promise.resolve(4);}).then(v=>log.push(v));Promise.reject('old').finally(()=>{}).catch(e=>log.push(e));Promise.resolve(1).finally(()=>{throw 'new';}).catch(e=>log.push(e));").unwrap();
    flush(&mut vm);
    let result=vm.eval("log.indexOf('finally')>=0 && log.indexOf(7)>=0 && log.indexOf('old')>=0 && log.indexOf('new')>=0").unwrap();
    assert_eq!(result, Value::Bool(true));
}
#[test]
fn promise_method_receivers_and_constructor_are_checked() {
    let mut vm = vm();
    for source in [
        "Promise(()=>{})",
        "new Promise(3)",
        "Promise.prototype.then.call({})",
        "Promise.resolve.call(1,2)",
        "Promise.reject.call(()=>{},2)",
    ] {
        assert_eq!(
            vm.eval(source).unwrap_err().kind,
            ErrorKind::TypeError,
            "{source}"
        );
    }
}
#[test]
fn generic_capability_constructor_and_with_resolvers_work() {
    let mut vm = vm();
    vm.eval("function C(executor){var self=this;executor(v=>self.value=v,e=>self.error=e);}var a=Promise.resolve.call(C,7);var b=Promise.reject.call(C,8);var cap=Promise.withResolvers();var result;cap.promise.then(v=>result=v);cap.resolve(11);").unwrap();
    flush(&mut vm);
    assert_eq!(
        vm.eval("a.value===7 && b.error===8 && result===11")
            .unwrap(),
        Value::Bool(true)
    );
}
#[test]
fn reaction_jobs_and_resolving_handles_are_gc_roots() {
    let mut vm = vm();
    let capability = vm.new_promise_capability().unwrap();
    let resolve = vm.root(capability.resolve);
    vm.set_global("p", capability.promise).unwrap();
    vm.eval("var result;p.then(v=>result=v);").unwrap();
    vm.set_global("p", Value::Null).unwrap();
    vm.collect_garbage();
    vm.call(
        vm.root_value(resolve).unwrap(),
        Value::Undefined,
        &[Value::from("kept")],
    )
    .unwrap();
    vm.unroot(resolve);
    vm.collect_garbage();
    flush(&mut vm);
    text(&mut vm, "result", "kept");
}
#[test]
fn pending_reactions_trace_native_handlers_and_are_collectable() {
    let mut vm = vm();
    let marker = Rc::new(Cell::new(0));
    let weak = Rc::downgrade(&marker);
    let f = vm.new_native_function("handler", move |_, _, _| {
        marker.set(marker.get() + 1);
        Ok(Value::Undefined)
    });
    vm.set_global("handler", f).unwrap();
    vm.eval("var p=new Promise(()=>{});p.then(handler);")
        .unwrap();
    vm.set_global("handler", Value::Null).unwrap();
    vm.collect_garbage();
    assert!(weak.upgrade().is_some());
    vm.set_global("p", Value::Null).unwrap();
    vm.collect_garbage();
    assert!(weak.upgrade().is_none());
}
#[test]
fn throwing_microtask_is_consumed_and_later_jobs_remain() {
    let mut vm = vm();
    vm.eval("var n=0;queueMicrotask(()=>{throw 5;});queueMicrotask(()=>n++);")
        .unwrap();
    assert_eq!(vm.drain_jobs(1).unwrap_err().kind, ErrorKind::Thrown);
    assert_eq!(vm.pending_job_count(), 1);
    vm.drain_jobs(1).unwrap();
    assert_eq!(vm.get_global("n").unwrap(), Value::from(1));
}
#[test]
fn pending_job_limits_and_clear_are_bounded() {
    let mut vm = Vm::with_limits(VmLimits {
        max_pending_jobs: 2,
        ..VmLimits::default()
    });
    let callback = vm.new_native_function("noop", |_, _, _| Ok(Value::Undefined));
    vm.enqueue_microtask(callback.clone()).unwrap();
    vm.enqueue_microtask(callback.clone()).unwrap();
    assert_eq!(
        vm.enqueue_microtask(callback).unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(vm.clear_jobs(), 2);
    assert!(!vm.has_pending_jobs());
}
#[test]
fn drain_job_count_and_outer_instruction_budget_are_enforced() {
    let mut vm = vm();
    vm.eval("var n=0;function again(){n++;queueMicrotask(again);}queueMicrotask(again);")
        .unwrap();
    assert_eq!(vm.drain_jobs(3).unwrap(), 3);
    assert_eq!(vm.get_global("n").unwrap(), Value::from(3));
    assert_eq!(vm.pending_job_count(), 1);
    assert_eq!(vm.clear_jobs(), 1);
}
#[test]
fn promises_respect_small_aggregate_budgets() {
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: 512 * 1024,
        ..VmLimits::default()
    });
    let error = vm
        .eval("var p=new Promise(()=>{});for(var i=0;i<2000;i++){p.then(()=>{});}")
        .unwrap_err();
    assert!(matches!(
        error.kind,
        ErrorKind::HeapLimit | ErrorKind::ExecutionLimit
    ));
    assert!(vm.estimated_heap_bytes() < 1024 * 1024);
}
#[test]
fn microtasks_preserve_undefined_this_for_strict_callbacks() {
    let mut vm = vm();
    vm.eval("var ok=false;queueMicrotask(function(){'use strict';ok=this===undefined;});")
        .unwrap();
    flush(&mut vm);
    assert_eq!(vm.get_global("ok").unwrap(), Value::Bool(true));
}
#[test]
fn native_constructor_and_error_helpers_are_real() {
    let mut vm = vm();
    let constructor = vm.new_native_constructor("Host", |vm, _, _, constructing| {
        if !constructing {
            return Err(JsError::type_error("Host requires new"));
        }
        Ok(vm.new_object())
    });
    vm.set_global("Host", constructor).unwrap();
    assert_eq!(vm.eval("Host()").unwrap_err().kind, ErrorKind::TypeError);
    assert!(vm.eval("new Host()").unwrap().as_object().is_some());
    let error = vm.error_value(&JsError::type_error("network")).unwrap();
    vm.set_global("error", error).unwrap();
    assert_eq!(
        vm.eval("error instanceof TypeError").unwrap(),
        Value::Bool(true)
    );
}
#[test]
fn queued_calls_and_intrinsic_json_jobs_trace_every_argument() {
    let mut vm = vm();
    let cap = vm.new_promise_capability().unwrap();
    vm.set_global("p", cap.promise).unwrap();
    vm.eval("var result;p.then(v=>result=v.n);").unwrap();
    vm.enqueue_json_parse(cap.resolve, cap.reject, "{\"n\":12}".into())
        .unwrap();
    vm.collect_garbage();
    flush(&mut vm);
    assert_eq!(vm.get_global("result").unwrap(), Value::from(12));
    let callback = vm.eval("(function(v){this.value=v;})").unwrap();
    let receiver = vm.new_object();
    vm.set_global("receiver", receiver.clone()).unwrap();
    vm.enqueue_call(callback, receiver, &[Value::from("traced")])
        .unwrap();
    vm.collect_garbage();
    flush(&mut vm);
    text(&mut vm, "receiver.value", "traced");
}
#[test]
fn host_slots_trace_wrappers_without_visible_properties_or_leaked_roots() {
    let mut vm = vm();
    let parent = vm.new_object();
    let child = vm.new_object();
    vm.set_property(child.clone(), "value", Value::from(7))
        .unwrap();
    vm.set_host_slot(parent.clone(), 0, child).unwrap();
    let root = vm.root(parent.clone());
    vm.collect_garbage();
    let child = vm.host_slot(&parent, 0).unwrap();
    assert_eq!(vm.get_property(child, "value").unwrap(), Value::from(7));
    vm.set_global("parent", parent.clone()).unwrap();
    assert_eq!(
        vm.eval("Object.keys(parent).length").unwrap(),
        Value::from(0)
    );
    vm.set_global("parent", Value::Null).unwrap();
    vm.unroot(root);
    vm.collect_garbage();
    assert!(vm.host_slot(&parent, 0).is_none());
}
#[test]
fn unhandled_rejections_are_traced_until_checkpoint_and_removed_when_handled() {
    let mut vm = vm();
    vm.eval("Promise.reject(1).catch(()=>{});Promise.reject(2).then();")
        .unwrap();
    flush(&mut vm);
    vm.collect_garbage();
    assert_eq!(vm.take_unhandled_rejections(), vec![Value::from(2)]);
    assert!(vm.take_unhandled_rejections().is_empty());
}
#[test]
fn late_assertion_rejections_keep_trusted_prototype_identity() {
    let mut vm = vm();
    let prototype = vm
        .eval("function Assertion(){};Assertion.prototype")
        .unwrap();
    let root = vm.root(prototype.clone());
    vm.eval("(function(error){Promise.resolve().then(()=>{throw error;});})(new Assertion());Assertion=null;")
        .unwrap();
    flush(&mut vm);
    vm.collect_garbage();
    let reasons = vm.take_unhandled_rejections();
    assert_eq!(reasons.len(), 1);
    assert!(vm.has_prototype(&reasons[0], &prototype).unwrap());
    vm.unroot(root);
}
#[test]
fn unhandled_records_have_a_separate_bounded_host_checkpoint() {
    let mut vm = Vm::with_limits(VmLimits {
        max_pending_jobs: 2,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("Promise.reject(1);Promise.reject(2);Promise.reject(3);")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(
        vm.take_unhandled_rejections(),
        vec![Value::from(1), Value::from(2)]
    );
    vm.collect_garbage();
    assert_eq!(vm.eval("1+2").unwrap(), Value::from(3));
}
#[test]
fn builtin_function_property_order_matches_create_builtin_function() {
    let mut vm = vm();
    text(
        &mut vm,
        "Object.getOwnPropertyNames(Promise.resolve).join(',')",
        "length,name",
    );
    vm.eval(
        "var keys;new Promise(resolve=>{keys=Object.getOwnPropertyNames(resolve).join(',');});",
    )
    .unwrap();
    text(&mut vm, "keys", "length,name");
    text(
        &mut vm,
        "Object.getOwnPropertyNames(function f(){}).slice(0,2).join(',')",
        "length,name",
    );
}
