use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

#[test]
fn constant_records_and_array_appends_keep_descriptors_order_and_prototypes() {
    let result=Vm::new().eval(r#"
        function record(){return {"10":3, "2":4, x:"first", ["__proto__"]:"own", x:"last", n:-0, b:!0, f:!1}}
        const a=record(), b=record(), descriptor=Object.getOwnPropertyDescriptor(a,"x");
        a.x="changed";
        let log=""; const k={toString(){log+="k";return "r"}};
        const dynamic={get q(){log+="g";return 3},[k]:(log+="v",9)};
        const aryPrototype=Object.create(Array.prototype);
        Object.defineProperty(aryPrototype,"0",{set(v){log+="bad"}});
        const array=[...["a"],,"b"];
        Object.setPrototypeOf(array,aryPrototype);
        const plain={__proto__:null,r:2};
        Object.keys(b).join(",")==="2,10,x,__proto__,n,b,f" && b.x==="last" &&
        b.__proto__==="own" && Object.getPrototypeOf(b)===Object.prototype &&
        descriptor.configurable && descriptor.writable && descriptor.enumerable && descriptor.value==="last" &&
        Object.is(b.n,-0) && b.b && !b.f && dynamic.q===3 && log==="kvg" && dynamic.r===9 &&
        array[0]==="a" && !(1 in array) && array.length===3 && Object.getPrototypeOf(plain)===null
    "#).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn descending_typed_stores_check_the_evaluated_range_and_preserve_observable_setters() {
    let mut vm = Vm::new();
    let result=vm.eval(r#"
        function set(a,n){for(var i=n;--i>=0;)a[i]=257;return i}
        function fromLength(a){for(var i=a.length;--i>=0;)a[i]=3;return i}
        const big=new Uint16Array(65536);
        let log="";
        const source=new Uint8Array([1,2,3,4,5,6]),view=source.subarray(1,5);
        Object.defineProperty(view,"length",{get(){log+="L";return 2}});
        const ordinary={get length(){log+="O";return 2},set 0(v){log+=v+"a"},set 1(v){log+=v+"b"}};
        const bigDone=set(big,65536)===-1 && big[0]===257 && big[32768]===257 && big[65535]===257;
        const viewDone=fromLength(view)===-1 && source.join(",")==="1,3,3,4,5,6";
        const plainDone=fromLength(ordinary)===-1 && log==="LO3b3a";
        let bigIntFailed=false;try{set(new BigUint64Array(1),1)}catch(e){bigIntFailed=e instanceof TypeError}
        function* later(a){yield 1; for(var i=a.length;--i>=0;)a[i]=23; return i}
        const iter=later(big);iter.next();const last=iter.next();
        bigDone && viewDone && plainDone && bigIntFailed && last.done && last.value===-1 &&
        big[0]===23 && big[65535]===23 && set(big,0)===-1 && set(big,-2)===-3
    "#).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn string_search_respects_overlapping_candidates_and_utf16_boundaries() {
    assert_eq!(
        Vm::new()
            .eval(
                r#"
        const s = "x".repeat(255) + "aaab\ud800z";
        const receiver = { length: 1, get 0() { log += "g"; return item } };
        const item = { toString() { log += "v"; return "value" } };
        const sep = { toString() { log += "s"; return "-" } };
        let log = "";
        s.indexOf("aab") === 256 && s.indexOf("aab",257) === -1 &&
        s.split("\ud800z")[0].length === 259 && s.split("unknown")[0] === s &&
        s.slice(0) === s && Array.prototype.join.call(receiver, sep) === "value" &&
        log === "sgv"
    "#
            )
            .unwrap(),
        Value::Bool(true)
    );

    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 5000,
        ..VmLimits::default()
    });
    vm.set_global("input", Value::from("a".repeat(2000)))
        .unwrap();
    vm.set_global("needle", Value::from(format!("{}b", "a".repeat(120))))
        .unwrap();
    assert_eq!(
        vm.eval("input.includes(needle)").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
}

#[test]
fn numeric_index_reads_and_updates_preserve_exotic_and_inherited_properties() {
    let result = Vm::new()
        .eval(
            r#"
        const a=[2,4,6], t=new Uint8Array(3);
        let seen="", failed=false;
        Object.defineProperty(a,"1",{get(){seen+="g";return 5},set(v){seen+=v}});
        const parent=Object.create(Array.prototype);
        Object.defineProperty(parent,"7",{get(){seen+="p";return 11}});
        Object.setPrototypeOf(a,parent);
        const hole=a[7]; a[1]+=3; a[-0]=10;
        const watch={valueOf(){seen+="c";return 9}};
        t[-0]=4; t[1.5]=watch; t[999999]=watch; t[NaN]=watch;
        try{new BigInt64Array(1)[-1]=1}catch(e){failed=e instanceof TypeError}
        const key={toString(){seen+="k";return "2"}};
        t[key]+=3;
        const proxy=new Proxy(t,{get(o,k){return "observed-"+k}});
        const text="\uD83C\uDF38";
        seen==="pg8ccck" && hole===11 && a[0]===10 && a[1]===5 && a[2]===6 &&
        t[0]===4 && t["-0"]===undefined && t[1.5]===undefined && t[2]===3 &&
        proxy[0]==="observed-0" && text[1].charCodeAt(0)===0xdf38 && failed
    "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn sorting_runs_preserves_ties_holes_and_abrupt_completion() {
    assert_eq!(
        Vm::new()
            .eval(
                r#"
        const a = [3,2,2,1,2].map((v,i)=>({v,i}));
        a.sort((x,y)=>x.v-y.v);
        const b = [3,,2,undefined,1]; b.sort((x,y)=>x-y);
        let thrown=false; const c=[8,1,7,6,1];
        try { c.sort(()=>{throw 1}) } catch(e) {thrown=e===1}
        const d = new Int16Array([5,4,3,2,1]); d.sort();
        const sorted = Array.from({length:100},(_,i)=>100-i);
        let calls=0; sorted.sort((x,y)=>{calls++;return x-y});
        a.map(x=>x.i).join() === "3,1,2,4,0" && !(4 in b) && 3 in b &&
        b.join() === "1,2,3,," && c.join() === "8,1,7,6,1" && thrown &&
        d.join() === "1,2,3,4,5" && sorted[0]===1 && calls===99
    "#
            )
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn end_anchored_class_runs_preserve_indices_captures_and_local_flags() {
    assert_eq!(
        Vm::new()
            .eval(
                r#"
        const m = /^([\w.]{1,40})$/d.exec("one.two_3");
        const unicode = /^[^!]*$/u.exec("a😀界");
        const re=/[a-z]*$/gy; re.lastIndex=2; const s=re.exec("!!abc");
        m[1] === "one.two_3" && m.indices[1][1] === 9 && unicode[0] === "a😀界" &&
        m.input === m[0] && /^([ab]*)$/.exec("ab\n") === null &&
        /(?m:^[ab]*$)/.exec("ab\n!")[0] === "ab" && s.index === 2 &&
        s[0] === "abc" && re.lastIndex === 5 && /(?<=a)[^!]*$/.exec("aa!") === null
    "#
            )
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn rejected_literal_searches_preserve_alternatives_folding_and_direction() {
    let mut vm = Vm::new();
    let result = vm
        .eval(
            r#"
        const x = /(abC)(?:def)?/di.exec("xxABC");
        const y = /(?<=prefix)end/gy; y.lastIndex=6;
        x[1] === "ABC" && x.indices[1][0] === 2 && y.exec("prefixend")[0] === "end" &&
        /(?i:abc)DEF/.test("AbcDEF") && !/(?i:abc)DEF/.test("Abcdef") &&
        /kilo/iu.test("KILO") && /\u212Afoo/iu.test("kfoo") &&
        /(?:doesnotexist|xyz)/.test("xyz") && /(?:needle)?x/.test("x") &&
        /(?<!prefix)foo/.test("foo") && !/pre(?:abc)+suf/i.test("PREaaSUF") &&
        /[a-z\d]+FOO/i.test("Z7foo") && /[^a-z]+FOO/i.test("7foo")
    "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn cached_primitive_methods_observe_new_descriptors_and_string_virtual_properties() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
        function read(s){return s.extra}
        String.prototype.extra=7;
        let first=read("a")+read("bb");
        Object.defineProperty(String.prototype,"extra",{configurable:true,
            get:function(){ "use strict"; return this }});
        String.prototype[0]="fallback";
        function start(s){ return s["0"] }
        const hit=start("")+start("")+start("x");
        const length=s=>s.length;
        first===14 && read("checked")==="checked" && hit==="fallbackfallbackx" &&
        length("one")===3 && length("different")===9
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn primitive_return_uses_normal_coercion_for_objects_and_respects_parameter_scope() {
    let mut vm = Vm::new();
    let result = vm
        .eval(
            r#"
        const pick=(x,y)=>x<y?1:-1, add=(a,b)=>a+b, or=(a,b)=>a|b;
        let log=""; const value=n=>({valueOf(){log+=n;return n}});
        function duplicate(x,x){return x+1}
        function defaults(a=4,b=2){return a+b}
        let bad=false; try{add(1n,1)}catch(e){bad=e instanceof TypeError}
        pick("a","b")===1 && pick(2,1)===-1 && pick(value(1),value(2))===1 &&
        log==="12" && or(4,1)===5 && add("a",3)==="a3" && add(2n,3n)===5n &&
        add(null,2)===2 && Number.isNaN(add(1)) && duplicate(3,5)===6 &&
        defaults()===6 && bad
    "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn compound_updates_hold_the_canonical_reference_across_rhs_and_yield() {
    let result = Vm::new()
        .eval(
            r#"
        let log="", backing=12;
        const obj={ get x(){log+="g";return backing}, set x(n){log+="s";backing=n} };
        const key={toString(){log+="k";return "x"}};
        function* test(){return obj[key]>>=yield 1}
        const it=test();const first=it.next();
        const before=log;backing=64;const last=it.next(2);
        const y=(obj[key] ||= 20);
        const n=(1<<30)>>>1;
        before==="kg" && last.value===3 && last.done && backing===3 && y===3 &&
        log==="kgskg" && n===536870912
    "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn fixed_calls_capture_getters_receivers_and_arguments_in_source_order() {
    let mut vm = Vm::new();
    let value = vm
        .eval(
            r#"
        let log="", n=0;
        const target={v:3,get f(){log+="f";return function(a,b,c){return this.v+a+b+c}}};
        const args={get x(){log+="x";return ++n}};
        const value=target.f(args.x,args.x,args.x);
        const direct=function(){let local=7;return eval("local")};
        const skipped=null?.f(args.x); const plain=(a,b)=>a+b;
        const spread=plain(...[1,2]); const optional=target.f?.(1,2,3);
        log==="fxxxf" && value===9 && direct()===7 && skipped===undefined &&
        spread===3 && optional===9
    "#,
        )
        .unwrap();
    assert_eq!(value, Value::Bool(true));

    let mut vm = Vm::with_limits(VmLimits {
        max_array_length: 1,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("var log='';function f(){};f(log+='a',log+='b',log+='c')")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(vm.eval("log").unwrap(), Value::from("ab"));
}
