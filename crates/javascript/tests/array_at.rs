use rust_browser_javascript::{Value, Vm, VmLimits};

fn check(source: &str) {
    for prefix in ["", "'use strict';\n"] {
        let mut vm = Vm::with_limits(VmLimits {
            max_instructions: 10_000,
            ..VmLimits::default()
        });
        assert_eq!(
            vm.eval(&format!("{prefix}{source}")).unwrap(),
            Value::Bool(true)
        );
    }
}

#[test]
fn array_at_treats_negative_zero_as_zero_and_infinities_as_out_of_range() {
    check(
        r#"
        var at = Array.prototype.at, reads = 0;
        var object = Object.create({
            get 0() { reads++; return 'first'; },
            get '-0'() { throw Error('read minus zero property'); },
            get 'Infinity'() { throw Error('read positive infinity property'); },
            get '-Infinity'() { throw Error('read negative infinity property'); }
        });
        object.length = 2;
        object[1] = 'last';
        at.call(object, -0) === 'first' &&
            at.call(object, -0.75) === 'first' &&
            at.call(object, NaN) === 'first' &&
            at.call(object, -1.9) === 'last' &&
            at.call(object, Infinity) === undefined &&
            at.call(object, -Infinity) === undefined &&
            reads === 3
        "#,
    );
}

#[test]
fn array_at_addresses_negative_indices_near_the_maximum_safe_length() {
    check(
        r#"
        var at = Array.prototype.at, limit = Number.MAX_SAFE_INTEGER;
        var object = {
            length: Infinity, 0: 'first',
            '9007199254740989': 'penultimate', '9007199254740990': 'last',
            get '9007199254740991'() { throw Error('read past the clamped length'); },
            get '-1'() { throw Error('read a negative property'); }
        };
        at.call(object, -1) === 'last' &&
            at.call(object, -2.9) === 'penultimate' &&
            at.call(object, -limit) === 'first' &&
            at.call(object, -(limit + 1)) === undefined &&
            at.call(object, limit) === undefined
        "#,
    );
}

#[test]
fn array_at_snapshots_length_before_coercing_index_then_reads_the_live_property() {
    check(
        r#"
        var at = Array.prototype.at, trace = '', length = 2.9, value = 'stale';
        var lengthValue = {valueOf: function () { trace += 'n'; return length; }};
        var object = Object.create({get 1() { trace += 'g'; return this === object && value; }});
        Object.defineProperty(object, 'length', {get: function () { trace += 'l'; return lengthValue; }});
        var index = {valueOf: function () { trace += 'i'; length = 0; value = 'live'; return -1; }};
        at.call(object, index) === 'live' && trace === 'lnig'
        "#,
    );
}

#[test]
fn array_at_propagates_coercion_errors_before_any_indexed_get() {
    check(
        r#"
        var at = Array.prototype.at, lengthError = {}, indexError = {}, trace = '';
        var caughtLength = false, caughtIndex = false, caughtNull = false;
        var index = {valueOf: function () { trace += 'i'; throw indexError; }};
        var badLength = {get length() { trace += 'l'; throw lengthError; }};
        var empty = {get length() { trace += 'e'; return 0; }, get 0() { trace += 'g'; }};
        try { at.call(null, index); } catch (error) { caughtNull = error instanceof TypeError; }
        try { at.call(badLength, index); } catch (error) { caughtLength = error === lengthError; }
        try { at.call(empty, index); } catch (error) { caughtIndex = error === indexError; }
        caughtNull && caughtLength && caughtIndex && trace === 'lei'
        "#,
    );
}
