use rust_browser_javascript::{ErrorKind, Value, Vm};

fn yes(source: &str) {
    let mut vm = Vm::new();
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true), "{source}");
}

#[test]
fn literal_accessors_have_named_functions_and_enumerable_descriptors() {
    yes(r#"
        const described = Symbol('token');
        const unnamed = Symbol();
        let reads = 0;
        const obj = {
            backing: 1,
            get read() { reads++; return this.backing; },
            set write({value} = {value: 7}) { this.backing = value; },
            get 'full name'() { return this.backing; },
            set 0x10(value) { this.backing = value; },
            get [described]() { return this.backing; },
            set [unnamed](value) { this.backing = value; },
            get() { return 'ordinary method'; }
        };
        const descriptor = Object.getOwnPropertyDescriptor;
        const read = descriptor(obj, 'read');
        const write = descriptor(obj, 'write');
        const name = descriptor(read.get, 'name');
        const functionsCorrect =
            read.get.name === 'get read' && read.get.length === 0 &&
            write.set.name === 'set write' && write.set.length === 0 &&
            descriptor(obj, 'full name').get.name === 'get full name' &&
            descriptor(obj, 16).set.name === 'set 16' &&
            descriptor(obj, described).get.name === 'get [token]' &&
            descriptor(obj, unnamed).set.name === 'set ' &&
            name.configurable && !name.writable && !name.enumerable;
        const descriptorsCorrect =
            read.enumerable && read.configurable && read.set === undefined &&
            write.enumerable && write.configurable && write.get === undefined &&
            !('value' in read) && !('writable' in read) &&
            Object.keys(obj).includes('read') && reads === 0;
        let constructorError;
        try { new read.get(); } catch (error) { constructorError = error; }
        obj.write = undefined;
        const usedDefault = obj.read === 7;
        obj[16] = 12;
        functionsCorrect && descriptorsCorrect && constructorError instanceof TypeError &&
            usedDefault && obj[described] === 12 && obj.get() === 'ordinary method'
        "#);
}

#[test]
fn bigint_property_names_use_the_exact_decimal_key() {
    yes(r#"
        const object = {
            get 0x10000000000000000n() { return this.stored; },
            set [18446744073709551616n](value) { this.stored = value; },
        };
        object['18446744073709551616'] = 3;
        const property = Object.getOwnPropertyDescriptor(object, 18446744073709551616n);
        property.get.name === 'get 18446744073709551616' &&
            property.set.name === 'set 18446744073709551616' &&
            object['18446744073709551616'] === 3
        "#);
}

#[test]
fn duplicate_accessor_definitions_merge_only_with_own_accessors() {
    yes(r#"
        let inheritedWrites = 0;
        const proto = { set item(value) { inheritedWrites++; } };
        const obj = {
            __proto__: proto,
            get item() { return 1; },
            set item(value) { this.stored = value + 1; },
            get item() { return 2; },
            set ['item'](value) { this.stored = value + 2; },
            get __proto__() { return 'own accessor'; },
            set reset(value) { throw 'overwritten setter'; },
            reset: 5,
            get reset() { return 6; },
            get spread() { return 7; },
            ...{spread: 8}
        };
        obj.item = 11;
        const descriptor = Object.getOwnPropertyDescriptor;
        const item = descriptor(obj, 'item');
        const reset = descriptor(obj, 'reset');
        const spread = descriptor(obj, 'spread');
        const getterOnly = {__proto__: proto, get item() { return 3; }};
        inheritedWrites === 0 && obj.item === 2 && obj.stored === 13 &&
            typeof item.get === 'function' && typeof item.set === 'function' &&
            reset.set === undefined && obj.reset === 6 &&
            spread.value === 8 && spread.writable && spread.enumerable && spread.configurable &&
            descriptor(getterOnly, 'item').set === undefined &&
            obj.__proto__ === 'own accessor' && Object.getPrototypeOf(obj) === proto
        "#);
}

#[test]
fn computed_keys_coerce_once_in_order_and_allow_reentry() {
    yes(r#"
        let trace = '';
        const key = {
            [Symbol.toPrimitive](hint) {
                trace += 'key:' + hint + ';';
                return {get name() { trace += 'nested;'; return 'value'; }}.name;
            }
        };
        const obj = {
            first: (trace += 'first;', 1),
            get [key]() {
                trace += 'get;';
                Object.defineProperty(this, 'value', { value: 9 });
                return this.value;
            },
            last: (trace += 'last;', 2)
        };
        const before = trace === 'first;key:string;nested;last;';
        const first = obj.value;
        const second = obj.value;
        before && first === 9 && second === 9 &&
            trace === 'first;key:string;nested;last;get;'
        "#);
}

#[test]
fn object_accessors_use_their_home_object_and_the_actual_receiver() {
    yes(r#"
        const proto = {
            get value() { return this.number + 1; },
            set value(value) { this.number = value * 2; },
        };
        const obj = {
            __proto__: proto,
            number: 3,
            get value() { return (() => super.value)() + 1; },
            set value(value) { super.value = value; },
            set withDefault(value = super.value) { this.observed = value; },
        };
        const before = obj.value === 5;
        obj.value = 4;
        obj.withDefault = undefined;
        const setterUsedSuper = obj.number === 8 && obj.observed === 9;
        const extracted = Object.getOwnPropertyDescriptor(obj, 'value').get;
        Object.setPrototypeOf(obj, {get value() { return this.number * 3; }});
        const outer = {
            __proto__: {name: 'outer'},
            build() { return {get [super.name]() { return 17; }}; },
        };
        before && setterUsedSuper && extracted.call({number: 10}) === 31 &&
            outer.build().outer === 17
        "#);
}

#[test]
fn invalid_object_accessor_forms_are_early_syntax_errors() {
    let mut vm = Vm::new();
    for source in [
        "({get item(value) {}});",
        "({set item() {}});",
        "({set item(first, second) {}});",
        "({set item(value,) {}});",
        "({set item(...value) {}});",
        "({set item([value, value]) {}});",
        "({get item: 3});",
        "({get *item() {}});",
        "({async get item() {}});",
        "({'get' item() {}});",
        r"({g\u0065t item() {}});",
        "({get [a, b]() {}});",
        "({get item() { super(); }});",
        "let {get item() {}} = {};",
        "class Test { set item(value,) {} }",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
}
