use rust_browser_javascript::{ErrorKind, JsString, Value, Vm, VmLimits};

#[test]
fn invalid_unicode_property_escapes_are_syntax_errors() {
    let mut vm = Vm::new();
    for source in [
        r"/\p/u",
        r"/\p{}/u",
        r"/\P{General_Category=}/u",
        r"/\p{Script=latn}/u",
        r"/\p{Script=Lu}/u",
        r"/\p{General_Category=Latn}/u",
        r"/\p{Ascii}/u",
        r"/\P{ASCII=Y}/u",
        r"/\p{Hyphen}/u",
        r"/\p{Basic_Emoji}/u",
        r"/[\p{Hex}--]/u",
        r"/[a-\p{Hex}]/u",
        r"/[\p{Alphabetic}-a]/u",
        r"/\p{ASCII}\q/u",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}",
        );
    }
    assert_eq!(
        vm.eval(r#"new RegExp("\\p{Script=not_a_script}", "u")"#)
            .unwrap_err()
            .kind,
        ErrorKind::SyntaxError,
    );
}

#[test]
fn valid_unicode_property_escapes_still_report_unimplemented_matching() {
    let mut vm = Vm::new();
    for source in [
        r"/\p{Alpha}/u",
        r"/\p{Lu}/u",
        r"/\P{General_Category=Uppercase_Letter}/u",
        r"/[\p{sc=Latn}-]/u",
        r"/\p{scx=Qaai}/u",
        r"/[\p{Hex}\p{Alphabetic}]/u",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::UnsupportedFeature,
            "{source}",
        );
    }
    assert!(vm.check_syntax(r"/\p{Basic_Emoji}/").is_ok());
}

#[test]
fn hexadecimal_property_escapes_match_exact_unicode_sets() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            [
                /^\p{Hex}+$/u.test('0Ａｆ９'),
                /^\p{AHex}+$/u.test('0Ａ'),
                /\p{Hex_Digit}/u.test('٠𝟘'),
                /\P{AHex}/iu.test('Ａ'),
                /\P{AHex}/iu.test('f'),
                /[^\P{Hex}]/iu.test('Ｆ'),
                /[^\p{Hex}]/u.test('G'),
                /^[\p{Hex}\P{Hex}]+$/u.test('\ud800𐌰'),
                new RegExp('^\\p{ASCII_Hex_Digit}+$', 'u').test('0fF'),
                /^\p{Hex}$/.test('p{Hex}')
            ].join(',');
            "#,
        )
        .unwrap(),
        "true,false,false,true,false,true,true,true,true,true".into()
    );
}

#[test]
fn hexadecimal_property_matching_keeps_the_native_work_budget() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1_000,
        ..VmLimits::default()
    });
    vm.set_global("hexInput", Value::String(JsString::from("f".repeat(4096))))
        .unwrap();
    assert_eq!(
        vm.eval(r"/^\p{Hex}+$/u.test(hexInput)").unwrap_err().kind,
        ErrorKind::ExecutionLimit,
    );
}

#[test]
fn binary_property_names_and_aliases_require_exact_spelling() {
    let mut vm = Vm::new();
    for name in [
        "Hex",
        "Hex_Digit",
        "AHex",
        "ASCII_Hex_Digit",
        "SD",
        "Soft_Dotted",
    ] {
        for escape in ['p', 'P'] {
            vm.check_syntax(&format!(r"/\{escape}{{{name}}}/iu"))
                .unwrap();
            for invalid in [
                name.to_ascii_lowercase(),
                format!("{name}=Yes"),
                format!("gc={name}"),
                format!(" {name}"),
                format!("{name} "),
            ] {
                assert_eq!(
                    vm.check_syntax(&format!(r"/\{escape}{{{invalid}}}/iu"))
                        .unwrap_err()
                        .kind,
                    ErrorKind::SyntaxError,
                    "{escape}{{{invalid}}}",
                );
            }
        }
    }
}

#[test]
fn soft_dotted_properties_complement_before_unicode_case_folding() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            [
                /^\p{SD}+$/u.test('ij\u{1D456}\u{1E068}'),
                /\p{Soft_Dotted}/u.test('I\u0131\u{1E067}'),
                /\p{SD}/iu.test('I'),
                /\P{Soft_Dotted}/u.test('i'),
                /\P{Soft_Dotted}/u.test('I'),
                /\P{SD}/iu.test('i'),
                /[^\P{SD}]/iu.test('i'),
                /[^\p{SD}]/iu.test('I'),
                /[^\P{SD}]/iu.test('\u{1D456}')
            ].join(',');
            "#,
        )
        .unwrap(),
        "true,false,true,false,true,true,false,false,true".into()
    );
}
