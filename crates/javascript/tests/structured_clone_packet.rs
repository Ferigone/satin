use rust_browser_javascript::{ErrorKind, JsError, StructuredCloneError, Value, Vm, VmLimits};

#[test]
fn independent_thread_receives_exact_values_and_graph_identity() {
    let mut sender = Vm::new();
    let value = sender
        .eval(
            r#"
            var seen = [], pair = Object.create(null), items = new Array(4);
            var source = Object.create({gone: 'prototype'});
            pair.root = source;
            items[2] = undefined;
            items.extra = pair;
            Object.defineProperty(pair, 'leaf', {enumerable: true, get: () => {
                seen.push('leaf');
                return 7;
            }});
            Object.defineProperty(source, 'first', {enumerable: true, get: () => {
                seen.push('first');
                delete source.gone;
                source.late = 1;
                return pair;
            }});
            source.gone = 1;
            Object.defineProperty(source, 'last', {enumerable: true, get: () => {
                seen.push('last');
                return pair;
            }});
            source.items = items;
            source['\ud800'] = '\udfff';
            source.number = -0;
            source.nan = NaN;
            source.big = -18446744073709551617n;
            source[Symbol('ignored')] = () => 1;
            Object.defineProperty(source, '__proto__', {value: null, enumerable: true});
            source
        "#,
        )
        .unwrap();
    let packet = sender.serialize_clone_packet(value, 16 * 1024).unwrap();
    assert_eq!(
        sender.eval("seen.join(',')").unwrap(),
        Value::from("first,leaf,last")
    );
    drop(sender);

    std::thread::spawn(move || {
        let mut vm = Vm::new();
        vm.eval(
            "Object.defineProperty(Object.prototype, 'first', {set: () => { throw 1; }});",
        )
        .unwrap();
        let value = vm.deserialize_clone_packet(packet).unwrap();
        vm.set_global("received", value).unwrap();
        assert_eq!(
            vm.eval(
                r#"
                received.first === received.last && received.first.root === received
                && received.first.leaf === 7 && received.items.extra === received.first
                && received.items.length === 4 && !(0 in received.items) && (2 in received.items)
                && received.items[2] === undefined && !(3 in received.items)
                && received['\ud800'].charCodeAt(0) === 57343
                && 1 / received.number === -Infinity && Number.isNaN(received.nan)
                && received.big === -18446744073709551617n
                && received.__proto__ === null && Object.getPrototypeOf(received) === Object.prototype
                && Object.getPrototypeOf(received.first) === Object.prototype
                && Object.getPrototypeOf(received.items) === Array.prototype
                && !('gone' in received) && !('late' in received)
                && Object.getOwnPropertySymbols(received).length === 0
                && Object.getOwnPropertyDescriptor(received, 'first').writable
            "#,
            )
            .unwrap(),
            Value::Bool(true)
        );
    })
    .join()
    .unwrap();
}

