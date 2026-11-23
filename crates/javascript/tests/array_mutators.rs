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
