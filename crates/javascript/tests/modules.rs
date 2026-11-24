use rust_browser_javascript::{ErrorKind, ModuleId, ModuleStatus, Value, Vm, VmLimits};

fn link(vm: &mut Vm, root: ModuleId, modules: &[(&str, ModuleId)]) {
    vm.link_module(root, |_, _, specifier| {
        Ok(modules
            .iter()
            .find(|(name, _)| specifier.to_string_lossy() == *name)
            .unwrap()
            .1)
    })
    .unwrap();
}

#[test]
fn module_bindings_are_live_across_reexports_and_separate_from_script_globals() {
    let mut vm = Vm::new();
    let counter = vm
        .parse_module(
            "export let n=1;export function increment(){n++;}export const top=this;",
            "counter",
        )
        .unwrap();
    let bridge = vm
        .parse_module(
            "export {n as count,increment,top} from './counter';",
            "bridge",
        )
        .unwrap();
    let main = vm.parse_module("import {count,increment,top} from './bridge';import * as ns from './bridge';var hidden=2;export function snapshot(){return [count,ns.count,top,hidden];} increment();", "https://example.test/main.js").unwrap();
    let root = vm.root_module(main).unwrap();
    assert_eq!(vm.module_requests(main).unwrap().len(), 1);
    link(
        &mut vm,
        main,
        &[("./counter", counter), ("./bridge", bridge)],
    );
    let ns = vm.module_namespace(main).unwrap();
    let snapshot = vm.get_property(ns, "snapshot").unwrap();
    let promise = vm.evaluate_module(main).unwrap();
    assert_eq!(vm.evaluate_module(main).unwrap(), promise);
    let values = vm.call(snapshot.clone(), Value::Undefined, &[]).unwrap();
    assert_eq!(
        vm.get_property(values.clone(), "0").unwrap(),
        Value::from(2)
    );
    assert_eq!(
        vm.get_property(values.clone(), "1").unwrap(),
        Value::from(2)
    );
    assert_eq!(
        vm.get_property(values.clone(), "2").unwrap(),
        Value::Undefined
    );
    assert_eq!(vm.get_property(values, "3").unwrap(), Value::from(2));
    assert_eq!(vm.get_global("hidden").unwrap(), Value::Undefined);
    vm.collect_garbage();
    let counter_ns = vm.module_namespace(counter).unwrap();
    let increment = vm.get_property(counter_ns, "increment").unwrap();
    vm.call(increment, Value::Undefined, &[]).unwrap();
    let values = vm.call(snapshot, Value::Undefined, &[]).unwrap();
    assert_eq!(vm.get_property(values, "0").unwrap(), Value::from(3));
    vm.unroot(root);
    vm.collect_garbage();
    assert!(vm.module_status(main).is_err());
    assert!(vm.module_status(counter).is_err());
}

#[test]
fn cyclic_modules_hoist_functions_and_evaluate_in_dependency_order_once() {
    let mut vm = Vm::new();
    vm.eval("var order=[];").unwrap();
    let a=vm.parse_module("import {b} from './b';export function a(){return 4;}var fromB=b();order.push('a'+fromB);", "a").unwrap();
    let b = vm
        .parse_module(
            "import {a} from './a';export function b(){return a()+1;}order.push('b'+a());",
            "b",
        )
        .unwrap();
    link(&mut vm, a, &[("./a", a), ("./b", b)]);
    let promise = vm.evaluate_module(a).unwrap();
    assert_eq!(vm.evaluate_module(b).unwrap(), promise);
    assert_eq!(vm.module_status(a).unwrap(), ModuleStatus::Evaluated);
    assert_eq!(vm.module_status(b).unwrap(), ModuleStatus::Evaluated);
    assert_eq!(vm.eval("order.join(',')").unwrap(), Value::from("b4,a5"));
}

