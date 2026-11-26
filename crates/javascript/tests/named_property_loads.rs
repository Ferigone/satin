use rust_browser_javascript::{Value, Vm};

#[test]
fn inherited_named_reads_fit_the_unchanged_task_budget() {
    let mut vm = Vm::new();
    let source = include_str!("fixtures/media_bootstrap_named_loads.js");
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true));
}

#[test]
fn named_reads_preserve_receiver_order_and_utf16() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(r#"
            let events = [];
            const base = { get x() { events.push(this.marker); return 4; } };
            const derived = Object.create(base);
            derived.marker = 'receiver';
            const proxy = new Proxy(derived, { get(t, k, r) {
                events.push(k);
                return Reflect.get(t, k, r);
            }});
            const get = () => { events.push('base'); return proxy; };
            const first = get().x;
            const second = ({ '\ud800': 1, '\ud801': 2 })['\ud801'];
            let bad = false;
            try { (null).x; } catch (e) { bad = e instanceof TypeError; }
            first === 4 && second === 2 && bad &&
                events.join(',') === 'base,x,marker,receiver'
        "#).unwrap(),
        Value::Bool(true),
    );
}
