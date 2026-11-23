use rust_browser_javascript::{Value, Vm};

#[test]
fn own_keys_keep_index_string_and_symbol_order_at_the_u32_boundary() {
    let source = r#"
        function check(target, label, indices, length) {
            var first = Symbol('same'), second = Symbol('same');
            target.head = 1;
            target[first] = 1;
            target['4294967295'] = 1;
            target['01'] = 1;
            target['4294967294'] = 1;
            target['-0'] = 1;
            target[second] = 1;
            target['4294967293'] = 1;
            target['4294967294.0'] = 1;
            target['04294967294'] = 1;
            target['4294967296'] = 1;
            target['4294967294e0'] = 1;
            target.tail = 1;

            function verify(stage, strings, a, b) {
                var names = Object.getOwnPropertyNames(target).join(',');
                var keys = Object.keys(target).join(',');
                var all = Reflect.ownKeys(target);
                var symbols = Object.getOwnPropertySymbols(target);
                var iterated = [];
                for (var name in target) iterated.push(name);
                if (names !== indices + length + strings ||
                    keys !== indices + strings || iterated.join(',') !== keys ||
                    all.slice(0, -2).join(',') !== names ||
                    all[all.length - 2] !== a || all[all.length - 1] !== b ||
                    symbols.length !== 2 || symbols[0] !== a || symbols[1] !== b) {
                    throw new Error(label + ' ' + stage + ': names=' + names + '; keys=' + keys);
                }
            }
            verify('initial',
                'head,4294967295,01,-0,4294967294.0,04294967294,4294967296,4294967294e0,tail',
                first, second);

            // An updated name does not move, while removal and reinsertion
            // move non-index names and symbols, but not numeric indices.
            target['4294967296'] = 2;
            if (!delete target['4294967294'] || !delete target['4294967295'] ||
                !delete target['01'] || !delete target[first]) {
                throw new Error('Expected successful deletion');
            }
            target['01'] = 2;
            target[first] = 2;
            target['4294967294'] = 2;
            target['4294967295'] = 2;
            verify('reinserted',
                'head,-0,4294967294.0,04294967294,4294967296,4294967294e0,tail,01,4294967295',
                second, first);
        }
        check({}, 'ordinary', '4294967293,4294967294,', '');
        check(new String('xy'), 'boxed String', '0,1,4294967293,4294967294,', 'length,');
        true;
    "#;
    for prefix in ["", "'use strict';\n"] {
        assert_eq!(
            Vm::new().eval(&format!("{prefix}{source}")).unwrap(),
            Value::Bool(true)
        );
    }
}