#[test]
fn cyclic_tdz_and_ordinary_throw_reject_with_the_original_reason() {
    let mut vm = Vm::new();
    let a = vm
        .parse_module("import './b';export const x=1;", "a")
        .unwrap();
    let b = vm.parse_module("import {x} from './a';x;", "b").unwrap();
    link(&mut vm, a, &[("./a", a), ("./b", b)]);
    let promise = vm.evaluate_module(a).unwrap();
    let error = vm.module_evaluation_error(a).unwrap().unwrap();
    assert_eq!(error.kind, ErrorKind::ReferenceError);
    assert_eq!(
        vm.module_evaluation_error(b).unwrap().unwrap().thrown,
        error.thrown
    );
    assert_eq!(vm.evaluate_module(b).unwrap(), promise);
    assert_eq!(vm.take_unhandled_rejections(), vec![error.thrown.unwrap()]);
    let module = vm.parse_module("throw 17;", "throws").unwrap();
    link(&mut vm, module, &[]);
    vm.evaluate_module(module).unwrap();
    assert_eq!(vm.take_unhandled_rejections(), vec![Value::from(17)]);
}

#[test]
fn namespaces_have_live_data_descriptors_read_only_writes_and_sorted_keys() {
    let mut vm = Vm::new();
    let module=vm.parse_module("let number=0;export {number as 'z',number as '10',number as '2'};export function change(){number++;}", "names").unwrap();
    link(&mut vm, module, &[]);
    let namespace = vm.module_namespace(module).unwrap();
    assert_eq!(vm.module_namespace(module).unwrap(), namespace);
    vm.set_global("ns", namespace.clone()).unwrap();
    assert_eq!(
        vm.eval("Object.getOwnPropertyNames(ns).join(',')").unwrap(),
        Value::from("10,2,change,z")
    );
    assert_eq!(
        vm.eval("Object.keys(ns)").unwrap_err().kind,
        ErrorKind::ReferenceError
    );
    assert_eq!(vm.eval("'z' in ns").unwrap(), Value::Bool(true));
    assert_eq!(
        vm.get_property(namespace, "z").unwrap_err().kind,
        ErrorKind::ReferenceError
    );
    vm.evaluate_module(module).unwrap();
    assert_eq!(vm.eval("ns.change();var p=Object.getOwnPropertyDescriptor(ns,'z');p.value===1&&p.writable&&p.enumerable&&!p.configurable&&Object.getPrototypeOf(ns)===null&&!Object.isExtensible(ns)&&Object.isSealed(ns)&&!Object.isFrozen(ns)&&Object.prototype.toString.call(ns)==='[object Module]'").unwrap(),Value::Bool(true));
    assert_eq!(vm.eval("ns.z=42;delete ns.z").unwrap(), Value::Bool(false));
    assert_eq!(
        vm.eval("'use strict';ns.z=42").unwrap_err().kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        vm.eval("Object.defineProperty(ns,'z',{value:1})===ns")
            .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        vm.eval("Object.defineProperty(ns,'z',{value:2})")
            .unwrap_err()
            .kind,
        ErrorKind::TypeError
    );
    assert_eq!(
        vm.eval("Object.freeze(ns)").unwrap_err().kind,
        ErrorKind::TypeError
    );
}

#[test]
fn star_resolution_distinguishes_conflicts_and_shared_namespace_identity() {
    let mut vm = Vm::new();
    let a = vm
        .parse_module("export const x=1;export default 7;", "a")
        .unwrap();
    let b = vm.parse_module("export const x=2;", "b").unwrap();
    let c = vm
        .parse_module("export * from './a';export * from './b';", "c")
        .unwrap();
    let d = vm
        .parse_module("export * as same from './a';", "d")
        .unwrap();
    let e = vm
        .parse_module("export * as same from './a';", "e")
        .unwrap();
    let f = vm
        .parse_module("export * from './d';export * from './e';", "f")
        .unwrap();
    let bad = vm.parse_module("import {x} from './c';", "bad").unwrap();
    let all = [("./a", a), ("./b", b), ("./c", c), ("./d", d), ("./e", e)];
    link(&mut vm, c, &all);
    let namespace = vm.module_namespace(c).unwrap();
    assert_eq!(
        vm.get_property(namespace.clone(), "x").unwrap(),
        Value::Undefined
    );
    assert_eq!(
        vm.get_property(namespace, "default").unwrap(),
        Value::Undefined
    );
    let failure = vm.link_module(bad, |_, _, _| Ok(c)).unwrap_err();
    assert_eq!(failure.kind, ErrorKind::SyntaxError);
    assert_eq!(vm.module_status(bad).unwrap(), ModuleStatus::Unlinked);
    // A retry can use a different host mapping after the failed link.
    link(&mut vm, bad, &[("./c", a)]);
    link(&mut vm, f, &all);
    let ns = vm.module_namespace(f).unwrap();
    assert_eq!(
        vm.get_property(ns, "same").unwrap(),
        vm.module_namespace(a).unwrap()
    );
}

