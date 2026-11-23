use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};
fn yes(source: &str) {
    let mut vm = Vm::new();
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true), "{source}");
}
#[test]
fn templates_preserve_raw_cooked_utf16_and_substitution_order() {
    yes(
        r#"var log=[],o={[Symbol.toPrimitive](hint){log.push(hint);return 'x';}};var s=`a${o}${(log.push('second'),3)}\uD800`;s==='ax3\uD800' && log.join(',')==='string,second' && String.raw`a\nb${2}c`==='a\\nb2c'"#,
    );
    yes("var t;function tag(x){t=x;}tag`a\r\nb`;t[0]==='a\\nb'&&t.raw[0]==='a\\nb'");
    yes(r#"function tag(x){return x[0]===undefined && x.raw[0]==='\\u{}';}tag`\u{}`"#);
    let mut vm = Vm::new();
    assert_eq!(
        vm.check_syntax(r#"`\u{}`"#).unwrap_err().kind,
        ErrorKind::SyntaxError
    );
}
#[test]
fn tagged_template_site_identity_freezing_receiver_and_gc() {
    let mut vm = Vm::new();
    assert_eq!(vm.eval("var saved,other;function tag(x){if(saved===undefined)saved=x;return x;}function f(){return tag`x${1}y`;}var a=f(),b=f();other=tag`x${1}y`;a===b && a!==other && Object.isFrozen(a) && Object.isFrozen(a.raw) && Object.getOwnPropertyDescriptor(a,'raw').enumerable===false").unwrap(),Value::Bool(true));
    vm.collect_garbage();
    assert_eq!(vm.eval("f()===saved").unwrap(), Value::Bool(true));
    yes("var obj={n:7,tag(s,x){return this.n+x;}};obj.tag`a${3}`===10 && (obj.tag)`a${2}`===9");
}
#[test]
fn nested_templates_and_braces_have_real_lexical_boundaries() {
    yes("`a${({x:`b${2}c`}).x}d`==='ab2cd' && `escaped \\${x}`==='escaped ${x}' && `${1,2}`==='2'");
}
#[test]
fn optional_chains_skip_keys_calls_and_keep_receivers_and_group_boundaries() {
    yes(
        "var n=0,x=null,obj={v:7,f(){return this.v;}};x?.[(n++,0)]?.(n++)===undefined && n===0 && obj?.f()===7 && obj.f?.()===7 && (obj?.f)()===7 && delete x?.a && 0?.toString()==='0'",
    );
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("var x=null;(x?.a).b").unwrap_err().kind,
        ErrorKind::TypeError
    );
    yes("var x=1;function f(){var x=2;return eval?.('x');}f()===1 && (true?.3:0.5)===0.3");
}
#[test]
fn logical_assignments_read_once_skip_rhs_and_only_set_when_required() {
    yes(
        "let a=true,b=false,c; a &&= ()=>{};b ||= function(){};c ??= (function(){});a.name==='a'&&b.name==='b'&&c.name==='c'",
    );
    yes(
        "var log=[],obj={};Object.defineProperty(obj,'x',{get(){log.push('get');return 0;},set(x){log.push('set'+x);}});var a=(obj.x&&=7),b=(obj.x||=3),c=(obj.x??=9);var n=0,o={x:null};o[(n++,'x')]??=5;log.join(',')==='get,get,set3,get' && a===0 && b===3 && c===0 && n===1 && o.x===5",
    );
    yes("const x=1;x ||= 2;x ??= 3;x===1");
}
#[test]
fn object_spread_copies_enumerable_string_symbol_data_in_order() {
    yes(
        "var s=Symbol(),log=[],a={getx:1};Object.defineProperty(a,'x',{enumerable:true,get(){log.push('x');return 2;}});a[s]=3;var b={...null,...a,x:4,...'ab',__proto__:null};b.x===4&&b[s]===3&&b[0]==='a'&&b[1]==='b'&&log.join(',')==='x'&&Object.getPrototypeOf(b)===null&&Object.getOwnPropertyDescriptor(b,'x').writable",
    );
}
#[test]
fn new_syntax_rejects_invalid_targets_and_respects_small_work_budgets() {
    let mut vm = Vm::new();
    for source in ["x?.y=1", "x?.y++", "new x?.y()", "x?.y`bad`"] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1000,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("var x='x'.repeat(128);for(var i=0;i<100;i++)`${x}${x}`")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
}

#[test]
fn template_cache_releases_dead_code_and_respects_small_aggregate_limits() {
    let mut vm = Vm::new();
    vm.eval("function tag(template){return template;}").unwrap();
    vm.collect_garbage();
    let baseline = vm.heap_cell_count();
    let saved = vm.eval("tag`same`").unwrap();
    let root = vm.root(saved.clone());
    for _ in 0..24 {
        vm.eval("tag`temporary`").unwrap();
    }
    assert!(vm.heap_cell_count() > baseline + 40);
    vm.collect_garbage();
    assert_eq!(vm.heap_cell_count(), baseline + 2);
    assert_ne!(vm.eval("tag`same`").unwrap(), saved);
    vm.unroot(root);
    vm.collect_garbage();
    assert_eq!(vm.heap_cell_count(), baseline);

    let mut realm = Vm::new();
    realm.collect_garbage();
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: realm.estimated_heap_bytes() + 64 * 1024,
        ..VmLimits::default()
    });
    vm.collect_garbage();
    assert_eq!(vm.eval("1+2").unwrap(), Value::from(3));
    assert_eq!(
        vm.eval("function tag(t){return t;}for(var i=0;i<300;i++)eval('tag`x`');")
            .unwrap_err()
            .kind,
        ErrorKind::HeapLimit
    );
    vm.collect_garbage();
    assert_eq!(vm.eval("1+2").unwrap(), Value::from(3));
}
