use rust_browser_javascript::{Value, Vm};

fn yes(source: &str) {
    assert_eq!(Vm::new().eval(source).unwrap(), Value::Bool(true), "{source}");
}

#[test]
fn apply_checks_callability_before_reading_arguments_and_rejects_primitives() {
    yes(r#"
        const apply = Function.prototype.apply;
        let touched = false;
        const args = { get length() { touched = true; throw 'length'; } };
        let rejected = 0;
        try { apply.call({}, null, args); }
        catch (e) { if (e instanceof TypeError) rejected++; }
        for (const value of [true, 1, 'x', Symbol(), 0n]) {
            try { apply.call(function () {}, null, value); }
            catch (e) { if (e instanceof TypeError) rejected++; }
        }
        rejected === 6 && !touched
    "#);
}

#[test]
fn call_and_apply_forward_receiver_and_apply_reads_array_like_in_order() {
    yes(r#"
        const receiver = {};
        let log = '';
        function target(a, b, c) {
            'use strict';
            log += 'call;';
            return this === receiver && a === 5 && b === undefined && c === 7;
        }
        const proto = { get 0() { log += '0;'; return 5; } };
        const args = {
            __proto__: proto,
            get length() { log += 'length;'; return { valueOf() { log += 'number;'; return 3.8; } }; },
            get 2() { log += '2;'; return 7; }
        };
        const called = target.call(receiver, 5, undefined, 7);
        const applied = target.apply(receiver, args);
        const count = function () { return arguments.length; };
        called && applied && log === 'call;length;number;0;2;call;' &&
            count.apply(null, null) === 0 && count.apply(null) === 0
    "#);
}

#[test]
fn bind_uses_own_numeric_length_and_preserves_string_name_units() {
    yes(r#"
        const bind = Function.prototype.bind;
        let log = '';
        const poison = { valueOf() { throw 'length coercion'; }, toString() { throw 'name coercion'; } };
        function target() {}
        const proto = {
            get length() { throw 'inherited length'; },
            get name() { log += 'name;'; return '\uD800'; }
        };
        Object.setPrototypeOf(target, proto);
        delete target.length;
        delete target.name;
        const inherited = bind.call(target, null);
        Object.defineProperty(target, 'length', { get() { log += 'length;'; return poison; }, configurable: true });
        const notNumber = bind.call(target, null);
        Object.defineProperty(target, 'length', { value: Infinity });
        Object.defineProperty(target, 'name', { value: poison });
        const infinite = bind.call(target, null, 1);
        Object.defineProperty(target, 'length', { value: -Infinity });
        const negative = bind.call(target, null);
        Object.defineProperty(target, 'length', { value: 1e30 });
        const large = bind.call(target, null, 1);
        log === 'name;length;name;' &&
            Object.getPrototypeOf(inherited) === proto && inherited.length === 0 &&
            inherited.name === 'bound \uD800' && notNumber.length === 0 &&
            infinite.name === 'bound ' && infinite.length === Infinity &&
            1 / negative.length === Infinity && large.length === 1e30
    "#);
}

#[test]
fn bound_calls_and_construction_have_distinct_receiver_and_new_target_rules() {
    yes(r#"
        const receiver = {};
        function Target(a, b, c) {
            'use strict';
            this.result = a + b + c;
            this.newTarget = new.target;
        }
        const first = Target.bind(receiver, 'a');
        const second = first.bind({}, 'b');
        second.call({}, 'c');
        const called = receiver.result === 'abc' && receiver.newTarget === undefined;
        const constructed = new second('d');
        function Alternate() {}
        const alternate = Reflect.construct(second, ['e'], Alternate);
        const arrow = (() => 1).bind(null);
        let rejected = false;
        try { new arrow(); } catch (e) { rejected = e instanceof TypeError; }
        called && constructed.result === 'abd' && constructed.newTarget === Target &&
            constructed instanceof Target && constructed instanceof second &&
            !Object.hasOwn(second, 'prototype') &&
            alternate.result === 'abe' && alternate.newTarget === Alternate &&
            Object.getPrototypeOf(alternate) === Alternate.prototype && rejected
    "#);
}