#[test]
fn import_meta_uses_one_mutable_null_prototype_object_per_record() {
    let mut vm = Vm::new();
    let module=vm.parse_module("export const saved=import.meta;export function read(){return import.meta;}saved.extra=4;", "https://example.test/module.js").unwrap();
    let meta = vm.module_import_meta(module).unwrap();
    link(&mut vm, module, &[]);
    vm.evaluate_module(module).unwrap();
    let ns = vm.module_namespace(module).unwrap();
    assert_eq!(vm.get_property(ns.clone(), "saved").unwrap(), meta);
    assert_eq!(
        vm.get_property(meta.clone(), "url").unwrap(),
        Value::from("https://example.test/module.js")
    );
    assert_eq!(
        vm.get_property(meta.clone(), "extra").unwrap(),
        Value::from(4)
    );
    let read = vm.get_property(ns, "read").unwrap();
    assert_eq!(vm.call(read, Value::Undefined, &[]).unwrap(), meta);
}

#[test]
fn module_early_errors_and_unsupported_productions_are_distinct() {
    let mut vm = Vm::new();
    vm.check_module_syntax("let item; for await ([item] of []) {}")
        .unwrap();
    vm.check_module_syntax("import './m' with {type:'json'};").unwrap();
    for source in [
        "import './m' with {type:'json', 'type':'text'};",
        "export {missing};",
        "export let n;export {n};",
        "function f(){} function f(){}",
        "import {x} from './m';var x;",
        "import x from './m';import x from './m';",
        "with({}){}",
        "if(true){export let n;}",
    ] {
        assert_eq!(
            vm.check_module_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
    for source in [
        "let item; for await ([item = await 1] of []) {}",
        "import source m from './m';",
    ] {
        assert_eq!(
            vm.check_module_syntax(source).unwrap_err().kind,
            ErrorKind::UnsupportedFeature,
            "{source}"
        );
    }
}

#[test]
fn large_graphs_use_explicit_frames_and_task_limits_are_sticky() {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let mut vm = Vm::new();
            let mut modules = Vec::new();
            for index in 0..180 {
                let source = if index == 0 {
                    "export const item=1;".to_owned()
                } else {
                    format!("export {{item}} from '{}';", index - 1)
                };
                modules.push(vm.parse_module(&source, &index.to_string()).unwrap());
            }
            let root = *modules.last().unwrap();
            vm.link_module(root, |_, _, request| {
                Ok(modules[request.to_string_lossy().parse::<usize>().unwrap()])
            })
            .unwrap();
            vm.evaluate_module(root).unwrap();
            let namespace = vm.module_namespace(root).unwrap();
            assert_eq!(vm.get_property(namespace, "item").unwrap(), Value::from(1));
        })
        .unwrap()
        .join()
        .unwrap();
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 300,
        ..VmLimits::default()
    });
    let module = vm.parse_module("while(true){}", "infinite").unwrap();
    link(&mut vm, module, &[]);
    for _ in 0..2 {
        assert_eq!(
            vm.evaluate_module(module).unwrap_err().kind,
            ErrorKind::ExecutionLimit
        );
    }
    assert_eq!(vm.module_status(module).unwrap(), ModuleStatus::Errored);
}

#[test]
fn observing_module_result_handles_only_the_evaluation_rejection() {
    let mut vm = Vm::new();
    let module = vm
        .parse_module("Promise.reject('ordinary');throw 42;", "errors")
        .unwrap();
    link(&mut vm, module, &[]);
    assert!(vm.module_evaluation_result(module).unwrap().is_none());
    vm.evaluate_module(module).unwrap();
    let error = vm
        .module_evaluation_result(module)
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.thrown, Some(Value::from(42)));
    assert_eq!(
        vm.take_unhandled_rejections(),
        vec![Value::from("ordinary")]
    );
}
#[test]
fn module_handles_are_checked_against_their_own_vm() {
    let mut first = Vm::new();
    let module = first.parse_module("export const x=1;", "first").unwrap();
    let mut second = Vm::new();
    second.parse_module("export const x=2;", "second").unwrap();
    assert_eq!(
        second.module_status(module).unwrap_err().kind,
        ErrorKind::TypeError
    );
}

