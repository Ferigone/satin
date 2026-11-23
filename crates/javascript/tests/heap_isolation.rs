use rust_browser_javascript::{ErrorKind, Value, Vm};

#[test]
fn independent_heaps_cannot_resolve_each_others_object_handles() {
    let mut first = Vm::new();
    let mut second = Vm::new();
    let a = first.new_object();
    let b = second.new_object();
    first
        .set_property(a.clone(), "label", "first".into())
        .unwrap();
    second
        .set_property(b.clone(), "label", "second".into())
        .unwrap();

    assert_eq!(
        second.get_property(a.clone(), "label").unwrap_err().kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        first
            .set_property(b.clone(), "label", "foreign".into())
            .unwrap_err()
            .kind,
        ErrorKind::TypeError
    );
    assert_ne!(a, b);
    assert_eq!(
        first.get_property(a, "label").unwrap(),
        Value::from("first")
    );
    assert_eq!(
        second.get_property(b, "label").unwrap(),
        Value::from("second")
    );
}

#[test]
fn a_foreign_root_cannot_mark_a_local_heap_slot() {
    let mut first = Vm::new();
    let mut second = Vm::new();
    let foreign = first.new_object();
    let local = second.new_object();

    let root = second.root(foreign);
    second.collect_garbage();
    assert_eq!(
        second.get_property(local, "unused").unwrap_err().kind,
        ErrorKind::TypeError
    );
    second.unroot(root);
}
