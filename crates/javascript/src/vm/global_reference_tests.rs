use super::*;

#[test]
fn resolved_global_references_keep_their_binding_object() {
    let mut vm = Vm::new();
    let ambient = vm.global();
    let bindings = vm.new_object();
    let receiver = vm.new_object();
    let foreign = vm.heap.allocate_env(Environment::new_global(
        bindings.as_object().unwrap(),
        receiver.as_object().unwrap(),
    ));
    let nested = vm
        .heap
        .allocate_env(Environment::new(Some(foreign), EnvKind::Block, None));
    let shared = vm.new_object();
    vm.set_property(bindings.clone(), "common", shared.clone())
        .unwrap();
    vm.set_property(ambient.clone(), "common", 1.into())
        .unwrap();
    vm.set_property(receiver.clone(), "common", 2.into())
        .unwrap();
    vm.set_property(bindings.clone(), "otherOnly", 3.into())
        .unwrap();
    vm.set_property(ambient.clone(), "ambientOnly", 4.into())
        .unwrap();

    let reference = vm.resolve(nested, &"common".into()).unwrap();
    let other_only = vm.resolve(nested, &"otherOnly".into()).unwrap();
    assert!(matches!(
        vm.resolve(nested, &"ambientOnly".into()).unwrap(),
        Reference::Unresolvable(_)
    ));
    // One heap, identity preserved; neither the ambient global nor the
    // separate `this` value supplies this binding. Do not enter a realm or
    // switch intrinsics in this reduction.
    assert_eq!(vm.global(), ambient);
    assert_eq!(vm.read_reference(reference.clone()).unwrap(), shared);
    assert_eq!(vm.read_reference(other_only).unwrap(), 3.into());
    vm.write_reference(reference.clone(), 8.into(), false)
        .unwrap();
    assert_eq!(
        vm.get_property(bindings.clone(), "common").unwrap(),
        8.into()
    );

    let mut operands = vec![Item::Reference(reference)];
    vm.run_reference_op(&Op::Delete, &mut operands, false)
        .unwrap();
    assert_eq!(vm.pop_value(&mut operands).unwrap(), Value::Bool(true));
    assert_eq!(
        vm.get_property(bindings, "common").unwrap(),
        Value::Undefined
    );
    assert_eq!(vm.get_property(ambient, "common").unwrap(), 1.into());
    assert_eq!(vm.get_property(receiver, "common").unwrap(), 2.into());
}
