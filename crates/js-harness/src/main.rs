//! A process boundary for conformance tests. Every request gets a fresh Rust VM.
//! Node/Python orchestrators must never evaluate the test source themselves.
mod dynamic_host;
mod missing_host;
mod module_host;

use dynamic_host::DynamicHost;
use module_host::{ModuleEdge, ModuleFile};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm, VmLimits};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::{self, BufRead, Write},
    rc::Rc,
    time::Instant,
};

const PROTOCOL_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    source: String,
    #[serde(default)]
    setup: Vec<Setup>,
    #[serde(default)]
    strict: bool,
    #[serde(default)]
    parse_only: bool,
    #[serde(default)]
    async_test: bool,
    max_instructions: Option<u64>,
    #[serde(default)]
    module_goal: bool,
    #[serde(default)]
    module_files: Vec<ModuleFile>,
    #[serde(default)]
    module_edges: Vec<ModuleEdge>,
    #[serde(default)]
    dynamic_files: Option<Vec<ModuleFile>>,
    #[serde(default)]
    link_only: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Setup {
    name: String,
    source: String,
}
#[derive(Default)]
struct AsyncStatus {
    complete: usize,
    failure: Option<String>,
}

fn error_json(error: &JsError) -> Json {
    let mut details = json!({"kind":error.kind.name(), "message":error.message,
        "line":error.line,"column":error.column});
    if let Some(facility) = missing_host::facility(error) {
        details["unsupported_host"] = json!(facility);
    }
    details
}

fn value_json(value: &Value) -> Json {
    match value {
        Value::Undefined => json!({"type":"undefined"}),
        Value::Null => json!({"type":"null"}),
        Value::Bool(value) => json!({"type":"boolean","value":value}),
        Value::Number(value) => json!({"type":"number","value":if value.is_nan() {
            Json::String("NaN".into())
        } else if *value == f64::INFINITY { Json::String("Infinity".into())
        } else if *value == f64::NEG_INFINITY { Json::String("-Infinity".into())
        } else if *value == 0. && value.is_sign_negative() { Json::String("-0".into())
        } else { json!(value) }}),
        Value::String(value) => json!({"type":"string","utf16":value.as_utf16()}),
        Value::Object(_) => json!({"type":"object"}),
        Value::BigInt(value) => json!({"type":"bigint","value":value.to_string()}),
        Value::Symbol(_) => json!({"type":"symbol"}),
    }
}

fn install_test262_host(
    vm: &mut Vm,
    identifier: String,
    messages: Rc<RefCell<Vec<String>>>,
    printed_status: Rc<RefCell<AsyncStatus>>,
) -> Result<Value, JsError> {
    let next_identifier = identifier.clone();
    let child_messages = messages.clone();
    let child_status = printed_status.clone();
    let create = vm.new_native_function("createRealm", move |vm, _, _| {
        let (realm, _) = vm.new_realm()?;
        vm.with_realm(realm, |vm| {
            install_test262_host(
                vm,
                next_identifier.clone(),
                child_messages.clone(),
                child_status.clone(),
            )
        })
    });
    let host = vm.new_object();
    let eval_script = vm.new_native_function("evalScript", move |vm, _, args| {
        let Some(Value::String(source)) = args.first() else {
            return Err(JsError::type_error("evalScript requires a source string"));
        };
        // The worker's source parser takes UTF-8. Do not silently rewrite
        // unpaired surrogates, or allocate an unbounded conversion buffer.
        if source.len() > vm.limits().max_source_bytes {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "evalScript source exceeds configured byte limit",
            ));
        }
        vm.charge_work(source.len().saturating_mul(2))?;
        vm.ensure_string_allocation(source.len())?;
        let source = String::from_utf16(source.as_utf16()).map_err(|_| {
            JsError::new(
                ErrorKind::UnsupportedFeature,
                "evalScript source containing isolated surrogate code units is not implemented",
            )
        })?;
        // Use ScriptEvaluation (not eval code) in the current VM and task.
        vm.eval_with_identifier(&source, &identifier)
    });
    let detach = vm.new_native_function("detachArrayBuffer", |vm, _, args| {
        if args.get(1).is_some_and(|key| !key.is_undefined()) {
            return Err(JsError::type_error(
                "ArrayBuffer detachment key does not match",
            ));
        }
        vm.detach_array_buffer(args.first().unwrap_or(&Value::Undefined))?;
        Ok(Value::Undefined)
    });
    vm.define_data_property(host.clone(), "evalScript", eval_script, true, true, true)?;
    vm.define_data_property(host.clone(), "detachArrayBuffer", detach, true, true, true)?;
    vm.define_data_property(host.clone(), "global", vm.global(), true, true, true)?;
    vm.define_data_property(host.clone(), "createRealm", create, true, true, true)?;
    missing_host::install(vm, &host)?;
    let print = vm.new_native_function("print", move |vm, _, args| {
        // INTERPRETING.md specifies print's first argument. Keep protocol
        // observations even after the diagnostic log's capacity is exhausted.
        let value = vm.to_string(args.first().unwrap_or(&Value::Undefined))?;
        if value.as_utf16()
            == "Test262:AsyncTestComplete"
                .encode_utf16()
                .collect::<Vec<_>>()
        {
            printed_status.borrow_mut().complete += 1;
        }
        let line = String::from_utf16_lossy(&value.as_utf16()[..value.len().min(2048)]);
        if value.as_utf16().starts_with(
            &"Test262:AsyncTestFailure:"
                .encode_utf16()
                .collect::<Vec<_>>(),
        ) {
            printed_status.borrow_mut().failure = Some(line.clone());
        }
        // A test can spam print; diagnostics are deliberately bounded.
        let mut messages = messages.borrow_mut();
        if messages.len() < 64 {
            messages.push(line.chars().take(2048).collect());
        }
        Ok(Value::Undefined)
    });
    vm.define_data_property(vm.global(), "print", print, true, false, true)?;
    vm.define_data_property(vm.global(), "$262", host.clone(), true, false, true)?;
    Ok(host)
}

