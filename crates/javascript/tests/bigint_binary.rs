use rust_browser_javascript::{JsBigInt, Value, Vm};

fn yes(vm: &mut Vm, source: &str) {
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true), "{source}");
}

#[test]
fn wide_coercion_and_index_descriptors_use_bigint_primitives() {
    let mut vm = Vm::new();
    yes(
        &mut vm,
        r#"
        const signed = new BigInt64Array([2n ** 128n - 1n, '9223372036854775808', true]);
        const unsigned = new BigUint64Array(signed.buffer);
        const descriptor = Object.getOwnPropertyDescriptor(unsigned, '0');
        let coerced = 0;
        const primitive = { [Symbol.toPrimitive](hint) {
          if (hint !== 'number') throw Error('wrong coercion hint');
          coerced++;
          return '-18446744073709551617';
        }};
        unsigned['-0'] = primitive;
        unsigned[30] = primitive;
        unsigned[2] = primitive;
        Reflect.defineProperty(unsigned, '1', {value: '340282366920938463463374607431768211461'});
        let numberRejected = false;
        try { unsigned[100] = 3; } catch (error) { numberRejected = error instanceof TypeError; }
        signed[0] === -1n && signed[1] === 5n && signed[2] === -1n &&
          unsigned[0] === 18446744073709551615n && unsigned[2] === unsigned[0] &&
          descriptor.value === unsigned[0] && typeof descriptor.value === 'bigint' &&
          descriptor.configurable && descriptor.enumerable && descriptor.writable &&
          coerced === 3 && numberRejected && !('-0' in unsigned) && unsigned[30] === undefined &&
          ArrayBuffer.isView(unsigned) && BigInt64Array.BYTES_PER_ELEMENT === 8 &&
          Object.prototype.toString.call(unsigned) === '[object BigUint64Array]'
        "#,
    );
}

#[test]
fn data_view_bigint_byte_order_matches_an_unaligned_shared_buffer() {
    let mut vm = Vm::new();
    yes(
        &mut vm,
        r#"
        const buffer = new ArrayBuffer(20);
        const view = new DataView(buffer, 1, 18);
        const big = view.setBigUint64(0, '0x0102030405060708');
        const little = view.setBigInt64(9, -(2n ** 127n) - 128n, true);
        const before = view.getBigUint64(0, true) === 0x0807060504030201n &&
          view.getBigInt64(9, true) === -128n &&
          view.getBigUint64(9, true) === 18446744073709551488n &&
          new Uint8Array(buffer, 1, 8).join() === '1,2,3,4,5,6,7,8' &&
          new Uint8Array(buffer, 10, 8).join() === '128,255,255,255,255,255,255,255';
        view.setFloat64(0, 1, true);
        big === undefined && little === undefined && before &&
          view.getBigUint64(0, true) === 0x3ff0000000000000n &&
          DataView.prototype.getBigInt64.length === 1 && DataView.prototype.setBigUint64.length === 2
        "#,
    );
}

#[test]
fn data_view_rechecks_bounds_after_bigint_coercion_and_prioritizes_abrupt_values() {
    let mut vm = Vm::new();
    yes(
        &mut vm,
        r#"
        const buffer = new ArrayBuffer(8, {maxByteLength: 24});
        const view = new DataView(buffer);
        const log = [];
        view.setBigUint64({valueOf() { log.push('offset'); return 8; }}, {
          valueOf() { log.push('value'); buffer.resize(16); return 9n; }
        });
        const written = view.getBigUint64(8) === 9n;
        const token = {};
        let keepThrown = false;
        try { view.setBigInt64(100, { valueOf() { throw token; } }); }
        catch (error) { keepThrown = error === token; }
        let detached = false;
        try { view.setBigInt64(0, { valueOf() { buffer.transfer(); return 1n; } }); }
        catch (error) { detached = error instanceof TypeError; }
        let offsetFirst = false;
        try { view.setBigInt64(-1, { valueOf() { throw token; } }); }
        catch (error) { offsetFirst = error instanceof RangeError; }
        written && log.join() === 'offset,value' && keepThrown && detached && offsetFirst
        "#,
    );
}

