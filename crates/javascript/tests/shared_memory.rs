use rust_browser_javascript::{StructuredCloneError, Value, Vm, VmLimits};

#[test]
fn cloned_shared_views_alias_one_growable_store_through_collection() {
    let mut vm = Vm::new();
    let value = vm.eval("var input = new SharedArrayBuffer(8, {maxByteLength: 16}); var view = new Int32Array(input); ({input,view})").unwrap();
    let cloned = vm.structured_clone(value, &[]).unwrap();
    vm.set_global("cloned", cloned).unwrap();
    vm.collect_garbage();
    assert_eq!(
        vm.eval(
            r#"
        cloned.input.grow(16);
        Atomics.store(view, 2, 57);
        var old = Atomics.compareExchange(cloned.view, 2, 57, 101);
        cloned.input !== input && cloned.view.buffer === cloned.input
          && view.length === 4 && old === 57 && Atomics.load(view, 2) === 101
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn shared_memory_cannot_be_transferred_or_copied_across_isolated_agents() {
    let mut vm = Vm::new();
    let shared = vm
        .eval("var shared = new SharedArrayBuffer(4); shared")
        .unwrap();
    assert!(vm.detach_array_buffer(&shared).is_err());
    assert!(matches!(
        vm.structured_clone(shared.clone(), &[shared.clone()]),
        Err(StructuredCloneError::DataClone(_))
    ));
    let view = vm.eval("new Uint8Array(shared)").unwrap();
    assert!(vm.fixed_buffer_source_bytes(&view).is_err());
    assert!(vm.fixed_uint8_array_bytes_mut(&view).is_err());
    for value in [shared, view] {
        assert!(matches!(
            vm.serialize_clone_packet(value, 4096),
            Err(StructuredCloneError::DataClone(_))
        ));
    }
}

#[test]
fn hosts_can_keep_shared_memory_unexposed_and_use_ordinary_integer_views() {
    let mut vm = Vm::with_limits(VmLimits {
        shared_array_buffer: false,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval(
            r#"
        var a = new Uint8Array([255]);
        typeof SharedArrayBuffer === "undefined" && Atomics.add(a, 0, 3) === 255 && a[0] === 2
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn asynchronous_waiters_use_fifo_across_cloned_stores_and_element_widths() {
    let mut vm = Vm::new();
    let buffer = vm.eval("var buf = new SharedArrayBuffer(16); buf").unwrap();
    let alias = vm.structured_clone(buffer, &[]).unwrap();
    vm.set_global("alias", alias).unwrap();
    vm.eval(
        r#"
        var a = new Int32Array(buf), b = new BigInt64Array(alias, 8), log = [];
        Atomics.waitAsync(a, 2, 0).value.then(v => log.push('first:' + v));
        Atomics.waitAsync(b, 0, 0n).value.then(v => log.push('second:' + v));
        Atomics.waitAsync(a, 1, 0).value.then(v => log.push('other:' + v));
    "#,
    )
    .unwrap();
    vm.collect_garbage();
    assert_eq!(
        vm.eval(
            r#"
        Atomics.notify(new Int32Array(new SharedArrayBuffer(16)), 2) === 0 &&
        Atomics.notify(b, 0, NaN) === 0 && Atomics.notify(a, 2, 1.9) === 1 &&
        (Promise.resolve().then(() => log.push('between')), true) &&
        Atomics.notify(b, 0) === 1 && Atomics.notify(b, 0) === 0 &&
        Atomics.notify(a, 1) === 1 && log.length === 0
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
    vm.drain_jobs(16).unwrap();
    assert_eq!(
        vm.eval("log.join(',')").unwrap(),
        Value::from("first:ok,between,second:ok,other:ok")
    );
}

#[test]
fn timeouts_run_on_the_monotonic_job_checkpoint() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var a = new Int32Array(new SharedArrayBuffer(4)), log = [];
        var pending = Atomics.waitAsync(a, 0, 0, 2);
        pending.value.then(v => log.push(v));
    "#,
    )
    .unwrap();
    let deadline = vm.next_atomic_wait_deadline().unwrap();
    std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
    assert!(vm.has_pending_jobs());
    vm.drain_jobs(8).unwrap();
    assert!(vm.next_atomic_wait_deadline().is_none());
    assert_eq!(
        vm.eval("pending.async && log[0] === 'timed-out' && Atomics.notify(a, 0) === 0")
            .unwrap(),
        true.into()
    );

    vm.eval("Atomics.waitAsync(a, 0, 0, 1).value.then(v => log.push(v));")
        .unwrap();
    std::thread::sleep(
        vm.next_atomic_wait_deadline()
            .unwrap()
            .saturating_duration_since(std::time::Instant::now()),
    );
    assert_eq!(vm.eval("Atomics.notify(a, 0)").unwrap(), 1.into());
    vm.drain_jobs(8).unwrap();
    assert_eq!(vm.eval("log.join(',')").unwrap(), "timed-out,ok".into());
}

#[test]
fn unreachable_indefinite_waits_release_the_host_queue_on_collection() {
    let mut vm = Vm::with_limits(VmLimits {
        max_pending_jobs: 1,
        ..VmLimits::default()
    });
    for _ in 0..3 {
        assert_eq!(
            vm.eval("Atomics.waitAsync(new Int32Array(new SharedArrayBuffer(4)), 0, 0).async")
                .unwrap(),
            Value::Bool(true)
        );
        vm.collect_garbage();
    }
}
