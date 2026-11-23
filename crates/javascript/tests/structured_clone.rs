use rust_browser_javascript::{ErrorKind, StructuredCloneError, Value, Vm, VmLimits};

fn publish(vm: &mut Vm, source: &str) {
    let value = vm.eval(source).unwrap();
    let cloned = vm.structured_clone(value, &[]).unwrap();
    vm.set_global("cloned", cloned).unwrap();
}

fn yes(vm: &mut Vm, script: &str) {
    assert_eq!(vm.eval(script).unwrap(), Value::Bool(true), "{script}");
}

#[test]
fn ordinary_graphs_preserve_cycles_aliases_holes_and_string_keys() {
    let mut vm = Vm::new();
    publish(
        &mut vm,
        r#"
        var pair = Object.create(null), holes = new Array(4);
        var source = {left: pair, right: pair, holes: holes, number: -0, nan: NaN};
        pair.root = source;
        holes[2] = undefined;
        holes.extra = pair;
        source[Symbol('ignored')] = () => 1;
        Object.defineProperty(source, 'hidden', {value: Symbol(), enumerable: false});
        Object.defineProperty(source, '__proto__', {value: 7, enumerable: true});
        source['\ud800'] = '\udfff';
        Object.freeze(source);
        source
    "#,
    );
    yes(
        &mut vm,
        r#"
        cloned !== source && cloned.left === cloned.right && cloned.left !== pair
        && cloned.left.root === cloned && cloned.holes.extra === cloned.left
        && cloned.holes.length === 4 && !(0 in cloned.holes) && (2 in cloned.holes) && !(3 in cloned.holes)
        && 1/cloned.number === -Infinity && cloned.nan !== cloned.nan
        && cloned.__proto__ === 7 && Object.getPrototypeOf(cloned) === Object.prototype
        && Object.getPrototypeOf(cloned.left) === Object.prototype
        && Object.getOwnPropertySymbols(cloned).length === 0 && !('hidden' in cloned)
        && cloned['\ud800'].charCodeAt(0) === 57343
        && Object.getOwnPropertyDescriptor(cloned, '__proto__').writable
    "#,
    );
}

#[test]
fn getter_order_is_depth_first_and_properties_are_snapshotted() {
    let mut vm = Vm::new();
    publish(
        &mut vm,
        r#"
        var seen = [], source = Object.create({gone: 'prototype'}), child = {};
        Object.defineProperty(child, 'leaf', {enumerable: true, get: () => {
            seen.push('leaf');
            return 4;
        }});
        Object.defineProperty(source, 'first', {enumerable: true, get: () => {
            seen.push('first');
            delete source.gone;
            Object.defineProperty(source, 'hiddenLater', {enumerable: false});
            source.late = 17;
            return child;
        }});
        source.gone = 1;
        source.hiddenLater = 2;
        Object.defineProperty(source, 'last', {enumerable: true, get: () => {
            seen.push('last');
            return child;
        }});
        source
    "#,
    );
    yes(
        &mut vm,
        r#"
        seen.join() === 'first,leaf,last'
        && cloned.first === cloned.last && cloned.first.leaf === 4
        && !('gone' in cloned) && !('late' in cloned) && cloned.hiddenLater === 2
        && Object.getOwnPropertyDescriptor(cloned, 'first').get === undefined
        && Object.keys(cloned).join() === 'first,hiddenLater,last'
    "#,
    );
}

#[test]
fn special_values_use_intrinsic_prototypes_and_skip_expandos() {
    let mut vm = Vm::new();
    publish(
        &mut vm,
        r#"
        var mapProto = Map.prototype, setProto = Set.prototype, dateProto = Date.prototype;
        var reProto = RegExp.prototype, typeProto = TypeError.prototype;
        class MapSubclass extends Map {}
        var key = {key: true}, value = {value: true};
        var map = new MapSubclass([[key, value]]), set = new Set([key, map]);
        map.set(map, set);
        var date = new Date(321), invalid = new Date(NaN), regexp = /a+b/gi;
        regexp.lastIndex = 7;
        date.visible = () => 1;
        var typeError = new TypeError('message');
        typeError.cause = typeError;
        var source = {
            map: map, set: set, key: key, value: value, date: date, invalid: invalid,
            re: regexp, error: typeError, string: new String('\ud800'), negative: new Number(-0),
            boolean: new Boolean(false),
        };
        Map = Set = Date = RegExp = TypeError = null;
        mapProto.set = () => { throw 1; };
        setProto.add = () => { throw 2; };
        source
    "#,
    );
    yes(
        &mut vm,
        r#"
        Object.getPrototypeOf(cloned.map) === mapProto && Object.getPrototypeOf(cloned.set) === setProto
        && cloned.map.get(cloned.key) === cloned.value && cloned.map.get(cloned.map) === cloned.set
        && cloned.set.has(cloned.key) && cloned.set.has(cloned.map)
        && Object.getPrototypeOf(cloned.date) === dateProto && cloned.date.getTime() === 321
        && Number.isNaN(cloned.invalid.getTime()) && !('visible' in cloned.date)
        && Object.getPrototypeOf(cloned.re) === reProto && cloned.re.source === 'a+b'
        && cloned.re.flags === 'gi' && cloned.re.lastIndex === 0
        && Object.getPrototypeOf(cloned.error) === typeProto && cloned.error.message === 'message'
        && cloned.error.cause === cloned.error
        && cloned.string.valueOf().charCodeAt(0) === 55296
        && 1/cloned.negative.valueOf() === -Infinity && cloned.boolean.valueOf() === false
    "#,
    );
}

