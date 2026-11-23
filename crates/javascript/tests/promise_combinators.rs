use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};
fn run(source: &str, check: &str) {
    let mut vm = Vm::new();
    vm.eval(source).unwrap();
    vm.with_task(|vm| {
        while vm.has_pending_jobs() {
            vm.drain_jobs(1)?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(vm.eval(check).unwrap(), Value::Bool(true), "{check}");
}

#[test]
fn all_preserves_input_order_and_empty_is_asynchronous() {
    run(
        "var log=[],r;Promise.all([]).then(()=>log.push('empty'));Promise.all([Promise.resolve(1),2,new Promise(resolve=>resolve(3))]).then(x=>r=x);log.push('sync');",
        "r.join(',')==='1,2,3' && log.join(',')==='sync,empty'",
    );
}
#[test]
fn all_rejects_and_race_selects_first_settled_in_fifo() {
    run(
        "var a,b,c='pending';Promise.all([1,Promise.reject(7),3]).catch(x=>a=x);Promise.race([Promise.resolve(4),Promise.reject(5)]).then(x=>b=x);Promise.race([]).then(()=>c='bad');",
        "a===7 && b===4 && c==='pending'",
    );
}
#[test]
fn all_settled_reports_each_outcome_and_any_uses_authentic_aggregate_error() {
    run(
        "var a,b,c,d;Promise.allSettled([Promise.resolve(1),Promise.reject(2)]).then(x=>a=x);Promise.any([Promise.reject(3),Promise.resolve(4)]).then(x=>b=x);Promise.any([Promise.reject('a'),Promise.reject('b')]).catch(x=>c=x);Promise.any([]).catch(x=>d=x);",
        "a[0].status==='fulfilled' && a[0].value===1 && a[1].status==='rejected' && a[1].reason===2 && b===4 && c instanceof AggregateError && c instanceof Error && c.errors.join(',')==='a,b' && Object.getOwnPropertyDescriptor(c,'errors').enumerable===false && d.errors.length===0",
    );
}
#[test]
fn combinators_use_custom_iterators_and_cache_constructor_resolve_once() {
    run(
        "var gets=0,calls=0,i=0,result,source={};source[Symbol.iterator]=function(){return {next:function(){i++;return {value:i,done:i>3};}};};function C(executor){executor(function(x){result=x;},function(e){throw e;});}Object.defineProperty(C,'resolve',{get:function(){gets++;return function(x){calls++;return {then:function(ok){ok(x*2);ok(999);}};};}});Promise.all.call(C,source);",
        "gets===1 && calls===3 && result.join(',')==='2,4,6'",
    );
}
#[test]
fn all_settled_shares_once_flag_and_any_rejections_are_once_only() {
    run(
        "var a,b;function C(e){e(function(x){a=x;},function(x){b=x;});}C.resolve=function(x){return {then:function(ok,bad){ok(x);bad(99);ok(100);}};};Promise.allSettled.call(C,[1,2]);",
        "a.length===2 && a[0].status==='fulfilled' && a[1].value===2 && b===undefined",
    );
    run(
        "var result;function C(e){e(function(){},function(x){result=x;});}C.resolve=function(x){return {then:function(ok,bad){bad(x);bad(99);}};};Promise.any.call(C,[1,2]);",
        "result instanceof AggregateError && result.errors.join(',')==='1,2'",
    );
}
#[test]
fn iterator_close_obeys_throw_precedence_and_next_errors_do_not_close() {
    run(
        "var closed=0,a,b,source={};source[Symbol.iterator]=function(){return {next:function(){return {value:1,done:false};},return:function(){closed++;throw 9;}};};function C(e){return new Promise(e);}C.resolve=function(){throw 7;};Promise.all.call(C,source).catch(x=>a=x);var source2={};source2[Symbol.iterator]=function(){return {next:function(){throw 8;},return:function(){closed++;return {};}};};Promise.race(source2).catch(x=>b=x);",
        "closed===1 && a===7 && b===8",
    );
}
#[test]
fn species_is_observed_for_then_and_finally() {
    run(
        "var constructions=0,a,b;function C(e){constructions++;return new Promise(e);}var p=Promise.resolve(1),ctor={};ctor[Symbol.species]=C;p.constructor=ctor;p.then(x=>a=x);p.finally(()=>2).then(x=>b=x);",
        "a===1 && b===1 && constructions>=2 && Promise[Symbol.species]===Promise && Object.prototype.toString.call(Promise.resolve())==='[object Promise]'",
    );
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            "var p=Promise.resolve(1),c={};c[Symbol.species]=()=>1;p.constructor=c;p.then(()=>1)"
        )
        .unwrap_err()
        .kind,
        ErrorKind::TypeError
    );
}
#[test]
fn aggregate_error_constructor_observes_message_cause_then_iteration() {
    let mut vm = Vm::new();
    assert_eq!(vm.eval("var log=[],source={},message={toString:function(){log.push('message');return 'bad';}},options={};Object.defineProperty(options,'cause',{get:function(){log.push('cause');return 3;}});source[Symbol.iterator]=function(){log.push('iterator');return [1,2].values();};var e=AggregateError(source,message,options);e instanceof AggregateError && e.message==='bad' && e.cause===3 && e.errors.join(',')==='1,2' && log.join(',')==='message,cause,iterator'").unwrap(),Value::Bool(true));
}
#[test]
fn combinator_reactions_trace_aggregate_and_iterator_targets_after_gc() {
    let mut vm = Vm::new();
    vm.eval(
        "var resolve,result;var p=new Promise(r=>resolve=r);Promise.all([p,2]).then(x=>result=x);",
    )
    .unwrap();
    vm.collect_garbage();
    vm.eval("resolve(1)").unwrap();
    vm.collect_garbage();
    vm.drain_jobs(100).unwrap();
    assert_eq!(vm.eval("result.join(',')").unwrap(), Value::from("1,2"));
}
#[test]
fn infinite_combinator_iteration_returns_uncatchable_limit() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 2000,
        ..VmLimits::default()
    });
    assert_eq!(vm.eval("var source={};source[Symbol.iterator]=function(){return {next:function(){return {value:1,done:false};}};};Promise.all(source)").unwrap_err().kind,ErrorKind::ExecutionLimit);
}

#[test]
fn promise_try_uses_capability_and_assimilates_or_rejects_callback_result() {
    run(
        "var a,b,c,now=0;Promise.try(function(x,y){'use strict';now=this===undefined?x+y:99;return Promise.resolve(now);},2,3).then(x=>a=x);Promise.try(function(){throw 7;}).catch(x=>b=x);Promise.try(undefined).catch(x=>c=x);",
        "now===5 && a===5 && b===7 && c instanceof TypeError",
    );
    run(
        "var p=Promise.resolve(1),same=Promise.try(()=>p)===p,order=[];function C(e){order.push('construct');return new Promise(e);}Promise.try.call(C,()=>{order.push('callback');return 2;});",
        "same && order.join(',')==='callback,construct'",
    );
}

#[test]
fn aggregate_records_respect_small_heap_allowance() {
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: 512 * 1024,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("Promise.all(Array.from({length:1000},function(x,i){return i;}))")
            .unwrap_err()
            .kind,
        ErrorKind::HeapLimit
    );
}
