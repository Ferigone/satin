use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

fn expect(source: &str, expected: impl Into<Value>) {
    let actual = Vm::new()
        .eval(source)
        .unwrap_or_else(|error| panic!("{error}\n{source}"));
    assert_eq!(actual, expected.into(), "{source}");
}

#[test]
fn parameters_run_at_call_and_sent_values_resume_the_yield_expression() {
    expect(
        r#"
        var log=[];
        function* sequence(x=(log.push('parameter'),4)) {
            log.push('body');
            var sent = yield x;
            yield
            log.push('resumed');
            return sent;
        }
        var it=sequence(), first=log.join(','), a=it.next('ignored'), b=it.next(19), c=it.next(70);
        [first,log.join(','),a.value,a.done,b.value===undefined,b.done,c.value,c.done,it.next().done].join('|');
    "#,
        "parameter|parameter,body,resumed|4|false|true|false|19|true|true",
    );
    expect(
        r#"
        var mark={};
        function fail(){throw mark;}
        function* g(a=fail()){yield 1;}
        try{g();false;}catch(e){e===mark;}
    "#,
        true,
    );
}

#[test]
fn abrupt_resume_runs_pending_finally_and_catch_without_replaying_code() {
    expect(
        r#"
        var log=[];
        function* a() {
            try { let x=yield 'inside'; log.push('wrong'); }
            finally { log.push('cleanup'); yield 'cleanup'; log.push('after-cleanup'); }
        }
        function* b() {
            try { throw (yield 'ready'); }
            catch (e) { yield e; return 'caught'; }
            finally { log.push('last'); }
        }
        var x=a(), y=b();
        var one=x.next(), two=x.return(42), three=x.next();
        var four=y.next(), five=y.throw('sent'), six=y.next();
        [one.value,two.value,two.done,three.value,three.done,
         four.value,five.value,six.value,six.done,log.join(',')].join('|');
    "#,
        "inside|cleanup|false|42|true|ready|sent|caught|true|cleanup,after-cleanup,last",
    );
}

#[test]
fn expression_return_unwinds_loop_switch_and_arguments() {
    expect(
        r#"
        function* a(){try { if (yield 1) return 'wrong'; }finally{yield 2;}}
        function* b(){while(yield 1){} return 'wrong';}
        function* c(){switch(0){case yield 1:return 'wrong';}}
        function* d(){return Number(yield 1);}
        function* e(){try {throw (yield 1);} finally {return 'override';}}
        var x=a(), y=b(), z=c(), w=d(), v=e();
        x.next();y.next();z.next();w.next();v.next();
        [x.return(10).value,x.next().value,y.return(20).value,z.return(30).value,
         w.return(40).value,v.return(50).value].join(',');
    "#,
        "2,10,20,30,40,override",
    );
}

#[test]
fn closed_and_running_generator_brands_and_partial_execution() {
    expect(
        r#"
        var effects=0, it;
        function* g(){effects++;try{it.next()}catch(e){yield e instanceof TypeError;}return 8;}
        var initial=g(), early=initial.return(5);
        var thrown=g(), marker={}, same=false;
        try{thrown.throw(marker);}catch(e){same=e===marker;}
        it=g(); var a=it.next(), b=it.next();
        var bad=false;try{g.prototype.next.call(g.prototype);}catch(e){bad=e instanceof TypeError;}
        [early.value,early.done,same,thrown.next().done,a.value,b.value,effects,bad].join('|');
    "#,
        "5|true|true|true|true|8|1|true",
    );
}

#[test]
fn yield_star_caches_next_preserves_result_identity_and_forwards_abrupt_methods() {
    expect(
        r#"
        var queued={done:false,value:'first'}, returned={done:false,value:'part-return'}, calls=[];
        var counter=0;
        var inner={
            [Symbol.iterator](){return this;},
            next(v){calls.push('next',arguments.length,v); return counter++===0 ? queued : {done:true,value:'finished'};},
            return(v){calls.push('return',arguments.length,v);return returned;},
            throw(v){calls.push('throw',arguments.length,v);return {done:true,value:'handled'};}
        };
        function* g(){return 'outer:' + (yield* inner);}
        var x=g(), a=x.next('discarded')===queued;
        inner.next=function(){throw 'should use cached next';};
        var b=x.return('request')===returned, c=x.next('sent');
        inner.next=function(){return queued;}; var y=g(); y.next(); var d=y.throw('exception');
        [a,b,c.value,c.done,d.value,d.done,calls.join(',')].join('|');
    "#,
        "true|true|outer:finished|true|outer:handled|true|next,1,,return,1,request,next,1,sent,throw,1,exception",
    );
}

