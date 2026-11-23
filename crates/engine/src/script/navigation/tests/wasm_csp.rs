use super::{evaluate, start, wait_for_worker_probes};
use rust_browser_javascript::Value;

const WINDOW_PROBE: &str = r#"
var wasmBytes = new Uint8Array([0,97,115,109,1,0,0,0]);
var savedConstructor = WebAssembly.Module;
function outcome(error) {
    return error instanceof WebAssembly.CompileError ? 'blocked' : error.name;
}
function probe() {
    try { new savedConstructor(wasmBytes); return 'allowed'; }
    catch (error) { return outcome(error); }
}
"#;

const META_DENY: &str =
    r#"<meta http-equiv="Content-Security-Policy" content="script-src 'unsafe-inline'">"#;

#[test]
fn wasm_csp_meta_follows_parser_order_and_does_not_invalidate_compiled_modules() {
    let source = format!(
        r#"<!doctype html><head>
        <script>{WINDOW_PROBE}
            var before = probe();
            var compiled = new WebAssembly.Module(wasmBytes);
        </script>
        <meta http-equiv="Content-Security-Policy" content="worker-src 'none'">
        <meta id="restrictive" http-equiv="Content-Security-Policy" content="script-src 'unsafe-inline'">
        <meta http-equiv="Content-Security-Policy" content="script-src 'unsafe-inline' 'wasm-unsafe-eval'">
        <script>
            document.getElementById('restrictive').remove();
            var after = probe();
            var instance = new WebAssembly.Instance(compiled);
            var reuse;
            WebAssembly.instantiate(compiled).then(function(value) {{ reuse = value; }});
        </script></head><body></body>"#
    );
    let (page, _, mut runtime) = start(&source);
    assert!(!page.document_policy.allows_blob());
    assert!(!page.document_policy.allows_wasm_compilation());
    assert_eq!(
        evaluate(
            &mut runtime,
            "before === 'allowed' && after === 'blocked' && \
            WebAssembly.validate(wasmBytes) && instance instanceof WebAssembly.Instance && \
            reuse instanceof WebAssembly.Instance"
        ),
        Value::Bool(true)
    );
    assert!(runtime.unwrap().diagnostics().is_empty());
}

#[test]
fn wasm_csp_head_meta_is_active_before_a_realm_exists() {
    const REPORT_ONLY: &str =
        r#"<meta http-equiv="Content-Security-Policy-Report-Only" content="script-src 'none'">"#;
    for (head, body, expected) in [
        (META_DENY, "", "blocked"),
        (REPORT_ONLY, META_DENY, "allowed"),
    ] {
        let source = format!(
            "<!doctype html><head>{head}</head><body>{body}\
            <script>{WINDOW_PROBE}var result = probe();</script></body>"
        );
        let (_, _, mut runtime) = start(&source);
        assert_eq!(evaluate(&mut runtime, "result"), expected.into());
    }
}

#[test]
fn wasm_csp_parser_written_meta_applies_before_nested_and_returned_code() {
    let source = format!(
        r#"<!doctype html><head><script>{WINDOW_PROBE}
            var before = probe();
            document.write('<meta http-equiv="Content-Security-Policy" content="script-src &#39;unsafe-inline&#39;">'
                + '<script>var nested = probe();</scr' + 'ipt>');
            var returned = probe();
        </script></head><body></body>"#
    );
    let (_, _, mut runtime) = start(&source);
    assert_eq!(
        evaluate(&mut runtime, "[before, nested, returned].join('|')"),
        "allowed|blocked|blocked".into()
    );
    assert!(runtime.unwrap().diagnostics().is_empty());
}

#[test]
fn wasm_csp_streaming_uses_the_policy_when_bytes_reach_the_compiler() {
    let source = format!(
        r#"<!doctype html><head><script>{WINDOW_PROBE}
            var release;
            var gate = new Promise(function(resolve) {{ release = resolve; }});
            function response() {{
                return new Response(wasmBytes, {{headers: {{"Content-Type": "application/wasm"}}}});
            }}
            var early = probe();
            var compileResult, instantiateResult;
            WebAssembly.compileStreaming(gate.then(response)).then(
                () => compileResult = 'allowed', error => compileResult = outcome(error));
            WebAssembly.instantiateStreaming(gate.then(response)).then(
                () => instantiateResult = 'allowed', error => instantiateResult = outcome(error));
        </script>{META_DENY}<script>release();</script></head><body></body>"#
    );
    let (_, _, mut runtime) = start(&source);
    runtime.as_mut().unwrap().pump();
    assert_eq!(
        evaluate(
            &mut runtime,
            "[early, compileResult, instantiateResult].join('|')"
        ),
        "allowed|blocked|blocked".into()
    );
    assert!(runtime.unwrap().diagnostics().is_empty());
}

#[test]
fn wasm_csp_blob_workers_snapshot_policy_at_construction() {
    let worker_source = serde_json::to_string(
        r#"
        onmessage = function() {
            try {
                new WebAssembly.Module(new Uint8Array([0,97,115,109,1,0,0,0]));
                postMessage('allowed');
            } catch (error) {
                postMessage(error instanceof WebAssembly.CompileError ? 'blocked' : error.name);
            }
            close();
        };
        "#,
    )
    .unwrap();
    let source = format!(
        r#"<!doctype html><head><script>{WINDOW_PROBE}
            var results = [];
            var address = URL.createObjectURL(new Blob([{worker_source}]));
            function startWorker(label) {{
                var worker = new Worker(address);
                worker.onmessage = event => results.push(label + ':' + event.data);
                worker.onerror = function(event) {{
                    event.preventDefault();
                    results.push(label + ':worker error');
                    worker.terminate();
                }};
                return worker;
            }}
            var earlier = startWorker('old');
        </script>
        <meta http-equiv="Content-Security-Policy" content="worker-src blob:; script-src 'unsafe-inline'">
        <script>
            var windowResult = probe();
            earlier.postMessage(null);
            startWorker('new').postMessage(null);
            URL.revokeObjectURL(address);
        </script></head><body></body>"#
    );
    let (page, _, mut runtime) = start(&source);
    assert!(page.document_policy.allows_blob());
    assert!(!page.document_policy.allows_wasm_compilation());
    wait_for_worker_probes(&mut runtime);
    assert_eq!(
        evaluate(
            &mut runtime,
            "windowResult === 'blocked' && results.length === 2 && \
            results.includes('new:blocked') && results.includes('old:allowed')"
        ),
        Value::Bool(true)
    );
}
