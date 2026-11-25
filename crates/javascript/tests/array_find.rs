use rust_browser_javascript::{Value, Vm};

fn yes(source: &str) {
    assert_eq!(
        Vm::new().eval(source).unwrap(),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn find_and_find_index_validate_after_length_coercion_and_can_stop_immediately() {
    yes(r#"
        var events = '';
        var source = {
            get length() {
                events += 'length,';
                return { valueOf: function () { events += 'coerce,'; return Infinity; } };
            },
            get 0() { events += 'get,'; return 'first'; },
            get 1() { throw new Error('read after matching'); }
        };
        var rejected;
        try { Array.prototype.find.call(source, 0); } catch (error) { rejected = error; }
        function predicate(value, index, object) {
            events += 'call,';
            return value === 'first' && index === 0 && object === source;
        }
        var found = Array.prototype.find.call(source, predicate);
        var index = Array.prototype.findIndex.call(source, predicate);
        rejected instanceof TypeError && found === 'first' && index === 0 &&
            events === 'length,coerce,length,coerce,get,call,length,coerce,get,call,'
    "#);
}

#[test]
fn find_from_last_visits_holes_and_observes_live_properties_in_descending_order() {
    yes(r#"
        function check(method, expected) {
            var prototype = {};
            var source = Object.create(prototype);
            var thisArg = {};
            source.length = 5.8;
            source[4] = 'start';
            source[1] = 'to delete';
            Object.defineProperty(prototype, '2', {
                get: function () {
                    if (this !== source) throw new Error('wrong getter receiver');
                    return 'inherited';
                }
            });
            var visits = '';
            var found = Array.prototype[method].call(source, function (value, index, object) {
                'use strict';
                if (this !== thisArg || object !== source) throw new Error('wrong predicate arguments');
                visits += index + ':' + value + ',';
                if (index === 4) {
                    delete source[1];
                    source[0] = 'end';
                    source[5] = 'outside initial length';
                    source.length = 1;
                }
                if (index === 0) source[0] = 'changed during match';
                return index === 0;
            }, thisArg);
            return found === expected && visits === '4:start,3:undefined,2:inherited,1:undefined,0:end,';
        }
        check('findLast', 'end') && check('findLastIndex', 0)
    "#);
}

#[test]
fn find_from_last_visits_the_highest_safe_hole_before_any_lower_index() {
    yes(r#"
        var source = { length: Number.MAX_VALUE };
        var highest = Number.MAX_SAFE_INTEGER - 1;
        Object.defineProperty(source, String(highest - 1), {
            get: function () { throw new Error('read after matching'); }
        });
        var calls = 0;
        function predicate(value, index, object) {
            if (value !== undefined || index !== highest || object !== source) {
                throw new Error('wrong highest-index visit');
            }
            calls++;
            return {};
        }
        Array.prototype.findLast.call(source, predicate) === undefined &&
            Array.prototype.findLastIndex.call(source, predicate) === highest && calls === 2
    "#);
}