#[test]
fn yield_star_close_errors_and_return_value_precedence() {
    expect(
        r#"
        var marker={}, closes=0;
        var source={
            [Symbol.iterator](){return this;},next(){return {value:1,done:false};},
            return(){closes++;throw marker;}
        };
        function* g(){ try{yield*source;}catch(e){yield e===marker;} }
        var it=g(), first=it.next().value, second=it.throw('original').value;
        source.return=function(v){closes++;return {done:true,value:v+1};};
        function* h(){yield*source;return 'wrong';}
        var another=h(); another.next(); var output=another.return(5);
        var noReturn=(function*(){yield*[1,2];return 'wrong';})(); noReturn.next();
        [first,second,closes,output.value,output.done,noReturn.return(70).value].join('|');
    "#,
        "1|true|2|6|true|70",
    );
}

#[test]
fn for_of_closes_generator_and_generator_return_closes_inner_iterator() {
    expect(
        r#"
        var events=[];
        function* outer(){try{yield 1;yield 2;}finally{events.push('closed');}}
        for (let x of outer()) {events.push(x);break;}
        var source={
            [Symbol.iterator](){return this;}, next(){return {done:false,value:7};},
            return(){events.push('inner-closed');return {};}
        };
        function* inner(){for (let x of source) { yield x; }}
        var it=inner(); events.push(it.next().value,it.return(9).value);
        events.join(',');
    "#,
        "1,closed,inner-closed,7,9",
    );
}

#[test]
fn method_home_objects_and_intrinsic_function_and_iterator_prototypes() {
    expect(
        r#"
        class Base { *values(){yield this.start;} }
        class Derived extends Base {
            *values(){yield*super.values();yield new.target;}
            static *key(){yield 10;}
        }
        var o=new Derived();o.start=5;
        var literal={*["values"](){yield this.n;},n:6};
        var fn=o.values, proto=Object.getPrototypeOf(fn), it=fn.call(o);
        var iterable=it[Symbol.iterator]()===it, construct=false;
        try{new fn();}catch(e){construct=e instanceof TypeError;}
        [it.next().value,it.next().value===undefined,Derived.key().next().value,literal.values().next().value,
         iterable,construct,Object.getPrototypeOf(it)===fn.prototype,
         Object.getPrototypeOf(fn.prototype)===proto.prototype,fn.prototype.constructor===proto,
         Object.prototype.toString.call(it),Object.prototype.toString.call(fn),typeof GeneratorFunction].join('|');
    "#,
        "5|true|10|6|true|true|true|true|true|[object Generator]|[object GeneratorFunction]|undefined",
    );
}

#[test]
fn dynamic_generator_constructor_parses_formals_and_body_and_uses_global_scope() {
    expect(
        r#"
        var globalValue=3;
        var C=Object.getPrototypeOf(function*(){}).constructor;
        var make=(function(){let globalValue=9;return new C('value=2','yield value + globalValue;');})();
        var x=make();
        [C.name,C.length,Object.getPrototypeOf(C)===Function,x.next().value,x.next().done].join('|');
    "#,
        "GeneratorFunction|1|true|5|true",
    );
}

