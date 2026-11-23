use rust_browser_javascript::{ErrorKind, JsError, JsString, Value, Vm, VmLimits};
use std::{cell::RefCell, rc::Rc};

fn run(source: &str) -> Value {
    Vm::new()
        .eval(source)
        .unwrap_or_else(|e| panic!("{e}\n{source}"))
}
fn number(source: &str, expected: f64) {
    assert_eq!(run(source), Value::Number(expected));
}
fn string(source: &str, expected: &str) {
    assert_eq!(run(source), Value::from(expected));
}

#[test]
fn arithmetic_and_precedence() {
    number("1 + 2 * 3 ** 2", 19.0);
    number("(1 + 2) * 3", 9.0);
    number("4294967295 >>> 0", 4294967295.0);
    number("-1 >> 1", -1.0);
}
#[test]
fn utf16_lone_surrogates_round_trip() {
    let value = run(r"'\uD800x\uDC00'");
    assert_eq!(
        value.as_string().unwrap().as_utf16(),
        &[0xd800, 120, 0xdc00]
    );
    number(r"'\u{1F600}'.length", 2.0);
    number(r"'\ud800'.charCodeAt(0)", 55296.0);
    let value = run(r"'\uD800'.toUpperCase().slice(0,1)");
    assert_eq!(value.as_string().unwrap().as_utf16(), &[0xd800]);
}
#[test]
fn surrogate_property_names_remain_distinct() {
    number(r"var o={}; o['\ud800']=3; o['\ufffd']=7; o['\ud800']", 3.0);
}
#[test]
fn lexical_closures_are_independent() {
    number(
        "function counter(n){return function(){n++;return n;};} var a=counter(0);var b=counter(10);a()+a()+b()",
        14.0,
    );
}
#[test]
fn hoisting_and_block_scope() {
    string(
        "var result=typeof x;{var x=2;let y=3;}result+':'+x+':'+typeof y",
        "undefined:2:undefined",
    );
}
#[test]
fn temporal_dead_zone_and_const() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("{x;let x=1;}").unwrap_err().kind,
        ErrorKind::ReferenceError
    );
    assert_eq!(
        vm.eval("const c=1;c=2;").unwrap_err().kind,
        ErrorKind::TypeError
    );
}
#[test]
fn early_errors_without_execution() {
    let mut vm = Vm::new();
    vm.check_syntax("throw new Error('not executed')").unwrap();
    assert_eq!(
        vm.check_syntax("let x; let x;").unwrap_err().kind,
        ErrorKind::SyntaxError
    );
    assert_eq!(
        vm.check_syntax("return 1").unwrap_err().kind,
        ErrorKind::SyntaxError
    );
    assert_eq!(
        vm.check_syntax("import('./example.js', {})")
            .unwrap_err()
            .kind,
        ErrorKind::UnsupportedFeature
    );
}
#[test]
fn per_iteration_closure_environments() {
    number(
        "var f=[];for(let i=0;i<4;i++){f.push(function(){return i;});} f[0]()+f[1]()+f[2]()+f[3]()",
        6.0,
    );
}
#[test]
fn loops_and_control_completion() {
    number(
        "var n=0;for(var i=0;i<10;i++){if(i===2)continue;if(i===6)break;n+=i;}while(n<20){n++;}do{n--;}while(n>18);n",
        18.0,
    );
}
#[test]
fn this_and_arrow_capture() {
    number(
        "var o={n:7,method:function(){var f=()=>this.n;return f.call({n:99});}};o.method()",
        7.0,
    );
    assert_eq!(
        run("function f(){'use strict';return this;} f()"),
        Value::Undefined
    );
}
#[test]
fn references_are_evaluated_once() {
    number(
        "var calls=0;var o={x:2};function get(){calls++;return o;}get().x+=3;calls*10+o.x",
        15.0,
    );
}
#[test]
fn coercion_and_short_circuit() {
    string(
        "var o={valueOf:function(){return 9;}};o+':'+(0||4)+':'+(null??5)",
        "9:4:5",
    );
    number("var n=0;false&&(n=1);true||(n=2);n", 0.0);
    assert_eq!(
        run("null==undefined && '3'==3 && '3'!==3"),
        Value::Bool(true)
    );
}
#[test]
fn constructors_and_prototypes() {
    number(
        "function Point(x){this.x=x;}Point.prototype.get=function(){return this.x;};var p=new Point(8);p instanceof Point ? p.get() : 0",
        8.0,
    );
}
#[test]
fn named_function_expression_recursion() {
    number(
        "var f=function factorial(n){return n<=1?1:n*factorial(n-1);};f(6)",
        720.0,
    );
    assert_eq!(run("typeof factorial"), Value::from("undefined"));
}
#[test]
fn accessors_and_attributes() {
    number(
        "var o={};var n=1;Object.defineProperty(o,'x',{get:function(){return n;},set:function(v){n=v;},enumerable:true});o.x=9;o.x",
        9.0,
    );
    assert_eq!(
        run("var o={};Object.defineProperty(o,'x',{value:1});o.x=2;delete o.x;o.x===1"),
        Value::Bool(true)
    );
}
#[test]
fn sparse_arrays_and_length() {
    string(
        "var a=[1,,3];a.length=5;a[4]=9;a.map(function(x){return x*2;}).join('|')",
        "2||6||18",
    );
    number("var a=[1,2,3];a.length=1;a.push(7);a.pop()+a.length", 8.0);
}
#[test]
fn array_callbacks_and_bindings() {
    number(
        "[1,2,3,4].filter(x=>x%2===0).map(x=>x*3).reduce((a,b)=>a+b,0)",
        18.0,
    );
    number(
        "function add(a,b){return this.n+a+b;}var f=add.bind({n:5},3);f.call({n:99},2)",
        10.0,
    );
}
#[test]
fn enumerable_key_order() {
    string(
        "var o={z:0,2:2,1:1,a:0};Object.keys(o).join(',')",
        "1,2,z,a",
    );
    string(
        "var o=Object.create({inherited:1});o.own=2;var keys=[];for(var key in o){keys.push(key);}keys.join(',')",
        "own,inherited",
    );
}
#[test]
fn try_catch_finally_return() {
    number(
        "function f(){try{throw 7;}catch(e){return e+1;}finally{var x=2;}}f()",
        8.0,
    );
    number("function f(){try{return 1;}finally{return 3;}}f()", 3.0);
}
#[test]
fn built_in_error_identity_cannot_be_forged() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("throw new TypeError('bad')").unwrap_err().kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        vm.eval("throw {name:'TypeError'}").unwrap_err().kind,
        ErrorKind::Thrown
    );
    assert_eq!(
        run("try{null.x;}catch(e){e instanceof Error && e.name==='TypeError';}"),
        Value::Bool(true)
    );
}
#[test]
fn switch_fallthrough() {
    number(
        "var x=0;switch(2){case 1:x=1;break;case 2:x=3;default:x+=4;}x",
        7.0,
    );
}
#[test]
fn native_dom_style_accessors_and_explicit_this() {
    let mut vm = Vm::new();
    let text = Rc::new(RefCell::new(String::new()));
    let read = text.clone();
    let get = vm.new_native_function("get textContent", move |_, _, _| {
        Ok(Value::from(read.borrow().clone()))
    });
    let write = text.clone();
    let set = vm.new_native_function("set textContent", move |vm, _, a| {
        *write.borrow_mut() = vm.to_string(&a[0])?.to_string_lossy();
        Ok(Value::Undefined)
    });
    let proto = vm.new_object();
    vm.define_accessor(proto.clone(), "textContent", Some(get), Some(set))
        .unwrap();
    let node = vm.new_object_with_prototype(proto).unwrap();
    vm.set_global("node", node.clone()).unwrap();
    vm.eval("node.textContent='ready '+(2+3)").unwrap();
    assert_eq!(&*text.borrow(), "ready 5");
    let callback = vm.eval("(function(){this.textContent+='!';})").unwrap();
    let root = vm.root(callback);
    vm.collect_garbage();
    vm.call(vm.root_value(root).unwrap(), node, &[]).unwrap();
    assert_eq!(&*text.borrow(), "ready 5!");
}
#[test]
fn tracing_gc_keeps_closures_cycles_and_accessors() {
    let mut vm = Vm::new();
    let callback = vm.eval("(function(){let n=40;return ()=>++n;})()").unwrap();
    let root = vm.root(callback);
    for _ in 0..10 {
        vm.eval("({cycle:null})").unwrap();
    }
    let stats = vm.collect_garbage();
    assert!(stats.collected > 0);
    let callback = vm.root_value(root).unwrap();
    assert_eq!(
        vm.call(callback, Value::Undefined, &[]).unwrap(),
        Value::Number(41.0)
    );
    vm.unroot(root);
    let before = vm.heap_cell_count();
    assert!(vm.collect_garbage().after < before);
}
#[test]
fn swept_native_closures_release_host_data() {
    let mut vm = Vm::new();
    let marker = Rc::new(());
    let weak = Rc::downgrade(&marker);
    let callback = vm.new_native_function("host", move |_, _, _| {
        let _ = &marker;
        Ok(Value::Undefined)
    });
    assert!(weak.upgrade().is_some());
    drop(callback);
    vm.collect_garbage();
    assert!(weak.upgrade().is_none());
}
#[test]
fn stale_handles_are_generation_checked() {
    let mut vm = Vm::new();
    let object = vm.new_object();
    vm.collect_garbage();
    let _replacement = vm.new_object();
    assert_eq!(
        vm.get_property(object, "x").unwrap_err().kind,
        ErrorKind::TypeError
    );
}
#[test]
fn host_callback_gc_is_deferred() {
    let mut vm = Vm::new();
    let f = vm.new_native_function("collect", |vm, _, _| {
        assert!(vm.collect_garbage().skipped_active_execution);
        Ok(Value::from(3))
    });
    vm.set_global("collect", f).unwrap();
    number("1+2", 3.0);
    assert_eq!(vm.eval("collect()+4").unwrap(), Value::Number(7.0));
}
#[test]
fn loops_and_recursion_have_uncatchable_budgets() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 2000,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("try{for(;;){}}catch(e){42;}").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(
        vm.eval("function recurse(){return recurse();}recurse();")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(vm.eval("1+2").unwrap(), Value::from(3));
}
#[test]
fn native_errors_propagate() {
    let mut vm = Vm::new();
    let f = vm.new_native_function("bad", |_, _, _| Err(JsError::type_error("host failure")));
    vm.set_global("bad", f).unwrap();
    assert_eq!(
        vm.eval("try{bad();}catch(e){e.name;}").unwrap(),
        Value::from("TypeError")
    );
}
#[test]
fn host_string_constructor_preserves_units() {
    let value = Value::from(JsString::from_utf16(vec![0xdfff]));
    assert_eq!(value.as_string().unwrap().as_utf16(), &[0xdfff]);
}
#[test]
fn direct_and_indirect_eval_scopes() {
    number("function f(){let x=2;return eval('x+3');}f()", 5.0);
    number("function f(){eval('var x=7');return x;}f()", 7.0);
    string(
        "function f(){'use strict';eval('var x=7');return typeof x;}f()",
        "undefined",
    );
    string("(0,eval)('let hidden=1');typeof hidden", "undefined");
    assert_eq!(
        run(
            "function f(){let x=1;try{eval('var x=2');}catch(e){return e.constructor===SyntaxError;}}f()"
        ),
        Value::Bool(true)
    );
}
#[test]
fn dynamic_function_uses_global_environment() {
    number(
        "var x=3;function f(){let x=20;return Function('a','return a+x;')(4);}f()",
        7.0,
    );
}
#[test]
fn intrinsic_error_constructor_identity() {
    assert_eq!(
        run(
            "Object.getPrototypeOf(TypeError)===Error && Object.getPrototypeOf(RangeError)===Error"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        run("try{null.x;}catch(e){e.constructor===TypeError;}"),
        Value::Bool(true)
    );
    assert_eq!(
        run("try{missingName;}catch(e){e.constructor===ReferenceError;}"),
        Value::Bool(true)
    );
}
#[test]
fn delete_reinsert_preserves_key_order() {
    string(
        "var o={a:1,b:2};delete o.a;o.a=3;Object.keys(o).join(',')",
        "b,a",
    );
}
#[test]
fn number_exponent_format() {
    string(
        "String(1e21)+','+String(1e-7)+','+String(-0)",
        "1e+21,1e-7,0",
    );
}
#[test]
fn nested_tail_call_shapes_return_limits_without_host_stack_overflow() {
    let mut vm = Vm::new();
    let error = vm
        .eval("'use strict';(function f(n){if(n===0){return;}if(true){return f(n-1);}}(100000));")
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
    let source = format!(
        "function f(){{{}return f();{}}}f();",
        "if(true){".repeat(20),
        "}".repeat(20)
    );
    assert_eq!(
        vm.eval(&source).unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(vm.eval("1+2").unwrap(), Value::from(3));
}
#[test]
fn recursive_bytecode_is_bounded_on_windows_default_main_stack() {
    std::thread::Builder::new().stack_size(1024*1024).spawn(|| {
        let mut vm=Vm::new();
        for source in ["'use strict';(function f(n){if(n===0){return;}if(true){return f(n-1);}}(100000));", "'use strict';(function f(n){if(n===0){return;}if(false){}else{return f(n-1);}}(100000));"] {
            assert_eq!(vm.eval(source).unwrap_err().kind,ErrorKind::ExecutionLimit);
        }
    }).unwrap().join().unwrap();
}
#[test]
fn throwing_an_object_does_not_coerce_it() {
    number(
        "var count=0;var error={toString:function(){count++;return 'bad';}};try{throw error;}catch(caught){if(caught!==error){count+=100;}}count",
        0.0,
    );
}
#[test]
fn boxed_primitive_constructors() {
    assert_eq!(
        run(
            "typeof new Number(7)==='object' && (new Number(7)).valueOf()===7 && (new String('abc')).length===3 && (new Boolean(false)).valueOf()===false"
        ),
        Value::Bool(true)
    );
}
#[test]
fn bound_functions_can_be_constructed() {
    number(
        "function Point(x){this.x=x;}var Bound=Point.bind({x:99},7);var p=new Bound();p instanceof Bound && p instanceof Point ? p.x : 0",
        7.0,
    );
}
#[test]
fn dynamic_function_parameters_cannot_escape_into_body() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("Function('a) { return 1; } function injected(', '')")
            .unwrap_err()
            .kind,
        ErrorKind::SyntaxError
    );
}
#[test]
fn switch_has_one_lexical_scope() {
    assert_eq!(
        run(
            "try{switch(0){case 0:x;break;case 1:let x=2;}}catch(e){e.constructor===ReferenceError;}"
        ),
        Value::Bool(true)
    );
}
#[test]
fn unsupported_grammar_cannot_pass_negative_syntax_tests() {
    let mut vm = Vm::new();
    vm.check_syntax("async function g(){for await (target of source){}}")
        .unwrap();
    for source in [
        "async function g(){for await ([target] of source){}}",
        "for([x='x' in {}] of [[]]){}",
        "import('./module.js', {})",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::UnsupportedFeature,
            "{source}"
        );
    }
    for source in [
        "function f(a=){}",
        "function* g(",
        "async function* g(",
        "for await (const x of xs){}",
        "x ||= ;",
        "({x:1,...})",
        "const {x};",
        "[...x,]=a;",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    assert_eq!(
        vm.eval("var factorial=function f(n){return n<=1?1:n*f(n-1);};factorial(6)")
            .unwrap(),
        Value::from(720)
    );
}
#[test]
fn aggregate_retained_strings_stop_at_a_small_byte_budget() {
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: 1024 * 1024,
        ..VmLimits::default()
    });
    let error = vm
        .eval("var s='x'.repeat(1024);var kept=[];for(var i=0;i<2000;i++){kept.push(s+i);}")
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::HeapLimit);
    assert!(vm.estimated_heap_bytes() <= 1024 * 1024 + 1024);
    vm.set_global("kept", Value::Null).unwrap();
    vm.set_global("s", Value::Null).unwrap();
    let before = vm.estimated_heap_bytes();
    vm.collect_garbage();
    assert!(vm.estimated_heap_bytes() < before);
    assert_eq!(vm.eval("2+3").unwrap(), Value::from(5));
}
#[test]
fn one_object_cannot_bypass_the_aggregate_property_budget() {
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: 512 * 1024,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("var o={};for(var i=0;i<10000;i++){o['p'+i]=i;}")
            .unwrap_err()
            .kind,
        ErrorKind::HeapLimit
    );
}
#[test]
fn native_builders_check_before_large_temporary_allocations() {
    for source in [
        "'x'.repeat(8192).split('')",
        "Object.keys('x'.repeat(8192))",
        "(function(){}).apply(null,{length:100000})",
        "var s='x'.repeat(1024);Function.apply(null,[s,s,s,s,s,s,s,s,s,s,s,s,s,s,s,s,s,s,s,s,'']);",
    ] {
        let mut vm = Vm::with_limits(VmLimits {
            max_heap_bytes: 512 * 1024,
            ..VmLimits::default()
        });
        let error = vm.eval(source).unwrap_err();
        assert!(
            matches!(error.kind, ErrorKind::HeapLimit | ErrorKind::ExecutionLimit),
            "{source}: {error}"
        );
        assert!(vm.estimated_heap_bytes() < 1024 * 1024);
    }
}
#[test]
fn replacement_expansion_checks_each_append() {
    let mut vm = Vm::with_limits(VmLimits {
        max_string_units: 2048,
        max_heap_bytes: 1024 * 1024,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("'x'.repeat(128).replace('x',\"$'\".repeat(128))")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
}
#[test]
fn native_search_comparisons_consume_the_task_budget() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 3000,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("var a='a'.repeat(512);a.indexOf('a'.repeat(256)+'b')")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(
        vm.eval("var a='a'.repeat(512);a.lastIndexOf('a'.repeat(256)+'b')")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
}
#[test]
fn task_scope_shares_one_budget_across_multiple_calls() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1000,
        ..VmLimits::default()
    });
    let source = "var n=0;while(n<50){n++;}";
    for _ in 0..4 {
        vm.eval(source).unwrap();
    }
    let error = vm
        .with_task(|vm| {
            for _ in 0..4 {
                vm.eval(source)?;
            }
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
    assert!(!vm.collect_garbage().skipped_active_execution);
    assert_eq!(vm.eval("1+2").unwrap(), Value::from(3));
}
#[cfg(windows)]
#[test]
fn nested_execution_uses_available_windows_stack_and_recovers_at_limits() {
    for stack_size in [1024 * 1024, 4 * 1024 * 1024] {
        std::thread::Builder::new()
            .stack_size(stack_size)
            .spawn(move || {
                let mut vm = Vm::new();
                vm.eval("function down(n) { return n === 0 ? 42 : down(n - 1); }")
                    .unwrap();
                if stack_size > 1024 * 1024 {
                    assert_eq!(vm.eval("down(22)").unwrap(), Value::Number(42.));
                }
                let error = vm.eval("down(10000)").unwrap_err();
                assert_eq!(
                    error.kind,
                    rust_browser_javascript::ErrorKind::ExecutionLimit
                );
                assert_eq!(vm.eval("down(3)").unwrap(), Value::Number(42.));
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
