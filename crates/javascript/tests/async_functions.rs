use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

#[test]
fn async_arrow_cover_checks_await_in_nested_parameters() {
    let mut vm = Vm::new();
    for source in [
        "async(a = await => {}) => {}",
        "async(a = (await) => {}) => {}",
        "async(a = (...await) => {}) => {}",
        "async(a = await) => {}",
        "async(a = {await}) => {}",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    for source in [
        "(await) => await",
        "async(a = function(await){return await;}) => a",
        "async(a = {m(await){return await;}}) => a",
        "async(a = {await: /x/}) => a",
        "async function f(){ return async(await 1, /x/); }",
    ] {
        vm.check_syntax(source)
            .unwrap_or_else(|error| panic!("{source}: {error}"));
    }
}

fn install_microtask(vm: &mut Vm) {
    let queue = vm.new_native_function("queueMicrotask", |vm, _, args| {
        vm.enqueue_microtask(args.first().cloned().unwrap_or_default())?;
        Ok(Value::Undefined)
    });
    vm.set_global("queueMicrotask", queue).unwrap();
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

#[test]
fn async_starts_synchronously_then_resumes_in_the_promise_job_queue() {
    let mut vm = Vm::new();
    install_microtask(&mut vm);
    vm.eval(
        r#"
        var log=[], value, thenable={};
        Object.defineProperty(thenable,'then',{get() {
            log.push('get');
            return resolve => {log.push('call');resolve(7);};
        }});
        async function task() {
            log.push('start');
            var n=await thenable;
            log.push('value'+n);
            return (await n)+1;
        }
        var p=task();
        p.then(v=>{log.push('done');value=v;});
        queueMicrotask(()=>log.push('queued'));
        log.push('end');
    "#,
    )
    .unwrap();
    let p = vm.get_global("p").unwrap();
    assert!(vm.is_promise(&p));
    assert_eq!(
        vm.eval("log.join(',')").unwrap(),
        Value::from("start,get,end")
    );
    flush(&mut vm);
    assert_eq!(
        vm.eval("log.join(',')").unwrap(),
        Value::from("start,get,end,call,queued,value7,done")
    );
    assert_eq!(vm.get_global("value").unwrap(), Value::from(8));
}

#[test]
fn await_uses_intrinsic_promise_and_rejection_flows_through_finally() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var Original=Promise, log=[], result, marker={}, gate=Promise.withResolvers();
        var promise=Promise.resolve(4);
        promise.then=()=>{throw 'overridden then';};
        Promise=function(){throw 'overridden constructor';};
        var f=async (arg)=> {
            try {
                log.push(await promise);
                throw arg;
            } catch (error) {
                log.push(error===arg);
                throw error;
            } finally {
                log.push('finally');
                await gate.promise;
                log.push('completed');
            }
        };
        var p=f(marker);
        p.catch(error=>result=error);
    "#,
    )
    .unwrap();
    flush(&mut vm);
    assert_eq!(
        vm.eval("log.join(',')").unwrap(),
        Value::from("4,true,finally")
    );
    assert_eq!(vm.get_global("result").unwrap(), Value::Undefined);
    vm.collect_garbage();
    vm.eval("gate.resolve()").unwrap();
    vm.collect_garbage();
    flush(&mut vm);
    assert_eq!(
        vm.eval("log.join(',')").unwrap(),
        Value::from("4,true,finally,completed")
    );
    assert_eq!(
        vm.eval("p instanceof Original && result===marker").unwrap(),
        Value::Bool(true)
    );
    assert!(vm.take_unhandled_rejections().is_empty());
}

