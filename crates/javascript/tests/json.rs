use rust_browser_javascript::{ErrorKind, JsString, Value, Vm, VmLimits};
fn run(source: &str) -> Value {
    Vm::new()
        .eval(source)
        .unwrap_or_else(|e| panic!("{e}\n{source}"))
}
#[test]
fn json_has_the_standard_to_string_tag() {
    assert_eq!(
        run(r#"
            var desc = Object.getOwnPropertyDescriptor(JSON, Symbol.toStringTag);
            desc.value === "JSON" && !desc.writable && !desc.enumerable &&
                desc.configurable && Object.prototype.toString.call(JSON) === "[object JSON]"
            "#),
        Value::Bool(true)
    );
}
#[test]
fn parses_ordinary_json_and_numbers() {
    assert_eq!(
        run(
            r#"var v=JSON.parse('{"a":[true,false,null,-0,1.25e2],"b":"ok"}');v.a[4]===125 && 1/v.a[3]===-Infinity && v.b==='ok'"#
        ),
        Value::Bool(true)
    );
    assert_eq!(run("JSON.parse('1e400')"), Value::Number(f64::INFINITY));
}
#[test]
fn invalid_json_is_not_executed_as_javascript() {
    let mut vm = Vm::new();
    for text in [
        "",
        "undefined",
        "NaN",
        "Infinity",
        "+1",
        "01",
        "-01",
        "1.",
        "1e",
        ".1",
        "0x10",
        "[1,]",
        "{\"a\":1,}",
        "{a:1}",
        "'x'",
        "/*x*/1",
        "true false",
        "\u{feff}1",
        "\"a\nb\"",
        "\"\\v\"",
        "\"\\u12xz\"",
    ] {
        let error = vm.parse_json(&JsString::from(text)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::SyntaxError, "{text:?}: {error}");
    }
}
#[test]
fn utf16_surrogates_and_property_names_are_preserved() {
    let mut vm = Vm::new();
    let value = vm
        .parse_json(&JsString::from(r#""\ud800x\udfff""#))
        .unwrap();
    assert_eq!(
        value.as_string().unwrap().as_utf16(),
        &[0xd800, 120, 0xdfff]
    );
    let raw = JsString::from_utf16(vec![34, 0xd800, 34]);
    assert_eq!(
        vm.parse_json(&raw).unwrap().as_string().unwrap().as_utf16(),
        &[0xd800]
    );
    assert_eq!(
        run(r#"var o=JSON.parse('{"\ud800":1,"\ufffd":2}');o['\ud800']+o['\ufffd']"#),
        Value::from(3)
    );
}
#[test]
fn proto_is_an_ordinary_json_data_property() {
    assert_eq!(
        run(
            r#"var o=JSON.parse('{"__proto__":{"polluted":true},"x":1,"x":2}');Object.getPrototypeOf(o)===Object.prototype && o.__proto__.polluted && o.x===2"#
        ),
        Value::Bool(true)
    );
}
#[test]
fn stringify_omits_objects_but_uses_null_in_arrays() {
    assert_eq!(
        run(
            "JSON.stringify({a:undefined,b:function(){},c:null,d:[undefined,function(){},NaN,Infinity,-0]})"
        ),
        Value::from("{\"c\":null,\"d\":[null,null,null,null,0]}")
    );
    assert_eq!(run("JSON.stringify(undefined)"), Value::Undefined);
    assert_eq!(run("JSON.stringify(function(){})"), Value::Undefined);
}
#[test]
fn well_formed_stringify_escapes_only_lone_surrogates() {
    assert_eq!(
        run(r#"JSON.stringify('\ud800x\udfff')"#),
        Value::from(r#""\ud800x\udfff""#)
    );
    let value = run(r#"JSON.stringify('\ud83d\ude00')"#);
    assert_eq!(
        value.as_string().unwrap().as_utf16(),
        &[34, 0xd83d, 0xde00, 34]
    );
    assert_eq!(
        run(r#"JSON.stringify('\u0000\b\f\n\r\t"\\/')"#),
        Value::from(r#""\u0000\b\f\n\r\t\"\\/""#)
    );
}
#[test]
fn stringify_key_order_and_sparse_arrays() {
    assert_eq!(
        run("var a=[];a.length=3;a[1]=7;JSON.stringify({z:1,2:2,1:1,a:a})"),
        Value::from("{\"1\":1,\"2\":2,\"z\":1,\"a\":[null,7,null]}")
    );
}
#[test]
fn to_json_and_replacer_have_correct_order_this_and_keys() {
    assert_eq!(
        run(
            "var log=[];var x={toJSON:function(k){log.push('json:'+k);return {n:2};}};var text=JSON.stringify({x:x},function(k,v){log.push('replace:'+k);return typeof v==='number'?v*3:v;});text+'|'+log.join(',')"
        ),
        Value::from("{\"x\":{\"n\":6}}|replace:,json:x,replace:x,replace:n")
    );
}
#[test]
fn replacer_array_supports_boxed_keys_and_inherited_properties() {
    assert_eq!(
        run(
            "var o=Object.create({inherited:3});o.a=1;o.b=2;JSON.stringify(o,['b',new String('a'),'b',true,new Number(4),'inherited'])"
        ),
        Value::from("{\"b\":2,\"a\":1,\"inherited\":3}")
    );
}
#[test]
fn pretty_print_and_boxed_primitives() {
    assert_eq!(
        run("JSON.stringify({a:[1,new Number(2)],b:new Boolean(false)},null,2)"),
        Value::from("{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": false\n}")
    );
    assert_eq!(
        run("JSON.stringify({a:1},null,'--')"),
        Value::from("{\n--\"a\": 1\n}")
    );
}
#[test]
fn cycles_reject_but_repeated_siblings_are_allowed() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("var a={};a.a=a;JSON.stringify(a)")
            .unwrap_err()
            .kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        vm.eval("var x={a:1};JSON.stringify([x,x])").unwrap(),
        Value::from("[{\"a\":1},{\"a\":1}]")
    );
}
#[test]
fn getters_and_callbacks_throw_authentic_errors() {
    let mut vm = Vm::new();
    assert_eq!(vm.eval("var o={};Object.defineProperty(o,'x',{enumerable:true,get:()=>{throw 7;}});JSON.stringify(o)").unwrap_err().thrown,Some(Value::from(7)));
    assert_eq!(
        vm.eval("JSON.parse('1',()=>{throw new TypeError('reviver');})")
            .unwrap_err()
            .kind,
        ErrorKind::TypeError
    );
}
#[test]
fn reviver_is_postorder_and_can_delete_or_replace_root() {
    assert_eq!(
        run(
            r#"var keys=[];var o=JSON.parse('{"a":[1,2],"b":3}',function(k,v){keys.push(k);if(k==='b')return undefined;return typeof v==='number'?v*2:v;});JSON.stringify(o)+'|'+keys.join(',')"#
        ),
        Value::from("{\"a\":[2,4]}|0,1,a,b,")
    );
    assert_eq!(run("JSON.parse('1',()=>undefined)"), Value::Undefined);
}
#[test]
fn reviver_context_preserves_exact_primitive_source() {
    assert_eq!(
        run(
            r#"var sources=[];JSON.parse('{"n":9007199254740993,"s":"\\ud800"}',function(k,v,c){if(k)sources.push(c.source);return v;});sources.join('|')"#
        ),
        Value::from("9007199254740993|\"\\ud800\"")
    );
}
#[test]
fn reviver_context_is_empty_for_mutated_values() {
    assert_eq!(
        run(
            r#"var text;JSON.parse('{"a":1,"b":2}',function(k,v,c){if(k==='a')this.b=3;if(k==='b')text=typeof c.source;return v;});text"#
        ),
        Value::from("undefined")
    );
}
#[test]
fn reviver_propagates_typed_array_write_back_exceptions() {
    assert_eq!(
        run(r#"
            var events = [], marker = {}, caught;
            try {
                JSON.parse("[0,0]", function(key, value) {
                    events.push(key);
                    if (key === "0" && Array.isArray(this)) {
                        this[1] = new Uint8Array(1);
                    } else if (key === "0") {
                        return { valueOf: function() { events.push("coerce"); throw marker; } };
                    }
                    return value;
                });
            } catch (e) {
                caught = e;
            }
            caught === marker && events.join(",") === "0,0,coerce"
            "#),
        Value::Bool(true)
    );
    assert_eq!(
        run(r#"
            var caught = false;
            try {
                JSON.parse("[0,0]", function(key, value) {
                    if (key === "0" && Array.isArray(this)) {
                        this[1] = new Uint8Array(1);
                    } else if (key === "0") {
                        return 1n;
                    }
                    return value;
                });
            } catch (e) {
                caught = e instanceof TypeError;
            }
            caught
            "#),
        Value::Bool(true)
    );
}
#[test]
fn reviver_does_not_convert_a_value_for_a_detached_typed_array_index() {
    assert_eq!(
        run(r#"
            var called = false;
            var result = JSON.parse("[0,0]", function(key, value) {
                if (key === "0" && Array.isArray(this)) {
                    this[1] = new Uint8Array(1);
                } else if (key === "0") {
                    this.buffer.transfer();
                    return { valueOf: function() { called = true; throw 1; } };
                }
                return value;
            });
            !called && result[1].length === 0
            "#),
        Value::Bool(true)
    );
}
#[test]
fn raw_json_is_branded_frozen_and_emitted_after_the_replacer() {
    assert_eq!(
        run(r#"
            var observedHint;
            var raw = JSON.rawJSON({
                [Symbol.toPrimitive]: function(hint) {
                    observedHint = hint;
                    return "9007199254740993";
                }
            });
            var desc = Object.getOwnPropertyDescriptor(raw, "rawJSON");
            observedHint === "string" && JSON.isRawJSON(raw) &&
                !JSON.isRawJSON(Object.create(raw)) &&
                !JSON.isRawJSON({ rawJSON: raw.rawJSON }) &&
                Object.getPrototypeOf(raw) === null && !Object.isExtensible(raw) &&
                desc.value === "9007199254740993" &&
                desc.enumerable && !desc.writable && !desc.configurable &&
                JSON.stringify({ toJSON: function() { return raw; } }, function(key, value) {
                    return value === raw ? "seen" : value;
                }) === '"seen"' &&
                JSON.stringify([{}, raw], function(key, value) {
                    return key === "0" ? JSON.rawJSON("-0") : value;
                }) === "[-0,9007199254740993]"
            "#),
        Value::Bool(true)
    );
}
#[test]
fn raw_json_uses_the_primitive_grammar_and_keeps_exact_utf16() {
    let mut vm = Vm::new();
    for text in ["", "1 ", "\t1", "[]", "{}", "+1", "01", "1e+", "1 2"] {
        vm.set_global("text", Value::from(text)).unwrap();
        assert_eq!(
            vm.eval("JSON.rawJSON(text)").unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{text:?}"
        );
    }
    assert_eq!(
        vm.eval("JSON.rawJSON({toString: function() { throw 42; }})")
            .unwrap_err()
            .thrown,
        Some(Value::from(42))
    );
    let value = run(r#"JSON.stringify([JSON.rawJSON('"\ud800"'), JSON.rawJSON('"\\uDFFF"')])"#);
    let mut expected = vec![91, 34, 0xd800, 34, 44];
    expected.extend(r#""\uDFFF"]"#.encode_utf16());
    assert_eq!(value.as_string().unwrap().as_utf16(), expected);
}
#[test]
fn raw_json_preserves_the_parser_work_and_serializer_output_caps() {
    let source = Value::from(format!("\"{}\"", "x".repeat(200)));
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 100,
        ..VmLimits::default()
    });
    vm.set_global("text", source.clone()).unwrap();
    assert_eq!(
        vm.eval("JSON.rawJSON(text)").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );

    let mut vm = Vm::with_limits(VmLimits {
        max_string_units: 300,
        ..VmLimits::default()
    });
    vm.set_global("text", source).unwrap();
    assert_eq!(
        vm.eval("var raw = JSON.rawJSON(text); JSON.stringify(raw).length")
            .unwrap(),
        Value::from(202)
    );
    assert_eq!(
        vm.eval("JSON.stringify([raw, raw])").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
}
#[test]
fn public_json_helpers_ignore_overwritten_global_object() {
    let mut vm = Vm::new();
    vm.eval("JSON=null;").unwrap();
    let value = vm.parse_json(&JsString::from("[1,2]")).unwrap();
    let string = vm.stringify_json(value).unwrap().unwrap();
    assert_eq!(string, JsString::from("[1,2]"));
    assert!(vm.stringify_json(Value::Undefined).unwrap().is_none());
}
#[test]
fn resource_budgets_and_deep_json_do_not_overflow_one_mib_stack() {
    std::thread::Builder::new().stack_size(1024*1024).spawn(||{let mut vm=Vm::new();let deep=format!("{}0{}","[".repeat(100),"]".repeat(100));assert_eq!(vm.parse_json(&JsString::from(deep)).unwrap_err().kind,ErrorKind::ExecutionLimit);let error=vm.eval("var a={};var root=a;for(var i=0;i<100;i++){a.next={};a=a.next;}JSON.stringify(root);").unwrap_err();assert_eq!(error.kind,ErrorKind::ExecutionLimit);assert!(vm.parse_json(&JsString::from("[1,2]")).unwrap().as_object().is_some());}).unwrap().join().unwrap();
}
#[test]
fn json_builders_respect_small_heap_and_work_caps() {
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: 512 * 1024,
        ..VmLimits::default()
    });
    let source = JsString::from(format!(
        "[{}]",
        std::iter::repeat_n("0", 5000).collect::<Vec<_>>().join(",")
    ));
    assert_eq!(
        vm.parse_json(&source).unwrap_err().kind,
        ErrorKind::HeapLimit
    );
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 100,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.parse_json(&JsString::from(format!("\"{}\"", "x".repeat(1000))))
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
}
#[test]
fn host_payloads_are_private_and_drop_with_their_object() {
    use std::rc::Rc;
    let mut vm = Vm::new();
    let data = Rc::new(String::from("private"));
    let weak = Rc::downgrade(&data);
    let object = vm.new_object();
    vm.set_host_data(object.clone(), data).unwrap();
    assert_eq!(&*vm.host_data::<String>(&object).unwrap(), "private");
    vm.set_global("host", object).unwrap();
    assert_eq!(vm.eval("Object.keys(host).length").unwrap(), Value::from(0));
    vm.set_global("host", Value::Null).unwrap();
    vm.collect_garbage();
    assert!(weak.upgrade().is_none());
}
