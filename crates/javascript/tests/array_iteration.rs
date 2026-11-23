use rust_browser_javascript::{ErrorKind, Value, Vm};

fn yes(source: &str) {
    assert_eq!(
        Vm::new().eval(source).unwrap(),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn callback_validation_follows_length_coercion_and_precedes_output_creation() {
    yes(r#"
        var events = '';
        var object = {
            get length() {
                events += 'length,';
                return { valueOf: function () { events += 'valueOf,'; return Infinity; } };
            },
            get 0() { throw new Error('unexpected indexed read'); }
        };
        var invalid;
        try { Array.prototype.map.call(object, null); } catch (error) { invalid = error; }
        var impossible;
        try { Array.prototype.map.call(object, function () {}); }
        catch (error) { impossible = error; }
        invalid instanceof TypeError && impossible instanceof RangeError &&
            events === 'length,valueOf,length,valueOf,'
    "#);
}

#[test]
fn visit_snapshots_length_but_resolves_each_index_and_preserves_holes() {
    yes(r#"
        var object = Object.create({1: 'inherited'});
        object.length = 4.9;
        object[0] = 'first';
        object[2] = 'removed';
        var receiver = {};
        var visited = '';
        var result = Array.prototype.map.call(object, function (value, index, source) {
            if (source !== object || this !== receiver) throw new Error('wrong arguments');
            visited += index + ':' + value + ',';
            if (index === 0) {
                delete source[2];
                source[3] = 'inserted';
                source[4] = 'past initial length';
                source.length = 5;
            }
            return value;
        }, receiver);
        var filtered = Array.prototype.filter.call(result, function (value) {
            return value !== 'first';
        });
        visited === '0:first,1:inherited,3:inserted,' && result.length === 4 &&
            !(2 in result) && result.hasOwnProperty(1) && !(4 in result) &&
            filtered.join(',') === 'inherited,inserted'
    "#);
}

#[test]
fn large_array_like_visits_can_short_circuit_but_full_scans_are_metered() {
    yes(r#"
        var object = {
            length: Infinity,
            0: 'first',
            get 1() { throw new Error('should have short circuited'); }
        };
        Array.prototype.some.call(object, function (value) { return value === 'first'; }) &&
            !Array.prototype.every.call(object, function () { return false; })
    "#);
    let mut vm = Vm::with_limits(rust_browser_javascript::VmLimits {
        max_instructions: 1_000,
        ..Default::default()
    });
    let error = vm
        .eval("Array.prototype.forEach.call({ length: Infinity }, function () {});")
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
}

#[test]
fn reduction_direction_and_initial_value_use_present_properties() {
    yes(r#"
        var object = Object.create({3: 'inherited'});
        object.length = 6;
        object[1] = 'first';
        object[4] = 'last';
        var visited = '';
        var result = Array.prototype.reduceRight.call(object, function (acc, value, index, source) {
            'use strict';
            if (source !== object || this !== undefined) throw new Error('wrong arguments');
            visited += index + ',';
            return acc + ':' + value;
        });
        var explicit = [, 'value'].reduceRight(function (acc, value, index) {
            return acc === undefined && value === 'value' && index === 1;
        }, undefined);
        var boxed;
        var joined = Array.prototype.reduce.call('ab', function (acc, value, index, source) {
            boxed = source;
            return acc + value;
        }, '');
        var empty;
        try { Array(3).reduceRight(function () {}); }
        catch (error) { empty = error; }
        visited === '3,1,' && result === 'last:inherited:first' && explicit &&
            joined === 'ab' && boxed instanceof String && empty instanceof TypeError
    "#);
}

#[test]
fn reduce_right_uses_exact_indices_near_the_safe_integer_limit() {
    yes(r#"
        var limit = Number.MAX_SAFE_INTEGER;
        var target = { length: Infinity };
        target[limit - 1] = 'top';
        target[limit - 3] = 'third';
        var sentinel = {};
        var trace = '';
        var thrown;
        try {
            Array.prototype.reduceRight.call(target, function (acc, value, index) {
                if (index === limit - 1) return value;
                if (index !== limit - 3) throw new Error('visited an unexpected index');
                trace = acc + ':' + value;
                throw sentinel;
            }, undefined);
        } catch (error) { thrown = error; }
        thrown === sentinel && trace === 'top:third'
    "#);
}