#[test]
fn collection_entries_are_snapshotted_before_walking_any_key_or_value() {
    let mut vm = Vm::new();
    publish(
        &mut vm,
        r#"
        var map = new Map(), key = {}, records = [];
        Object.defineProperty(key, 'first', {enumerable: true, get: () => {
            records.push('key');
            map.clear();
            return 5;
        }});
        var item = {};
        Object.defineProperty(item, 'afterKey', {enumerable: true, get: () => {
            records.push('value');
            return 8;
        }});
        map.set(key, item);
        map.set('cycle', map);
        map
    "#,
    );
    yes(
        &mut vm,
        r#"
        var rows = [...cloned];
        map.size === 0 && records.join() === 'key,value' && rows.length === 2
        && rows[0][0].first === 5 && rows[0][1].afterKey === 8 && rows[0][0] !== key
        && rows[1][0] === 'cycle' && rows[1][1] === cloned
    "#,
    );
}

#[test]
fn buffer_copies_preserve_aliases_types_offsets_and_resize_behavior() {
    let mut vm = Vm::new();
    publish(
        &mut vm,
        r#"
        var source = new ArrayBuffer(8, {maxByteLength: 32});
        new Uint8Array(source).set([1,2,3,4,5,6,7,8]);
        var growing = new Uint16Array(source, 2), fixed = new Uint8Array(source, 3, 2);
        var view = new DataView(source, 1);
        ({buffer: source, growing: growing, fixed: fixed, view: view, alias: view})
    "#,
    );
    yes(
        &mut vm,
        r#"
        var before = cloned.view.getUint8(0) === 2 && cloned.fixed.join() === '4,5';
        cloned.buffer.resize(12);
        cloned.view.setUint8(0, 91);
        before && cloned.buffer !== source && source.byteLength === 8 && !source.detached
        && cloned.buffer.resizable && cloned.buffer.maxByteLength === 32
        && cloned.view === cloned.alias && cloned.view.buffer === cloned.buffer
        && cloned.growing.buffer === cloned.buffer && cloned.fixed.buffer === cloned.buffer
        && cloned.growing instanceof Uint16Array && cloned.view instanceof DataView
        && cloned.growing.byteOffset === 2 && cloned.growing.length === 5
        && cloned.view.byteLength === 11 && cloned.fixed.length === 2 && view.getUint8(0) === 2
    "#,
    );
}

#[test]
fn transfer_moves_one_store_and_detaches_even_unreachable_list_items() {
    let mut vm = Vm::new();
    let value = vm
        .eval(
            r#"
        var source = new ArrayBuffer(4, {maxByteLength: 16});
        var unused = new ArrayBuffer(3);
        var view = new Uint8Array(source, 1);
        view.set([2,3,4]);
        ({first: view, alias: source, again: view})
    "#,
        )
        .unwrap();
    let source = vm.get_global("source").unwrap();
    let unused = vm.get_global("unused").unwrap();
    let cloned = vm.structured_clone(value, &[source, unused]).unwrap();
    vm.set_global("cloned", cloned).unwrap();
    vm.collect_garbage();
    yes(
        &mut vm,
        r#"
        var bytes = cloned.first.join();
        cloned.alias.resize(6);
        source.detached && unused.detached && view.length === 0
        && bytes === '2,3,4' && cloned.alias === cloned.first.buffer
        && cloned.first === cloned.again && cloned.first.length === 5 && cloned.alias.maxByteLength === 16
    "#,
    );
}

