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
