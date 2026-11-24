use rust_browser_javascript::{ErrorKind, JsString, Value, Vm, VmLimits};

fn expect(source: &str, expected: Value) {
    let result = Vm::new()
        .eval(source)
        .unwrap_or_else(|error| panic!("{error}\n{source}"));
    assert_eq!(result, expected, "{source}");
}

#[test]
fn literal_lexical_goal_and_cover_grammar_preserve_division_and_delimiters() {
    expect(
        r#"
        var q = 12 / 3 / 2; q /= 2;
        var f = (r = /[/*)]/) => r.test('/');
        var g = async (r = /[}\/)]/) => r.test('}');
        var h = (x, y) => x / y;
        var result = false;
        if (q === 1) result = /[/*]/.test('*');
        var t = `nested ${/}/.test('}')} ${h(8, 4)}`;
        [q, f(), result, /=/.test('='), t].join('|');
    "#,
        "1|true|true|true|nested true 2".into(),
    );
    expect(
        "function f(){return /x/;} (f() !== f()) && f().test('x');",
        true.into(),
    );
}

#[test]
fn async_arrow_lookahead_preserves_template_and_slash_positions() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        let result;
        const format = async value => `${/[}]/.test('}')}:${value / 2}`;
        let value = 12;
        value /= 3;
        format(value).then(text => result = text);
        "#,
    )
    .unwrap();
    vm.drain_jobs(10).unwrap();
    assert_eq!(vm.eval("result").unwrap(), "true:2".into());
}

#[test]
fn regex_literal_syntax_is_checked_even_in_dead_code() {
    let mut vm = Vm::new();
    for source in [
        "if(false) /x/gg;",
        "/[a/;",
        "/a\\\n/;",
        "/(a/;",
        "/\\a/u;",
        "/[z-a]/;",
        "/a{2,1}/;",
        "/(?<x>a)(?<x>b)/;",
        "/\\2(a)/u;",
        "/./uv;",
        "/./K;",
        "/./\u{0300};",
        "/./\\u0067;",
        "/^*/;",
        "/(?=x)*/u;",
    ] {
        let error = vm.check_syntax(source).unwrap_err();
        assert_eq!(error.kind, ErrorKind::SyntaxError, "{source}: {error}");
        assert!(error.line.is_some(), "{source}: {error}");
    }
    vm.check_syntax(r"/\p{Alphabetic}/u").unwrap();
    vm.check_syntax(r"/[a&&b]/v").unwrap();
    for source in [
        r"/[ab&&c]/v",
        r"/[a&&]/v",
        r"/[^\q{|x}]/v",
        r"/[a--b&&c]/v",
        r"/[a--]/v",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    assert_eq!(
        vm.eval("new RegExp('a','gg')").unwrap_err().kind,
        ErrorKind::SyntaxError
    );
}

#[test]
fn unicode_sets_use_longest_strings_and_backtrack_in_both_directions() {
    expect(
        r#"
        const match = /[\q{a|ab|abc}]/v.exec("abc");
        [match[0], /^[\q{a|ab}]b$/v.test("ab"),
         /(?<=[\q{xy|y}])z/v.test("xyz"),
         /^[\p{RGI_Emoji}&&\q{🇵🇱|no}]$/v.test("🇵🇱"),
         /^[\P{Lowercase_Letter}]$/iv.test("A"),
         /^[\q{|abc}]x$/v.test("x")].join("|")
    "#,
        "abc|true|true|true|false|true".into(),
    );
}

#[test]
fn constructor_brand_subclass_and_intrinsic_lifetime() {
    expect(
        r#"
        class Rx extends RegExp {}
        var r = /a/gi;
        var copy = new Rx(r);
        r.lastIndex = 4;
        var fake = { [Symbol.match]: true, source: 'b', flags: 'y' };
        var constructor = RegExp;
        var before = RegExp(r) === r && copy instanceof Rx && copy.lastIndex === 0;
        globalThis.RegExp = null;
        before && new constructor(fake).test('b') &&
            /a/i.test('A') && Object.prototype.toString.call(copy) === '[object RegExp]' &&
            Object.prototype.toString.call(constructor.prototype) === '[object Object]';
    "#,
        true.into(),
    );
}

#[test]
fn capture_order_quantifier_reset_and_nonparticipating_backreferences() {
    expect(
        r#"
        var x = /(z)((a+)?(b+)?(c))*/.exec('zaacbbbcac');
        var y = /^(a+?)(a*)$/.exec('aaaa');
        [x[0],x[1],x[2],x[3],x[4]===undefined,x[5],y[1],y[2],
            /^(a|(b))\2c$/.test('ac'), /^\1(a)$/.test('a'),
            /^(?:$)+$/.test(''), /(ab|a)b/.exec('ab')[1]].join('|');
    "#,
        "zaacbbbcac|z|ac|a|true|c|a|aaa|true|true|true|a".into(),
    );
}

#[test]
fn lookaround_is_atomic_and_lookbehind_runs_right_to_left() {
    expect(
        r#"
        var x = /(?<=([ab]+)([bc]+))$/.exec('abc');
        var y = /(?=(a+))a*b\1/.exec('baabac');
        var z = /(?<!(b))(a)/.exec('ca');
        [x[0],x[1],x[2],x.index,y[0],y[1],y.index,z[1]===undefined,
            z[2],/(?<=\1a(b))c/.test('babc'),/(?<!ab)c/.test('abc'),
            /(?<=a(?=b))b/.test('ab')].join('|');
    "#,
        "|a|bc|3|aba|a|2|true|a|true|false|true".into(),
    );
}

#[test]
fn named_group_indices_and_scoped_flags() {
    expect(
        r#"
        var r=/(?:(?<word>a)|(?<word>b))\k<word>/d;
        var a=r.exec(' bb');
        var b=/(?<x>a)?(?<y>b)/d.exec('b');
        [a.groups.word, a.indices[0].join(','),a.indices.groups.word.join(','),
         Object.getPrototypeOf(a.groups)===null,Object.keys(a.groups).join(','),
         b.groups.x===undefined,b.indices.groups.y[1], /(?i:a)(?-i:b)/i.test('Ab'),
         /(?i:a)(?-i:b)/i.test('AB'), /(?s-:.)/.test('\n')].join('|');
    "#,
        "b|1,3|1,2|true|word|true|1|true|false|true".into(),
    );
}

#[test]
fn unicode_and_legacy_execution_preserve_utf16() {
    expect(
        r#"
        var x=/.{2}/du.exec('𐐀!');
        var y=/./d.exec('\ud800');
        var z=/./uy; z.lastIndex=1;
        var astral=z.exec('😀');
        [x[0].length,x.indices[0][1],y[0].charCodeAt(0),y.indices[0][1],
         astral[0].length,astral.index,z.lastIndex,
         /^.$/u.test('\ud800'),/^.$/.test('😀'),/\uD83D\uDE00/u.test('😀'),
         /\u{1f600}/u.test('😀'),/\u{1f600}/.test('u{1f600}'),
         /\w/ui.test('\u212a'), /\w/i.test('\u212a'), /[A-Z]/ui.test('\u017f'),
         /ß/ui.test('ẞ'),/ß/i.test('ẞ'),/\s/.test('\ufeff'),/\s/.test('\u0085')].join('|');
    "#,
        "3|3|55296|1|2|0|2|true|false|true|true|true|true|false|true|true|false|true|false".into(),
    );
}

#[test]
fn last_index_observes_coercion_sticky_search_and_strict_writes() {
    expect(
        r#"
        var global=/a/g; global.lastIndex = {valueOf:()=>1};
        var sticky=/a/y; sticky.lastIndex = 1;
        var plain=/a/; var coercions=0;
        plain.lastIndex={valueOf(){coercions++;return 9;}};
        var once=global.exec('baa'), twice=global.exec('baa'), absent=global.exec('baa');
        var noSticky=sticky.exec('bba')===null && sticky.lastIndex===0;
        var one=plain.exec('ba');
        var pinned=/b/g; pinned.lastIndex=-0; var search='ab'.search(pinned);
        var readOnly=/a/g;
        Object.defineProperty(readOnly,'lastIndex',{writable:false});
        var error=false; try{readOnly.exec('a');}catch(e){error=e instanceof TypeError;}
        [once.index,twice.index,absent===null,global.lastIndex,noSticky,one.index,
         coercions,search,Object.is(pinned.lastIndex,-0),error].join('|');
    "#,
        "1|2|true|0|true|1|1|1|true|true".into(),
    );
}

#[test]
fn string_protocol_dispatch_coercion_and_overridden_exec() {
    expect(
        r#"
        var log='';
        var needle = {[Symbol.match](input){log+='M';return input;}};
        var original = {toString(){log+='X';throw 0;}};
        var result = String.prototype.match.call(original,needle);
        var fake = {exec(s){log+=s;return {index:7};}, lastIndex:9};
        var searched=RegExp.prototype[Symbol.search].call(fake,'Y');
        var bad=false;
        try { 'abc'.startsWith(/a/); } catch(e) {bad=e instanceof TypeError;}
        var unbranded=/a/;unbranded[Symbol.match]=false;
        [result===original,searched,fake.lastIndex,log,bad,
         '/a/test'.startsWith(unbranded), 'abc'.search('b'), 'abc'.match('b')[0]].join('|');
    "#,
        "true|7|9|MY|true|true|1|b".into(),
    );
    expect(
        "RegExp.prototype.test.call({exec(){return {a:1};}}, '');",
        true.into(),
    );
    let mut vm = Vm::new();
    for src in [
        "RegExp.prototype.test.call({exec(){return true;}}, '')",
        "'x'.matchAll(/x/)",
        "'x'.replaceAll(/x/,'y')",
        "String.prototype.match.call(null, {[Symbol.match](){throw 1;}})",
    ] {
        assert_eq!(vm.eval(src).unwrap_err().kind, ErrorKind::TypeError);
    }
}

#[test]
fn replace_split_and_lazily_iterate_matches() {
    expect(r#"
        var replacementPattern=/(?<a>[A-Z])(\d+)/g;
        var template="A2,B34".replace(replacementPattern,"$<a>[$2]-$$");
        var extras="zaaz".replace(/(a)(a)/, "[$&][$01][$10][$0][$99][$\x60][$']");
        var log=[];
        var calls="A2,B34".replace(replacementPattern,function(m,a,n,i,s,groups){
            log.push(m,i,groups.a,s);
            return n;
        });
        var cut="a::b".split(/(:)/, 4);
        var iter='a1a2'.matchAll(/a(\d)/g);
        var matches=[iter.next().value[1],iter.next().value[1],iter.next().done,iter.next().done].join(',');
        [template,extras,calls,log.join(','),cut.join(','),matches,'x'.replaceAll('','_'),
         '😀'.match(/(?:)/gu).length, '😀'.split(/(?:)/u).length].join('|');
    "#, "A[2]-$,B[34]-$|z[aa][a][a0][$0][$99][z][z]z|2,34|A2,0,A,A2,B34,B34,3,B,A2,B34|a,:,,:|1,2,true,true|_x_|2|1".into());
}

#[test]
fn match_all_is_species_aware_lazy_and_gc_traced() {
    let mut vm = Vm::new();
    let iterator = vm
        .eval(
            r#"
        var calls = 0; var description;
        (function(){
            var re=/a/g;
            re.lastIndex=2;
            re.constructor = {[Symbol.species]: function(original,flags){
                description = flags;
                return {lastIndex:0,exec:function(input){
                    calls++;
                    return calls === 1 ? {0: input, index: this.lastIndex} : null;
                }};
            }};
            return 'aba'.matchAll(re);
        })();
    "#,
        )
        .unwrap();
    let root = vm.root(iterator.clone());
    vm.collect_garbage();
    assert_eq!(
        vm.eval("[calls,description].join(',')").unwrap(),
        "0,g".into()
    );
    let next = vm.get_property(iterator.clone(), "next").unwrap();
    let result = vm.call(next.clone(), iterator.clone(), &[]).unwrap();
    let matched = vm.get_property(result, "value").unwrap();
    assert_eq!(vm.get_property(matched.clone(), "0").unwrap(), "aba".into());
    assert_eq!(vm.get_property(matched, "index").unwrap(), 2.into());
    vm.collect_garbage();
    let result = vm.call(next, iterator, &[]).unwrap();
    assert_eq!(vm.get_property(result, "done").unwrap(), true.into());
    vm.unroot(root);
    vm.collect_garbage();
}

#[test]
fn source_escape_and_annex_b_compile_round_trip() {
    expect(r"RegExp.escape('\x60')", r"\x60".into());
    expect(
        r#"
        var r = new RegExp('a/b\nc');
        var s = r.source;
        var t = r.toString();
        var ok = r.compile(/x+/y) === r && r.flags === 'y' && r.test('xx');
        var escaped=RegExp.escape('A-.\n /\ud800😀');
        [s,t,ok,escaped,new RegExp(escaped,'u').test('A-.\n /\ud800😀'),
         RegExp.prototype.source,RegExp.prototype.flags].join('|');
    "#,
        r"a\/b\nc|/a\/b\nc/|true|\x41\x2d\.\n\x20\/\ud800😀|true|(?:)|".into(),
    );
}

#[test]
fn pathological_backtracking_and_output_expansion_are_budgeted() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 4_000,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("/(a+)+$/.test('aaaaaaaaaaaaaaaaaaaaa!')")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
    let mut vm = Vm::with_limits(VmLimits {
        max_string_units: 2048,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval(r#""x".repeat(128).replace(/x/g, "$'".repeat(32))"#)
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
}

#[test]
fn parsing_matching_and_dropping_a_pattern_on_a_one_mib_stack() {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let mut vm = Vm::new();
            let supported = format!("/{0}a{1}/.test('a')", "(?:".repeat(32), ")+".repeat(32));
            assert_eq!(vm.eval(&supported).unwrap(), true.into());
            let too_deep = format!("/{0}a{1}/", "(".repeat(64), ")".repeat(64));
            assert_eq!(
                vm.check_syntax(&too_deep).unwrap_err().kind,
                ErrorKind::ExecutionLimit
            );
            let flat = JsString::from("a".repeat(8_192));
            vm.set_global("flatRegExp", Value::String(flat)).unwrap();
            assert_eq!(
                vm.eval("new RegExp(flatRegExp).test(flatRegExp)").unwrap(),
                true.into()
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn large_ascii_matches_preserve_ecmascript_priority_boundaries_and_empty_matches() {
    let mut regular = Vm::with_limits(VmLimits {
        max_instructions: 16_000_000,
        ..Default::default()
    });
    let mut document = Vm::with_limits(VmLimits {
        buffer_units_per_instruction: 256,
        max_instructions: 16_000_000,
        ..Default::default()
    });
    let input = format!(
        "{} aaab aabb bab true false 44e+ 44e+4 \"source\" ",
        "#".repeat(4096)
    );
    for vm in [&mut regular, &mut document] {
        vm.set_global("input", Value::from(input.as_str())).unwrap();
    }
    for pattern in [
        "a|ab",
        "ab|a",
        "a+?b",
        "(a|ab)+",
        "(?:a?)*b",
        "(a)(b)\\2",
        r#"\d+(?:[eE][+\-]?\d+)?"#,
        r#""[^"\\\n\r]*"|true|false|null"#,
        r#"^(\s|\u00a0)+|(\s|\u00a0)+$"#,
        "(?:^|$)",
        r#"^[^\n]*$"#,
    ] {
        document.set_global("pattern", pattern.into()).unwrap();
        regular.set_global("pattern", pattern.into()).unwrap();
        for script in [
            r#"var re = new RegExp(pattern, 'g'); input.replace(re, '@') + ':' + re.lastIndex"#,
            r#"new RegExp(pattern).test(input)"#,
        ] {
            assert_eq!(
                document.eval(script).unwrap(),
                regular.eval(script).unwrap(),
                "{pattern}"
            );
        }
    }
}

#[test]
fn large_regexp_does_not_bypass_custom_exec_utf16_or_capture_replacements() {
    let mut vm = Vm::with_limits(VmLimits {
        buffer_units_per_instruction: 256,
        max_instructions: 16_000_000,
        ..Default::default()
    });
    let input = format!("{}xé", "#".repeat(4096));
    vm.set_global("input", Value::from(input.as_str())).unwrap();
    assert_eq!(
        vm.eval(r#"input.replace(/(x)/g, '<$1>').slice(-4)"#)
            .unwrap(),
        "<x>é".into()
    );
    assert_eq!(
        vm.eval(r#"input.replace(/x|é/g, '!').slice(-2)"#).unwrap(),
        "!!".into()
    );
    assert_eq!(
        vm.eval(
            r#"var calls = 0, re = /x/g;
        Object.defineProperty(re, 'exec', { get() { calls++; return RegExp.prototype.exec; } });
        input.replace(re, '!').slice(-2) + ':' + calls + ':' + re.lastIndex"#
        )
        .unwrap(),
        "!é:2:0".into()
    );
    assert_eq!(
        vm.eval(
            r#"var converted = 0, r = /x/; r.lastIndex = {valueOf(){converted++; return 0;}};
        r.test(input) + ':' + converted"#
        )
        .unwrap(),
        "true:1".into()
    );
}

#[test]
fn legacy_regexp_statics_observe_bulk_matches_and_subclass_invalidation() {
    let mut vm = Vm::with_limits(VmLimits {
        buffer_units_per_instruction: 256,
        ..VmLimits::default()
    });
    let text = format!("{}axbcy", ".".repeat(4096));
    vm.set_global("text", text.as_str().into()).unwrap();
    let result = vm
        .eval(
            r#"
        /(a)x/.test(text);
        let first = RegExp.lastMatch + RegExp.$1;
        text.replace(/(a|b)(x|c)/g, "!");
        RegExp.input = { toString() { return "new"; } };
        let parts = [first, RegExp.$_, RegExp.lastMatch, RegExp.$1, RegExp.lastParen,
                     RegExp.leftContext.slice(-2), RegExp.rightContext];
        class Derived extends RegExp {}
        new Derived("z").test("z");
        try { RegExp.$1; } catch(e) { parts.push(e instanceof TypeError); }
        RegExp.input = "restored";
        parts.push(RegExp.$_);
        parts.join("|")
    "#,
        )
        .unwrap();
    assert_eq!(result, "axa|new|bc|b|c|ax|y|true|restored".into());
}

#[test]
fn large_regexp_speculative_scans_still_consume_the_vm_work_budget() {
    let mut vm = Vm::with_limits(VmLimits {
        buffer_units_per_instruction: 256,
        max_instructions: 5000,
        ..Default::default()
    });
    vm.set_global("input", Value::from("a".repeat(20_000)))
        .unwrap();
    let result = vm.eval("input.replace(/a*b|a/g, 'x')");
    assert_eq!(result.unwrap_err().kind, ErrorKind::ExecutionLimit);
}

#[test]
fn unicode_properties_match_categories_scripts_and_closed_complements() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
        /\p{gc=Lu}/u.test('Ł') && /[\p{Script=Greek}]/u.test('Ω') &&
        /\p{scx=Hira}/u.test('ー') && /\p{ASCII}/u.test('x') &&
        /\P{Lowercase_Letter}/iu.test('k') && !/\p{L}/u.test('3') &&
        (()=>{try{new RegExp('\\p{letter}','u')}catch(e){return e instanceof SyntaxError}})()
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}