#[test]
fn top_level_await_starts_siblings_then_releases_dependents() {
    let mut vm = Vm::new();
    vm.eval("var gate=Promise.withResolvers(), order=[];")
        .unwrap();
    let dependency = vm
        .parse_module(
            "order.push('start');export const n=await gate.promise;order.push('value:'+n);",
            "dep",
        )
        .unwrap();
    let sibling = vm
        .parse_module("order.push('sibling');", "sibling")
        .unwrap();
    let root = vm.parse_module(
        "import {n} from './dep';import './sibling';order.push('root:'+n);export const result=n*2;", "root",
    ).unwrap();
    let handle = vm.root_module(root).unwrap();
    link(
        &mut vm,
        root,
        &[("./dep", dependency), ("./sibling", sibling)],
    );
    let promise = vm.evaluate_module(root).unwrap();
    assert_eq!(vm.evaluate_module(root).unwrap(), promise);
    assert!(vm.is_promise(&promise));
    assert!(vm.module_evaluation_result(root).unwrap().is_none());
    assert_eq!(
        vm.module_status(dependency).unwrap(),
        ModuleStatus::Evaluating
    );
    assert_eq!(vm.module_status(sibling).unwrap(), ModuleStatus::Evaluated);
    assert_eq!(
        vm.eval("order.join(',')").unwrap(),
        Value::from("start,sibling")
    );
    let dependency_namespace = vm.module_namespace(dependency).unwrap();
    assert_eq!(
        vm.get_property(dependency_namespace, "n").unwrap_err().kind,
        ErrorKind::ReferenceError
    );
    vm.collect_garbage();
    vm.eval("gate.resolve(21)").unwrap();
    vm.collect_garbage();
    assert_eq!(vm.drain_jobs(1).unwrap(), 1);
    assert!(vm.module_evaluation_result(root).unwrap().is_none());
    vm.drain_jobs(100).unwrap();
    assert!(vm.module_evaluation_result(root).unwrap().unwrap().is_ok());
    assert_eq!(
        vm.eval("order.join(',')").unwrap(),
        Value::from("start,sibling,value:21,root:21")
    );
    let namespace = vm.module_namespace(root).unwrap();
    assert_eq!(
        vm.get_property(namespace, "result").unwrap(),
        Value::from(42)
    );
    vm.unroot(handle);
    vm.collect_garbage();
    assert!(vm.module_status(root).is_err());
}

#[test]
fn cyclic_and_concurrent_roots_share_a_suspended_module() {
    let mut vm = Vm::new();
    vm.eval("var gate=Promise.withResolvers(), trace=[];")
        .unwrap();
    let a = vm
        .parse_module(
            "import {b} from './b';export function hoisted(){return 3;}trace.push('a'+b);",
            "a",
        )
        .unwrap();
    let b=vm.parse_module("import {hoisted} from './a';trace.push('b'+hoisted());export const b=await gate.promise;", "b").unwrap();
    let extra = vm
        .parse_module("import {b} from './b';trace.push('extra'+b);", "extra")
        .unwrap();
    let modules = [("./a", a), ("./b", b)];
    link(&mut vm, a, &modules);
    link(&mut vm, extra, &modules);
    let first = vm.evaluate_module(a).unwrap();
    assert_eq!(vm.evaluate_module(b).unwrap(), first);
    assert_ne!(vm.evaluate_module(extra).unwrap(), first);
    assert!(vm.module_evaluation_result(extra).unwrap().is_none());
    assert_eq!(vm.eval("trace.join(',')").unwrap(), Value::from("b3"));
    vm.eval("gate.resolve(5)").unwrap();
    vm.drain_jobs(100).unwrap();
    assert!(vm.module_evaluation_result(a).unwrap().unwrap().is_ok());
    assert!(vm.module_evaluation_result(extra).unwrap().unwrap().is_ok());
    assert_eq!(
        vm.eval("trace.join(',')").unwrap(),
        Value::from("b3,a5,extra5")
    );
}

