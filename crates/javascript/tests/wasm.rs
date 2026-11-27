use rust_browser_javascript::{StructuredCloneError, Value, Vm, VmLimits};

// (module
//   (import "env" "sum" (func $sum (param i32 i32) (result i32)))
//   (func (export "run") (param i32 i32) (result i32)
//     local.get 0 local.get 1 call $sum))
const FIXTURE: &str = r#"
    var wasmBytes = new Uint8Array([
        0, 97, 115, 109, 1, 0, 0, 0,
        1, 7, 1, 96, 2, 127, 127, 1, 127,
        2, 11, 1, 3, 101, 110, 118, 3, 115, 117, 109, 0, 0,
        3, 2, 1, 0,
        7, 7, 1, 3, 114, 117, 110, 0, 1,
        10, 10, 1, 8, 0, 32, 0, 32, 1, 16, 0, 11
    ]);
"#;

fn fixture() -> Vm {
    let mut vm = Vm::new();
    vm.eval(FIXTURE).unwrap();
    vm
}

fn flush(vm: &mut Vm) {
    vm.with_task(|vm| {
        while vm.has_pending_jobs() {
            vm.drain_jobs(1)?;
        }
        Ok(())
    })
    .unwrap();
}

#[test]
fn core_wasm_supports_typed_imports_tables_and_live_linear_memory() {
    let mut vm = Vm::new();
    let bytes = wat::parse_str(r#"
        (module
           (import "env" "callback" (func $callback (param i64 f64) (result i64)))
           (type $unary (func (param i32) (result i32)))
           (table (export "table") 1 funcref)
           (memory (export "memory") 1 4)
           (global $calls (mut i32) (i32.const 0))
           (func $increment (type $unary)
              local.get 0 i32.const 1 i32.add)
           (elem (i32.const 0) $increment)
           (func (export "indirect") (param i32) (result i32)
              local.get 0 i32.const 0 call_indirect (type $unary))
           (func (export "load") (param i32) (result i32)
              local.get 0 i32.load8_u)
           (func (export "run") (param i64 f64) (result i64)
              global.get $calls i32.const 1 i32.add global.set $calls
              i32.const 0 i32.const 42 i32.store8
              local.get 0 local.get 1 call $callback)
           (func (export "count") (result i32) global.get $calls)
           (func (export "grow") (result i32) i32.const 1 memory.grow))
    "#).unwrap();
    let input = vm.new_uint8_array(bytes).unwrap();
    vm.set_global("input", input).unwrap();
    assert_eq!(vm.eval(r#"
        var ex, memory, old, array, seen, fail = false, kept;
        var marker = {};
        ex = new WebAssembly.Instance(new WebAssembly.Module(input), {env: {
            callback(a, b) {
                if (fail) throw marker;
                seen = [a, b, array[0], ex.count()];
                array[1] = 99;
                return a + 1n;
            }
        }}).exports;
        memory = ex.memory;
        old = memory.buffer;
        array = new Uint8Array(old);
        kept = ex.run;
        let invoked = kept(4294967296n, 2.5);
        let ok = invoked === 4294967297n && seen[0] === 4294967296n &&
            seen[1] === 2.5 && seen[2] === 42 && seen[3] === 1 &&
            ex.load(1) === 99 && ex.table instanceof WebAssembly.Table &&
            ex.table.length === 1 && ex.table.get(0)(41) === 42 &&
            ex.indirect(41) === 42 && memory.buffer === old;
        var raised = false;
        fail = true;
        try { kept(0n, 0); } catch (error) { raised = error === marker; }
        let entry = ex.table.get(0);
        let grown = ex.table.grow(1) === 1 && ex.table.get(1) === null;
        ex.table.set(0, null);
        let cleared = ex.table.get(0) === null;
        ex.table.set(0, entry);
        ex.table.set(1, entry);
        let tableWorks = grown && cleared && ex.table.get(1)(41) === 42 && ex.indirect(41) === 42;
        ok && raised && ex.grow() === 1 && old.detached && array.length === 0 &&
            memory.buffer.byteLength === 131072 && new Uint8Array(memory.buffer)[1] === 99 &&
            memory.grow(0) === 2 && memory.buffer.byteLength === 131072 && tableWorks
    "#).unwrap(), Value::Bool(true));
    vm.collect_garbage();
    assert_eq!(vm.eval("ex.table.get(0)(41)").unwrap(), Value::from(42));
}

#[test]
fn core_wasm_checks_loop_fuel_in_the_current_js_task() {
    let mut vm = Vm::new();
    let bytes = wat::parse_str(r#"(module (func (export "spin") (loop br 0)))"#).unwrap();
    let input = vm.new_uint8_array(bytes).unwrap();
    vm.set_global("input", input).unwrap();
    vm.eval("var spin = new WebAssembly.Instance(new WebAssembly.Module(input)).exports.spin")
        .unwrap();
    let error = vm.eval("spin()").unwrap_err();
    assert_eq!(error.kind, rust_browser_javascript::ErrorKind::ExecutionLimit);
    assert_eq!(vm.eval("2 + 2").unwrap(), Value::from(4));
}

#[test]
fn bytes_use_the_visible_view_and_call_the_javascript_import() {
    let mut vm = fixture();
    vm.eval(
        r#"
        var result, calls = 0, phase = '';
        var padded = new Uint8Array(wasmBytes.length + 2);
        padded.set(wasmBytes, 1);
        var view = padded.subarray(1, padded.length - 1);
        var valid = WebAssembly.validate(view) && !WebAssembly.validate(padded);
        var promise = WebAssembly.instantiate(view, {get env() {
            phase += 'get';
            return {sum(a, b) { calls++; return a + b; }};
        }});
        phase += 'return';
        padded[1] = 99;
        promise.then(({module, instance}) => {
            result = module instanceof WebAssembly.Module &&
                instance instanceof WebAssembly.Instance &&
                Object.getPrototypeOf(instance.exports) === null &&
                Object.isFrozen(instance.exports) &&
                instance.exports.run.length === 2 &&
                instance.exports.run(19, 23) === 42 &&
                instance.exports.run(4294967297, 41.75) === 42;
        });
    "#,
    )
    .unwrap();
    assert_eq!(vm.eval("phase").unwrap(), Value::from("return"));
    flush(&mut vm);
    assert_eq!(vm.eval("phase").unwrap(), Value::from("returnget"));
    assert_eq!(
        vm.eval("valid && result && calls === 2").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn compiled_overload_creates_an_instance_and_retains_export_dependencies() {
    let mut vm = fixture();
    vm.eval(
        r#"
        var kept, newInstanceResult, order = '';
        WebAssembly.compile(wasmBytes).then(module => {
            newInstanceResult = new WebAssembly.Instance(module, {env: {sum(a, b) {
                return a - b;
            }}}).exports.run(65, 23);
            const ready = WebAssembly.instantiate(module, {get env() {
                order += 'get';
                return {sum: (a, b) => a + b};
            }});
            order += 'return';
            ready.then(instance => kept = instance.exports.run);
        });
    "#,
    )
    .unwrap();
    flush(&mut vm);
    vm.collect_garbage();
    assert_eq!(
        vm.eval("order === 'getreturn' && newInstanceResult === 42 && kept(19, 23) === 42")
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn invalid_bytes_and_incorrect_imports_reject_with_branded_errors() {
    let mut vm = fixture();
    vm.eval(
        r#"
        var compileFailure, linkFailure, typeFailure, getterFailure;
        var invalid = new Uint8Array([0, 97, 115, 109]);
        WebAssembly.instantiate(invalid).catch(error => {
            compileFailure = error instanceof WebAssembly.CompileError && error instanceof Error;
        });
        WebAssembly.instantiate(wasmBytes, {env: {}}).catch(error => {
            linkFailure = error instanceof WebAssembly.LinkError && error instanceof Error;
        });
        WebAssembly.instantiate(wasmBytes, {env: 0}).catch(error => {
            typeFailure = error instanceof TypeError;
        });
        const sentinel = {};
        WebAssembly.instantiate(wasmBytes, {get env() { throw sentinel; }}).catch(error => {
            getterFailure = error === sentinel;
        });
    "#,
    )
    .unwrap();
    flush(&mut vm);
    assert_eq!(
        vm.eval("compileFailure && linkFailure && typeFailure && getterFailure")
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn policy_blocks_new_modules_but_allows_validation_and_precompiled_modules() {
    let mut vm = fixture();
    vm.eval("var precompiled = new WebAssembly.Module(wasmBytes);")
        .unwrap();
    vm.set_wasm_compilation_allowed(false);
    vm.eval(
        r#"
        var constructorFailure, compileFailure, instantiateFailure, existing, importReads = 0;
        var imports = {get env() {
            importReads++;
            return {sum(a, b) { return a + b; }};
        }};
        try {
            new WebAssembly.Module(wasmBytes);
        } catch (error) {
            constructorFailure = error instanceof WebAssembly.CompileError;
        }
        WebAssembly.compile(wasmBytes).catch(error => {
            compileFailure = error instanceof WebAssembly.CompileError;
        });
        WebAssembly.instantiate(wasmBytes, imports).catch(error => {
            instantiateFailure = error instanceof WebAssembly.CompileError;
        });
        var valid = WebAssembly.validate(wasmBytes);
        var sync = new WebAssembly.Instance(precompiled, imports).exports.run(19, 23);
        WebAssembly.instantiate(precompiled, imports).then(instance => {
            existing = instance.exports.run(19, 23);
        });
    "#,
    )
    .unwrap();
    flush(&mut vm);
    assert_eq!(
        vm.eval(
            "constructorFailure && compileFailure && instantiateFailure && valid && \
             sync === 42 && existing === 42 && importReads === 2"
        )
        .unwrap(),
        Value::Bool(true)
    );
    vm.set_wasm_compilation_allowed(true);
    assert_eq!(
        vm.eval("new WebAssembly.Module(wasmBytes) instanceof WebAssembly.Module")
            .unwrap(),
        Value::Bool(true)
    );
}

fn uleb(mut number: u32, bytes: &mut Vec<u8>) {
    while number >= 128 {
        bytes.push((number as u8 & 127) | 128);
        number >>= 7;
    }
    bytes.push(number as u8);
}

fn section(module: &mut Vec<u8>, id: u8, bytes: &[u8]) {
    module.push(id);
    uleb(bytes.len() as u32, module);
    module.extend_from_slice(bytes);
}

fn exports(module: &mut Vec<u8>, entries: &[(&str, u8, u8)]) {
    let mut bytes = vec![entries.len() as u8];
    for (name, tag, index) in entries {
        bytes.push(name.len() as u8);
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend([*tag, *index]);
    }
    section(module, 7, &bytes);
}

fn bodies(module: &mut Vec<u8>, instructions: &[&[u8]]) {
    let mut bytes = vec![instructions.len() as u8];
    for body in instructions {
        bytes.extend([body.len() as u8 + 2, 0]); // size, no local declarations
        bytes.extend_from_slice(body);
        bytes.push(0x0b);
    }
    section(module, 10, &bytes);
}

fn install_module(vm: &mut Vm, name: &str, bytes: Vec<u8>) {
    let source = vm.new_uint8_array(bytes).unwrap();
    vm.set_global(name, source).unwrap();
}

fn defined_memory_module() -> Vec<u8> {
    let mut module = b"\0asm\x01\0\0\0".to_vec();
    section(
        &mut module,
        1,
        &[
            3, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 0, 0x60, 0, 1, 0x7f,
        ],
    );
    section(&mut module, 3, &[6, 0, 1, 2, 0, 0, 0]);
    section(&mut module, 5, &[1, 1, 1, 2]);
    exports(
        &mut module,
        &[
            ("memory", 2, 0),
            ("load", 0, 0),
            ("store", 0, 1),
            ("size", 0, 2),
            ("grow", 0, 3),
            ("loadWithOffset", 0, 4),
            ("loadSignedByte", 0, 5),
        ],
    );
    bodies(
        &mut module,
        &[
            &[0x20, 0, 0x28, 2, 0],          // i32.load
            &[0x20, 0, 0x20, 1, 0x36, 2, 0], // i32.store
            &[0x3f, 0],                      // memory.size
            &[0x20, 0, 0x40, 0],             // memory.grow
            &[0x20, 0, 0x28, 0, 1],          // i32.load offset=1
            &[0x20, 0, 0x2c, 0, 0],          // i32.load8_s
        ],
    );
    section(
        &mut module,
        11,
        &[1, 0, 0x41, 4, 0x0b, 4, 0x78, 0x56, 0x34, 0x12],
    );
    module
}

#[test]
fn wasm_and_js_access_the_same_storage_and_grow_detaches_old_views() {
    let mut vm = Vm::new();
    install_module(&mut vm, "memoryModule", defined_memory_module());
    assert_eq!(
        vm.eval(
            r#"
            var module = new WebAssembly.Module(memoryModule);
            var exp = new WebAssembly.Instance(module).exports;
            var independent = new WebAssembly.Instance(module).exports;
            var m = exp.memory, original = m.buffer;
            var view = new Uint8Array(original), data = new DataView(original);
            var initial = m instanceof WebAssembly.Memory && m.buffer === original &&
                original instanceof ArrayBuffer && !original.resizable &&
                exp.size() === 1 && exp.load(4) === 0x12345678 &&
                independent.memory.buffer !== original;
            data.setInt32(1, 0x01020304, true);
            var visibleInWasm = exp.load(1) === 0x01020304 &&
                independent.load(4) === 0x12345678;
            exp.store(65532, -2);
            var visibleInJs = view[65532] === 254 && data.getInt32(65532, true) === -2 &&
                exp.loadSignedByte(65532) === -2;
            var trapped = 0;
            try { exp.store(65533, 0); } catch (e) { trapped += e instanceof WebAssembly.RuntimeError; }
            try { exp.load(-1); } catch (e) { trapped += e instanceof WebAssembly.RuntimeError; }
            try { exp.loadWithOffset(-1); } catch (e) { trapped += e instanceof WebAssembly.RuntimeError; }
            var atomic = exp.load(65532) === -2;
            var previous = exp.grow(1);
            var newer = m.buffer;
            var grew = previous === 1 && exp.size() === 2 && original.detached &&
                view.length === 0 && newer.byteLength === 131072 &&
                exp.load(65532) === -2 && exp.load(65536) === 0;
            var refused = exp.grow(1) === -1 && exp.grow(-1) === -1 && m.buffer === newer;
            var zero = m.grow(0) === 2 && newer.detached &&
                m.buffer !== newer && exp.load(65532) === -2;
            initial && visibleInWasm && visibleInJs && trapped === 3 && atomic && grew && refused && zero
        "#,
        )
        .unwrap(),
        Value::Bool(true)
    );
}

fn imported_memory_module(invalid_data: bool) -> Vec<u8> {
    let mut module = b"\0asm\x01\0\0\0".to_vec();
    section(&mut module, 1, &[1, 0x60, 1, 0x7f, 1, 0x7f]);
    section(
        &mut module,
        2,
        &[
            2, 1, b'e', 1, b'm', 2, 1, 1, 2, // (import "e" "m" (memory 1 2))
            1, b'e', 1, b'f', 0, 0, // (import "e" "f" (func (param i32) (result i32)))
        ],
    );
    section(&mut module, 3, &[1, 0]);
    exports(&mut module, &[("memory", 2, 0), ("read", 0, 1)]);
    // Load from the i32 address returned by the imported function.
    bodies(&mut module, &[&[0x20, 0, 0x10, 0, 0x28, 0, 0]]);
    if invalid_data {
        section(
            &mut module,
            11,
            &[2, 0, 0x41, 0, 0x0b, 1, 0xff, 0, 0x41, 0x7f, 0x0b, 1, 0xff],
        );
    }
    module
}

#[test]
fn memory_imports_check_limits_preserve_order_and_observe_reentrant_growth() {
    let mut vm = Vm::new();
    install_module(&mut vm, "memoryModule", imported_memory_module(false));
    install_module(&mut vm, "badDataModule", imported_memory_module(true));
    vm.eval(
        r#"
        var order = '', kept, imported, rangeErrors = 0, trapped, unchanged;
        var mem = new WebAssembly.Memory({initial: 1, maximum: 2});
        var old = mem.buffer;
        var fixture = new WebAssembly.Module(memoryModule);
        function instantiateWith(memory) {
            return new WebAssembly.Instance(fixture, {e: {m: memory, f() { return 0; }}});
        }
        for (const memory of [
            {},
            new WebAssembly.Memory({initial: 0, maximum: 2}),
            new WebAssembly.Memory({initial: 1, maximum: 3}),
            new WebAssembly.Memory({initial: 1})
        ]) {
            try { instantiateWith(memory); } catch (e) { rangeErrors += e instanceof WebAssembly.LinkError; }
        }
        try {
            new WebAssembly.Instance(new WebAssembly.Module(badDataModule), {e: {m: mem, f() {}}});
        } catch (e) {
            trapped = e instanceof WebAssembly.RuntimeError;
        }
        unchanged = new Uint8Array(old)[0] === 0;
        WebAssembly.instantiate(memoryModule, {e: {
            get m() { order += 'm'; return mem; },
            get f() { order += 'f'; return function(a) {
                mem.grow(1);
                new DataView(mem.buffer).setInt32(65536, a, true);
                return 65536;
            }; }
        }}).then(({instance}) => {
            imported = instance.exports.memory === mem;
            kept = instance.exports.read;
        });
    "#,
    )
    .unwrap();
    flush(&mut vm);
    vm.collect_garbage();
    assert_eq!(
        vm.eval(
            "imported && trapped && unchanged && rangeErrors === 4 && order === 'mf' && \
             kept(42) === 42 && old.detached"
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn memory_cannot_be_detached_by_transfer_and_failed_growth_is_atomic() {
    let baseline = Vm::new().estimated_heap_bytes();
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: baseline + 4 * 65_536,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval(
            r#"
            var mem = new WebAssembly.Memory({initial: 1, maximum: 20});
            var buffer = mem.buffer, view = new Uint8Array(buffer);
            view[0] = 99;
            var errors = 0;
            try { mem.grow(8); } catch (e) { errors += e instanceof RangeError; }
            try { buffer.transfer(); } catch (e) { errors += e instanceof TypeError; }
            try { buffer.transferToFixedLength(); } catch (e) { errors += e instanceof TypeError; }
            errors === 3 && mem.buffer === buffer &&
                view.length === 65536 && view[0] === 99
        "#,
        )
        .unwrap(),
        Value::Bool(true)
    );
    let buffer = vm.get_global("buffer").unwrap();
    assert!(vm.detach_array_buffer(&buffer).is_err());
    assert!(vm.transfer_array_buffer(&buffer).is_err());
    assert!(matches!(
        vm.structured_clone(buffer.clone(), std::slice::from_ref(&buffer)),
        Err(StructuredCloneError::DataClone(_))
    ));
    let view = vm.get_global("view").unwrap();
    let copy = vm.structured_clone(view, &[]).unwrap();
    vm.set_global("copy", copy).unwrap();
    assert_eq!(
        vm.eval("copy[0] === 99 && copy.buffer !== buffer && !buffer.detached")
            .unwrap(),
        Value::Bool(true)
    );
    assert!(matches!(
        vm.serialize_clone_packet_with_transfer(
            buffer.clone(),
            &[buffer],
            128 * 1024,
            |_| -> Result<(), _> {
                panic!("a WebAssembly store must not be admitted for transfer")
            },
        ),
        Err(StructuredCloneError::DataClone(_))
    ));
}

#[test]
fn memory_grow_reads_current_state_after_coercion() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var m = new WebAssembly.Memory({initial: 0, maximum: 2});
            var empty = m.buffer, middle;
            var previous = m.grow({valueOf() {
                m.grow(1);
                middle = m.buffer;
                new Uint8Array(middle)[0] = 42;
                return 1;
            }});
            previous === 1 && empty.detached && middle.detached &&
                m.buffer.byteLength === 131072 && new Uint8Array(m.buffer)[0] === 42
        "#,
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn ivs_sized_memory_and_views_fit_a_single_backing_budget() {
    const BYTES: usize = 32 * 1024 * 1024;
    let baseline = Vm::new().estimated_heap_bytes();
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: baseline + BYTES + 512 * 1024,
        ..VmLimits::default()
    });
    vm.eval("var mem = new WebAssembly.Memory({initial:512, maximum:1024});")
        .unwrap();
    assert_eq!(
        vm.eval(
            r#"
            var u8 = new Uint8Array(mem.buffer), u32 = new Uint32Array(mem.buffer);
            var data = new DataView(mem.buffer);
            u8[u8.length - 1] = 42;
            data.getUint8(u32.length * 4 - 1)
            "#,
        )
        .unwrap(),
        Value::from(42)
    );
    vm.eval("mem = u8 = u32 = data = null;").unwrap();
    vm.collect_garbage();
    assert!(vm.estimated_heap_bytes() < baseline + 512 * 1024);
}