#[test]
fn live_references_arguments_and_temporary_objects_survive_collection_at_await() {
    let mut vm = Vm::new();
    vm.eval(r#"
        var gate=Promise.withResolvers(), result;
        var run=async () => {
            var reads=0, target={v:7,m(a,b){return this.v+a+b;}};
            Object.defineProperty(target,'x',{ get(){reads++;return 4;}, set(value){this.v=value;} });
            target.x += await gate.promise;
            return [({v:10,m:target.m}).m(1,await gate.promise),target.v,reads].join(',');
        };
        run().then(value=>result=value);
    "#).unwrap();
    assert!(!vm.has_pending_jobs());
    vm.collect_garbage();
    vm.eval("gate.resolve(3)").unwrap();
    vm.collect_garbage();
    flush(&mut vm);
    assert_eq!(vm.get_global("result").unwrap(), Value::from("14,7,1"));
}

#[test]
fn async_loops_switch_and_iterator_close_resume_without_replaying_prefixes() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var results=[], closed=0, calls=[];
        var iterable={[Symbol.iterator](){return {
            next(){return {done:false,value:4};},
            return(){closed++;return {};}
        };}};
        async function collect() {
            for (let i=await 0; await (i<3); i=await (i+1)) {
                calls.push(()=>i);
                if (i===1) continue;
                results.push(await i);
            }
            while (await (results.length<3)) results.push('while');
            do results.push('do'); while (await false);
            for (const n of (await iterable)) {
                results.push(await n);
                break;
            }
            for (const k in await {p:1,q:2}) results.push(await k);
            switch (await 2) {
                case await 0: results.push('wrong'); break;
                default: results.push('wrong-default'); break;
                case await 2: results.push(await 'case');
                case 3: results.push('fallthrough');
            }
            return 9;
        }
        collect().then(value=>results.push(value));
    "#,
    )
    .unwrap();
    flush(&mut vm);
    assert_eq!(
        vm.eval("results.join(',')").unwrap(),
        Value::from("0,2,while,do,4,p,q,case,fallthrough,9")
    );
    assert_eq!(
        vm.eval("[closed,calls[0](),calls[1](),calls[2]()].join(',')")
            .unwrap(),
        Value::from("1,0,1,2")
    );
}

#[test]
fn parameter_errors_reject_and_async_methods_keep_this_and_super() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var fail, received, marker={};
        async function bad(a=(()=>{throw marker;})()){return 'unused';}
        bad().catch(error=>fail=error);
        class Base { value() { return this.n; } }
        class Child extends Base {
            async m(input = 3) { return (await super.value())+input; }
        }
        var child=new Child();
        child.n=8;
        child.m().then(n=>received=n);
    "#,
    )
    .unwrap();
    flush(&mut vm);
    assert_eq!(
        vm.eval("fail===marker && received===11").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(vm.eval("new bad()").unwrap_err().kind, ErrorKind::TypeError);
    assert_eq!(
        vm.eval("new (child.m.bind(child))()").unwrap_err().kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        vm.eval("Object.hasOwn(bad,'prototype')").unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn async_function_intrinsics_compile_bodies_and_preserve_subclass_prototypes() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var C=(async function(){}).constructor, answer;
        class Derived extends C {}
        var dynamic=new Derived('n','return (await n)*2;');
        dynamic(21).then(result=>answer=result);
    "#,
    )
    .unwrap();
    flush(&mut vm);
    assert_eq!(
        vm.eval("answer===42 && dynamic instanceof Derived && dynamic instanceof C && Object.getPrototypeOf(C)===Function && Object.prototype.toString.call(dynamic)==='[object AsyncFunction]'").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        vm.eval("C('a=await 1','return a')").unwrap_err().kind,
        ErrorKind::SyntaxError
    );
}

#[test]
fn unreferenced_suspended_executions_are_collected() {
    let mut vm = Vm::new();
    let before = vm.heap_cell_count();
    vm.eval("(async ()=>{var payload={k:42}; await new Promise(()=>{}); return payload;})();")
        .unwrap();
    assert!(!vm.has_pending_jobs());
    assert!(vm.heap_cell_count() > before);
    vm.collect_garbage();
    assert_eq!(vm.heap_cell_count(), before);
}

#[test]
fn async_recursion_and_repeated_jobs_respect_budgets_on_one_mib_stack() {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let mut vm = Vm::new();
            assert_eq!(
                vm.eval("async function f(){return await f();} f()")
                    .unwrap_err()
                    .kind,
                ErrorKind::ExecutionLimit
            );

            let mut vm = Vm::with_limits(VmLimits {
                max_instructions: 300,
                ..VmLimits::default()
            });
            vm.eval("async function f(){while(true){await 0;}} f();")
                .unwrap();
            let error = vm
                .with_task(|vm| {
                    while vm.has_pending_jobs() {
                        vm.drain_jobs(1)?;
                    }
                    Ok(())
                })
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::ExecutionLimit);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn await_parameter_early_errors_and_script_identifiers_are_distinct() {
    let mut vm = Vm::new();
    for source in [
        "async function f(await) {}",
        "async function f(a=await 2) {}",
        "async a => { let await=1; }",
        "async (a=await y)=>a",
        "async ()=>await 2 ** 3",
        "class A { async constructor(){} }",
        "async function f(){ class A { static { await 1; } } }",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    assert_eq!(
        vm.eval("var async=3, await=4; async+await").unwrap(),
        Value::from(7)
    );
    assert_eq!(
        vm.check_syntax("async function f(){var [x=await 1]=[];}")
            .unwrap_err()
            .kind,
        ErrorKind::UnsupportedFeature
    );
}
