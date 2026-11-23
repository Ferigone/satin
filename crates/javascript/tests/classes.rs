use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};
fn yes(source: &str) {
    let mut vm = Vm::new();
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true), "{source}");
}
#[test]
fn class_declarations_expressions_methods_and_descriptors() {
    yes(
        "class A {constructor(x){this.x=x;} get value(){return this.x;} set value(x){this.x=x;} method(y){return this.x+y;} static make(x){return new this(x);}}var a=A.make(3);a.value=4;a.method(2)===6&&a instanceof A&&Object.getPrototypeOf(a)===A.prototype&&!Object.getOwnPropertyDescriptor(A.prototype,'method').enumerable&&A.name==='A'",
    );
    yes(
        "var C=class Named {method(){return Named;}};var a=new C();a.method()===C&&typeof Named==='undefined'&&C.name==='Named'&&C.length===0",
    );
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("class A{};A()").unwrap_err().kind,
        ErrorKind::TypeError
    );
}
#[test]
fn derived_constructors_and_super_references_use_actual_receiver() {
    yes(
        "class A {constructor(x){this.x=x;} method(y){return this.x+y;} get value(){return this.x;} set value(v){this.x=v;} static f(){return this.flag;}}class B extends A {constructor(x){super(x+1);} method(y){super.value=super.value+1;return super.method(y)+1;} static f(){return super.f()+1;}}B.flag=3;var b=new B(2);b.method(4)===9&&b.x===4&&B.f()===4&&b instanceof A&&b instanceof B&&Object.getPrototypeOf(B)===A",
    );
    yes("class A{constructor(x){this.x=x;}}class B extends A{}new B(7).x===7&&B.length===0");
    yes(
        "class A extends null{constructor(){return {x:3};}}new A().x===3&&Object.getPrototypeOf(A.prototype)===null",
    );
}
#[test]
fn derived_this_tdz_return_and_duplicate_super_are_real() {
    for source in [
        "class A{}class B extends A{constructor(){this.x=1;super();}}new B()",
        "class A{}class B extends A{constructor(){}}new B()",
        "class A{}class B extends A{constructor(){super();super();}}new B()",
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
        vm.eval("class A{}class B extends A{constructor(){return 3;}}new B()")
            .unwrap_err()
            .kind,
        ErrorKind::TypeError
    );
    yes(
        "class A{}class B extends A{constructor(){var f=()=>this;super();return f();}}new B() instanceof B",
    );
}
#[test]
fn fields_and_static_blocks_observe_keys_order_brands_and_strictness() {
    yes(
        "var order=[];class A {[(order.push('k1'),'a')]=(order.push('a'),2);static [(order.push('k2'),'x')]=(order.push('x'),3);static {order.push('block');this.y=this.x+1;}b=this.a+1;}var a=new A();order.join(',')==='k1,k2,x,block,a'&&A.y===4&&a.a===2&&a.b===3&&Object.getOwnPropertyDescriptor(a,'a').enumerable",
    );
    yes("class A{set x(v){throw 1;}}class B extends A{x=3;}new B().x===3");
    yes(
        "class A{f=()=>this; g=function(){};static n=new.target;}var a=new A();a.f()===a&&a.f.name==='f'&&a.g.name==='g'&&A.n===undefined",
    );
}
#[test]
fn new_target_super_arrows_and_ordinary_constructors_are_distinct() {
    yes(
        "function F(){this.target=new.target;}class A extends F{}var a=new A();a.target===A&&new F().target===F&&F.call({})===undefined",
    );
    yes(
        "class A{m(){return 3;}}class B extends A{m(){return (()=>super.m())()+1;}}new B().m()===4",
    );
    yes("var o={m(){return super.toString.call(this);}};o.m()==='[object Object]'");
}
#[test]
fn native_bases_keep_internal_brands_and_derived_prototypes() {
    yes(
        "class A extends Array{}var a=new A(1,2);a.length===2&&a[0]===1&&Array.isArray(a)&&a instanceof A&&a instanceof Array",
    );
    yes(
        "class A extends Error{}var a=new A('bad');a.message==='bad'&&a instanceof A&&a instanceof Error",
    );
    yes(
        "class A extends Object{}var source={x:1},a=new A(source);a!==source&&a.x===undefined&&a instanceof A",
    );
}
#[test]
fn class_tdz_and_early_errors_are_distinct() {
    let mut vm = Vm::new();
    for source in ["new A();class A{}", "class A extends A{}"] {
        let mut vm = Vm::new();
        assert_eq!(vm.eval(source).unwrap_err().kind, ErrorKind::ReferenceError);
    }
    for source in [
        "class A{constructor(){}constructor(){}}",
        "class A{get constructor(){}}",
        "class A{static prototype(){}}",
        "class A{constructor(){super();}}",
        "class A{x=arguments;}",
        "class A{static{return 1;}}",
        "new.target",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    yes("class A{static{this.fn=()=>{class await{};return await;};}}A.fn().name==='await'");
}
#[test]
fn class_closures_fields_and_home_objects_survive_gc() {
    let mut vm = Vm::new();
    vm.eval("class A{m(){return 2;}}class B extends A{x=()=>super.m();m(){return super.m()+1;}}var b=new B();").unwrap();
    vm.collect_garbage();
    assert_eq!(vm.eval("b.x()+b.m()").unwrap(), Value::from(5));
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1000,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("class A{x=(()=>{while(true){};})();}new A()")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
}
#[test]
fn classes_are_bounded_on_one_mib_native_stack() {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            for source in [
                "class A{constructor(){new A();}}new A()",
                "class A{x=new A();}new A()",
                "class A{}class B extends A{constructor(){super();new B();}}new B()",
            ] {
                let mut vm = Vm::new();
                assert_eq!(
                    vm.eval(source).unwrap_err().kind,
                    ErrorKind::ExecutionLimit,
                    "{source}"
                );
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn class_name_is_available_before_static_initializers_and_can_be_replaced() {
    yes(
        "var A=class{static seen=this.name;};var x=(A=>A)(class{});var Y=class{static name(){return 'own';}};var obj={['computed']:class{static get name(){return 'own';} static seen=this.name;}};A.seen==='A'&&x.name===''&&Y.name()==='own'&&obj.computed.seen==='own'",
    );
    yes(
        "var k=Symbol('x'),o={[k]:class{static seen=this.name;}};var C;C ??= class{static seen=this.name;};var {D=class{static seen=this.name;}}={};o[k].seen==='[x]'&&C.seen==='C'&&D.seen==='D'",
    );
}
#[test]
fn super_evaluates_this_and_constructor_before_user_expressions() {
    yes(
        "var touched=0;class A{}class B extends A{constructor(){try{super[touched++];}catch(e){if(!(e instanceof ReferenceError))throw e;}super();}}new B();touched===0",
    );
    yes(
        "var runs=[];class A{constructor(){runs.push('a');}}class C{constructor(){runs.push('c');}}class B extends A{constructor(){super(Object.setPrototypeOf(B,C));}}new B();new B();runs.join(',')==='a,c'",
    );
    yes(
        "var order=[];var key={[Symbol.toPrimitive](){order.push('key');return 'x';}};class A{get x(){return this.v||0;}set x(v){this.v=v;}}class B extends A{m(){super[key]=(order.push('rhs'),3);super[key]+=(order.push('add'),4);return this.v;}}var b=new B();b.m()===7&&order.join(',')==='rhs,key,key,add'",
    );
    yes(
        "var a={m(){return 1;}};var o={__proto__:a,m(){return super.m();}};var copy={m:o.m};copy.m()===1&&o.m()===1",
    );
}
#[test]
fn new_target_and_super_can_be_referenced_by_direct_eval() {
    yes(
        "function P(){this.target=eval('new.target');}class A extends P{}class B{m(){return 3;}}class C extends B{m(){return eval('super.m()')+1;}}new A().target===A&&new C().m()===4",
    );
    yes(
        "class G extends Function{}var fn=new G('return 7');fn()===7&&fn instanceof G&&fn instanceof Function",
    );
    yes(
        "class Prom extends Promise{}var p=new Prom(resolve=>resolve(2));p instanceof Prom&&p.then(x=>x+1) instanceof Prom",
    );
}
