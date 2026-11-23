use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

// A function that forwards two i32 arguments to the imported env.sum.
fn realm() -> ScriptRuntime {
    let mut runtime = ScriptRuntime::new(&LoadedPage::from_html(
        "https://example.test/wasm",
        "<!doctype html>",
    ))
    .unwrap();
    runtime
        .eval(
            r#"
            var bytes = new Uint8Array([
                0, 97, 115, 109, 1, 0, 0, 0,
                1, 7, 1, 96, 2, 127, 127, 1, 127,
                2, 11, 1, 3, 101, 110, 118, 3, 115, 117, 109, 0, 0,
                3, 2, 1, 0,
                7, 7, 1, 3, 114, 117, 110, 0, 1,
                10, 10, 1, 8, 0, 32, 0, 32, 1, 16, 0, 11
            ]);
            var mime = {"Content-Type": "application/wasm"};
            var imports = { env: { sum(a, b) { return a + b; } } };
            "#,
        )
        .unwrap();
    runtime
}

#[test]
fn streaming_instantiates_a_fetched_blob_and_compiles_a_direct_response() {
    let mut runtime = realm();
    runtime
        .eval(
            r#"
            var fetchResult, compileResult, compiled, patchedCalls = 0;
            function replacement() { patchedCalls++; throw new Error("replacement"); }
            Response.prototype.arrayBuffer = replacement;
            WebAssembly.instantiate = replacement;
            WebAssembly.compile = replacement;
            Promise.resolve = replacement;

            async function run() {
                const url = URL.createObjectURL(new Blob([bytes], {type:"application/wasm"}));
                const streaming = await WebAssembly.instantiateStreaming(fetch(url), imports);
                fetchResult = streaming.instance.exports.run(19, 23);
                URL.revokeObjectURL(url);

                const direct = new Response(bytes, {headers: {"Content-Type":"Application/WASM"}});
                Promise.prototype.then = replacement;
                compiled = await WebAssembly.compileStreaming(direct);
                compileResult = direct.bodyUsed && compiled instanceof WebAssembly.Module &&
                    new WebAssembly.Instance(compiled, imports).exports.run(20, 22);
            }
            run();
            "#,
        )
        .unwrap();
    runtime.pump();
    assert_eq!(
        runtime
            .eval("fetchResult === 42 && compileResult === 42 && patchedCalls === 0")
            .unwrap(),
        Value::Bool(true)
    );
    assert!(runtime.take_network_jobs().is_empty());
    assert!(runtime.diagnostics().is_empty());
}

#[test]
fn streaming_checks_response_metadata_and_propagates_body_errors() {
    let mut runtime = realm();
    runtime
        .eval(
            r#"
            var results = [];
            var wrongMime = new Response(bytes, {headers: {"Content-Type":"application/wasm; charset=utf-8"}});
            var failedStatus = new Response(bytes, {status:404, headers:mime});
            var used = new Response(bytes, {headers:mime});
            var locked = new Response(bytes, {headers:mime});
            var malformed = new Response(new Uint8Array([0]), {headers:mime});
            var sentinel = {};
            var broken = new Response(new ReadableStream({
                start(controller) { controller.error(sentinel); }
            }), {headers:mime});

            async function rejected(input) {
                try {
                    await WebAssembly.instantiateStreaming(input, imports);
                    return "resolved";
                } catch (error) {
                    return error === sentinel ? "sentinel" : error.name;
                }
            }

            async function run() {
                results.push(await rejected(wrongMime));
                results.push(await rejected(failedStatus));
                results.push(await rejected({status:200, headers:new Headers(mime)}));
                await used.arrayBuffer();
                results.push(await rejected(used));
                const reader = locked.body.getReader();
                results.push(await rejected(locked));
                reader.releaseLock();
                results.push(await rejected(malformed));
                results.push(await rejected(broken));
                results.push(await rejected(Promise.reject(sentinel)));
            }
            run();
            "#,
        )
        .unwrap();
    runtime.pump();
    assert_eq!(
        runtime
            .eval(
                r#"
                results.join("|") ===
                    "TypeError|TypeError|TypeError|TypeError|TypeError|CompileError|sentinel|sentinel" &&
                    !wrongMime.bodyUsed && !failedStatus.bodyUsed && !locked.bodyUsed &&
                    used.bodyUsed && malformed.bodyUsed
                "#,
            )
            .unwrap(),
        Value::Bool(true)
    );
    assert!(runtime.diagnostics().is_empty());
}
