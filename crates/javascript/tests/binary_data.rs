use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

#[test]
fn host_bytes_are_native_views_and_copy_only_visible_extent() {
    let mut vm = Vm::new();
    let bytes = vm.new_uint8_array(vec![11, 22, 33, 44]).unwrap();
    assert!(vm.is_uint8_array(&bytes));
    vm.set_global("bytes", bytes.clone()).unwrap();
    assert_eq!(
        vm.eval("bytes instanceof Uint8Array && ArrayBuffer.isView(bytes) && !Array.isArray(bytes) && bytes[1] === 22")
            .unwrap(),
        Value::Bool(true)
    );
    let view = vm.eval("new DataView(bytes.buffer, 1, 2)").unwrap();
    let copy = vm.copy_buffer_source(&view).unwrap().unwrap();
    assert!(!vm.is_uint8_array(&view));
    assert!(vm.is_buffer_source(&view));
    assert_eq!(vm.buffer_source_len(&view).unwrap(), Some(2));
    vm.eval("bytes[1] = 55").unwrap();
    assert_eq!(copy, [22, 33]);
    assert_eq!(vm.copy_buffer_source(&view).unwrap().unwrap(), [55, 33]);
}

#[test]
fn typed_indices_are_dense_numeric_slots_with_ordinary_named_properties() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            let a = new Uint8Array([1, 2, 3]);
            let calls = 0;
            let value = {valueOf() { calls++; return -2; }};
            a[1] = value;
            a['-0'] = value;
            a['1.5'] = value;
            a['NaN'] = value;
            a[10] = value;
            a['01'] = 40;
            let d = Object.getOwnPropertyDescriptor(a, '1');
            let p = Object.create(a);
            p[0] = 22;
            p[10] = 55;
            Object.defineProperty(a, '2', {value: 88});
            calls === 5 && a[1] === 254 && a[2] === 88 &&
                !('-0' in a) && !('NaN' in a) && a[10] === undefined &&
                p[0] === 22 && a[0] === 1 && p[10] === undefined &&
                d.value === 254 && d.writable && d.enumerable && d.configurable &&
                !Reflect.deleteProperty(a, '0') && Reflect.deleteProperty(a, '10') &&
                Object.keys(a).join('|') === '0|1|2|01';
        "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn typed_iterators_recheck_backing_extent_and_ignore_shadowed_length() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            let b = new ArrayBuffer(2, {maxByteLength: 8});
            let a = new Uint8Array(b);
            a[0] = 8;
            Object.defineProperty(a, 'length', {value: 0});
            let first = a.values().next().value;
            let pairs = a.entries();
            let p = pairs.next().value;
            b.resize(3);
            a[2] = 7;
            pairs.next();
            let grown = pairs.next().value;
            let fixed = new Uint8Array(b, 1, 2);
            let iter = fixed.values();
            b.resize(1);
            let invalid = false;
            try { iter.next(); } catch(e) { invalid = e instanceof TypeError; }
            first === 8 && p[0] === 0 && p[1] === 8 &&
                grown[0] === 2 && grown[1] === 7 && invalid &&
                fixed.length === 0 && !Reflect.preventExtensions(a) &&
                a[Symbol.iterator] === a.values;
        "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn data_view_endianness_and_numeric_families_share_real_bytes() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            let b = new ArrayBuffer(24);
            let d = new DataView(b, 2, 16);
            d.setUint32(0, 0x11223344);
            d.setInt16(4, -800, true);
            d.setFloat32(6, 1.5, true);
            d.setFloat16(10, 1.0004882812500002, true);
            let u = new Uint8Array(b);
            let i = new Int8Array([255]);
            let j = new Int16Array([65535]);
            let c = new Uint8ClampedArray([0.5, 1.5, 2.5, 300, -2]);
            let f = new Float64Array([-0]);
            d.getUint32(0, true) === 0x44332211 && u[2] === 0x11 &&
                d.getInt16(4, true) === -800 && d.getFloat32(6, true) === 1.5 &&
                d.getFloat16(10, true) === 1.0009765625 &&
                i[0] === -1 && j[0] === -1 && 1 / f[0] === -Infinity &&
                c[0] === 0 && c[1] === 2 && c[2] === 2 && c[3] === 255 && c[4] === 0;
        "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn transfers_detach_old_views_and_gc_keeps_the_backing_only_while_reachable() {
    let mut vm = Vm::new();
    vm.collect_garbage();
    let baseline = vm.estimated_heap_bytes();
    let view = vm.new_uint8_array(vec![7; 128 * 1024]).unwrap();
    let root = vm.root(view.clone());
    vm.collect_garbage();
    assert!(vm.estimated_heap_bytes() >= baseline + 128 * 1024 - 8192);
    let first = vm.get_property(view.clone(), "0").unwrap();
    assert_eq!(first.as_number(), Some(7.0));
    vm.set_global("view", view.clone()).unwrap();
    let transferred = vm.eval("view.buffer.transfer()").unwrap();
    assert!(vm.is_uint8_array(&view));
    assert_eq!(
        vm.buffer_source_len(&view).unwrap_err().kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        vm.buffer_source_len(&transferred).unwrap(),
        Some(128 * 1024)
    );
    vm.unroot(root);
    vm.set_global("view", Value::Undefined).unwrap();
    vm.collect_garbage();
    assert!(vm.estimated_heap_bytes() < baseline + 128 * 1024);
}

