use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

fn checkpoint(vm: &mut Vm) {
    vm.with_task(|vm| {
        while vm.has_pending_jobs() {
            vm.drain_jobs(1)?;
        }
        Ok(())
    })
    .unwrap();
}

#[test]
fn async_generator_requests_are_fifo_and_await_before_yield() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var seen=[], gate=Promise.withResolvers();
        async function* g(a=(seen.push('parameter'), 1)) {
            seen.push('entered');
            await gate.promise;
            try {
                var sent=yield Promise.resolve(a);
                yield sent;
            } finally { yield 'cleanup'; }
        }
        var it=g();
        function capture(label,p){p.then(r=>seen.push(label,r.value,r.done));}
        capture('a',it.next(99));
        capture('b',it.next(11));
        capture('c',it.return(Promise.resolve(17)));
        capture('d',it.next());
        capture('e',it.next());
    "#,
    )
    .unwrap();
    assert_eq!(
        vm.eval("seen.join('|')").unwrap(),
        Value::from("parameter|entered")
    );
    vm.collect_garbage();
    vm.eval("gate.resolve()").unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("seen.join('|')").unwrap(),
        Value::from("parameter|entered|a|1|false|b|11|false|c|cleanup|false|d|17|true|e||true")
    );
}

#[test]
fn async_return_value_rejection_resumes_the_generator_with_a_throw() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var log=[], failure={};
        async function* g(){
            try { yield 'ready'; }
            catch(e){yield e===failure; return 7;}
        }
        var it=g();
        it.next().then(r=>log.push(r.value));
        it.return(Promise.reject(failure)).then(r=>log.push(r.value,r.done));
        it.next().then(r=>log.push(r.value,r.done));
        it.throw(failure).catch(e=>log.push(e===failure));
    "#,
    )
    .unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("log.join('|')").unwrap(),
        Value::from("ready|true|false|7|true|true")
    );
    assert!(vm.take_unhandled_rejections().is_empty());
}

#[test]
fn async_generator_awaited_return_statement_can_be_caught() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var values=[];
        async function* g(){
            try { return Promise.reject('caught'); }
            catch (e) { yield e; }
            return Promise.resolve('success');
        }
        var it=g();
        it.next().then(r=>values.push(r.value,r.done));
        it.next().then(r=>values.push(r.value,r.done));
    "#,
    )
    .unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("values.join('|')").unwrap(),
        Value::from("caught|false|success|true")
    );
}

#[test]
fn async_generator_closed_requests_wait_for_return_and_preserve_prototypes() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var gate=Promise.withResolvers(), calls=[];
        var generator={
            n:4,
            async *items() { yield this.n; }
        };
        var fn=generator.items, C=Object.getPrototypeOf(fn).constructor;
        var it=generator.items(), dynamic=C('yield Promise.resolve(8);')();
        var done=it.return(gate.promise).then(r=>calls.push('return',r.value,r.done));
        it.next().then(r=>calls.push('next',r.done));
        dynamic.next().then(r=>calls.push('dynamic',r.value));
        fn.prototype.next.call({}).catch(e=>calls.push(e instanceof TypeError));
        calls.push(typeof it[Symbol.iterator],it[Symbol.asyncIterator]()===it);
    "#,
    )
    .unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("calls.join('|')").unwrap(),
        Value::from("undefined|true|true|dynamic|8")
    );
    vm.eval("gate.resolve(5)").unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("calls.join('|')").unwrap(),
        Value::from("undefined|true|true|dynamic|8|return|5|true|next|true")
    );
}

#[test]
fn async_generator_request_and_heap_resources_are_bounded() {
    let mut vm = Vm::with_limits(VmLimits {
        max_pending_jobs: 3,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval(
            r#"
        async function* g(){await new Promise(()=>{});}
        var it=g(); it.next();it.next();it.next();it.next();
    "#
        )
        .unwrap_err()
        .kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(vm.eval("1+2").unwrap(), Value::from(3));
}

#[test]
fn promised_next_keeps_a_suspended_async_generator_alive() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var gate=Promise.withResolvers(), result;
        ((async function* (){
            let saved={value:42};
            await gate.promise;
            yield saved;
        })()).next().then(r=>result=r.value);
    "#,
    )
    .unwrap();
    vm.collect_garbage();
    vm.eval("gate.resolve()").unwrap();
    vm.collect_garbage();
    checkpoint(&mut vm);
    assert_eq!(vm.eval("result.value").unwrap(), Value::from(42));
}

#[test]
fn for_await_uses_async_iterator_and_yields_control_during_close() {
    let mut vm = Vm::new();
    vm.eval(r#"
        var gate=Promise.withResolvers(), log=[], i=0;
        var input={
            [Symbol.iterator](){throw 'wrong iterator';},
            [Symbol.asyncIterator](){log.push('iterator');return this;},
            next(){log.push('next',arguments.length);return Promise.resolve({value:++i,done:false});},
            return(){log.push('closing',arguments.length);return gate.promise;}
        };
        async function consume(){
            for await (const value of input) {
                log.push(value);
                if (value === 1) continue;
                break;
            }
            log.push('after');
        }
        consume();
    "#).unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("log.join('|')").unwrap(),
        Value::from("iterator|next|0|1|next|0|2|closing|0")
    );
    vm.collect_garbage();
    vm.eval("gate.resolve({})").unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("log.join('|')").unwrap(),
        Value::from("iterator|next|0|1|next|0|2|closing|0|after")
    );
}