fn execute(request: Request) -> Json {
    let start = Instant::now();
    let limits = VmLimits {
        max_instructions: request
            .max_instructions
            .unwrap_or(1_000_000)
            .clamp(1, 100_000_000),
        ..VmLimits::default()
    };
    let mut vm = Vm::with_limits(limits);
    let logs = Rc::new(RefCell::new(Vec::<String>::new()));
    let async_status = Rc::new(RefCell::new(AsyncStatus::default()));
    if let Err(error) = install_test262_host(
        &mut vm,
        request.id.clone(),
        logs.clone(),
        async_status.clone(),
    ) {
        return json!({"id":request.id,"outcome":"error","phase":"host","error":error_json(&error)});
    }
    let original_for_import = request
        .dynamic_files
        .as_ref()
        .map(|_| request.source.clone());
    let source = if request.strict && !request.module_goal {
        format!("\"use strict\";\n{}", request.source)
    } else {
        request.source
    };
    let parsed = if request.module_goal {
        vm.parse_module(&source, &request.id).map(Some)
    } else {
        vm.check_syntax(&source).map(|()| None)
    };
    let result = match parsed {
        Err(error) => {
            json!({"id":request.id,"outcome":"error","phase":"parse","error":error_json(&error)})
        }
        Ok(module) if request.parse_only => {
            if let Some(module) = module {
                let requests = vm.module_requests(module).expect("fresh module record");
                let specifiers: Vec<_> = requests
                    .iter()
                    .map(|request| request.specifier.as_utf16())
                    .collect();
                json!({"id":request.id,"outcome":"ok","phase":"parse","module_requests":specifiers})
            } else {
                json!({"id":request.id,"outcome":"ok","phase":"parse"})
            }
        }
        Ok(module) => {
            let module_root =
                module.map(|module| vm.root_module(module).expect("fresh module record"));
            let mut failure = None;
            for setup in request.setup {
                if let Err(error) = vm.eval(&setup.source) {
                    failure = Some(json!({"id":request.id,"outcome":"error","phase":"setup",
                        "setup":setup.name,"error":error_json(&error)}));
                    break;
                }
                vm.collect_garbage();
            }
            let assertion_prototype = vm
                .get_global("Test262Error")
                .ok()
                .filter(|value| value.as_object().is_some())
                .and_then(|value| vm.get_property(value, "prototype").ok())
                .filter(|value| value.as_object().is_some());
            let assertion_root = assertion_prototype.map(|value| vm.root(value));
            let mut unhandled_count = 0usize;
            let mut module_pending = false;
            let mut linked = BTreeMap::new();
            if failure.is_none()
                && let Some(module) = module
            {
                match module_host::link(
                    &mut vm,
                    module,
                    &request.id,
                    request.module_files,
                    request.module_edges,
                ) {
                    Ok(modules) => linked = modules,
                    Err(link_error) => {
                        failure = Some(
                            json!({"id":request.id,"outcome":"error","phase":link_error.phase,"error":error_json(&link_error.error)}),
                        );
                    }
                }
            }
            if failure.is_none() && request.link_only && module.is_some() {
                failure = Some(json!({"id":request.id,"outcome":"ok","phase":"resolution"}));
            }
            let mut loader = None;
            if failure.is_none()
                && let Some(files) = request.dynamic_files
            {
                let original = original_for_import.unwrap();
                match DynamicHost::install(&mut vm, &request.id, original, files, linked) {
                    Ok(host) => loader = Some(host),
                    Err(error) => {
                        failure = Some(
                            json!({"id":request.id,"outcome":"error","phase":"runtime","error":error_json(&error)}),
                        );
                    }
                }
            }
            let result=failure.unwrap_or_else(|| match vm.with_task(|vm| {
                let value = if let Some(module) = module {
                    vm.evaluate_module(module)?;
                    Value::Undefined
                } else {
                    vm.eval_with_identifier(&source, &request.id)?
                };
                for _ in 0..10000 {
                    if !vm.has_pending_jobs() {
                        if let Some(loader) = loader.as_mut() && loader.step(vm)? {
                            continue;
                        }
                        if let Some(module) = module {
                            match vm.module_evaluation_result(module)? {
                                Some(result) => result?,
                                None => module_pending = true,
                            }
                        }
                        let reasons=vm.take_unhandled_rejections();unhandled_count=reasons.len();
                        if request.async_test && let Some(prototype)=assertion_root.and_then(|root|vm.root_value(root)) {
                            for reason in reasons {if vm.has_prototype(&reason,&prototype)? {return Err(JsError::new(ErrorKind::Thrown,"Unhandled original Test262Error assertion in a Promise job"));}}
                        }
                        return Ok(value);
                    }
                    vm.drain_jobs(1)?;
                }
                vm.clear_jobs();
                Err(JsError::new(ErrorKind::ExecutionLimit,"Test job checkpoint exceeds 10000 jobs"))
            }) {
                Ok(value) => {
                    let status = async_status.borrow();
                    if request.async_test && status.failure.is_some() {
                        json!({"id":request.id,"outcome":"error","phase":"async","error":{"kind":"AsyncTestFailure","message":status.failure}})
                    } else if module_pending {
                        json!({"id":request.id,"outcome":"error","phase":"async","error":{"kind":"AsyncIncomplete","message":"Realm became quiescent while module evaluation was pending"}})
                    } else if request.async_test && status.complete == 0 {
                        json!({"id":request.id,"outcome":"error","phase":"async","error":{"kind":"AsyncIncomplete","message":"Realm became quiescent without Test262:AsyncTestComplete"}})
                    } else if request.async_test && status.complete != 1 {
                        json!({"id":request.id,"outcome":"error","phase":"async","error":{"kind":"AsyncTestFailure","message":"Async completion was printed more than once"}})
                    } else { json!({"id":request.id,"outcome":"ok","phase":"runtime","value":value_json(&value),"async_completions":status.complete}) }
                },
                Err(error) => {
                    let mut details=error_json(&error);
                    if error.kind==ErrorKind::Thrown
                        && let Some(reason)=&error.thrown
                        && let Some(prototype)=assertion_root.and_then(|root|vm.root_value(root))
                        && vm.with_task(|vm|vm.has_prototype(reason,&prototype)).unwrap_or(false)
                    {
                        details["kind"]=json!("Test262Error");
                    }
                    json!({"id":request.id,"outcome":"error","phase":"runtime","error":details})
                },
            });
            if let Some(root) = assertion_root {
                vm.unroot(root);
            }
            if let Some(root) = module_root {
                vm.unroot(root);
            }
            let mut result = result;
            result["unhandled_rejections"] = json!(unhandled_count);
            result
        }
    };
    let mut result = result;
    result["logs"] = json!(&*logs.borrow());
    result["elapsed_us"] = json!(start.elapsed().as_micros());
    result
}

