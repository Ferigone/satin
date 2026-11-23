use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

#[test]
fn lowercases_sigma_in_the_context_of_the_original_code_points() {
    let mut vm = Vm::new();
    for method in ["toLowerCase", "toLocaleLowerCase"] {
        for (input, expected) in [
            (r"\u03A3", "σ"),
            (r"\u{1D4A2}\u03A3.\u180E", "𝒢ς.\u{180e}"),
            (r"A\u03A3\u{1D242}\u{1D4A2}", "aσ\u{1d242}𝒢"),
            (r"\u0345\u03A3\u0345", "\u{0345}σ\u{0345}"),
            (r"A\u0345\u03A3\u0345", "a\u{0345}ς\u{0345}"),
            (r"A\u03A3\u03A3 A\u03A31b", "aσς aς1b"),
        ] {
            assert_eq!(
                vm.eval(&format!("'{input}'.{method}()")).unwrap(),
                Value::from(expected),
                "{method}: {input}",
            );
        }
    }
}

#[test]
fn sigma_context_matches_the_unicode_version_of_scalar_case_mappings() {
    assert_eq!(char::UNICODE_VERSION, (17, 0, 0));
    let mut vm = Vm::new();
    for method in ["toLowerCase", "toLocaleLowerCase"] {
        // U+A7DD and U+1ADE have no contextual properties in Unicode 17.
        // U+A7DC is already cased in Unicode 17 and lowercases to U+019B.
        for (input, expected) in [
            (r"\uA7DD\u03A3", "\u{a7dd}σ"),
            (r"A\u03A3\u1ADEB", "aς\u{1ade}b"),
            (r"\uA7DC\u03A3", "\u{019b}ς"),
        ] {
            assert_eq!(
                vm.eval(&format!("'{input}'.{method}()")).unwrap(),
                Value::from(expected),
                "{method}: {input}",
            );
        }
    }
}

#[test]
fn locale_case_uses_the_default_mapping_without_reading_reserved_arguments() {
    let mut vm = Vm::new();
    let result = vm
        .eval(
            r#"
            String.prototype.toLowerCase = String.prototype.toUpperCase =
                function () { throw 'unexpected public method lookup'; };
            const unused = { get length() { throw 'unexpected locale lookup'; } };
            [
                'I\u0130'.toLocaleLowerCase('tr'),
                'i\u0131'.toLocaleUpperCase('tr'),
                'I\u0301'.toLocaleLowerCase('lt'),
                'I'.toLocaleLowerCase(unused, unused),
                'i'.toLocaleUpperCase(unused, unused)
            ].join('|')
            "#,
        )
        .unwrap();
    assert_eq!(result, Value::from("ii\u{307}|II|i\u{301}|i|I"));
}

#[test]
fn case_mapping_expands_scalars_and_preserves_lone_utf16_units() {
    let mut vm = Vm::new();
    for method in ["toLowerCase", "toLocaleLowerCase"] {
        let result = vm
            .eval(&format!(
                r"'\u0130\u03A3\uD800A\uDC00\u03A3\uD801\uDC00'.{method}()"
            ))
            .unwrap();
        assert_eq!(
            result.as_string().unwrap().as_utf16(),
            &[
                0x69, 0x307, 0x3c2, 0xd800, 0x61, 0xdc00, 0x3c3, 0xd801, 0xdc28
            ],
            "{method}",
        );
    }
    for method in ["toUpperCase", "toLocaleUpperCase"] {
        let result = vm
            .eval(&format!(
                r"'\uDC00\u00DF\uFB03\uD800\uD801\uDC28'.{method}()"
            ))
            .unwrap();
        assert_eq!(
            result.as_string().unwrap().as_utf16(),
            &[0xdc00, 0x53, 0x53, 0x46, 0x46, 0x49, 0xd800, 0xd801, 0xdc00],
            "{method}",
        );
    }
}

#[test]
fn case_methods_coerce_receivers_and_ignore_unused_arguments() {
    let mut vm = Vm::new();
    for (method, expected) in [
        ("toLowerCase", "a"),
        ("toUpperCase", "A"),
        ("toLocaleLowerCase", "a"),
        ("toLocaleUpperCase", "A"),
    ] {
        let result = vm
            .eval(&format!(
                r#"(() => {{
                    const convert = String.prototype.{method};
                    const receiver = new String('not this value');
                    let calls = 0;
                    receiver[Symbol.toPrimitive] = hint => {{
                        if (hint !== 'string') throw 'wrong hint';
                        calls++;
                        return 'a';
                    }};
                    const unused = {{toString() {{ throw 'unused argument' }}}};
                    const result = convert.call(receiver, unused);
                    return [convert.name, convert.length, calls, result].join(':');
                }})()"#
            ))
            .unwrap();
        assert_eq!(result, Value::from(format!("{method}:0:1:{expected}")));
        for receiver in ["undefined", "null", "Symbol()"] {
            assert_eq!(
                vm.eval(&format!("String.prototype.{method}.call({receiver})"))
                    .unwrap_err()
                    .kind,
                ErrorKind::TypeError,
            );
        }
    }
}

#[test]
fn case_mapping_respects_output_size_and_work_limits() {
    let mut vm = Vm::with_limits(VmLimits {
        max_string_units: 64,
        ..VmLimits::default()
    });
    vm.set_global("s", Value::from("ß".repeat(33))).unwrap();
    assert_eq!(
        vm.eval("s.toLocaleUpperCase()").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );

    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 2_000,
        ..VmLimits::default()
    });
    vm.set_global("s", Value::from(format!("AΣ{}B", "\u{345}".repeat(2048))))
        .unwrap();
    assert_eq!(
        vm.eval("s.toLowerCase()").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
}
