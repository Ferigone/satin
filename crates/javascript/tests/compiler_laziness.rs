use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

fn small_heap() -> Vm {
    let mut base = Vm::new();
    base.collect_garbage();
    Vm::with_limits(VmLimits {
        max_heap_bytes: base.estimated_heap_bytes() + 8 * 1024 * 1024,
        ..VmLimits::default()
    })
}

fn wide_bundle(wrapper_prologue: &str, extra: &str) -> String {
    let body = "v+=1;".repeat(100);
    let mut source = format!("(function(){{{wrapper_prologue}var base=10;");
    for i in 0..144 {
        source.push_str(&format!("function f{i}(v){{{body}return v+base;}}"));
    }
    source.push_str(extra);
    source.push_str("return f0(1)+f143(2);})()");
    source
}

#[test]
fn large_wrappers_release_validated_cold_functions_under_a_small_heap() {
    for directive in ["", "'use strict';"] {
        let mut vm = small_heap();
        let source = wide_bundle(directive, "");
        assert!(source.len() >= 64 * 1024);
        assert_eq!(vm.eval(&source).unwrap(), Value::Number(223.0));
    }
}

#[test]
fn unused_functions_keep_the_early_error_rules_of_the_wrapper() {
    let sloppy = wide_bundle("", "function unused(a,a){if(1){function b(){return a;}}return b();}");
    assert_eq!(small_heap().eval(&sloppy).unwrap(), Value::Number(223.0));
    let strict = wide_bundle("'use strict';", "function unused(a,a){}");
    assert_eq!(small_heap().eval(&strict).unwrap_err().kind, ErrorKind::SyntaxError);
    let scope_conflict = wide_bundle("", "function unused(){let a;var a;}");
    assert_eq!(small_heap().eval(&scope_conflict).unwrap_err().kind, ErrorKind::SyntaxError);
}

#[test]
fn first_invocation_preserves_inherited_strictness_and_sloppy_annex_bindings() {
    let sloppy = wide_bundle("", r#"
        function probe(a,a){ if (1) { function b(){return a;} }
            return this === globalThis && b() === 2;
        }
        if (!probe(1,2)) return false;
    "#);
    assert_eq!(small_heap().eval(&sloppy).unwrap(), Value::Number(223.0));
    let strict = wide_bundle("'use strict';", r#"
        function probe(){ return this === undefined; }
        if (!probe()) return false;
    "#);
    assert_eq!(small_heap().eval(&strict).unwrap(), Value::Number(223.0));
}
