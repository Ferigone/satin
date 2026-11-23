use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

fn assert_js(source: &str) {
    assert_eq!(
        Vm::new().eval(source).unwrap(),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn uri_and_component_use_distinct_character_sets() {
    assert_js(
        r#"
        var reserved = ";/?:@&=+$,#";
        var escaped = "%3b%2F%3f%3A%40%26%3d%2B%24%2c%23";
        var common = "AZaz09-_.!~*'()";
        encodeURI(reserved + common + "% []") === reserved + common + "%25%20%5B%5D" &&
            encodeURIComponent(reserved + common) === escaped.toUpperCase() + common &&
            decodeURI(escaped + "%41%252f") === escaped + "A%2f" &&
            decodeURIComponent(escaped + "+%25") === reserved + "+%"
        "#,
    );
}

#[test]
fn uri_codecs_translate_between_utf16_and_utf8() {
    assert_js(
        r#"
        var text = "\u0000\u007f\u0080\u07ff\u0800\ufeff\ufffd\ud800\udc00\udbff\udfff";
        var encoded = "%00%7F%C2%80%DF%BF%E0%A0%80%EF%BB%BF%EF%BF%BD%F0%90%80%80%F4%8F%BF%BF";
        encodeURI(text) === encoded && encodeURIComponent(text) === encoded &&
            decodeURI(encoded.toLowerCase()) === text && decodeURIComponent(encoded) === text &&
            decodeURI("\ud800%41\udfff") === "\ud800A\udfff" &&
            decodeURIComponent("\udfff%41\ud800") === "\udfffA\ud800"
        "#,
    );
}

#[test]
fn malformed_uri_errors_have_the_intrinsic_brand() {
    let mut vm = Vm::new();
    for func in ["encodeURI", "encodeURIComponent"] {
        for input in [
            r"\ud800",
            r"\udfff",
            r"\ud800x",
            r"\ud800\ud800",
            r"\udfff\ud800",
        ] {
            let script = format!("{func}('{input}')");
            assert_eq!(
                vm.eval(&script).unwrap_err().kind,
                ErrorKind::UriError,
                "{script}"
            );
        }
    }
    for func in ["decodeURI", "decodeURIComponent"] {
        for input in [
            "%",
            "%0",
            "%xz",
            "%Ｆ0",
            "%u0041",
            "%80",
            "%ff",
            "%c0%80",
            "%c2",
            "%c2x",
            "%c2%20",
            "%e0%9f%bf",
            "%ed%a0%80",
            "%f0%8f%bf%bf",
            "%f4%90%80%80",
            "%f8%88%80%80%80",
        ] {
            let script = format!("{func}('{input}')");
            assert_eq!(
                vm.eval(&script).unwrap_err().kind,
                ErrorKind::UriError,
                "{script}"
            );
        }
    }
    assert_js(
        r#"
        var intrinsic = URIError;
        URIError = function () { throw "replacement must not run"; };
        var branded = false;
        try { decodeURIComponent("%ed%a0%80"); }
        catch (e) {
            branded = e instanceof intrinsic && e instanceof Error &&
                Object.getPrototypeOf(e) === intrinsic.prototype &&
                Object.prototype.toString.call(e) === "[object Error]";
        }
        branded
        "#,
    );
}

#[test]
fn annex_b_codecs_preserve_code_units_and_tolerate_malformed_escapes() {
    assert_js(
        r#"
        var raw = "@*_+-./ é\u00ff\u0100\ud800\udfff\ud800";
        var encoded = "@*_+-./%20%E9%FF%u0100%uD800%uDFFF%uD800";
        escape(raw) === encoded && unescape(encoded) === raw &&
            escape("!~#%") === "%21%7E%23%25" &&
            unescape("%U0041%u12%41%u12xz%aB%00%") === "%U0041%u12A%u12xz\u00ab\u0000%" &&
            unescape("%C3%A9%25u0041") === "\u00c3\u00a9%u0041" &&
            unescape("\udc00") === "\udc00"
        "#,
    );
}

#[test]
fn codecs_are_nonconstructable_plain_globals_and_use_string_hint() {
    assert_js(
        r#"
        (function (global) {
            var names = ["encodeURI", "encodeURIComponent", "decodeURI", "decodeURIComponent", "escape", "unescape"];
            var marker = {};
            for (var i = 0; i < names.length; i++) {
                var name = names[i];
                var prop = Object.getOwnPropertyDescriptor(global, name);
                var f = prop.value;
                if (!prop.writable || prop.enumerable || !prop.configurable ||
                    f.name !== name || f.length !== 1 || f.prototype !== undefined ||
                    f() !== "undefined" || f(null) !== "null" || f(1) !== "1") return false;
                var count = 0;
                var input = {
                    valueOf: function () { throw "wrong hint"; },
                    toString: function () { count++; return "ok"; }
                };
                try { new f(input); return false; }
                catch (e) { if (!(e instanceof TypeError) || count !== 0) return false; }
                var ignored = { toString: function () { throw "extra argument"; } };
                if (f.call(ignored, input, ignored) !== "ok" || count !== 1) return false;
                try { f({ toString: function () { throw marker; } }); return false; }
                catch (e) { if (e !== marker) return false; }
                try { f(Symbol()); return false; }
                catch (e) { if (!(e instanceof TypeError)) return false; }
            }
            return true;
        })(this)
        "#,
    );
}

#[test]
fn codecs_respect_output_length_and_native_work_budgets() {
    let mut vm = Vm::with_limits(VmLimits {
        max_string_units: 128,
        ..VmLimits::default()
    });
    vm.set_global("s", Value::from("€".repeat(64))).unwrap();
    assert_eq!(
        vm.eval("encodeURI(s)").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(
        vm.eval("escape(s)").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(
        vm.eval("decodeURI(s)").unwrap(),
        Value::from("€".repeat(64))
    );

    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 6000,
        ..VmLimits::default()
    });
    vm.set_global("s", Value::from("x".repeat(4096))).unwrap();
    for func in ["encodeURI", "decodeURIComponent", "escape", "unescape"] {
        assert_eq!(
            vm.eval(&format!("{func}(s)")).unwrap_err().kind,
            ErrorKind::ExecutionLimit
        );
    }
}
