use rust_browser_javascript::{Value, Vm};

#[test]
fn array_and_string_length_precede_later_named_properties() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var array = [];
            array.note = true;
            array[9] = true;
            array[2] = true;
            Object.defineProperty(array, 'length', {value: 10});
            var text = new String('xy');
            text.note = true;
            text[8] = true;
            Object.getOwnPropertyNames(array).join(',') === '2,9,length,note' &&
                Object.keys(array).join(',') === '2,9,note' &&
                Object.getOwnPropertyNames(text).join(',') === '0,1,8,length,note' &&
                Object.keys(text).join(',') === '0,1,8,note';
            "#,
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn redefining_implicit_string_properties_does_not_duplicate_keys() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var text = Object('xy');
            text.note = true;
            text[8] = true;
            Object.defineProperty(text, '0', {value: 'x'});
            Object.defineProperty(text, 'length', {value: 2});
            var rejected = false;
            try {
                Object.defineProperty(text, '0', {enumerable: false});
            } catch (error) {
                rejected = error instanceof TypeError;
            }
            var visited = [];
            for (var key in text) visited.push(key);
            rejected &&
                text[0] === 'x' && text.length === 2 &&
                Object.keys(text).join(',') === '0,1,8,note' &&
                Object.getOwnPropertyNames(text).join(',') === '0,1,8,length,note' &&
                visited.join(',') === '0,1,8,note';
            "#,
        )
        .unwrap(),
        Value::Bool(true)
    );
}
