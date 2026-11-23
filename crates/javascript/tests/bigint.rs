use rust_browser_javascript::{ErrorKind, JsBigInt, Value, Vm, VmLimits};

fn yes(source: &str) {
    assert_eq!(
        Vm::new().eval(source).unwrap(),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn literals_radices_separators_and_exact_property_names() {
    yes(r#"
        const records = { 0xff_ffn: 'hex', 0b10n: 'binary', 9007199254740993n: 'exact' };
        class Keys { 0o1_0n() { return 8n; } }
        typeof 0n === 'bigint' && 0xff_ffn === 65_535n && 0b1_01n === 5n
          && 0o1_23n === 83n && BigInt(1_000.2_5e0_2) === 100025n
          && records[65535n] === 'hex' && records[2] === 'binary'
          && records['9007199254740993'] === 'exact' && new Keys()['8']() === 8n
    "#);
    let mut vm = Vm::new();
    for invalid in [
        "01n",
        "00n",
        "08n",
        "0_1n",
        "0_1",
        "01_1",
        "1__2n",
        "1_n",
        "1.n",
        ".1n",
        "1.0n",
        "1e2n",
        "0b2n",
        "0x_n",
        "0o_7n",
        "0x",
        "0b1_n",
        "1n1",
        "0xfnvalue",
        "1n\\u0061",
        "1._2",
        "1e_2",
        "1e+_2",
    ] {
        assert_eq!(
            vm.check_syntax(invalid).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{invalid}"
        );
    }
}

#[test]
fn exact_arithmetic_signs_bitwise_and_large_shift_counts() {
    yes(r#"
        const a = (1n << 130n) - 1n;
        const wide = 9223372036854775808n;
        a === 1361129467683753853853498429727072845823n
          && (a / 7n) * 7n + a % 7n === a && (wide * wide) === 1n << 126n
          && -9n / 4n === -2n && -9n % 4n === -1n && 9n % -4n === 1n
          && (-3n) ** 7n === -2187n && 0n ** 0n === 1n
          && (-1n) ** ((1n << 128n) + 1n) === -1n
          && (~a) === -(1n << 130n) && (a & -2n) === a - 1n
          && ((1n << 129n) | 3n) === (1n << 129n) + 3n
          && (a ^ -1n) === ~a && (-9n >> 2n) === -3n
          && (16n << -2n) === 4n && (1n >> -70n) === 1n << 70n
          && (-9n >> (1n << 100n)) === -1n && (0n << (1n << 100n)) === 0n
    "#);
}

#[test]
fn mixed_number_comparisons_are_exact_and_string_parsing_preserves_order() {
    yes(r#"
        const precise = 9007199254740993n;
        const large = (1n << 100n) + 1n;
        precise > 9007199254740992 && !(precise == 9007199254740993)
          && precise == ' 9007199254740993 ' && precise !== 9007199254740992
          && large > 2 ** 100 && !(large == Number(large))
          && 0n == -0 && 0n == '' && 1n == true && 1n == Object(1n)
          && 0n < 0.25 && 0n > -0.25 && -1n > -1.1 && -2n < -1.9
          && 1n < Infinity && -1n > -Infinity && !(1n == Infinity)
          && !(1n >= NaN) && !(NaN <= 1n) && !(1n == NaN)
          && 255n == '0xff' && 255n > '0o77' && '00256' > 255n
          && !(1n == '1e0') && !(1n <= '1.0') && !('1.0' >= 1n)
          && !(255n == '-0xff') && !(1n == '1_0')
    "#);
}

#[test]
fn update_operators_and_coercion_order_keep_bigint_values() {
    yes(r#"
        let x = 9007199254740993n, order = [];
        const first = x++, second = --x;
        const left = { [Symbol.toPrimitive](hint) { order.push('left ' + hint); return 6n; } };
        const right = { [Symbol.toPrimitive](hint) { order.push('right ' + hint); return 2n; } };
        const product = left * right;
        const text = left + 'cats';
        const object = Object(3n);
        object.n = object.valueOf();
        object.n *= 7n;
        first === 9007199254740993n && second === first && x === first
          && product === 12n && text === '6cats' && object.n === 21n
          && order.join() === 'left number,right number,left default'
    "#);
    let mut vm = Vm::new();
    for (source, kind) in [
        ("1n + 1", ErrorKind::TypeError),
        ("1 - 1n", ErrorKind::TypeError),
        ("~Symbol()", ErrorKind::TypeError),
        ("+1n", ErrorKind::TypeError),
        ("1n >>> 1n", ErrorKind::TypeError),
        ("1n >>> 1", ErrorKind::TypeError),
        ("1n << 1", ErrorKind::TypeError),
        ("Math.abs(1n)", ErrorKind::TypeError),
        ("1n / 0n", ErrorKind::RangeError),
        ("0n % 0n", ErrorKind::RangeError),
        ("1n ** -1n", ErrorKind::RangeError),
    ] {
        assert_eq!(vm.eval(source).unwrap_err().kind, kind, "{source}");
    }
}

#[test]
fn bigints_construct_only_through_conversion_and_have_real_wrapper_slots() {
    yes(r#"
        const a = Object(18446744073709551615n);
        const empty = Reflect.construct(Object, [], BigInt);
        let touched = 0, errors = 0;
        try { new BigInt({ valueOf() { touched++; return 1; } }); }
        catch(e) { if (e instanceof TypeError) errors++; }
        try { BigInt.prototype.valueOf.call(empty); } catch(e) { if (e instanceof TypeError) errors++; }
        try { BigInt.prototype.valueOf(); } catch(e) { if (e instanceof TypeError) errors++; }
        touched === 0 && errors === 3 && a.valueOf() === 18446744073709551615n
          && Object.getPrototypeOf(empty) === BigInt.prototype && a instanceof BigInt
          && Object.prototype.toString.call(a) === '[object BigInt]'
          && Object.prototype.toString.call(1n) === '[object BigInt]'
          && BigInt.length === 1 && BigInt.prototype.toString.length === 0
          && a.toString(16) === 'ffffffffffffffff' && (-35n).toString(36) === '-z'
          && BigInt(true) === 1n && BigInt(false) === 0n && BigInt('  -009 ') === -9n
          && BigInt('\ufeff\u00a0') === 0n && BigInt('0O17') === 15n
          && BigInt(9007199254740993) === 9007199254740992n
          && Number(18446744073709551615n) === 2 ** 64
          && Number(BigInt(Number.MAX_VALUE)) === Number.MAX_VALUE
          && new Number(1n << 2000n).valueOf() === Infinity
    "#);
    let mut vm = Vm::new();
    for (source, kind) in [
        ("BigInt()", ErrorKind::TypeError),
        ("BigInt(null)", ErrorKind::TypeError),
        ("BigInt(Symbol())", ErrorKind::TypeError),
        ("BigInt(1.5)", ErrorKind::RangeError),
        ("BigInt(Infinity)", ErrorKind::RangeError),
        ("BigInt('0xn')", ErrorKind::SyntaxError),
        ("BigInt('+0b1')", ErrorKind::SyntaxError),
        ("BigInt('1e3')", ErrorKind::SyntaxError),
        ("BigInt('2_0')", ErrorKind::SyntaxError),
        (r"BigInt('\u0085')", ErrorKind::SyntaxError),
        ("(1n).toString(37)", ErrorKind::RangeError),
        ("(1n).toString(10n)", ErrorKind::TypeError),
    ] {
        assert_eq!(vm.eval(source).unwrap_err().kind, kind, "{source}");
    }
}

#[test]
fn modular_wrapping_keeps_signed_edges_and_conversion_order() {
    yes(r#"
        const max = (1n << 64n) - 1n;
        let order = [];
        const bits = { valueOf() { order.push('bits'); return 0; } };
        const value = { valueOf() { order.push('value'); return 3n; } };
        BigInt.asUintN(64, -1n) === max && BigInt.asIntN(64, max) === -1n
          && BigInt.asIntN(64, 1n << 63n) === -(1n << 63n)
          && BigInt.asIntN(1, 1n) === -1n && BigInt.asUintN(5, '-7') === 25n
          && BigInt.asUintN(9007199254740991, 12n) === 12n
          && BigInt.asIntN(9007199254740991, -12n) === -12n
          && BigInt.asUintN(bits, value) === 0n && order.join() === 'bits,value'
    "#);
    let mut vm = Vm::new();
    for (source, kind) in [
        ("BigInt.asIntN(-1,0n)", ErrorKind::RangeError),
        ("BigInt.asIntN(1n,0n)", ErrorKind::TypeError),
        ("BigInt.asUintN(0,0)", ErrorKind::TypeError),
        ("BigInt.asUintN(1, null)", ErrorKind::TypeError),
    ] {
        assert_eq!(vm.eval(source).unwrap_err().kind, kind, "{source}");
    }
}

#[test]
fn collections_json_and_structured_clone_keep_value_types() {
    let mut vm = Vm::new();
    let source = vm
        .eval(
            r#"
        var n = (1n << 128n) - 1n;
        var map = new Map([[n, 'n'], [1n, 'big'], [1, 'number']]);
        var set = new Set([n, 1n, 1]);
        ({ n: n, boxed: Object(n), map: map, set: set })
    "#,
        )
        .unwrap();
    let copy = vm.structured_clone(source, &[]).unwrap();
    vm.set_global("copy", copy).unwrap();
    assert_eq!(
        vm.eval(
            r#"
        copy.n === n && copy.boxed.valueOf() === n
          && copy.map.get((1n << 128n) - 1n) === 'n' && copy.map.get(1n) === 'big'
          && copy.map.get(1) === 'number' && copy.set.has(n) && copy.set.size === 3
          && [n].includes((1n << 128n) - 1n) && [1n, 1].indexOf(1n) === 0
          && Object.is(copy.n, n) && !Object.is(1, 1n)
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        vm.eval("JSON.stringify(Object(n))").unwrap_err().kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        vm.eval("JSON.stringify({n})").unwrap_err().kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        vm.eval(
            r#"
        const keys = [];
        BigInt.prototype.toJSON = function(key) {
            'use strict';
            keys.push(typeof this + ':' + key);
            return this.toString(16);
        };
        JSON.stringify([255n, 256n]) === '["ff","100"]' && keys.join() === 'bigint:0,bigint:1'
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn native_integer_bridge_keeps_exact_low_bits_and_u64_identity() {
    let mut vm = Vm::new();
    let high = vm.bigint_from_u64(u64::MAX).unwrap();
    assert_eq!(high, Value::BigInt(JsBigInt::from_u64(u64::MAX)));
    assert_eq!(vm.to_bigint_low_u64(&high).unwrap(), u64::MAX);
    assert_eq!(
        vm.to_bigint_low_u64(&Value::from("-18446744073709551617"))
            .unwrap(),
        u64::MAX
    );
    let minimum = vm.bigint_from_i64(i64::MIN).unwrap();
    assert_eq!(minimum.as_bigint().unwrap().bit_length(), 64);
    assert_eq!(vm.to_bigint_low_u64(&minimum).unwrap(), 1u64 << 63);
    assert_eq!(
        vm.to_bigint_low_u64(&Value::from(1)).unwrap_err().kind,
        ErrorKind::TypeError
    );
}

#[test]
fn arithmetic_source_work_and_gc_storage_are_bounded() {
    let mut vm = Vm::new();
    let too_wide = format!("0b1{}n", "0".repeat(65_536));
    assert_eq!(
        vm.check_syntax(&too_wide).unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    for source in ["1n << (1n << 80n)", "(2n ** (1n << 80n))"] {
        assert_eq!(vm.eval(source).unwrap_err().kind, ErrorKind::ExecutionLimit);
    }
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1024,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("var x = 1n << 4096n; x * x").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    let mut vm = Vm::new();
    vm.eval("var keep = []; for (var i=0; i<200; i++) keep.push((1n<<2048n) + BigInt(i));")
        .unwrap();
    vm.collect_garbage();
    let bytes = vm.estimated_heap_bytes();
    vm.eval("keep = null").unwrap();
    vm.collect_garbage();
    assert!(bytes.saturating_sub(vm.estimated_heap_bytes()) > 75_000);
}