#[test]
fn for_await_sync_adapter_awaits_values_and_closes_when_value_rejects() {
    let mut vm = Vm::new();
    vm.eval(r#"
        var log=[], failure={};
        var source={
            count:0,
            [Symbol.iterator](){return this;},
            next(){return {done:false,value:++this.count===1 ? Promise.resolve(9) : Promise.reject(failure)};},
            return(){log.push('close');throw 'ignored';}
        };
        (async function(){
            try{for await (const x of source){log.push(x);}}
            catch(e){log.push(e===failure);}
        })();
    "#).unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("log.join('|')").unwrap(),
        Value::from("9|close|true")
    );
    assert!(vm.take_unhandled_rejections().is_empty());
}

#[test]
fn async_iterator_close_preserves_body_throw_and_awaits_a_return_override() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var log=[], body={}, close={};
        var source={
            [Symbol.asyncIterator](){return this;},
            next(){return {done:false,value:1};},
            return(){return Promise.reject(close);}
        };
        (async function(){
            try{for await (const x of source){throw body;}}
            catch(e){log.push(e===body);}
            try{for await (const x of source){break;}}
            catch(e){log.push(e===close);}
        })();
    "#,
    )
    .unwrap();
    checkpoint(&mut vm);
    assert_eq!(vm.eval("log.join('|')").unwrap(), Value::from("true|true"));
}

#[test]
fn async_yield_star_forwards_cached_next_return_and_throw() {
    let mut vm = Vm::new();
    vm.eval(r#"
        var log=[], sends=0, r={value:'first',done:false};
        var source={
            [Symbol.asyncIterator](){return this;},
            next(v){log.push('next',arguments.length,v);return Promise.resolve(sends++===0 ? r : {value:'done',done:true});},
            return(v){log.push('return',v);return {value:'partial',done:false};},
            throw(v){log.push('throw',v);return {value:'handled',done:true};}
        };
        async function* g(){return 'out:'+(yield*source);}
        var outer=g();
        outer.next('ignored').then(result=>{
            log.push(result===r,result.value);
            source.next=function(){throw 'uncached';};
            return outer.return(Promise.resolve(6));
        }).then(result=>{
            log.push(result.value,result.done);
            return outer.next(7);
        }).then(result=>{
            log.push(result.value,result.done);
            source.next=function(){return r;};
            var another=g();
            return another.next().then(()=>another.throw(8));
        }).then(result=>log.push(result.value,result.done));
    "#).unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("log.join('|')").unwrap(),
        Value::from(
            "next|1||false|first|return|6|partial|false|next|1|7|out:done|true|throw|8|out:handled|true"
        )
    );
}

#[test]
fn async_yield_star_missing_throw_awaits_close_and_sync_fallback() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var gate=Promise.withResolvers(), log=[];
        var source={
            [Symbol.asyncIterator](){return this;},
            next(){return {done:false,value:1};},
            return(){log.push('closing');return gate.promise;}
        };
        async function* g(){yield*source;}
        var it=g();
        it.next().then(r=>{log.push(r.value);return it.throw('sent');})
            .catch(e=>log.push(e instanceof TypeError));
        var fromSync=(async function*(){yield* [Promise.resolve(2)];})();
        fromSync.next().then(r=>log.push(r.value));
    "#,
    )
    .unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("log.join('|')").unwrap(),
        Value::from("1|closing|2")
    );
    vm.eval("gate.resolve({done:true})").unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("log.join('|')").unwrap(),
        Value::from("1|closing|2|true")
    );
}

#[test]
fn module_top_level_for_await_suspends_and_updates_its_live_binding() {
    let mut vm = Vm::new();
    let module = vm
        .parse_module(
            "export let total=0; for await (const next of [Promise.resolve(2), 3]) total += next;",
            "module-with-for-await",
        )
        .unwrap();
    let root = vm.root_module(module).unwrap();
    vm.link_module(module, |_, _, _| unreachable!()).unwrap();
    vm.evaluate_module(module).unwrap();
    assert!(vm.module_evaluation_result(module).unwrap().is_none());
    vm.collect_garbage();
    checkpoint(&mut vm);
    assert!(
        vm.module_evaluation_result(module)
            .unwrap()
            .unwrap()
            .is_ok()
    );
    let namespace = vm.module_namespace(module).unwrap();
    assert_eq!(vm.get_property(namespace, "total").unwrap(), Value::from(5));
    vm.unroot(root);
}

#[test]
fn for_await_body_uses_single_statement_lookahead() {
    let mut vm = Vm::new();
    vm.check_syntax("async function* f(){for await(var x of []) let\n{} }")
        .unwrap();
    for source in [
        "async function* f(){for await(var x of []) let\n[a] = 0;}",
        "async function f(){for await(var x of []) const y=1;}",
        "async function f(){for await(var x of []) class C{} }",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
}
