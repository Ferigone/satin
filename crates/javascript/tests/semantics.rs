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
fn expression_statements_keep_jump_targets_and_completion_values_in_their_block() {
    number("1; false || 2; let x; { 3; var v; }", 3.0);
    number("label: { 7; (null?.x ?? 8); break label; 9; }", 8.0);
    string(r#"'first'; "se\x63ond"; var v;"#, "second");
    number("var hits=0; 99; ({get p(){hits++;return 4}}).p; hits;", 1.0);
    number("var x=0; true ? (x+=2) : (x+=32); false && (x=9); x;", 2.0);
}

#[test]
fn expression_statement_suspension_keeps_its_operands_across_collection() {
    let mut vm = Vm::new();
    assert_eq!(vm.eval(r#"
        function* g() {
            let x = 0;
            100; x += (yield {n: 3}).n; x += (yield x);
            return x;
        }
        let it = g(), first = it.next().value;
        first.n;
    "#).unwrap(), Value::Number(3.0));
    vm.collect_garbage();
    assert_eq!(vm.eval("it.next(first).value").unwrap(), Value::Number(3.0));
    vm.collect_garbage();
    assert_eq!(vm.eval("it.next(7).value").unwrap(), Value::Number(10.0));
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
    vm.check_syntax("import('./example.js', {})").unwrap();
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
fn array_sort_is_stable_and_compares_utf16_strings_by_default() {
    assert_eq!(
        run(r"
            const sample = [10, 2, '3', undefined, , 1];
            const result = sample.sort();
            const text = ['\uE000', '\u{10000}', '\uD800'];
            text.sort();
            const tied = [{id:'first', rank:2}, {id:'second', rank:1}, {id:'third', rank:2}];
            tied.sort(function (a, b) { 'use strict'; return this === undefined ? a.rank - b.rank : 0; });
            const nan = [3, 1, 2];
            nan.sort(() => NaN);
            result === sample && sample.join(',') === '1,10,2,3,,' &&
                4 in sample && !(5 in sample) &&
                text[0].length === 1 && text[0].charCodeAt(0) === 55296 &&
                text[1].length === 2 && text[2].charCodeAt(0) === 57344 &&
                tied[0].id === 'second' && tied[1].id === 'first' && tied[2].id === 'third' &&
                nan.join(',') === '3,1,2'
        "),
        Value::Bool(true)
    );
}

#[test]
fn array_sort_preserves_property_order_and_uses_inherited_indices() {
    assert_eq!(
        run(r"
            const settings = Object.create({1: 'b'});
            settings.length = 5;
            settings[2] = 'a';
            settings[4] = undefined;
            const events = [];
            const entry = {
                length: 3,
                get 0() { events.push('read 0'); return 3; },
                set 0(value) { events.push('write ' + value); },
                get 1() { events.push('read 1'); return 1; },
                set 1(value) { events.push('write ' + value); },
                2: 2
            };
            const comparator = (a, b) => {
                events.push('compare');
                return {valueOf() { return a - b; }};
            };
            const sorted = Array.prototype.sort.call(settings, (a, b) => {
                if (a === undefined || b === undefined) throw Error('unexpected undefined');
                return a < b ? -1 : 1;
            });
            Array.prototype.sort.call(entry, comparator);
            sorted === settings && settings[0] === 'a' && settings[1] === 'b' &&
                2 in settings && settings[2] === undefined && !(3 in settings) && !(4 in settings) &&
                events[0] === 'read 0' && events[1] === 'read 1' && events[2] === 'compare' &&
                events[events.length - 2] === 'write 1' && events[events.length - 1] === 'write 2' &&
                entry[2] === 3
        "),
        Value::Bool(true)
    );
}

#[test]
fn array_sort_propagates_exceptions_from_comparing_writing_and_deleting() {
    assert_eq!(
        run(r"
            const values = [2, 1];
            const reason = {};
            let comparatorThrew = false;
            try { values.sort(() => { throw reason; }); }
            catch (error) { comparatorThrew = error === reason; }
            let lengthRead = false;
            const guarded = {get length() { lengthRead = true; return 1; }};
            let invalidComparator = false;
            try { Array.prototype.sort.call(guarded, null); }
            catch (error) { invalidComparator = error instanceof TypeError; }
            const locked = {length:3, 2:'last'};
            Object.defineProperty(locked, '2', {configurable:false});
            let deletionFailed = false;
            try { Array.prototype.sort.call(locked); }
            catch (error) { deletionFailed = error instanceof TypeError; }
            const readOnly = [2, 1];
            Object.defineProperty(readOnly, '0', {writable:false});
            let writeFailed = false;
            try { readOnly.sort(); }
            catch (error) { writeFailed = error instanceof TypeError; }
            comparatorThrew && values[0] === 2 && values[1] === 1 &&
                invalidComparator && !lengthRead &&
                deletionFailed && locked[0] === 'last' && locked[2] === 'last' &&
                writeFailed && readOnly[0] === 2 && readOnly[1] === 1
        "),
        Value::Bool(true)
    );
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
    vm.check_syntax("async function g(){for await ([target] of source){}}")
        .unwrap();
    for source in ["async function g(){for await ([target = await 1] of source){}}"] {
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
fn compilation_budget_charges_syntax_density_and_retains_large_literals() {
    let mut vm = Vm::new();
    let source = format!(
        "var config = {{ payload: '{}', values: [2, 4, 8] }}; config.payload.length;",
        "x".repeat(800_000)
    );
    assert_eq!(vm.eval(&source).unwrap(), Value::from(800_000));
    vm.collect_garbage();
    assert_eq!(vm.eval("config.values.join(',')").unwrap(), "2,4,8".into());

    let mut bounded = Vm::with_limits(VmLimits {
        max_heap_bytes: vm.estimated_heap_bytes() + 4 * 1024 * 1024,
        ..VmLimits::default()
    });
    // These statements are cheap to execute but expensive to represent in
    // parser/bytecode buffers. Reject them during compilation.
    let dense = ";".repeat(20_000);
    assert_eq!(
        bounded.check_syntax(&dense).unwrap_err().kind,
        ErrorKind::HeapLimit
    );
    assert_eq!(bounded.eval("1 + 2").unwrap(), Value::from(3));
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
fn error_stack_contains_the_message_without_calling_an_installed_getter() {
    let mut vm = Vm::new();
    assert_eq!(vm.eval(r#"
        function cause() { return new TypeError("invalid \uD800"); }
        const error = cause();
        const initial = error.stack;
        let gets = 0;
        Object.defineProperty(error, "message", {get(){ gets++; return "changed"; }});
        initial.startsWith("TypeError: invalid \uD800\n") &&
            initial.includes("at cause") && error.stack.startsWith("TypeError\n") && gets === 0
    "#).unwrap(), Value::Bool(true));
}
#[test]
fn error_stack_can_be_replaced_with_arbitrary_values() {
    let mut vm = Vm::new();
    assert_eq!(vm.eval(r#"
        const error = new Error("first");
        const frames = [];
        error.stack = frames;
        const first = error.stack === frames && !Object.keys(error).includes("stack");
        (new TypeError()).stack = undefined;
        error.stack = undefined;
        const own = Object.hasOwn(error, "stack") && error.stack === undefined;
        Error.prototype.stack = null;
        first && own && Object.hasOwn(Error.prototype, "stack") && Error.prototype.stack === null
    "#).unwrap(), Value::Bool(true));
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
fn interpreted_promise_handler_collects_while_preserving_stack_and_weak_targets() {
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_objects: 10_000,
        max_heap_bytes: 12 * 1024 * 1024,
        max_instructions: 8_000_000,
        ..VmLimits::default()
    });
    vm.eval(r#"
        var finished;
        function measure(input, prefix, suffix) {
            const weak = new WeakRef({state: 'live'});
            function visit(i) {
                const local = [{index: i}, () => local[0].index];
                return local[1]();
            }
            let total = 0;
            for (let i = 0; i < 16000; i++) total += visit(i);
            return this.owner + '/' + input.label + '/' + prefix + suffix +
                '/' + weak.deref().state + '/' + total;
        }
        function viaBoundFunctions(input) {
            const first = measure.bind({owner: 'bound'}, input, 'pre');
            return first.bind({owner: 'ignored'}, 'fix')();
        }
        Promise.resolve({label: 'kept'}).then(viaBoundFunctions)
            .then(result => { finished = result; });
    "#).unwrap();
    vm.drain_jobs_at_callback_boundary(8).unwrap();
    assert_eq!(vm.eval("finished").unwrap(), Value::from("bound/kept/prefix/live/127992000"));
    assert!(!vm.collect_garbage().skipped_active_execution);
}

#[test]
fn host_callback_collection_reclaims_cells_without_resetting_task_fuel() {
    let mut vm = Vm::new();
    let rooted = vm.new_object();
    let root = vm.root(rooted.clone());
    let error = vm.with_task_limits(10, None, |vm| {
        vm.charge_work(8)?;
        for _ in 0..40 {
            vm.new_object();
        }
        vm.with_task(|vm| {
            assert!(vm.collect_between_callbacks().is_none());
            Ok(())
        })?;
        let stats = vm.collect_between_callbacks().unwrap();
        assert!(stats.collected >= 40);
        assert_eq!(vm.root_value(root), Some(rooted));
        vm.charge_work(8)
    }).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
    vm.unroot(root);
}

#[test]
fn deep_ordinary_calls_preserve_returns_and_finally_on_a_small_native_stack() {
    std::thread::Builder::new().stack_size(2 * 1024 * 1024).spawn(|| {
        let mut vm = Vm::with_limits(VmLimits {
            max_bytecode_depth: 4096,
            max_call_depth: 1024,
            ..VmLimits::default()
        });
        let result = vm.eval(r#"
            let tree = null, exits = 0, fail = false;
            for (let i = 0; i < 400; ++i) tree = { child: tree };
            function visit(node) {
                try { return children(node); }
                finally { ++exits; }
            }
            function children(node) {
                if (node) return 1 + visit(node.child);
                if (fail) throw new Error("marker");
                return 0;
            }
            const value = visit(tree);
            const baseline = exits === 401;
            fail = true;
            let caught = false;
            try { visit(tree); } catch (error) { caught = error.message === "marker"; }
            baseline && value === 400 && caught && exits === 802
        "#).unwrap();
        assert_eq!(result, Value::Bool(true));
        assert!(!vm.collect_garbage().skipped_active_execution);
        assert_eq!(vm.eval("2 + 3").unwrap(), Value::from(5));
    }).unwrap().join().unwrap();
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

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn embedder_bytecode_depth_keeps_its_own_limit_and_native_stack_guard() {
    let nested = "function effect(n) { if(n) { if(true) { try { return effect(n-1); } finally {} } } return 42; }";
    for stack_bytes in [1024 * 1024, 4 * 1024 * 1024, 8 * 1024 * 1024] {
        std::thread::Builder::new()
            .stack_size(stack_bytes)
            .spawn(move || {
                let mut standard = Vm::new();
                let mut browser = Vm::with_limits(VmLimits {
                    max_bytecode_depth: 96,
                    ..VmLimits::default()
                });
                for vm in [&mut standard, &mut browser] {
                    vm.eval(nested).unwrap();
                }
                if stack_bytes == 8 * 1024 * 1024 {
                    let first_limit = (1..40)
                        .find(|n| standard.eval(&format!("effect({n})")).is_err())
                        .expect("nested blocks must exhaust the standalone depth allowance");
                    assert_eq!(
                        browser.eval(&format!("effect({first_limit})")).unwrap(),
                        Value::from(42)
                    );
                }
                for vm in [&mut standard, &mut browser] {
                    let error = vm.eval("effect(1000)").unwrap_err();
                    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
                    assert_eq!(vm.eval("effect(2)").unwrap(), Value::from(42));
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[test]
fn unbraced_statement_bodies_reject_lexical_declarations_before_execution() {
    let mut vm = Vm::new();
    for body in [
        "const x = null;",
        "let x = 1;",
        "class C {}",
        "async function f() {}",
        "function* g() {}",
    ] {
        for source in [
            format!("do {body} while (false);"),
            format!("while (false) {body}"),
            format!("if (false) {body}"),
            format!("if (true) {{}} else {body}"),
            format!("for (; false;) {body}"),
            format!("for (var item in {{}}) {body}"),
            format!("for (var item of []) {body}"),
        ] {
            assert_eq!(
                vm.eval(&source).unwrap_err().kind,
                ErrorKind::SyntaxError,
                "{source}"
            );
        }
    }
    number(
        "var n = 0; do { const x = 1; let y = 1; class C {}; n = x+y; } while(false); if(n) var z=2; n+z",
        4.0,
    );
    number(
        "var let=1; do let++; while(false); if(true) let\nlet += 2; let",
        4.0,
    );
    vm.eval("var touched = false").unwrap();
    assert_eq!(
        vm.eval("touched = true; do const x = null; while(false);")
            .unwrap_err()
            .kind,
        ErrorKind::SyntaxError
    );
    assert_eq!(vm.eval("touched").unwrap(), Value::Bool(false));
}

#[test]
fn only_sloppy_if_clauses_allow_a_direct_ordinary_function() {
    let mut vm = Vm::new();
    for source in [
        "while(false) function f() {}",
        "do function f() {} while(false);",
        "for(;false;) function f() {}",
        "'use strict'; if (false) function f() {}",
        "'use strict'; if (true) {} else function f() {}",
        "function strict(){ 'use strict'; if(true) function f(){} }",
        "if(false) label: function f() {}",
    ] {
        assert_eq!(
            vm.eval(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    assert_eq!(
        vm.check_module_syntax("if (false) function f() {}")
            .unwrap_err()
            .kind,
        ErrorKind::SyntaxError
    );
    assert!(
        vm.eval("if(false) function f() {} else function g() {}")
            .is_ok()
    );
}

#[test]
fn with_environment_tracks_live_properties_unscopables_and_suspended_frames() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
        var x=1, box={x:2, method(){return this}};
        var seen, receiver;
        with(box) { seen=()=>x; receiver=method(); }
        function* gen(){with(box){yield x; yield x}}
        var iter=gen(), first=iter.next().value;
        box[Symbol.unscopables]={x:true};
        first===2 && seen()===1 && receiver===box && iter.next().value===1
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        vm.check_syntax("'use strict'; with({}){}")
            .unwrap_err()
            .kind,
        ErrorKind::SyntaxError
    );
}

#[test]
fn block_functions_copy_the_current_lexical_value_to_the_variable_scope() {
    let mut vm = Vm::new();
    let result = vm.eval(r#"
        function probe(param) {
            let before = f;
            { function f() {}; f = 3; }
            { function param() {}; }
            try { throw null } catch (f) { { function f(){return 4} } }
            return before === undefined && f() === 4 && param === 7;
        }
        probe(7)
    "#).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn callable_source_preserves_comments_and_ignores_overridden_names() {
    let mut vm = Vm::new();
    let result = vm.eval(r#"
        const fn = ( x /* in */ ) => /pattern/u;
        Object.defineProperty(fn, "name", { get(){ throw "name getter called"; } });
        const C = class /* keep */ { static method /* keep */ () {} };
        fn.toString() === "( x /* in */ ) => /pattern/u" &&
            C.toString() === "class /* keep */ { static method /* keep */ () {} }" &&
            C.method.toString() === "method /* keep */ () {}"
    "#).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn mapped_arguments_survive_collection_and_disconnect_on_redefinition() {
    let mut vm = Vm::new();
    vm.eval("function capture(x, x) { x = 3; return [() => arguments, () => x, v => x = v]; } var api = capture(1, 2);").unwrap();
    vm.collect_garbage();
    assert_eq!(vm.eval(r#"
        const args = api[0]();
        if (Object.getPrototypeOf(args) !== Object.prototype || Array.isArray(args)) throw 1;
        if (args[0] !== 1 || args[1] !== 3) throw 2;
        args[1] = 4;
        if (api[1]() !== 4) throw 3;
        api[2](5);
        Object.defineProperty(args, "1", {writable:false});
        api[2](6);
        args[1] === 5 && Object.getOwnPropertyDescriptor(args, "1").value === 5
    "#).unwrap(), Value::Bool(true));
}

#[test]
fn deleted_eval_binding_is_recreated_by_an_already_resolved_sloppy_assignment() {
    number("function f(){eval('var q'); q = (delete q, 42); return q;} f()", 42.0);
    string("function f(){eval('var q'); return (()=>{'use strict'; try {q = evalSloppy();} catch(e){return e.name;}})(); function evalSloppy(){eval('delete q');return 42;}} f()", "ReferenceError");
}