#[test]
fn view_snapshots_share_one_copied_backing_in_the_receiving_vm() {
    let mut sender = Vm::new();
    let value = sender
        .eval(
            r#"
            var original = new ArrayBuffer(4, {maxByteLength: 8});
            var all = new Uint8Array(original);
            var view = new Uint8Array(original, 1, 3), data = new DataView(original, 1);
            all.set([42, 10, 20, 7]);
            ({first: view, a: original, b: original, again: view, data: data})
            "#,
        )
        .unwrap();
    let packet = sender.serialize_clone_packet(value, 4096).unwrap();
    sender.eval("all[0] = 9; original.resize(6)").unwrap();
    assert_eq!(
        sender
            .eval("original.byteLength === 6 && all[0] === 9")
            .unwrap(),
        Value::Bool(true)
    );

    let mut receiver = Vm::new();
    let value = receiver.deserialize_clone_packet(packet).unwrap();
    receiver.set_global("received", value).unwrap();
    assert_eq!(
        receiver
            .eval(
                r#"
                var copied = new Uint8Array(received.a);
                var before = received.a.byteLength === 4 && copied[0] === 42 && copied[3] === 7;
                received.a.resize(6);
                received.data.setUint8(0, 9);
                before && received.a === received.b && received.first === received.again
                && received.a.resizable && received.a.maxByteLength === 8
                && Object.getPrototypeOf(received.a) === ArrayBuffer.prototype
                && Object.getPrototypeOf(received.first) === Uint8Array.prototype
                && Object.getPrototypeOf(received.data) === DataView.prototype
                && received.first.buffer === received.a && received.data.buffer === received.a
                && received.first.byteOffset === 1 && received.first.length === 3
                && received.data.byteOffset === 1 && received.data.byteLength === 5
                && received.first[0] === 9 && copied[1] === 9
            "#,
            )
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn packet_rejects_unsupported_values_and_respects_both_realms_limits() {
    let mut sender = Vm::new();
    for source in [
        "Symbol()",
        "() => 1",
        "var detached = new ArrayBuffer(4); var view = new DataView(detached); detached.transfer(); view",
        "var resized = new ArrayBuffer(4, {maxByteLength: 8}); var fixed = new Uint8Array(resized, 2, 2); resized.resize(1); fixed",
    ] {
        let value = sender.eval(source).unwrap();
        assert!(matches!(
            sender.serialize_clone_packet(value, 4096),
            Err(StructuredCloneError::DataClone(_))
        ));
    }
    let marker = sender.eval("var marker = {}; marker").unwrap();
    let value = sender.eval("({get value() { throw marker; }})").unwrap();
    assert!(matches!(
        sender.serialize_clone_packet(value, 4096),
        Err(StructuredCloneError::JavaScript(error)) if error.thrown == Some(marker)
    ));

    let value = sender.eval("({items: new Array(32)})").unwrap();
    let packet = sender.serialize_clone_packet(value.clone(), 4096).unwrap();
    let bytes = packet.retained_bytes();
    assert!(matches!(
        sender.serialize_clone_packet(value, bytes - 1),
        Err(StructuredCloneError::JavaScript(error)) if error.kind == ErrorKind::HeapLimit
    ));
    let mut receiver = Vm::with_limits(VmLimits {
        max_array_length: 16,
        ..VmLimits::default()
    });
    let error = receiver.deserialize_clone_packet(packet).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
}

#[test]
fn transfer_uses_the_final_resizable_store_and_detaches_unreferenced_buffers() {
    let mut sender = Vm::new();
    let value = sender
        .eval(
            r#"
            var source = new ArrayBuffer(4, {maxByteLength: 16});
            var unused = new ArrayBuffer(16 * 1024);
            var typed = new Uint16Array(source, 2);
            var data = new DataView(source, 2);
            data.setUint8(0, 42);
            ({
                typed: typed,
                data: data,
                same: typed,
                get appended() {
                    source.resize(8);
                    data.setUint8(5, 7);
                    return true;
                }
            })
            "#,
        )
        .unwrap();
    let source = sender.get_global("source").unwrap();
    let unused = sender.get_global("unused").unwrap();
    let (packet, reserved) = sender
        .serialize_clone_packet_with_transfer(value, &[source, unused], 4096, Ok)
        .unwrap();
    assert_eq!(packet.retained_bytes(), reserved);
    assert_eq!(
        sender
            .eval("source.detached && unused.detached && typed.length === 0")
            .unwrap(),
        Value::Bool(true)
    );

    std::thread::spawn(move || {
        let mut receiver = Vm::new();
        let value = receiver.deserialize_clone_packet(packet).unwrap();
        receiver.set_global("received", value).unwrap();
        assert_eq!(
            receiver
                .eval(
                    r#"
                    var transferred = received.typed.buffer;
                    var before = transferred.byteLength === 8
                        && received.typed.length === 3 && received.data.getUint8(5) === 7;
                    transferred.resize(12);
                    before && received.appended && received.typed === received.same
                    && received.data.buffer === transferred
                    && transferred.maxByteLength === 16 && received.typed.byteOffset === 2
                    && received.typed.length === 5 && received.data.byteLength === 10
                    && received.data.getUint8(0) === 42
                "#,
                )
                .unwrap(),
            Value::Bool(true)
        );
    })
    .join()
    .unwrap();
}

#[test]
fn transferred_storage_is_moved_once_and_subject_to_receiver_heap_limits() {
    const LENGTH: usize = 512 * 1024;
    let baseline = Vm::new().estimated_heap_bytes();
    let mut sender = Vm::with_limits(VmLimits {
        max_heap_bytes: baseline + LENGTH + 16 * 1024,
        ..VmLimits::default()
    });
    let source = sender.new_array_buffer(vec![42; LENGTH]).unwrap();
    let address = sender.fixed_buffer_source_bytes(&source).unwrap().as_ptr();
    assert!(matches!(
        sender.serialize_clone_packet(source.clone(), LENGTH + 4096),
        Err(StructuredCloneError::JavaScript(error)) if error.kind == ErrorKind::HeapLimit
    ));
    let before = sender.estimated_heap_bytes();
    let (packet, ()) = sender
        .serialize_clone_packet_with_transfer(source.clone(), &[source], LENGTH + 4096, |_| Ok(()))
        .unwrap();
    assert!(packet.retained_bytes() >= LENGTH);
    assert!(sender.estimated_heap_bytes() < before - LENGTH / 2);
    let mut receiver = Vm::new();
    let value = receiver.deserialize_clone_packet(packet).unwrap();
    let bytes = receiver.fixed_buffer_source_bytes(&value).unwrap();
    assert_eq!(bytes.as_ptr(), address);
    assert_eq!(bytes.len(), LENGTH);
    assert_eq!(bytes[0], 42);

    let mut backing = Vec::with_capacity(LENGTH);
    backing.resize(LENGTH / 4, 21);
    let capacity = backing.capacity();
    let source = sender.new_array_buffer(backing).unwrap();
    let (packet, ()) = sender
        .serialize_clone_packet_with_transfer(
            source.clone(),
            &[source],
            capacity + 4096,
            |_| Ok(()),
        )
        .unwrap();
    // The receiver has room for the visible bytes but not the moved capacity.
    let mut receiver = Vm::with_limits(VmLimits {
        max_heap_bytes: baseline + LENGTH / 2,
        ..VmLimits::default()
    });
    let error = receiver.deserialize_clone_packet(packet).unwrap_err();
    assert_eq!(error.kind, ErrorKind::HeapLimit);
}

#[test]
fn aliased_buffer_packets_charge_one_backing_at_its_retained_capacity() {
    const LENGTH: usize = 64 * 1024;
    const METADATA_BUDGET: usize = 4096;
    let mut backing = Vec::with_capacity(2 * LENGTH);
    backing.resize(LENGTH, 42);
    let capacity = backing.capacity();
    let mut sender = Vm::new();
    let source = sender.new_array_buffer(backing).unwrap();
    let address = sender.fixed_buffer_source_bytes(&source).unwrap().as_ptr();
    sender.set_global("source", source.clone()).unwrap();
    let message = sender
        .eval(
            "var typed = new Uint8Array(source), data = new DataView(source);\n\
             ({typed: typed, buffer: source, same: source, again: typed, data: data})",
        )
        .unwrap();
    let check_backing = |packet, transferred| {
        let mut receiver = Vm::new();
        let value = receiver.deserialize_clone_packet(packet).unwrap();
        let buffer = receiver.get_property(value.clone(), "buffer").unwrap();
        let bytes = receiver.fixed_buffer_source_bytes(&buffer).unwrap();
        assert_eq!(bytes.as_ptr() == address, transferred);
        assert_eq!(bytes.len(), LENGTH);
        assert_eq!(bytes[LENGTH - 1], 42);
        drop(bytes);
        receiver.set_global("received", value).unwrap();
        assert_eq!(
            receiver
                .eval(
                    r#"
                    received.data.setUint8(0, 9);
                    received.buffer === received.same && received.typed === received.again
                    && received.typed.buffer === received.buffer && received.data.buffer === received.buffer
                    && received.typed[0] === 9
                    "#,
                )
                .unwrap(),
            Value::Bool(true)
        );
    };

    let copied = sender
        .serialize_clone_packet(message.clone(), LENGTH + METADATA_BUDGET)
        .unwrap();
    let copy_bytes = copied.retained_bytes();
    assert!((LENGTH..=LENGTH + METADATA_BUDGET).contains(&copy_bytes));
    check_backing(copied, false);

    let transfer = std::slice::from_ref(&source);
    assert!(matches!(
        sender.serialize_clone_packet_with_transfer(
            message.clone(),
            transfer,
            copy_bytes,
            |_| -> Result<(), JsError> { panic!("the retained capacity exceeds the copy budget") },
        ),
        Err(StructuredCloneError::JavaScript(error)) if error.kind == ErrorKind::HeapLimit
    ));
    let bytes = sender.fixed_buffer_source_bytes(&source).unwrap();
    assert_eq!(bytes.as_ptr(), address);
    assert_eq!(bytes[0], 42);
    drop(bytes);

    let (packet, admitted_bytes) = sender
        .serialize_clone_packet_with_transfer(message, transfer, capacity + METADATA_BUDGET, Ok)
        .unwrap();
    assert!((capacity..=capacity + METADATA_BUDGET).contains(&admitted_bytes));
    assert_eq!(packet.retained_bytes(), admitted_bytes);
    assert_eq!(sender.eval("source.detached").unwrap(), Value::Bool(true));
    drop(sender);
    check_backing(packet, true);
}

#[test]
fn failed_decode_leaves_no_root_for_a_materialized_transfer() {
    const LENGTH: usize = 64 * 1024;
    let mut sender = Vm::new();
    let source = sender.new_array_buffer(vec![42; LENGTH]).unwrap();
    sender.set_global("source", source.clone()).unwrap();
    // Put the buffer before the over-limit array in the packet record order.
    let message = sender
        .eval("({buffer: source, items: new Array(32)})")
        .unwrap();
    let (packet, ()) = sender
        .serialize_clone_packet_with_transfer(message, &[source], LENGTH + 4096, |_| Ok(()))
        .unwrap();
    assert_eq!(sender.eval("source.detached").unwrap(), Value::Bool(true));

    let mut receiver = Vm::with_limits(VmLimits {
        max_array_length: 16,
        ..VmLimits::default()
    });
    receiver.collect_garbage();
    let baseline = receiver.estimated_heap_bytes();
    let error = receiver.deserialize_clone_packet(packet).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
    receiver.collect_garbage();
    assert_eq!(receiver.estimated_heap_bytes(), baseline);
}

#[test]
fn transfer_admission_is_after_getters_and_budget_checks_but_before_detach() {
    let mut sender = Vm::new();
    let value = sender
        .eval(
            r#"
            var source = new ArrayBuffer(4, {maxByteLength: 4096});
            var other = new ArrayBuffer(4);
            var typed = new Uint8Array(source), visits = 0;
            var message = {
                other: other,
                source: source,
                get appended() { visits++; source.resize(2048); return true; }
            };
            message
            "#,
        )
        .unwrap();
    let source = sender.get_global("source").unwrap();
    let typed = sender.get_global("typed").unwrap();
    for transfer in [&[source.clone(), source.clone()][..], &[typed][..]] {
        assert!(matches!(
            sender.serialize_clone_packet_with_transfer(
                value.clone(),
                transfer,
                4096,
                |_| -> Result<(), JsError> { panic!("invalid list cannot reach admission") },
            ),
            Err(StructuredCloneError::DataClone(_))
        ));
    }
    assert_eq!(sender.eval("visits").unwrap(), Value::Number(0.0));

    let transfer = &[sender.get_global("other").unwrap(), source];
    assert!(matches!(
        sender.serialize_clone_packet_with_transfer(
            value.clone(),
            transfer,
            1024,
            |_| -> Result<(), JsError> { panic!("oversized packet cannot reach admission") },
        ),
        Err(StructuredCloneError::JavaScript(error))
            if error.kind == ErrorKind::HeapLimit
    ));
    let result = sender.serialize_clone_packet_with_transfer(value, transfer, 4096, |bytes| {
        assert!(bytes >= 2048);
        Err::<(), _>(JsError::new(ErrorKind::HeapLimit, "Mailbox is full"))
    });
    assert!(matches!(
        result,
        Err(StructuredCloneError::JavaScript(error)) if error.message == "Mailbox is full"
    ));
    assert_eq!(
        sender
            .eval(
                "visits === 2 && !other.detached && !source.detached && source.byteLength === 2048"
            )
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn every_list_member_is_revalidated_before_any_transfer_commits() {
    let mut sender = Vm::new();
    let value = sender
        .eval(
            "var first = new ArrayBuffer(4), second = new ArrayBuffer(3);\n\
             ({first: first, get changed() { second.transfer(); return true; }})",
        )
        .unwrap();
    let first = sender.get_global("first").unwrap();
    let second = sender.get_global("second").unwrap();
    let result = sender.serialize_clone_packet_with_transfer(
        value,
        &[first, second],
        4096,
        |_| -> Result<(), JsError> { panic!("detached source cannot reach admission") },
    );
    assert!(matches!(result, Err(StructuredCloneError::DataClone(_))));
    assert_eq!(
        sender.eval("!first.detached && second.detached").unwrap(),
        Value::Bool(true)
    );
}