#[test]
fn yield_early_errors_preserve_sloppy_identifier_and_regexp_goal() {
    let mut vm = Vm::new();
    for source in [
        "function* fail(yield){}",
        "function* fail(x=yield 1){}",
        "(function* yield(){})",
        "function* fail(){ var yield=2; }",
        "function* fail(){ let f=(x=yield 1)=>x; }",
        "function* fail(){ yield*; }",
        "function* fail(){ 1 + yield 2; }",
        "class C{ *constructor(){} }",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    expect(
        r#"
        var yield = 4;
        function* g(){var divide=12/3; yield /[/*]/.test('/') && divide;}
        [yield,g().next().value].join(',');
    "#,
        "4,4",
    );
    expect("function* yield(){yield 4;} yield().next().value", 4);
}

#[test]
fn suspended_operands_iterators_and_pending_return_values_survive_collection() {
    let mut vm = Vm::new();
    let generator = vm
        .eval(
            r#"
        (function*(){
            var kept=(function*(){yield {x:7};return {x:8};})();
            try{var sum={n:3}.n+(yield*kept).x; return sum;}
            finally{yield 'cleanup';}
        })()
    "#,
        )
        .unwrap();
    let root = vm.root(generator.clone());
    vm.collect_garbage();
    let next = vm.get_property(generator.clone(), "next").unwrap();
    let first = vm.call(next.clone(), generator.clone(), &[]).unwrap();
    let value = vm.get_property(first, "value").unwrap();
    assert_eq!(vm.get_property(value, "x").unwrap(), Value::from(7));
    vm.collect_garbage();
    let return_method = vm.get_property(generator.clone(), "return").unwrap();
    let pending = vm.eval("({x:9})").unwrap();
    let cleanup = vm
        .call(return_method, generator.clone(), &[pending])
        .unwrap();
    assert_eq!(
        vm.get_property(cleanup, "value").unwrap(),
        Value::from("cleanup")
    );
    vm.collect_garbage();
    let complete = vm.call(next, generator, &[]).unwrap();
    let value = vm.get_property(complete, "value").unwrap();
    assert_eq!(vm.get_property(value, "x").unwrap(), Value::from(9));
    vm.unroot(root);
    assert!(vm.collect_garbage().collected > 0);
}

#[test]
fn recursive_delegation_and_long_generators_are_bounded_on_one_mib_stack() {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let mut vm = Vm::new();
            assert_eq!(
                vm.eval(
                    "function* chain(n){if(n)yield*chain(n-1);else yield 1;} chain(1000).next()"
                )
                .unwrap_err()
                .kind,
                ErrorKind::ExecutionLimit
            );
            let mut limited = Vm::with_limits(VmLimits {
                max_instructions: 500,
                ..VmLimits::default()
            });
            assert_eq!(
                limited
                    .eval("function* g(){while(true){}} g().next()")
                    .unwrap_err()
                    .kind,
                ErrorKind::ExecutionLimit
            );
            assert_eq!(
                vm.eval("function* a(){yield 4} a().next().value").unwrap(),
                Value::from(4)
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn suspended_destructuring_preserves_target_order_rest_and_assignment_value() {
    let mut vm = Vm::new();
    let first = vm.eval(r#"
        var log = [], target = {}, src = {
            get entry() { log.push("get"); return [undefined, 12, 13]; },
            other: 40
        };
        var it = (function* () {
            var result = ({
                [yield "key"]: [target[yield "target"] = yield "default", ...target.rest],
                ...target.other
            } = src);
            return result === src;
        })();
        [it.next().value, it.next("entry").value, log.join(), it.next("first").value, log.join()].join("|");
    "#).unwrap();
    assert_eq!(first, Value::from("key|target|get|default|get"));
    vm.collect_garbage();
    assert_eq!(
        vm.eval(
            r#"
        var final = it.next(86);
        [final.value, final.done, target.first, target.rest.join(","), target.other.other,
         'entry' in target.other].join("|")
    "#
        )
        .unwrap(),
        Value::from("true|true|86|12,13|40|false")
    );
}

#[test]
fn abrupt_destructuring_closes_nested_iterators_once_and_preserves_the_throw() {
    expect(
        r#"
        var log = [];
        function iterable(name, value) {
            return {
                [Symbol.iterator]() { return this; },
                next() { log.push("next-" + name); return {value: value, done: false}; },
                return() { log.push("close-" + name); throw name; }
            };
        }
        function* work() {
            var x;
            [[ x = yield 1 ]] = iterable("outer", iterable("inner", undefined));
        }
        var a = work(), b = work();
        a.next();
        try { a.throw("original"); } catch (e) { log.push(e); }
        b.next();
        try { b.return(9); } catch (e) { log.push(e); }
        log.join();
    "#,
        "next-outer,next-inner,close-inner,close-outer,original,next-outer,next-inner,close-inner,close-outer,inner",
    );
}