#[test]
fn copying_and_species_preserve_bit_patterns_and_reject_mixed_content() {
    let mut vm = Vm::new();
    yes(
        &mut vm,
        r#"
        function typeError(body) {
          try { body(); } catch (error) { return error instanceof TypeError; }
          return false;
        }
        const original = new BigInt64Array([-1n, 2n, -3n, 4n]);
        const snapshot = new BigUint64Array(original);
        const alias = new BigUint64Array(original.buffer);
        alias.set(original.subarray(0, 3), 1);
        const copied = original.join() === '-1,-1,2,-3' &&
          snapshot.join() === '18446744073709551615,2,18446744073709551613,4' &&
          snapshot.buffer !== original.buffer;
        original.constructor = {[Symbol.species]: BigUint64Array};
        const copiedSpecies = original.slice(0, 2);
        const aliasedSpecies = original.subarray(0, 2);
        original.constructor = {[Symbol.species]: Float64Array};
        copied && copiedSpecies instanceof BigUint64Array && copiedSpecies[0] === snapshot[0] &&
          aliasedSpecies instanceof BigUint64Array && aliasedSpecies.buffer === original.buffer &&
          typeError(() => original.slice(0, 0)) && typeError(() => original.subarray(0, 0)) &&
          typeError(() => original.map(value => value)) && typeError(() => original.filter(() => false)) &&
          typeError(() => new BigInt64Array(new Float64Array(0))) &&
          typeError(() => new Float64Array(new BigUint64Array(0))) &&
          typeError(() => snapshot.set(new Float64Array(0))) &&
          typeError(() => new Float64Array(4).set(snapshot)) &&
          typeError(() => BigInt64Array.of(1))
        "#,
    );
}

#[test]
fn mapping_iteration_and_copying_keep_each_value_in_the_bigint_domain() {
    let mut vm = Vm::new();
    yes(
        &mut vm,
        r#"
        const a = BigUint64Array.from(new Set([2n, 3n, 4n]), x => 2n ** 63n + x);
        const before = a.includes(9223372036854775811n) && !a.includes(9223372036854775811) &&
          a.lastIndexOf(9223372036854775812n) === 2 && a.indexOf(4n) === -1 &&
          a.entries().next().value[1] === a[0];
        a.fill('18446744073709551617', 1, 2);
        a.copyWithin(2, 1);
        const b = a.map(x => x - 1n);
        const c = b.with(-1, true).toReversed();
        const filtered = c.filter(x => x > 0n);
        const initial = BigInt64Array.of(1n, '-2', false);
        before && [...b].join() === '9223372036854775809,0,0' && c.join() === '1,0,9223372036854775809' &&
          filtered instanceof BigUint64Array && filtered.reduce((sum, x) => sum + x, 0n) === 9223372036854775810n &&
          initial.toString() === '1,-2,0'
        "#,
    );
}

#[test]
fn sorting_keeps_signed_and_unsigned_order_and_uses_tonumber_for_comparator_results() {
    let mut vm = Vm::new();
    yes(
        &mut vm,
        r#"
        const signed = new BigInt64Array([9007199254740993n, -1n, -9223372036854775808n, 9007199254740992n]);
        const unsigned = new BigUint64Array([-1n, 1n, -2n, 0n]);
        const result = signed.sort();
        const ascending = unsigned.toSorted();
        let types = true;
        const descending = unsigned.toSorted((x, y) => {
          types = types && typeof x === 'bigint' && typeof y === 'bigint';
          return x > y ? -1 : x < y ? 1 : 0;
        });
        const before = unsigned.join();
        let rejected = false;
        try { unsigned.sort((x, y) => x - y); } catch (error) { rejected = error instanceof TypeError; }
        result === signed && signed.join() === '-9223372036854775808,-1,9007199254740992,9007199254740993' &&
          ascending.join() === '0,1,18446744073709551614,18446744073709551615' &&
          descending.join() === '18446744073709551615,18446744073709551614,1,0' &&
          before === unsigned.join() && rejected && types
        "#,
    );
}

