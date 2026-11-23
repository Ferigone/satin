use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};
fn yes(source: &str) {
    let mut vm = Vm::new();
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true), "{source}");
}
#[test]
fn array_object_nested_bindings_defaults_and_rest() {
    yes(
        "let [a,,b=3,...rest]=[1,2,undefined,4,5];const {x:y=7,nested:{z},...copy}={nested:{z:9},keep:10};a===1&&b===3&&rest.join(',')==='4,5'&&y===7&&z===9&&copy.keep===10&&!copy.hasOwnProperty('nested')",
    );
    yes("var let=1;var object={let};let+=1;for(let;let<3;let++){}object.let===1&&let===3");
    yes("var yield=4,x;[x=yield]=[];var {yield:y=yield}={};x===4&&y===4");
    yes(
        "var {['__proto__']:p,__proto__:q}={['__proto__']:5};var [first,...[second,third]]=[1,2,3];p===5&&q===5&&first===1&&second===2&&third===3",
    );
}
#[test]
fn assignment_patterns_return_rhs_use_setters_and_symbol_rest_keys() {
    yes(
        "var a,b,o={},s=Symbol(),input={a:1,b:2,[s]:3};var result=({a,...o.rest}=input);[o.x,b=7]=[4,undefined];result===input&&a===1&&o.x===4&&b===7&&o.rest.b===2&&o.rest[s]===3&&!o.rest.hasOwnProperty('a')",
    );
    yes(
        "var log=[],o={},source={[Symbol.iterator](){return {next(){log.push('next');return {value:1,done:false};},return(){log.push('close');return {};}};}};function lhs(){log.push('lhs');return o;}[lhs().x]=source;log.join(',')==='lhs,next,close'&&o.x===1",
    );
}
#[test]
fn elisions_do_not_read_values_and_normal_or_abrupt_completion_closes() {
    yes(
        "var log=[],iter={next(){log.push('next');return {done:false};},return(){log.push('return');return {};}};Object.defineProperty(iter,'unused',{value:0});var source={[Symbol.iterator](){return iter;}};const []=source;const [,]=source;log.join(',')==='return,next,return'",
    );
    yes(
        "var closed=0,source={[Symbol.iterator](){return {next(){return {done:false,value:undefined};},return(){closed++;throw 9;}};}},caught;try{let [x=(()=>{throw 7;})()]=source;}catch(e){caught=e;}closed===1&&caught===7",
    );
    yes(
        "var valueGets=0,s={[Symbol.iterator](){return {next(){var r={done:false};Object.defineProperty(r,'value',{get(){valueGets++;throw 5;}});return r;},return(){return {};}};}};let [,]=s;valueGets===0",
    );
}
#[test]
fn object_keys_and_assignment_references_are_evaluated_before_gets() {
    yes(
        "var log=[],target={},source={};Object.defineProperty(source,'x',{get(){log.push('get');return 2;}});function key(){log.push('key');return 'x';}function lhs(){log.push('lhs');return target;}({[key()]:lhs().x}=source);log.join(',')==='key,lhs,get'&&target.x===2",
    );
    yes(
        "var called=0;function key(){called++;return 'x';}try{let {[key()]:x}=null;}catch(e){}called===0",
    );
}
#[test]
fn default_and_rest_parameters_have_lengths_arguments_and_distinct_environments() {
    yes(
        "function f(a=2,b=a+3,...r){return [a,b,r.join(','),arguments.length];}var a=f(undefined,undefined,8,9);f.length===0&&a.join('|')==='2|5|8,9|4'&&((x,...r)=>r.length).length===1",
    );
    yes(
        "var x='outer';function f(a=()=>x,b=2){var x='body';var b=3;return [a(),b];}f().join(',')==='outer,3'",
    );
    yes("function f(a=()=>b,b=2){var b=3;return a();}f()===2");
    yes("function f({x=3}={},[a,...r]=[4,5]){return x+a+r[0];}f()===12");
    yes("var f=({x=2}={},...r)=>x+r.length;f(undefined,3,4)===4");
    yes(
        "function f({a}){eval('var a');return a;}function g(...a){eval('var a');return a.length;}f({a:3})===3&&g(1,2)===2",
    );
    yes(
        "function f(){let arguments=3;return arguments;}function g(...r){function arguments(){return 4;}return arguments();}f()===3&&g()===4",
    );
    yes("var ref=function f(a=()=>f){f=1;return a()===f;};ref()===true");
}
#[test]
fn parameter_tdz_and_body_hoists_are_not_visible_to_defaults() {
    for source in [
        "function f(a=a){}f()",
        "function f(a=b,b=1){}f()",
        "function f(a=g()){function g(){return 1;}}f()",
    ] {
        let mut vm = Vm::new();
        assert_eq!(
            vm.eval(source).unwrap_err().kind,
            ErrorKind::ReferenceError,
            "{source}"
        );
    }
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("function f(a=eval('var a=42')){}f()")
            .unwrap_err()
            .kind,
        ErrorKind::SyntaxError
    );
    yes("function f(a=eval('var z=42')){return z;}f()===42");
    yes(
        "function f(a=()=>{}){return a.name;}var {g=()=>{}}={};let [h=function(){}]=[];f()==='a'&&g.name==='g'&&h.name==='h'",
    );
}
#[test]
fn loop_and_catch_patterns_have_lexical_bindings_and_close_iterators() {
    yes(
        "var n=0;for(let [a=('x' in {})]=[];n<1;n++){if(a!==false)throw 1;}for(const {x:a='x' in {}} of [{}]){if(a!==false)throw 2;}n===1",
    );
    yes(
        "var n=0;for(var a=[0 in []],f=(x='p' in {})=>{return 'q' in {};};n<1;n++){if(a[0]!==false||f()!==false)throw 1;}n===1",
    );
    yes(
        "var closures=[];for(const [key,value=3] of [['a',1],['b']]){closures.push(()=>key+value);}var caught;try{throw {message:'ok'};}catch({message:m}){caught=m;}closures[0]()==='a1'&&closures[1]()==='b3'&&caught==='ok'",
    );
    yes(
        "var finished=false;try{try{try{throw null;}catch({x}){}}finally{finished=true;}}catch(error){}finished",
    );
}
#[test]
fn early_errors_reject_invalid_patterns_rest_duplicates_and_strict_directives() {
    let mut vm = Vm::new();
    for source in [
        "let [a,a]=[];",
        "for(let [a,a] of []){}",
        "for(let [a,a]=[];;){}",
        "for(let {a}=obj;;){var a;}",
        "for(const [a] of []){var a;}",
        "for(let {a} in {}){var a;}",
        "'use strict';var yield=1;",
        "'use strict';function f([yield]){}",
        "'use strict';typeof yield;",
        "try{}catch({x,x}){}",
        "try{}catch({x}){let x;}",
        "function f(a=1,a){}",
        "function f(...a,b){}",
        "function f(...a=[]){}",
        "function f([o.x]=[]){}",
        "function f(a=1){'use strict';}",
        "({...a,}=obj)",
        "[...a,]=obj;",
        "var [...[a]=[]]=[];",
        "a??b||c",
        "a&&b??c",
        "({0})",
        "({'x'})",
        "var {0}=obj;",
        "let let=1;",
        "const {let}=obj;",
        "for(let [let] of []){}",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    assert_eq!(
        vm.eval("Function('a=1','\"use strict\";')")
            .unwrap_err()
            .kind,
        ErrorKind::SyntaxError
    );
}
#[test]
fn patterns_are_bounded_and_gc_keeps_default_closures_and_rest_values() {
    let mut vm = Vm::new();
    vm.eval("function f(a=()=>b,b={x:3}){return a;}var kept=f();")
        .unwrap();
    vm.collect_garbage();
    assert_eq!(vm.eval("kept().x").unwrap(), Value::from(3));
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1000,
        ..VmLimits::default()
    });
    assert_eq!(vm.eval("var s={[Symbol.iterator](){return {next(){return {done:false,value:1};}};}};var [...rest]=s;").unwrap_err().kind,ErrorKind::ExecutionLimit);
}