#[test]
fn async_module_rejection_preserves_the_original_reason_and_blocks_importers() {
    let mut vm = Vm::new();
    vm.eval("var gate=Promise.withResolvers(), reason={}, started=false;")
        .unwrap();
    let dependency = vm
        .parse_module("try { await gate.promise; } finally { await 0; }", "dep")
        .unwrap();
    let main = vm
        .parse_module("import './dep';started=true;", "main")
        .unwrap();
    link(&mut vm, main, &[("./dep", dependency)]);
    vm.evaluate_module(main).unwrap();
    vm.eval("gate.reject(reason)").unwrap();
    vm.drain_jobs(100).unwrap();
    let error = vm
        .module_evaluation_result(main)
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.thrown, Some(vm.get_global("reason").unwrap()));
    assert_eq!(vm.module_status(dependency).unwrap(), ModuleStatus::Errored);
    assert_eq!(vm.get_global("started").unwrap(), Value::Bool(false));
    assert!(vm.take_unhandled_rejections().is_empty());
}

#[test]
fn async_module_dependency_chains_and_resumption_limits_fit_a_one_mib_stack() {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let mut vm = Vm::new();
            let mut modules = Vec::new();
            for index in 0..180 {
                let source = if index == 0 {
                    "export const v=await 9;".to_string()
                } else {
                    format!("export {{v}} from '{}';", index - 1)
                };
                modules.push(vm.parse_module(&source, &index.to_string()).unwrap());
            }
            let root = *modules.last().unwrap();
            vm.link_module(root, |_, _, request| {
                Ok(modules[request.to_string_lossy().parse::<usize>().unwrap()])
            })
            .unwrap();
            vm.evaluate_module(root).unwrap();
            assert!(vm.module_evaluation_result(root).unwrap().is_none());
            vm.drain_jobs(100).unwrap();
            assert!(vm.module_evaluation_result(root).unwrap().unwrap().is_ok());
            let namespace = vm.module_namespace(root).unwrap();
            assert_eq!(vm.get_property(namespace, "v").unwrap(), Value::from(9));

            let mut vm = Vm::with_limits(VmLimits {
                max_instructions: 300,
                ..VmLimits::default()
            });
            let fatal = vm.parse_module("await 0;while(true){}", "fatal").unwrap();
            link(&mut vm, fatal, &[]);
            vm.evaluate_module(fatal).unwrap();
            assert_eq!(
                vm.drain_jobs(100).unwrap_err().kind,
                ErrorKind::ExecutionLimit
            );
            assert_eq!(vm.module_status(fatal).unwrap(), ModuleStatus::Errored);
            assert_eq!(
                vm.evaluate_module(fatal).unwrap_err().kind,
                ErrorKind::ExecutionLimit
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn clearing_a_queued_resume_records_module_failure() {
    let mut vm = Vm::new();
    let root = vm.parse_module("await 1;", "discarded").unwrap();
    link(&mut vm, root, &[]);
    vm.evaluate_module(root).unwrap();
    assert_eq!(vm.clear_jobs(), 1);
    assert_eq!(
        vm.module_evaluation_result(root)
            .unwrap()
            .unwrap()
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit
    );
}

#[test]
fn typed_module_requests_preserve_distinct_instances_and_raw_bytes() {
    use rust_browser_javascript::ModuleType;
    let mut vm = Vm::new();
    let bytes = [0xef, 0xbb, 0xbf, 0xff];
    let raw = vm.parse_module_bytes(&bytes, "shared", ModuleType::Bytes).unwrap();
    let text = vm.parse_module_bytes(&bytes, "shared", ModuleType::Text).unwrap();
    let main = vm.parse_module(r"import b from 'shared' with {type:'bytes'}; import t from 'shared' with {type:'text'}; export const ok = b[0] === 239 && b[3] === 255 && b.buffer.immutable && t === '\uFFFD';", "main").unwrap();
    vm.link_module_with_requests(main, |_, _, request| match request.module_type()? {
        ModuleType::Text => Ok(text),
        ModuleType::Bytes => Ok(raw),
        _ => unreachable!(),
    }).unwrap();
    vm.evaluate_module(main).unwrap();
    let ns = vm.module_namespace(main).unwrap();
    assert_eq!(vm.get_property(ns, "ok").unwrap(), Value::Bool(true));
}
