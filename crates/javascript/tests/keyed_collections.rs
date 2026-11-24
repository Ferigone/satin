use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

fn yes(source: &str) {
    let mut vm = Vm::new();
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true), "{source}");
}

#[test]
fn keys_use_same_value_zero_and_identity_without_coercion() {
    yes(r#"
        var a = { [Symbol.toPrimitive]() { throw new Error('coerced object'); } };
        var b = {};
        var first = Symbol('same'), second = Symbol('same');
        var m = new Map([
            [NaN, 1], [0 / 0, 2], [-0, 3], [0, 4], ['0', 5], [false, 6],
            [null, 7], [undefined, 8], [first, 9], [second, 10], [a, 11], [b, 12],
            ['\ud800', 13], ['\ud801', 14], ['\ufffd', 15], [true, 16]
        ]);
        var keys = m.keys();
        var set = new Set([-0, +0, NaN, 0 / 0, a, b, a, first, second]);
        m.size === 14 && m.get(NaN) === 2 && m.get(-0) === 4 && m.get('0') === 5
            && m.get(false) === 6 && m.get(null) === 7 && m.get(undefined) === 8
            && m.get(first) === 9 && m.get(second) === 10 && m.get(a) === 11
            && m.get(b) === 12 && !m.has({}) && m.get('\ud800') === 13
            && m.get('\ud801') === 14 && m.get('\ufffd') === 15 && m.get(true) === 16
            && Number.isNaN(keys.next().value) && Object.is(keys.next().value, +0)
            && set.size === 6 && set.has(-0) && Object.is(set.values().next().value, +0)
    "#);
}

#[test]
fn iterators_follow_live_order_and_stay_done_after_exhaustion() {
    yes(r#"
        var map = new Map([['first',1], ['replace',2], ['remove',3]]);
        var entries = map.entries();
        var untouched = map.keys();
        var first = entries.next().value.join(':');
        map.set('replace', 22);
        map.delete('remove');
        map.set('first', 11);
        var second = entries.next().value.join(':');
        map.clear();
        map.set('readded', 42);
        var third = entries.next().value.join(':');
        var done = entries.next();
        map.set('later', 99);
        var set = new Set([1, 2]);
        var setEntries = set.entries();
        set.delete(1);
        set.add(1);
        first === 'first:1' && second === 'replace:22' && third === 'readded:42'
            && done.done && done.value === undefined && entries.next().done
            && untouched.next().value === 'readded' && untouched.next().value === 'later'
            && setEntries.next().value.join(':') === '2:2'
            && setEntries.next().value.join(':') === '1:1'
    "#);
}

#[test]
fn for_each_observes_reinsertion_clearing_and_receiver_arguments() {
    yes(r#"
        var set = new Set([1, 2, 3]), visited = [], context = {};
        set.forEach(function(value, key, receiver) {
            'use strict';
            visited.push([value, key, this === context, receiver === set].join(':'));
            if (value === 1) {
                set.delete(2);
                set.add(4);
            }
            if (value === 3) {
                set.clear();
                set.add(5);
            }
        }, context);
        var map = new Map([['a',1], ['b',2], ['c',3]]), trace = [];
        map.forEach(function(value, key, receiver) {
            trace.push(key + ':' + value + ':' + (receiver === map));
            if (key === 'a') {
                map.delete('b');
                map.set('b',20);
                map.set('c',30);
            }
        });
        visited.join('|') === '1:1:true:true|3:3:true:true|5:5:true:true'
            && trace.join('|') === 'a:1:true|c:30:true|b:20:true'
    "#);
}

#[test]
fn iterable_construction_caches_adder_and_respects_prototypes() {
    yes(r#"
        var steps = [];
        class Child extends Map {
            get set() {
                steps.push('adder');
                return function(key, value) {
                    steps.push('set:' + key);
                    return Map.prototype.set.call(this, key, value);
                };
            }
        }
        var entry = {};
        Object.defineProperty(entry, '0', {get() { steps.push('key'); return 'a'; }});
        Object.defineProperty(entry, '1', {get() { steps.push('value'); return 2; }});
        var iterable = { [Symbol.iterator]() {
            steps.push('iterator');
            return [entry, ['b', 3]].values();
        } };
        var result = new Child(iterable);
        var fromSet = new Set('x🦀x');
        var noIterable = new Child(null);
        result instanceof Child && result instanceof Map && result.get('b') === 3
            && Object.getPrototypeOf(result) === Child.prototype && noIterable.size === 0
            && steps.join(',') === 'adder,iterator,key,value,set:a,set:b'
            && fromSet.size === 2 && fromSet.has('🦀')
    "#);
}

#[test]
fn constructor_closes_abrupt_entries_and_preserves_original_error() {
    yes(r#"
        var original = {}, other = {}, closes = 0;
        function iterable(item) {
            return { [Symbol.iterator]() {
                return { next() { return {value: item, done: false}; },
                    return() { closes++; throw other; } };
            } };
        }
        var mapCaught, setCaught, nextCaught;
        var broken = {};
        Object.defineProperty(broken, '0', {get() { throw original; }});
        try { new Map(iterable(broken)); }
        catch (error) { mapCaught = error; }
        class BadSet extends Set { add() { throw original; } }
        try { new BadSet(iterable('value')); }
        catch (error) { setCaught = error; }
        var itemResult = {done: false};
        Object.defineProperty(itemResult, 'value', {get() { throw original; }});
        var failedStep = { [Symbol.iterator]() {
            return { next() { return itemResult; },
                return() { closes++; return {}; } };
        } };
        try { new Map(failedStep); } catch (error) { nextCaught = error; }
        mapCaught === original && setCaught === original && nextCaught === original && closes === 2
    "#);
}

#[test]
fn computed_insert_normalizes_keys_and_respects_callback_side_effects() {
    yes(r#"
        var map = new Map([['undef',undefined]]), calls = 0, keySeen, caught = false;
        var same = map.getOrInsert('undef', 'default');
        var ignored = map.getOrInsertComputed('undef', function() { calls++; });
        try { map.getOrInsertComputed('undef', null); } catch (error) { caught = error instanceof TypeError; }
        var created = map.getOrInsertComputed(-0, function(key) {
            'use strict';
            keySeen = Object.is(key, +0) && this === undefined;
            map.set('side', 1);
            map.set(key, 'reentrant');
            map.set('last', 2);
            return 'computed';
        });
        same === undefined && ignored === undefined && caught && calls === 0
            && keySeen && created === 'computed' && map.get(0) === 'computed'
            && Array.from(map.keys()).join(':') === 'undef:side:0:last'
    "#);
}

#[test]
fn group_by_keeps_value_order_and_closes_on_callback_error() {
    yes(r#"
        var object = {}, calls = [], marker = {}, retained, closed = 0;
        class Child extends Map {}
        var groups = Child.groupBy([2, 4, 6, 8, 10, 12], function(value, index) {
            'use strict';
            calls.push([value, index, this === undefined].join(':'));
            if (index < 2) return index === 0 ? -0 : +0;
            return index % 2 === 0 ? object : NaN;
        });
        var failing = { [Symbol.iterator]() {
            return { next() { return {done:false,value:42}; },
                return() { closed++; throw new Error('ignored close error'); } };
        } };
        try { Map.groupBy(failing, () => { throw marker; }); }
        catch (error) { retained = error; }
        groups instanceof Map && !(groups instanceof Child) && groups.size === 3
            && groups.get(0).join(':') === '2:4' && Object.is(groups.keys().next().value, +0)
            && groups.get(object).join(':') === '6:10' && groups.get(NaN).join(':') === '8:12'
            && calls.join(',') === '2:0:true,4:1:true,6:2:true,8:3:true,10:4:true,12:5:true'
            && retained === marker && closed === 1
    "#);
}

#[test]
fn set_algebra_uses_set_like_methods_and_the_correct_order() {
    yes(r#"
        class Child extends Set {}
        var first = new Child([3, 2, 1, 0]);
        var other = new Map([[1,'one'], [3,'three'], [9,'nine']]);
        var union = first.union(other);
        var intersection = first.intersection(other);
        var difference = first.difference(other);
        var symmetric = first.symmetricDifference(other);
        var getterTrace = [];
        class SetLike {
            get size() { getterTrace.push('size'); return 10; }
            get has() { getterTrace.push('has'); return () => true; }
            get keys() { getterTrace.push('keys'); return () => { throw new Error('did not use has'); }; }
        }
        var subset = first.isSubsetOf(new SetLike());
        Object.getPrototypeOf(union) === Set.prototype
            && Array.from(union).join(':') === '3:2:1:0:9'
            && Array.from(intersection).join(':') === '1:3'
            && Array.from(difference).join(':') === '2:0'
            && Array.from(symmetric).join(':') === '2:0:9'
            && !first.isDisjointFrom(other) && !first.isSupersetOf(other) && subset
            && getterTrace.join(':') === 'size:has:keys'
    "#);
}

#[test]
fn set_algebra_distinguishes_live_and_snapshot_paths_and_closes_early() {
    yes(r#"
        var original = new Set([1, 2]), seen = [];
        var other = {
            size: 8,
            has(value) {
                if (value === 1) { original.delete(2); original.add(3); }
                seen.push(value);
                return true;
            },
            keys() { throw new Error('expected membership path'); }
        };
        var intersect = original.intersection(other);
        var live = seen.join(',');
        original = new Set([1, 2]);
        seen = [];
        var differ = original.difference(other);
        var closed = 0;
        var small = {
            size: 1,
            has() { throw new Error('expected iteration path'); },
            keys() {
                return { next() { return {value:1,done:false}; },
                    return() { closed++; return {}; } };
            }
        };
        !original.isDisjointFrom(small) && closed === 1 && live === '1,3'
            && seen.join(',') === '1,2' && differ.size === 0
            && Array.from(intersect).join(',') === '1,3'
    "#);
}

#[test]
fn native_brands_intrinsics_and_aliases_have_the_expected_descriptors() {
    yes(r#"
        var map = new Map(), set = new Set();
        var mapIterator = map.entries(), setIterator = set.values();
        var mapNext = mapIterator.next, setNext = setIterator.next;
        var mapSize = Object.getOwnPropertyDescriptor(Map.prototype, 'size');
        var species = Object.getOwnPropertyDescriptor(Map, Symbol.species);
        var alias = Object.getOwnPropertyDescriptor(Set.prototype, Symbol.iterator);
        var rejected = 0;
        function rejects(task) { try { task(); } catch(error) { if (error instanceof TypeError) rejected++; } }
        rejects(() => Map());
        rejects(() => Set());
        rejects(() => Map.prototype.get.call(Object.create(Map.prototype), 0));
        rejects(() => Map.prototype.has.call(set, 0));
        rejects(() => mapSize.get.call(Map.prototype));
        rejects(() => mapNext.call(setIterator));
        rejects(() => setNext.call(mapIterator));
        rejects(() => new Map.prototype.get());
        var base = Object.getPrototypeOf(Object.getPrototypeOf([].values()));
        rejected === 8 && Object.getPrototypeOf(Map) === Function.prototype
            && Map.length === 0 && Set.length === 0 && Map.prototype.set.length === 2
            && mapSize.get.name === 'get size' && mapSize.set === undefined
            && !mapSize.enumerable && mapSize.configurable && species.get.call(set) === set
            && Object.getOwnPropertyDescriptor(Map, 'prototype').writable === false
            && Map.prototype[Symbol.iterator] === Map.prototype.entries
            && Set.prototype.keys === Set.prototype.values && alias.value === Set.prototype.values
            && alias.writable && alias.configurable && !alias.enumerable
            && Object.getPrototypeOf(Object.getPrototypeOf(mapIterator)) === base
            && Object.getPrototypeOf(Object.getPrototypeOf(setIterator)) === base
            && mapIterator[Symbol.iterator]() === mapIterator
            && Object.prototype.toString.call(mapIterator) === '[object Map Iterator]'
            && Object.prototype.toString.call(setIterator) === '[object Set Iterator]'
    "#);
}

#[test]
fn iterators_trace_collection_keys_values_and_release_target_when_done() {
    let mut vm = Vm::new();
    let key = vm
        .eval(
            r#"
        var key = { marker: 24 }, value = { marker: 42 };
        var map = new Map([[key, value]]);
        var iterator = map.entries();
        var oldKey = key;
        key = value = map = null;
        oldKey
    "#,
        )
        .unwrap();
    vm.eval("oldKey = null").unwrap();
    vm.collect_garbage();
    assert_eq!(vm.get_property(key.clone(), "marker").unwrap(), 24.into());
    assert_eq!(
        vm.eval("var pair = iterator.next().value; pair[0].marker + pair[1].marker")
            .unwrap(),
        66.into()
    );
    assert_eq!(
        vm.eval("pair = null; iterator.next().done").unwrap(),
        true.into()
    );
    vm.collect_garbage();
    assert_eq!(
        vm.get_property(key, "marker").unwrap_err().kind,
        ErrorKind::TypeError
    );
}

#[test]
fn retained_strings_are_charged_and_deleted_entries_really_release_storage() {
    let mut vm = Vm::new();
    vm.eval("var map = new Map(); var iterator = map.entries();")
        .unwrap();
    vm.collect_garbage();
    let baseline = vm.estimated_heap_bytes();
    vm.eval("map.set('x'.repeat(32000), 'y'.repeat(32000));")
        .unwrap();
    vm.collect_garbage();
    let retained = vm.estimated_heap_bytes();
    assert!(retained >= baseline + 128_000);
    vm.eval(
        r#"
        map.clear();
        for (let i = 0; i < 5000; i++) {
            map.set(i, i);
            map.delete(i);
        }
        map.set('last', 99);
    "#,
    )
    .unwrap();
    vm.collect_garbage();
    assert!(vm.estimated_heap_bytes() < baseline + 12_000);
    assert_eq!(
        vm.eval("iterator.next().value.join(':')").unwrap(),
        "last:99".into()
    );
}

#[test]
fn limits_leave_existing_entries_intact_and_repeated_mutation_spends_fuel() {
    let mut vm = Vm::with_limits(VmLimits {
        max_array_length: 3,
        ..VmLimits::default()
    });
    vm.eval("var map=new Map(); map.set(0,0).set(1,1).set(2,2).set(1,12);")
        .unwrap();
    assert_eq!(
        vm.eval("map.set(3,3)").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(
        vm.eval("map.size === 3 && map.get(1) === 12 && !map.has(3)")
            .unwrap(),
        true.into()
    );

    let mut bounded = Vm::with_limits(VmLimits {
        max_instructions: 4000,
        ..VmLimits::default()
    });
    assert_eq!(
        bounded
            .eval(
                r#"
        var set = new Set([0]);
        set.forEach(value => { set.add(value + 1); });
    "#
            )
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(
        bounded.eval("set.has(0) && set.size > 1").unwrap(),
        true.into()
    );
}

#[test]
fn weak_collections_accept_objects_and_non_registered_symbols() {
    yes(r#"
        var object = {}, key = Symbol('key'), duplicate = Symbol('key');
        var registered = Symbol.for('registered');
        class Child extends WeakMap {}
        var map = new Child([[object, 1], [key, 2], [Symbol.iterator, 3]]);
        var set = new WeakSet([object, key, Symbol.iterator]);
        var called = 0, rejected = 0;
        var assigned = map.getOrInsertComputed(duplicate, function(seen) {
            'use strict';
            if (seen === duplicate && this === undefined) called++;
            map.set(seen, 9);
            return 42;
        });
        try { map.set(registered, 0); } catch (error) { if (error instanceof TypeError) rejected++; }
        try { set.add('bad'); } catch (error) { if (error instanceof TypeError) rejected++; }
        try { WeakMap.prototype.has.call(set, key); } catch (error) { if (error instanceof TypeError) rejected++; }
        var untouched = map.getOrInsert(key, 100);
        map instanceof WeakMap && map instanceof Child && assigned === 42 && called === 1
            && map.get(duplicate) === 42 && untouched === 2 && map.get(object) === 1
            && map.get(Symbol.iterator) === 3 && map.get(registered) === undefined
            && set.has(object) && set.has(key) && set.has(Symbol.iterator) && !set.has(duplicate)
            && !set.delete(registered) && !set.has(null) && rejected === 3
            && Object.prototype.toString.call(map) === '[object WeakMap]'
            && Object.prototype.toString.call(set) === '[object WeakSet]'
            && !('size' in map) && !('entries' in set)
    "#);
}

#[test]
fn weak_keys_and_value_to_key_cycles_are_reclaimed() {
    let mut vm = Vm::new();
    let key = vm
        .eval(
            r#"
        var weak = new WeakMap(), membership = new WeakSet();
        var key = { marker: 12 };
        var data = { key: key, marker: 34 };
        weak.set(key, data);
        membership.add(key);
        key
    "#,
        )
        .unwrap();
    let value = vm.eval("data").unwrap();
    vm.eval("data = null").unwrap();
    vm.collect_garbage();
    assert_eq!(vm.get_property(value.clone(), "marker").unwrap(), 34.into());
    assert_eq!(
        vm.eval("weak.get(key).key === key && membership.has(key)")
            .unwrap(),
        true.into()
    );

    vm.eval("key = null").unwrap();
    vm.collect_garbage();
    assert_eq!(
        vm.get_property(key, "marker").unwrap_err().kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        vm.get_property(value, "marker").unwrap_err().kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        vm.eval("var replacement = {}; !weak.has(replacement) && !membership.has(replacement)")
            .unwrap(),
        true.into()
    );
}

#[test]
fn marking_reaches_fixed_point_across_weak_owners_object_keys_and_symbol_keys() {
    let mut vm = Vm::new();
    let leaf = vm
        .eval(
            r#"
        var outer = new WeakMap(), inner = new WeakMap(), seed = {};
        var symbol = Symbol('conditional'), next = {}, leaf = { marker: 42 };
        outer.set(seed, { table: inner, token: symbol });
        inner.set(symbol, next);
        outer.set(next, leaf);
        var saved = leaf;
        inner = symbol = next = leaf = null;
        saved
    "#,
        )
        .unwrap();
    vm.eval("saved = null").unwrap();
    vm.collect_garbage();
    assert_eq!(
        vm.eval("var pack = outer.get(seed); outer.get(pack.table.get(pack.token)).marker")
            .unwrap(),
        42.into()
    );
    assert_eq!(vm.get_property(leaf.clone(), "marker").unwrap(), 42.into());
    vm.eval("pack = seed = null").unwrap();
    vm.collect_garbage();
    assert_eq!(
        vm.get_property(leaf, "marker").unwrap_err().kind,
        ErrorKind::TypeError
    );
}

#[test]
fn symbol_weak_keys_follow_strong_property_keys_and_host_roots() {
    let mut vm = Vm::new();
    let symbol = vm
        .eval(
            r#"
        var weak = new WeakMap(), properties = {}, token = Symbol('rooted');
        var value = { marker: 23, token: token };
        weak.set(token, value);
        properties[token] = true;
        token
    "#,
        )
        .unwrap();
    let payload = vm.eval("value").unwrap();
    vm.eval("token = value = null").unwrap();
    vm.collect_garbage();
    assert_eq!(
        vm.eval("weak.get(Object.getOwnPropertySymbols(properties)[0]).marker")
            .unwrap(),
        23.into()
    );

    let root = vm.root(symbol);
    vm.eval("properties = null").unwrap();
    vm.collect_garbage();
    assert_eq!(
        vm.get_property(payload.clone(), "marker").unwrap(),
        23.into()
    );

    vm.unroot(root);
    vm.collect_garbage();
    assert_eq!(
        vm.get_property(payload, "marker").unwrap_err().kind,
        ErrorKind::TypeError
    );
}

#[test]
fn weak_symbol_marking_includes_async_job_arguments_and_well_known_symbols() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var memo = new WeakMap(), answer, symbol = Symbol('pending job');
        memo.set(symbol, { marker: 46 });
        memo.set(Symbol.iterator, { marker: 17 });
        async function read() {
            // The fulfilled argument will be held by the queued async resume job.
            return memo.get(await Promise.resolve(symbol)).marker;
        }
        read().then(value => { answer = value; });
        symbol = null;
    "#,
    )
    .unwrap();
    vm.collect_garbage();
    vm.drain_jobs(16).unwrap();
    assert_eq!(
        vm.eval("answer === 46 && memo.get(Symbol.iterator).marker === 17")
            .unwrap(),
        true.into()
    );
}

#[test]
fn collecting_weak_entries_frees_insertion_budget() {
    let mut vm = Vm::with_limits(VmLimits {
        max_array_length: 2,
        ..VmLimits::default()
    });
    vm.eval("var weak = new WeakMap(); weak.set({}, 1).set(Symbol(), 2);")
        .unwrap();
    assert_eq!(
        vm.eval("weak.set({}, 3)").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    vm.collect_garbage();
    assert_eq!(
        vm.eval("var live = {}; weak.set(live, 3); weak.get(live)")
            .unwrap(),
        3.into()
    );
}

#[test]
fn weak_targets_and_pending_cleanup_follow_heap_reachability() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var called = [], token = {}, live = {}, w, sym;
        var registry = new FinalizationRegistry(value => called.push(value));
        (function () {
            let target = {}, weakSymbol = Symbol();
            w = new WeakRef(target);
            sym = new WeakRef(weakSymbol);
            registry.register(target, "object");
            registry.register(weakSymbol, "symbol", token);
            registry.register(live, "live");
            if (w.deref() !== target || sym.deref() !== weakSymbol) throw Error();
        })()
    "#,
    )
    .unwrap();
    vm.collect_garbage();
    assert_eq!(vm.eval("w.deref() === undefined && sym.deref() === undefined && called.length === 0 && registry.unregister(token)").unwrap(), Value::Bool(true));
    vm.drain_jobs(10).unwrap();
    assert_eq!(vm.eval("called.join(',')").unwrap(), Value::from("object"));
    vm.eval("live = undefined").unwrap();
    vm.collect_garbage();
    vm.drain_jobs(10).unwrap();
    assert_eq!(
        vm.eval("called.join(',')").unwrap(),
        Value::from("object,live")
    );
}