#[test]
fn transfer_validation_and_getter_failures_leave_uncommitted_stores_intact() {
    let mut vm = Vm::new();
    let value = vm
        .eval(
            r#"
        var first = new ArrayBuffer(4), second = new ArrayBuffer(6), visits = 0;
        var sentinel = {}, input = {};
        Object.defineProperty(input, 'value', {enumerable: true, configurable: true, get: () => {
            visits++;
            throw sentinel;
        }});
        input
    "#,
        )
        .unwrap();
    let first = vm.get_global("first").unwrap();
    let second = vm.get_global("second").unwrap();
    assert!(matches!(
        vm.structured_clone(value.clone(), &[first.clone(), first.clone()]),
        Err(StructuredCloneError::DataClone(_))
    ));
    yes(&mut vm, "visits === 0 && !first.detached");
    let Err(StructuredCloneError::JavaScript(error)) =
        vm.structured_clone(value.clone(), &[first.clone(), second.clone()])
    else {
        panic!("the getter's thrown value must propagate");
    };
    assert_eq!(error.thrown, Some(vm.get_global("sentinel").unwrap()));
    yes(
        &mut vm,
        "visits === 1 && !first.detached && !second.detached",
    );
    vm.eval(
        r#"
        Object.defineProperty(input, 'value', {enumerable: true, get: () => {
            visits++;
            second.transfer();
            return 1;
        }});
    "#,
    )
    .unwrap();
    assert!(matches!(
        vm.structured_clone(value, &[first.clone(), second]),
        Err(StructuredCloneError::DataClone(_))
    ));
    yes(
        &mut vm,
        "visits === 2 && !first.detached && second.detached",
    );
    let value = vm.eval("({bad: Symbol()})").unwrap();
    assert!(matches!(
        vm.structured_clone(value, &[first]),
        Err(StructuredCloneError::DataClone(_))
    ));
    yes(&mut vm, "!first.detached");
}

#[test]
fn uncloneable_slots_and_out_of_bounds_views_fail() {
    let mut vm = Vm::new();
    let platform = vm.new_object();
    vm.mark_platform_object(&platform).unwrap();
    assert!(matches!(
        vm.structured_clone(platform.clone(), &[]),
        Err(StructuredCloneError::DataClone(_))
    ));
    let inherits_platform = vm.new_object_with_prototype(platform).unwrap();
    assert!(vm.structured_clone(inherits_platform, &[]).is_ok());
    for input in [
        "Symbol()",
        "Object(Symbol())",
        "()=>1",
        "Promise.resolve(1)",
        "new WeakMap()",
        "[new Set([Symbol()])]",
        "[1][Symbol.iterator]()",
        "var buffer=new ArrayBuffer(4);buffer.transfer();buffer",
        "var arr=new ArrayBuffer(4,{maxByteLength:16});var view=new Uint8Array(arr,2,2);arr.resize(1);view",
        "var b=new ArrayBuffer(4);var dv=new DataView(b);b.transfer();dv",
    ] {
        let value = vm.eval(input).unwrap();
        assert!(
            matches!(
                vm.structured_clone(value, &[]),
                Err(StructuredCloneError::DataClone(_))
            ),
            "{input}"
        );
    }
}

#[test]
fn budget_failure_cannot_detach_transfer_list_members() {
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 128,
        ..VmLimits::default()
    });
    let first = vm.new_array_buffer(vec![0; 8]).unwrap();
    let large = vm.new_array_buffer(vec![0; 64 * 1024]).unwrap();
    let result = vm.structured_clone(large, std::slice::from_ref(&first));
    assert!(
        matches!(result, Err(StructuredCloneError::JavaScript(error)) if error.kind == ErrorKind::ExecutionLimit)
    );
    assert_eq!(vm.array_buffer_byte_length(&first).unwrap(), 8);
}

#[test]
fn deeply_nested_graphs_use_an_iterative_walk_on_one_mib_stack() {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let mut vm = Vm::new();
            let mut chain = Value::Null;
            for _ in 0..2500 {
                let node = vm.new_object();
                vm.set_property(node.clone(), "next", chain).unwrap();
                chain = node;
            }
            let mut result = vm.structured_clone(chain, &[]).unwrap();
            vm.root(result.clone());
            vm.collect_garbage();
            for _ in 0..2500 {
                result = vm.get_property(result, "next").unwrap();
            }
            assert_eq!(result, Value::Null);
        })
        .unwrap()
        .join()
        .unwrap();
}
