use rust_browser_javascript::{ErrorKind, Value, Vm};
#[test]
fn templates_parameters_and_patterns_are_bounded_on_one_mib_stack() {
    std::thread::Builder::new().stack_size(1024*1024).spawn(||{
        for source in ["(function(){function getF(){return f;}function f(_,n){if(n===0)return;return getF()`${n-1}`;}f(null,100000);}());","(function(){function f(_,n){if(n===0)return;return f`${n-1}`;}f(null,100000);}());","function f(a=f()){}f();","function f({a}={a:1},...r){return f();}f();"]{let mut vm=Vm::new();assert_eq!(vm.eval(source).unwrap_err().kind,ErrorKind::ExecutionLimit,"{source}");}
        let mut vm=Vm::new();assert_eq!(vm.eval("function f({a=3}={},[b,...r]=[4,5]){return `${a+b+r[0]}`;}f()").unwrap(),Value::from("12"));
    }).unwrap().join().unwrap();
}