#[test]
fn reflective_descriptors_preserve_reentrant_growth_and_coercion_errors() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            let buffer = new ArrayBuffer(0, {maxByteLength: 4});
            let view = new Uint8Array(buffer);
            let grow = Object.defineProperty({}, 'value', {
                get: function () { buffer.resize(2); return 77; }
            });
            let update = Object.defineProperty({}, 'enumerable', {
                get: function () { view[0] = 99; return true; }
            });
            let created = Reflect.defineProperty(view, '0', grow);
            let same = Reflect.defineProperty(view, '0', update);
            let error = new TypeError('coercion');
            let rethrown = false;
            try {
                Reflect.defineProperty(view, '0', {value: {valueOf() { throw error; }}});
            } catch (e) { rethrown = e === error; }
            created && same && rethrown && view[0] === 99 &&
                !Reflect.defineProperty(view, '0', {writable: false}) &&
                !Reflect.defineProperty(view, '3', {value: 20});
        "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn typed_view_operations_handle_overlap_species_and_callback_mutation() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(r#"
            let a = new Uint8Array([1, 2, 3, 4]);
            let alias = a.subarray(1);
            alias[0] = 5;
            a.set(a.subarray(0, 3), 1);
            let shifted = a.join('|');
            let copied = a.slice(1);
            copied[0] = 90;
            let b = new Uint8Array([7, 9, 10, 1, 5]);
            b.constructor = {[Symbol.species]: function () { return new Uint8Array(b.buffer, 1, 3); }};
            let overlap = b.slice(0, 3).join('|');
            let c = new Float32Array([10, -0, NaN, 2, 0]);
            let sorted = c.toSorted();
            let mapped = Uint16Array.from(c, (value, index) => index * 400);
            let raw = new ArrayBuffer(4, {maxByteLength: 8});
            let tracking = new Uint8Array(raw);
            tracking.fill(7);
            let seen = tracking.map((value, index) => {
                if (index === 0) raw.resize(1);
                return value;
            });
            shifted === '1|1|5|3' && a[1] === 1 && overlap === '7|7|7' &&
                Object.is(sorted[0], -0) && sorted[1] === 0 && sorted[2] === 2 &&
                sorted[3] === 10 && Number.isNaN(sorted[4]) && c[0] === 10 &&
                mapped[3] === 1200 && seen.join('|') === '7|0|0|0';
        "#).unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn binary_allocations_charge_the_aggregate_budget_before_reserving() {
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: 2 * 1024 * 1024,
        ..VmLimits::default()
    });
    let before = vm.estimated_heap_bytes();
    let error = vm.eval("new Uint8Array(1024 * 1024 * 1024)").unwrap_err();
    assert_eq!(error.kind, ErrorKind::HeapLimit);
    assert!(vm.estimated_heap_bytes() < before + 65536);
}
