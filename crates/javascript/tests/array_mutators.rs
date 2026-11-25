use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

fn check(source: &str) {
    assert_eq!(
        Vm::new()
            .eval(source)
            .unwrap_or_else(|error| panic!("{error}\n{source}")),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn splice_preserves_sparse_and_inherited_indices() {
    check(
        r"
        const receiver = Object.create({1: 'inherited'});
        receiver.length = 5;
        receiver[3] = 'tail';
        const removed = Array.prototype.splice.call(receiver, 0, 3, 'replacement');
        const sparse = [0, , 2, , 4];
        const grown = sparse.splice(1, 1, 'a', 'b');
        const unchanged = sparse.splice();
        const remainder = sparse.splice(-2);
        Array.isArray(removed) && removed.length === 3 &&
            !(0 in removed) && removed.hasOwnProperty(1) &&
            removed[1] === 'inherited' && !(2 in removed) &&
            receiver.length === 3 && receiver[0] === 'replacement' &&
            receiver[1] === 'tail' && !(2 in receiver) && !(3 in receiver) &&
            grown.length === 1 && !(0 in grown) &&
            unchanged.length === 0 && remainder.length === 2 &&
            !(0 in remainder) && remainder[1] === 4 &&
            sparse.join(',') === '0,a,b,2'
        ",
    );
}

#[test]
fn splice_defines_removed_data_then_moves_and_deletes_in_order() {
    check(
        r"
        const events = [];
        const receiver = [1, , 3, 4];
        const result = Object.create({set 0(value) { throw new Error('inherited setter'); }});
        Object.defineProperty(result, 'length', { set(value) { events.push(value); } });
        receiver.constructor = {[Symbol.species]: function(length) {
            events.push(length);
            return result;
        }};
        Object.defineProperty(receiver, '0', {
            get() { events.push('read'); return 1; },
            set(value) { events.push(value); }
        });
        Object.defineProperty(receiver, '2', {configurable: false});
        let error;
        try { receiver.splice(0, 2); } catch (e) { error = e; }
        error instanceof TypeError && result.hasOwnProperty(0) && result[0] === 1 &&
            !result.hasOwnProperty(1) && events.join(',') === '2,read,2,3' &&
            receiver.length === 4 && receiver[1] === 4 && receiver[2] === 3 &&
            !(3 in receiver)
        ",
    );
}

#[test]
fn splice_charges_work_and_enforces_array_length_limits() {
    let mut vm = Vm::with_limits(VmLimits {
        max_array_length: 4,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("[0, 1, 2, 3].splice(0, 0, 'extra')")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1_000,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("Array(10000).splice(0, 0, 'extra')")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
}

#[test]
fn splice_checks_safe_length_after_argument_coercion_and_before_budgets() {
    check(
        r"
        let trace = '';
        const receiver = {
            get length() {
                trace += 'get,';
                return {valueOf() { trace += 'length,'; return Infinity; }};
            },
            set length(value) { trace += 'set,'; }
        };
        const start = {valueOf() { trace += 'start,'; return -1; }};
        const count = {valueOf() { trace += 'count,'; return 0; }};
        let error;
        try { Array.prototype.splice.call(receiver, start, count, 'new'); }
        catch (e) { error = e; }
        error instanceof TypeError && trace === 'get,length,start,count,' &&
            !receiver.hasOwnProperty('9007199254740990')
        ",
    );

    let mut vm = Vm::with_limits(VmLimits {
        max_array_length: 4,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("Array.prototype.splice.call({length: Infinity}, -1, 1, 'replacement').length")
            .unwrap(),
        Value::from(1)
    );
    assert_eq!(
        vm.eval("Array.prototype.splice.call({length: 5}, 0, 1)")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
}

#[test]
fn sparse_length_charges_stored_values_and_preserves_index_boundaries() {
    let mut vm = Vm::with_limits(VmLimits {
        max_array_length: 4,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval(
            r#"
        const a = new Array(4294967295);
        a[4294967294] = 7;
        a[4294967295] = 'named';
        a[0] = 1;
        const ok = a.indexOf(7, 4294967294) === 4294967294 && a.pop() === 7;
        a.length = 0;
        ok && a.length === 0 && a[4294967295] === 'named' && !(0 in a);
    "#
        )
        .unwrap(),
        true.into()
    );
    assert_eq!(
        vm.eval("for (let n=0; n<5; n++) a[n * 1000] = n;")
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(
        vm.eval("a.length === 3001 && !(4000 in a)").unwrap(),
        true.into()
    );
}

#[test]
fn truncated_length_observes_blockers_conversion_order_and_write_mode() {
    check(
        r#"
        function blocked() {
            const a = new Array(4000000000);
            Object.defineProperty(a, 5, {value: 5, configurable: false});
            a[3999999999] = 7;
            return a;
        }
        const sloppy = blocked();
        sloppy.length = 1;
        const strict = blocked();
        let threw = false;
        try { (function(){ 'use strict'; strict.length = 1; })(); }
        catch(e) { threw = e instanceof TypeError; }
        const frozen = blocked();
        const rejected = !Reflect.defineProperty(frozen, "length", {value: 1, writable: false});
        let calls = 0;
        let marker = {};
        const mutating = [1];
        const operand = {valueOf() {
            if (++calls === 1) Object.defineProperty(mutating, 'length', {writable:false});
            return 1;
        }};
        const same = Reflect.defineProperty(mutating, 'length', {value:operand});
        let abrupt = false;
        try { Reflect.set(sloppy, "length", {valueOf(){throw marker;}}); }
        catch (e) { abrupt = e === marker; }
        const truncated = [sloppy, strict, frozen].every(a => a.length === 6 && !(3999999999 in a));
        threw && rejected && truncated && !Object.getOwnPropertyDescriptor(frozen,"length").writable &&
            !Reflect.set(frozen, 7, 1) && same && calls === 2 && abrupt
    "#,
    );
}

#[test]
fn reverse_getters_and_empty_shift_preserve_observable_operation_order() {
    check(r#"
        const source = {
            length: 2,
            get 0() { delete this[1]; return "moved"; },
            1: "removed"
        };
        Array.prototype.reverse.call(source);
        let trace = [];
        const holes = new Proxy({length:2}, {
            has(target, key) { trace.push("has " + key); return false; },
            get(target, key) { trace.push("get " + key); return target[key]; },
            deleteProperty() { throw new Error("delete on two absent properties"); }
        });
        Array.prototype.reverse.call(holes);
        const empty = {
            get length() { return -10; },
            set length(value) { trace.push("set " + value); },
            get 0() { throw new Error("empty shift read index zero"); }
        };
        !(0 in source) && source[1] === "moved" &&
            Array.prototype.shift.call(empty) === undefined &&
            trace.join(",") === "get length,has 0,has 1,set 0"
    "#);
}
