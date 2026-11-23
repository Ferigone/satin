use rust_browser_javascript::{Value, Vm};

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
