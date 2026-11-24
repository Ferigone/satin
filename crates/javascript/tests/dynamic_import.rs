use rust_browser_javascript::{DynamicImportRequest, ErrorKind, RootId, Value, Vm};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

struct PendingImport {
    request: DynamicImportRequest,
    roots: [RootId; 3],
}

#[test]
fn dynamic_import_returns_intrinsic_promises_and_preserves_defining_referrers() {
    let mut vm = Vm::new();
    let imports = Rc::new(RefCell::new(VecDeque::new()));
    let loaded = imports.clone();
    vm.set_dynamic_import_hook(move |vm, request| {
        let roots = [
            vm.root(request.capability.promise.clone()),
            vm.root(request.capability.resolve.clone()),
            vm.root(request.capability.reject.clone()),
        ];
        loaded
            .borrow_mut()
            .push_back(PendingImport { request, roots });
        Ok(())
    });
    let script = "https://example.test/scripts/defining.js";
    let module_url = "https://example.test/modules/defining.js";
    vm.eval_with_identifier(
        "var seen=[], later=async()=>{await 0;return import('./dependency.js');};",
        script,
    )
    .unwrap();
    let owner = vm
        .parse_module(
            "export async function load(){await 0;return import('./dependency.js',);}",
            module_url,
        )
        .unwrap();
    vm.link_module(owner, |_, _, _| unreachable!()).unwrap();
    vm.evaluate_module(owner).unwrap();
    let namespace = vm.module_namespace(owner).unwrap();
    let load = vm.get_property(namespace, "load").unwrap();
    vm.set_global("moduleLoad", load).unwrap();
    vm.collect_garbage();
    vm.eval_with_identifier(
        "var first=later(),second=moduleLoad(); first.then(m=>seen.push(m)); second.then(m=>seen.push(m));",
        "https://example.test/callers/caller.js",
    ).unwrap();
    let first = vm.get_global("first").unwrap();
    let second = vm.get_global("second").unwrap();
    assert_ne!(first, second);
    assert!(vm.is_promise(&first));
    assert!(vm.is_promise(&second));
    assert!(imports.borrow().is_empty());
    vm.drain_jobs(100).unwrap();
    assert_eq!(imports.borrow().len(), 2);
    vm.collect_garbage();
    let dependency = vm
        .parse_module("export const result=42;", "dependency")
        .unwrap();
    vm.link_module(dependency, |_, _, _| unreachable!())
        .unwrap();
    vm.evaluate_module(dependency).unwrap();
    let dep_namespace = vm.module_namespace(dependency).unwrap();
    for expected in [script, module_url] {
        let pending = imports.borrow_mut().pop_front().unwrap();
        assert_eq!(
            pending.request.referrer.unwrap().to_string_lossy(),
            expected
        );
        assert_eq!(
            pending.request.specifier.to_string_lossy(),
            "./dependency.js"
        );
        vm.call(
            pending.request.capability.resolve,
            Value::Undefined,
            std::slice::from_ref(&dep_namespace),
        )
        .unwrap();
        for root in pending.roots {
            vm.unroot(root);
        }
    }
    vm.drain_jobs(100).unwrap();
    assert_eq!(
        vm.eval("seen.length===2 && seen[0]===seen[1] && seen[0].result===42")
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn import_expression_throws_before_loading_and_coercion_rejects_the_promise() {
    let mut vm = Vm::new();
    vm.set_dynamic_import_hook(|_, _| unreachable!("failed coercion must not call the host"));
    vm.eval(
        r#"
        var marker={}, sync=false, rejected=false, symbolError=false;
        var o={};
        Object.defineProperty(o,'key',{get(){throw marker;}});
        try { import(o.key); } catch(error) { sync=error===marker; }
        import({toString(){throw marker;}}).catch(error=>rejected=error===marker);
        import(Symbol()).catch(error=>symbolError=error instanceof TypeError);
    "#,
    )
    .unwrap();
    assert_eq!(vm.get_global("sync").unwrap(), Value::Bool(true));
    assert_eq!(vm.get_global("rejected").unwrap(), Value::Bool(false));
    vm.drain_jobs(100).unwrap();
    assert_eq!(
        vm.eval("rejected && symbolError").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        vm.check_syntax("new import('./x')").unwrap_err().kind,
        ErrorKind::SyntaxError
    );
    vm.check_syntax("import('./x', {})").unwrap();
}

#[test]
fn dynamic_import_in_later_class_field_initialization_uses_original_script() {
    let mut vm = Vm::new();
    vm.set_dynamic_import_hook(|vm, request| {
        let identifier = request.referrer.unwrap();
        vm.call(
            request.capability.resolve,
            Value::Undefined,
            &[Value::String(identifier)],
        )?;
        Ok(())
    });
    vm.eval_with_identifier(
        "class Original { fromField=import('./side.js'); }",
        "source.js",
    )
    .unwrap();
    vm.eval_with_identifier(
        "var captured;new Original().fromField.then(value=>captured=value);",
        "caller.js",
    )
    .unwrap();
    vm.drain_jobs(100).unwrap();
    assert_eq!(vm.get_global("captured").unwrap(), Value::from("source.js"));
}

#[test]
fn dynamic_import_settlement_follows_module_completion_order() {
    for reject in [false, true] {
        let mut vm = Vm::new();
        vm.eval("var gate=Promise.withResolvers(), order=[], reason={};")
            .unwrap();
        let leaf = vm
            .parse_module("export const value=await gate.promise;", "leaf")
            .unwrap();
        let root = vm
            .parse_module("import './leaf'; export const value=4;", "root")
            .unwrap();
        vm.link_module(root, |_, _, _| Ok(leaf)).unwrap();
        let root_import = vm.new_promise_capability().unwrap();
        let leaf_import = vm.new_promise_capability().unwrap();
        vm.set_global("rootImport", root_import.promise.clone())
            .unwrap();
        vm.set_global("leafImport", leaf_import.promise.clone())
            .unwrap();
        vm.eval(r#"
            rootImport.then(ns=>order.push('root'+ns.value), e=>order.push(e===reason?'root error':'wrong'));
            leafImport.then(ns=>order.push('leaf'+ns.value), e=>order.push(e===reason?'leaf error':'wrong'));
        "#).unwrap();
        vm.settle_import_after_evaluation(root, root_import)
            .unwrap();
        vm.settle_import_after_evaluation(leaf, leaf_import)
            .unwrap();
        vm.collect_garbage();
        if reject {
            vm.eval("gate.reject(reason)").unwrap();
        } else {
            vm.eval("gate.resolve(7)").unwrap();
        }
        vm.drain_jobs(100).unwrap();
        let expected = if reject {
            "leaf error,root error"
        } else {
            "leaf7,root4"
        };
        assert_eq!(vm.eval("order.join(',')").unwrap(), Value::from(expected));
        assert!(vm.take_unhandled_rejections().is_empty());
    }
}

#[test]
fn import_options_preserve_evaluation_order_and_reject_attribute_errors() {
    let mut vm = Vm::new();
    vm.set_dynamic_import_hook(|vm, request| {
        vm.call(
            request.capability.resolve,
            Value::Undefined,
            &[Value::from("loaded")],
        )?;
        Ok(())
    });
    vm.eval(
        r#"
        let order = "";
        const marker = {};
        const attributes = {
          get a() { order += "a"; return 1; },
          get b() { order += "b"; throw marker; }
        };
        let reason, loaded;
        import({toString(){order += "s"; return "x";}},
               (order += "o", {get with(){order += "w"; return attributes;}}))
          .catch(e => reason = e);
        import("x", {with: {}}).then(v => loaded = v);
    "#,
    )
    .unwrap();
    vm.drain_jobs(100).unwrap();
    assert_eq!(
        vm.eval(r#"order === "oswab" && reason === marker && loaded === "loaded""#)
            .unwrap(),
        Value::Bool(true)
    );
    for source in [
        "import.defer()",
        "import.source(...x)",
        "import.defer('x', {}, 1)",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError
        );
    }
    vm.check_syntax("if (false) import.defer('x', {});")
        .unwrap();
}
