use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};
fn yes(source: &str) {
    let mut vm = Vm::new();
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true), "{source}");
}

#[test]
fn computed_keys_coerce_before_value_and_infer_function_names() {
    yes(
        "var log=[],key={[Symbol.toPrimitive](hint){log.push(hint);return Symbol.for('key');}},s=Symbol('method');var o={[key]:(log.push('value'),3),[s](){return this.n;},n:7,other:function(){}};log.join(',')==='string,value' && o[Symbol.for('key')]===3 && o[s]()===7 && o[s].name==='[method]' && o.other.name==='other' && !o[s].hasOwnProperty('prototype')",
    );
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("var o={m(){}};new o.m()").unwrap_err().kind,
        ErrorKind::TypeError
    );
}
#[test]
fn computed_proto_is_data_but_colon_proto_sets_prototype() {
    yes(
        "var p={x:1},__proto__=2;var a={__proto__:p},b={['__proto__']:3},c={__proto__};Object.getPrototypeOf(a)===p && b.hasOwnProperty('__proto__') && b.__proto__===3 && c.hasOwnProperty('__proto__') && c.__proto__===2",
    );
    let mut vm = Vm::new();
    assert_eq!(
        vm.check_syntax("({__proto__:null,__proto__:{}})")
            .unwrap_err()
            .kind,
        ErrorKind::SyntaxError
    );
    assert_eq!(
        vm.check_syntax("({[x]:})").unwrap_err().kind,
        ErrorKind::SyntaxError
    );
}
#[test]
fn for_of_has_fresh_lexical_closures_tdz_and_utf16_iteration() {
    yes(
        "var f=[],out=[];for(let x of [1,2,3]){f.push(()=>x);if(x===2)continue;out.push(x);}var text=[];for(const c of 'a\\uD83D\\uDE00\\uD800')text.push(c);f[0]()===1 && f[1]()===2 && f[2]()===3 && out.join(',')==='1,3' && text.length===3 && text[2]==='\\uD800'",
    );
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("let x=[1];for(let x of x){}").unwrap_err().kind,
        ErrorKind::ReferenceError
    );
}

#[test]
fn iteration_assignment_writes_existing_names_and_reevaluates_members() {
    yes(r#"
        var config = {data: {}, assign(values) {
            var key;
            for (key in values) this.data[key] = values[key];
            return key;
        }};
        var last = config.assign({a: 7, b: 11});
        var log = [], destination = [], slot = 0;
        items: for (destination[(log.push('target'), slot++)] of (log.push('source'), [2, 3, 5])) {
            if (slot === 1) continue items;
            break items;
        }
        last === 'b' && config.data.a === 7 && config.data.b === 11 &&
        typeof key === 'undefined' && destination.join(',') === '2,3' &&
        slot === 2 && log.join(',') === 'source,target,target';
        "#);
}

#[test]
fn failing_iteration_assignment_closes_the_iterator_and_keeps_the_original_error() {
    yes(r#"
        var closed = 0, caught;
        var target = {set value(next) { throw next; }};
        var source = {[Symbol.iterator]() {
            return {
                next() { return {value: 23, done: false}; },
                return() { closed++; throw 42; }
            };
        }};
        try { for (target.value of source) throw 'unreachable'; }
        catch (error) { caught = error; }
        caught === 23 && closed === 1;
        "#);
}

#[test]
fn iteration_assignment_in_a_resumable_function_keeps_its_scope() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var result;
        async function run() {
            let key, holder = {};
            for (key in await Promise.resolve({first: 1, second: 2})) {}
            for await (holder[key] of [7, 13]) {}
            result = key + ':' + holder.second;
        }
        run();
        "#,
    )
    .unwrap();
    vm.with_task(|vm| {
        while vm.has_pending_jobs() {
            vm.drain_jobs(1)?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(vm.eval("result").unwrap(), Value::from("second:13"));
}

#[test]
fn iteration_assignment_rejects_invalid_targets() {
    let mut vm = Vm::new();
    for source in [
        "for (a + b in {}) {}",
        "for (target?.property of []) {}",
        "for (async of []) {}",
        "for (let.property of []) {}",
        "'use strict'; for (arguments in {}) {}",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
}

#[test]
fn for_of_closes_on_break_return_and_throw_but_not_continue_or_next_error() {
    yes(
        "var close=0;function source(){var i=0;return {[Symbol.iterator](){return {next(){return {value:++i,done:i>3};},return(){close++;return {};}};}};}for(const x of source()){if(x===1)continue;break;}function f(){for(let x of source())return x;}var r=f();var caught;try{for(var x of source())throw 7;}catch(e){caught=e;}close===3 && r===1 && caught===7",
    );
    yes(
        "var close=0,s={[Symbol.iterator](){return {next(){throw 4;},return(){close++;return {};}};}};try{for(let x of s){}}catch(e){}close===0",
    );
}
#[test]
fn close_throw_overrides_break_or_return_and_original_throw_wins() {
    yes(
        "var s={[Symbol.iterator](){return {next(){return {value:1,done:false};},return(){throw 9;}};}},a,b,c;try{for(let x of s)break;}catch(e){a=e;}function f(){for(let x of s)return 3;}try{f();}catch(e){b=e;}try{for(let x of s)throw 7;}catch(e){c=e;}a===9 && b===9 && c===7",
    );
}
#[test]
fn spread_uses_iterators_in_arrays_calls_and_construction() {
    yes(
        "var s={[Symbol.iterator](){var i=0;return {next(){return {value:++i,done:i>2};}};}},a=[0,...s,,3],obj={n:7,f(x,y,z){return this.n+x+y+z;}};function C(x,y){this.sum=x+y;}var c=new C(...s);a.length===5 && a[1]===1 && a[2]===2 && !(3 in a) && a[4]===3 && obj.f(...s,3)===13 && c.sum===3 && [...'\\uD83D\\uDE00'].length===1",
    );
}
#[test]
fn calls_snapshot_callee_before_argument_effects_and_spreads_preserve_order() {
    yes(
        "var log=[],o={};Object.defineProperty(o,'f',{get(){log.push('get');return function(){log.push('call');};}});function arg(){log.push('arg');return 1;}o.f(arg());var s={[Symbol.iterator](){log.push('iter');return [2,3].values();}};function f(){log.push(arguments.join(','));}f(arg(),...s,arg());log.join('|')==='get|arg|call|arg|iter|arg|1,2,3,1'",
    );
}
#[test]
fn spread_and_for_of_resource_caps_are_uncatchable_on_one_mib_stack() {
    std::thread::Builder::new().stack_size(1024*1024).spawn(||{
        for source in ["var a=[...[1,2,3]];a.length","function f(...x){}", "var s={[Symbol.iterator](){return {next(){return {value:1,done:false};}};}};for(let x of s){}", "var s={[Symbol.iterator](){return {next(){return {value:1,done:false};}};}};[...s]"] {
            let mut vm=Vm::with_limits(VmLimits{max_instructions:2000,..VmLimits::default()});let result=vm.eval(source);if source.contains("function f(..."){assert!(result.unwrap().is_undefined());}else if source.contains("done:false"){assert_eq!(result.unwrap_err().kind,ErrorKind::ExecutionLimit);}else{assert_eq!(result.unwrap(),Value::from(3));}
        }
    }).unwrap().join().unwrap();
}