fn read_request(input: &mut impl BufRead, bytes: &mut Vec<u8>) -> io::Result<bool> {
    bytes.clear();
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            return Ok(!bytes.is_empty());
        }
        let length = chunk
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(chunk.len(), |at| at + 1);
        if bytes.len().saturating_add(length) > PROTOCOL_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "worker request exceeds 16 MiB",
            ));
        }
        bytes.extend_from_slice(&chunk[..length]);
        let complete = chunk[length - 1] == b'\n';
        input.consume(length);
        if complete {
            return Ok(true);
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--version"] {
        println!("Rust Browser JavaScript {}", env!("CARGO_PKG_VERSION"));
    } else if args == ["--worker"] {
        let mut input = io::stdin().lock();
        let mut output = io::stdout().lock();
        let mut bytes = Vec::new();
        while read_request(&mut input, &mut bytes)? {
            let result = match serde_json::from_slice::<Request>(&bytes) {
                Ok(request) => execute(request),
                Err(error) => {
                    json!({"id":null,"outcome":"error","phase":"protocol","error":error.to_string()})
                }
            };
            serde_json::to_writer(&mut output, &result)?;
            writeln!(output)?;
            output.flush()?;
        }
    } else if args.len() == 2 && matches!(args[0].as_str(), "--eval-file" | "--check-file") {
        let request = Request {
            id: args[1].clone(),
            source: std::fs::read_to_string(&args[1])?,
            setup: vec![],
            strict: false,
            parse_only: args[0] == "--check-file",
            async_test: false,
            max_instructions: None,
            module_goal: false,
            module_files: vec![],
            module_edges: vec![],
            dynamic_files: None,
            link_only: false,
        };
        let result = execute(request);
        println!("{}", serde_json::to_string_pretty(&result)?);
        if result["outcome"] != "ok" {
            std::process::exit(1);
        }
    } else {
        return Err(
            "usage: rb-js --worker | --eval-file FILE | --check-file FILE | --version".into(),
        );
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("rb-js: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(source: &str, async_test: bool) -> Json {
        execute(Request {
            id: "protocol".into(),
            source: source.into(),
            setup: vec![],
            strict: false,
            parse_only: false,
            async_test,
            max_instructions: Some(100000),
            module_goal: false,
            module_files: vec![],
            module_edges: vec![],
            dynamic_files: None,
            link_only: false,
        })
    }
    #[test]
    fn eval_script_runs_global_script_code_and_returns_its_completion() {
        let result = request(
            r#"
            function caller() {
                "use strict";
                let hostGlobal = 40;
                let run = $262.evalScript;
                return run("var hostGlobal = 2; let hostLexical = 3; this === $262.global");
            }
            if (!caller() || hostGlobal !== 2 || hostLexical !== 3) throw "not global script code";
            if ("hostLexical" in $262.global || delete $262.global.hostGlobal) throw "bad global declaration";
            if ($262.evalScript("hostGlobal + hostLexical") !== 5) throw "bad completion";
            if ($262.evalScript("var noResult") !== undefined) throw "bad empty completion";
            let marker = {};
            try { $262.evalScript("throw marker"); throw "expected throw"; }
            catch (e) { if (e !== marker) throw e; }
            "#,
            false,
        );
        assert_eq!(result["outcome"], "ok", "{result}");
        let result = request("typeof hostLexical;", false);
        assert_eq!(result["value"], value_json(&Value::from("undefined")));
    }
    #[test]
    fn reflect_construct_uses_the_new_targets_original_realm_intrinsics() {
        let result = request(
            r#"
            const other = $262.createRealm();
            const target = other.evalScript("(function C(){})");
            target.prototype = null;
            const original = other.global.Array.prototype;
            other.global.Array = function Replacement() {};
            for (const T of [target, new Proxy(target, {}), target.bind(null)]) {
                const value = Reflect.construct(Array, [], T);
                if (Object.getPrototypeOf(value) !== original) throw "foreign prototype lost";
            }
            const sentinel = {};
            target.prototype = sentinel;
            if (Object.getPrototypeOf(Reflect.construct(Array, [], target)) !== sentinel)
                throw "explicit prototype lost";
        "#,
            false,
        );
        assert_eq!(result["outcome"], "ok", "{result}");
    }
    #[test]
    fn eval_script_preserves_phase_and_text_and_shares_the_execution_budget() {
        let result = request(r#"$262.evalScript("let =");"#, false);
        assert_eq!(result["phase"], "runtime", "{result}");
        assert_eq!(result["error"]["kind"], "SyntaxError", "{result}");
        let result = request(r#"$262.evalScript("'\uD800'");"#, false);
        assert_eq!(result["error"]["kind"], "UnsupportedFeature", "{result}");
        let result = request(r#"while (true) $262.evalScript("1");"#, false);
        assert_eq!(result["error"]["kind"], "ExecutionLimit", "{result}");
    }
    #[test]
    fn test262_host_globals_have_the_required_descriptors() {
        let result = request(
            r#"
            for (let name of ["$262", "print"]) {
                let desc = Object.getOwnPropertyDescriptor($262.global, name);
                if (!desc.writable || !desc.configurable || desc.enumerable) throw name;
            }
            "#,
            false,
        );
        assert_eq!(result["outcome"], "ok", "{result}");
    }
    #[test]
    fn created_realms_preserve_identity_globals_errors_and_nested_hosts() {
        let result = request(
            r#"
            let child = $262.createRealm(), other = child.createRealm();
            if (child.evalScript("globalThis") !== child.global) throw "global";
            let arr = child.evalScript("var data = []; data");
            if (arr !== child.global.data || !(arr instanceof child.global.Array) ||
                arr instanceof Array || other.global.Array === child.global.Array)
                throw "identity";
            try { child.evalScript("null.x"); throw "missing error" }
            catch (e) { if (!(e instanceof child.global.TypeError)) throw "wrong realm" }
            child.evalScript("print('from child')");
        "#,
            false,
        );
        assert_eq!(result["outcome"], "ok", "{result}");
        assert_eq!(result["logs"][0], "from child");
    }
    #[test]
    fn unknown_host_operations_expose_no_dummy_result_and_cannot_be_caught() {
        for property in ["agent", "gc", "AbstractModuleSource"] {
            let result = request(
                &format!("try {{ typeof $262.{property}; }} catch (e) {{ 42; }}"),
                false,
            );
            assert_eq!(result["phase"], "runtime", "{result}");
            assert_eq!(result["error"]["kind"], "UnsupportedFeature", "{result}");
            assert_eq!(
                result["error"]["unsupported_host"],
                format!("$262.{property}")
            );
        }
        let result = request("if (false) $262.createRealm(); 42;", false);
        assert_eq!(result["value"], value_json(&Value::from(42)));
    }
    #[test]
    fn unsupported_setup_host_is_distinct_from_an_upstream_setup_exception() {
        for (setup, expected) in [
            (
                "$262.agent.getReport.bind($262.agent);",
                json!("$262.agent"),
            ),
            ("throw new TypeError('$262.agent');", Json::Null),
        ] {
            let result = execute(
                serde_json::from_value(json!({
                    "id": "setup-host",
                    "source": "throw new TypeError('runtime');",
                    "setup": [{"name": "upstream", "source": setup}],
                }))
                .unwrap(),
            );
            assert_eq!(result["phase"], "setup", "{result}");
            assert_eq!(result["error"]["unsupported_host"], expected, "{result}");
        }
    }
    #[test]
    fn async_completion_is_observed_after_jobs_and_logs_cannot_hide_failure() {
        let result = request(
            "Promise.resolve().then(function(){print('Test262:AsyncTestComplete');});",
            true,
        );
        assert_eq!(result["outcome"], "ok");
        assert_eq!(result["async_completions"], 1);
        let result = request(
            "for(var i=0;i<100;i++)print('log');Promise.resolve().then(function(){print('Test262:AsyncTestFailure:Error: failed');});",
            true,
        );
        assert_eq!(result["error"]["kind"], "AsyncTestFailure");
    }
    #[test]
    fn completion_does_not_mask_later_failure_or_missing_completion() {
        let result = request(
            "print('Test262:AsyncTestComplete');Promise.resolve().then(function(){print('Test262:AsyncTestFailure:Error: later');});",
            true,
        );
        assert_eq!(result["outcome"], "error");
        let result = request("Promise.resolve(1);", true);
        assert_eq!(result["error"]["kind"], "AsyncIncomplete");
    }
    #[test]
    fn unhandled_assertions_use_trusted_prototype_not_mutable_names() {
        fn run(source: &str) -> Json {
            execute(Request {
                id: "unhandled".into(),
                source: source.into(),
                setup: vec![Setup {
                    name: "sta contract".into(),
                    source: "function Test262Error(message){this.message=message;}".into(),
                }],
                strict: false,
                parse_only: false,
                async_test: true,
                max_instructions: Some(100000),
                module_goal: false,
                module_files: vec![],
                module_edges: vec![],
                dynamic_files: None,
                link_only: false,
            })
        }
        let result = run(
            "print('Test262:AsyncTestComplete');Promise.resolve().then(function(){var e=new Test262Error('assertion');e.name='Error';throw e;});",
        );
        assert_eq!(result["outcome"], "error");
        let result =
            run("print('Test262:AsyncTestComplete');Promise.reject({name:'Test262Error'});");
        assert_eq!(result["outcome"], "ok");
        assert_eq!(result["unhandled_rejections"], 1);
        let result = run(
            "print('Test262:AsyncTestComplete');Promise.reject(new Test262Error('handled')).catch(function(){});",
        );
        assert_eq!(result["outcome"], "ok");
        assert_eq!(result["unhandled_rejections"], 0);
    }
    #[test]
    fn synchronous_promise_error_sentinels_are_diagnostic_only() {
        let result=execute(Request{
            id:"synchronous sentinel".into(),
            source:"var sentinel=new Test262Error('iterator abrupt completion');Promise.reject(sentinel);".into(),
            setup:vec![Setup{name:"sta contract".into(),source:"function Test262Error(message){this.message=message;}".into()}],
            strict:false,parse_only:false,async_test:false,max_instructions:Some(100000),module_goal:false,module_files:vec![],module_edges:vec![],dynamic_files:None,link_only:false,
        });
        assert_eq!(result["outcome"], "ok");
        assert_eq!(result["unhandled_rejections"], 1);
    }
    #[test]
    fn module_worker_distinguishes_resolution_runtime_and_missing_fixture_data() {
        let run = |source: &str, fixtures: bool, link_only: bool| {
            execute(Request {
                id: "test/main.js".into(),
                source: source.into(),
                setup: vec![],
                strict: false,
                parse_only: false,
                async_test: false,
                max_instructions: Some(100_000),
                module_goal: true,
                dynamic_files: None,
                link_only,
                module_files: if fixtures {
                    vec![ModuleFile {
                        identifier: "test/dep.js".into(),
                        source: "export let n=4;export function inc(){n++;}".into(),
                    }]
                } else {
                    vec![]
                },
                module_edges: if fixtures {
                    vec![ModuleEdge {
                        referrer: "test/main.js".into(),
                        specifier: "./dep.js".encode_utf16().collect(),
                        target: "test/dep.js".into(),
                    }]
                } else {
                    vec![]
                },
            })
        };
        let result = run(
            "import {n,inc} from './dep.js';inc();if(n!==5)throw 'not live';",
            true,
            false,
        );
        assert_eq!(result["outcome"], "ok");
        let result = run("import {missing} from './dep.js';throw 1;", true, true);
        assert_eq!(result["phase"], "resolution");
        assert_eq!(result["error"]["kind"], "SyntaxError");
        let result = run("import {missing} from './dep.js';", false, true);
        assert_eq!(result["phase"], "host");
        let result = run("throw new TypeError('evaluation');", false, false);
        assert_eq!(result["phase"], "runtime");
        assert_eq!(result["error"]["kind"], "TypeError");
    }

    #[test]
    fn dynamic_fixture_jobs_can_finish_top_level_await_and_share_module_identity() {
        let result = execute(serde_json::from_value(json!({
            "id": "test/main.js#module",
            "source": "var pair=await Promise.all([import('./side_FIXTURE.js'),import('./side_FIXTURE.js')]); if(pair[0]!==pair[1]||pair[0].count!==7)throw 'fixture';",
            "module_goal": true,
            "dynamic_files": [{"identifier":"test/side_FIXTURE.js","source":"export const count=await 7;"}],
        })).unwrap());
        assert_eq!(result["outcome"], "ok", "{result}");
        let pending = execute(
            serde_json::from_value(json!({
                "id": "test/pending.js#module",
                "source": "await new Promise(()=>{});",
                "module_goal": true,
            }))
            .unwrap(),
        );
        assert_eq!(pending["error"]["kind"], "AsyncIncomplete");
    }
}
