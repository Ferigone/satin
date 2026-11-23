use rust_browser_javascript::{Value, Vm};

#[test]
fn entries_and_values_keep_snapshot_index_order_and_do_not_read_symbols() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            const ignored = Symbol();
            function target() {
                return {
                    9: 'nine',
                    get 2() {
                        this[1] = 'new';
                        this[9] = 'updated';
                        return 'two';
                    },
                    text: 'text',
                    get [ignored]() { throw new Error('read a symbol key'); },
                };
            }
            Object.entries(target()).join('|') === '2,two|9,updated|text,text' &&
                Object.values(target()).join('|') === 'two|updated|text'
            "#,
        )
        .unwrap(),
        Value::Bool(true)
    );
}
