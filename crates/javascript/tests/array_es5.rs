use rust_browser_javascript::{Value, Vm};

fn check(source: &str) {
    let result = Vm::new()
        .eval(source)
        .unwrap_or_else(|error| panic!("{error}\n{source}"));
    assert_eq!(result, Value::Bool(true), "{source}");
}

#[test]
fn array_search_methods_have_the_expected_descriptors() {
    check(
        r#"
        function checkMethod(name) {
            var property = Object.getOwnPropertyDescriptor(Array.prototype, name);
            var fn = property.value;
            var length = Object.getOwnPropertyDescriptor(fn, 'length');
            var notConstructor = false;
            try { new fn(); } catch (error) { notConstructor = error instanceof TypeError; }
            return property.writable && !property.enumerable && property.configurable &&
                fn.name === name && length.value === 1 && !length.writable &&
                !length.enumerable && length.configurable && notConstructor;
        }
        checkMethod('indexOf') && checkMethod('lastIndexOf')
        "#,
    );
}

#[test]
fn array_search_captures_length_before_coercion_and_reads_in_direction_order() {
    check(
        r#"
        var events = '';
        var proto = {};
        Object.defineProperty(proto, '1', {get: function () { events += 'b'; return 'hit'; }});
        var object = Object.create(proto);
        Object.defineProperty(object, 'length', {get: function () { events += 'l'; return 3; }});
        Object.defineProperty(object, '0', {get: function () { events += 'a'; return 'miss'; }});
        Object.defineProperty(object, '2', {get: function () { events += 'c'; return 'miss'; }});
        var from = {valueOf: function () { events += 'f'; object[3] = 'hit'; return -4; }};
        var index = Array.prototype.indexOf.call(object, 'hit', from);
        var last = Array.prototype.lastIndexOf.call(object, 'hit');
        var poison = {valueOf: function () { throw new Error('unexpected coercion'); }};
        index === 1 && last === 1 && events === 'lfablcb' &&
            [].indexOf('hit', poison) === -1 && [].lastIndexOf('hit', poison) === -1
        "#,
    );
}

#[test]
fn array_search_distinguishes_holes_and_undefined_with_strict_equality() {
    check(
        r#"
        var array = ['hit', , 1, NaN, undefined, 'hit'];
        array.indexOf(undefined) === 4 && array.lastIndexOf(undefined, 3) === -1 &&
            array.indexOf('1') === -1 && array.lastIndexOf(NaN) === -1 &&
            array.indexOf('hit', -1.5) === 5 && array.lastIndexOf('hit', -2.5) === 0 &&
            array.lastIndexOf('hit') === 5 && array.lastIndexOf('hit', undefined) === 0 &&
            1 / array.lastIndexOf('hit', -0) === Infinity &&
            array.indexOf('hit', Infinity) === -1 && array.indexOf('hit', -Infinity) === 0 &&
            array.lastIndexOf('hit', Infinity) === 5 && array.lastIndexOf('hit', -Infinity) === -1 &&
            Array.prototype.indexOf.call('satin', 'a') === 1
        "#,
    );
}

#[test]
fn array_search_budgets_visited_indices_not_generic_length() {
    check(
        r#"
        var limit = Number.MAX_SAFE_INTEGER;
        var array = {length: Infinity};
        array[limit - 2] = 'hit';
        Array.prototype.indexOf.call(array, 'hit', limit - 4) === limit - 2 &&
            Array.prototype.lastIndexOf.call(array, 'hit') === limit - 2
        "#,
    );
}

#[test]
fn array_search_matches_sparse_and_generic_index_tables() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var sparse = [-0, , undefined, '0', NaN, 'needle'];
        var generic = Object.create({1: 'needle', 2: 'shadowed', 5: undefined});
        generic.length = {valueOf: function () { return '6.9'; }};
        generic[0] = 0;
        generic[2] = undefined;
        generic[4] = 'needle';
        generic[6] = 'needle';
        "#,
    )
    .unwrap();

    // Expected matches are listed independently of JS property lookup.
    // Neither the hole at sparse[1] nor generic[3] matches undefined.
    let matches: [(&str, &[i32], &[i32]); 8] = [
        ("undefined", &[2], &[2, 5]),
        ("0", &[0], &[0]),
        ("-0", &[0], &[0]),
        ("'0'", &[3], &[]),
        ("'needle'", &[5], &[1, 4]),
        ("'shadowed'", &[], &[]),
        ("NaN", &[], &[]),
        ("null", &[], &[]),
    ];
    // For normalized length 6, these are the inclusive lower and upper
    // bounds for forward and backward search, respectively.
    let bounds = [
        ("", 0, 5),
        (", undefined", 0, 0),
        (", NaN", 0, 0),
        (", -0", 0, 0),
        (", -0.9", 0, 0),
        (", -2.75", 4, 4),
        (", -6", 0, 0),
        (", -7", 0, -1),
        (", 2.75", 2, 2),
        (", '3'", 3, 3),
        (", 6", 6, 5),
        (", Infinity", 6, 5),
        (", -Infinity", 0, -1),
    ];
    for (needle, sparse, generic) in matches {
        for (receiver, indices) in [("sparse", sparse), ("generic", generic)] {
            for (from, lower, upper) in bounds {
                let first = indices.iter().copied().find(|&index| index >= lower);
                let last = indices.iter().rev().copied().find(|&index| index <= upper);
                for (method, expected) in [("indexOf", first), ("lastIndexOf", last)] {
                    let source =
                        format!("Array.prototype.{method}.call({receiver}, {needle}{from})");
                    let actual = vm
                        .eval(&source)
                        .unwrap_or_else(|error| panic!("{source}: {error}"));
                    let expected = f64::from(expected.unwrap_or(-1));
                    assert_eq!(
                        actual.as_number().map(f64::to_bits),
                        Some(expected.to_bits()),
                        "{source}: expected {expected}, got {actual:?}"
                    );
                }
            }
        }
    }
}