#[test]
fn resizable_view_tracking_and_structured_transfer_keep_real_bigint_views() {
    let mut vm = Vm::new();
    let input = vm
        .eval(
            r#"
        var buffer = new ArrayBuffer(24, {maxByteLength: 40});
        var tracking = new BigInt64Array(buffer, 8);
        var fixed = tracking.subarray(0, 1), sub = tracking.subarray(1);
        tracking.set([-10n, -12n]);
        buffer.resize(31);
        var preserved = tracking.length === 2 && sub.length === 1;
        buffer.resize(32);
        var grown = tracking.length === 3 && sub.length === 2 && fixed.length === 1;
        var view = new DataView(buffer, 8);
        ({buffer, tracking, fixed, sub, view})
    "#,
        )
        .unwrap();
    let source = vm.get_property(input.clone(), "buffer").unwrap();
    let cloned = vm.structured_clone(input, &[source]).unwrap();
    vm.set_global("cloned", cloned).unwrap();
    yes(
        &mut vm,
        r#"
        cloned.buffer.resize(40);
        cloned.sub[2] = 15n;
        const endianness = new Uint8Array(new Uint16Array([1]).buffer)[0] === 1;
        preserved && grown && buffer.detached && tracking.length === 0 && tracking[0] === undefined &&
          cloned.tracking instanceof BigInt64Array && cloned.tracking.buffer === cloned.fixed.buffer &&
          cloned.fixed.length === 1 && cloned.tracking.join() === '-10,-12,0,15' &&
          cloned.sub.byteOffset === 16 && cloned.view.getBigInt64(24, endianness) === 15n
        "#,
    );
}

#[test]
fn reading_high_unsigned_elements_repeatedly_does_not_allocate_heap_values() {
    let mut vm = Vm::new();
    let view = vm
        .eval("new BigUint64Array([18446744073709551615n])")
        .unwrap();
    let value = Value::BigInt(JsBigInt::from_u64(u64::MAX));
    let before = vm.estimated_heap_bytes();
    for _ in 0..128 {
        assert_eq!(vm.get_property(view.clone(), "0").unwrap(), value);
    }
    assert_eq!(vm.estimated_heap_bytes(), before);
}

#[test]
fn locale_fallback_delegates_each_visible_element_and_uses_the_initial_length() {
    let mut vm = Vm::new();
    yes(
        &mut vm,
        r#"
        const plain = [1, , null, undefined, 'x'].toLocaleString();
        const floats = new Float64Array([1, 1.5, -0]).toLocaleString();
        const object = Object.prototype.toLocaleString.call({toString() { return 'delegated'; }});
        const buffer = new ArrayBuffer(16, {maxByteLength: 24});
        const values = new BigUint64Array(buffer);
        values[0] = 4n;
        const calls = [];
        Object.defineProperty(BigInt.prototype, 'toLocaleString', { get: function() {
          calls.push('lookup');
          return function() {
            calls.push(this.valueOf() === 4n && arguments.length === 0 ? 'invoke' : 'incorrect');
            buffer.resize(8);
            return {toString() { calls.push('string'); return 'value'; }};
          };
        }});
        const reserved = {valueOf() { throw Error('unexpected conversion'); }};
        const formatted = values.toLocaleString(reserved, reserved);
        plain === '1,,,,x' && floats === '1,1.5,0' && object === 'delegated' &&
          formatted === 'value,' && calls.join() === 'lookup,invoke,string'
        "#,
    );
}
