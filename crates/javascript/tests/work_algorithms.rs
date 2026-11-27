use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

#[test]
fn string_search_respects_overlapping_candidates_and_utf16_boundaries() {
    assert_eq!(Vm::new().eval(r#"
        const s = "x".repeat(255) + "aaab\ud800z";
        const receiver = { length: 1, get 0() { log += "g"; return item } };
        const item = { toString() { log += "v"; return "value" } };
        const sep = { toString() { log += "s"; return "-" } };
        let log = "";
        s.indexOf("aab") === 256 && s.indexOf("aab",257) === -1 &&
        s.split("\ud800z")[0].length === 259 && s.split("unknown")[0] === s &&
        s.slice(0) === s && Array.prototype.join.call(receiver, sep) === "value" &&
        log === "sgv"
    "#).unwrap(), Value::Bool(true));

    let mut vm = Vm::with_limits(VmLimits { max_instructions: 5000, ..VmLimits::default() });
    vm.set_global("input", Value::from("a".repeat(2000))).unwrap();
    vm.set_global("needle", Value::from(format!("{}b", "a".repeat(120)))).unwrap();
    assert_eq!(vm.eval("input.includes(needle)").unwrap_err().kind, ErrorKind::ExecutionLimit);
}

#[test]
fn sorting_runs_preserves_ties_holes_and_abrupt_completion() {
    assert_eq!(Vm::new().eval(r#"
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
    "#).unwrap(), Value::Bool(true));
}

#[test]
fn end_anchored_class_runs_preserve_indices_captures_and_local_flags() {
    assert_eq!(Vm::new().eval(r#"
        const m = /^([\w.]{1,40})$/d.exec("one.two_3");
        const unicode = /^[^!]*$/u.exec("a😀界");
        const re=/[a-z]*$/gy; re.lastIndex=2; const s=re.exec("!!abc");
        m[1] === "one.two_3" && m.indices[1][1] === 9 && unicode[0] === "a😀界" &&
        m.input === m[0] && /^([ab]*)$/.exec("ab\n") === null &&
        /(?m:^[ab]*$)/.exec("ab\n!")[0] === "ab" && s.index === 2 &&
        s[0] === "abc" && re.lastIndex === 5 && /(?<=a)[^!]*$/.exec("aa!") === null
    "#).unwrap(), Value::Bool(true));
}
