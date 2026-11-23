use rust_browser_engine::{
    LoadedPage,
    network::{ResponseType, WebResponse},
    script::ScriptRuntime,
};
use rust_browser_javascript::Value;
use std::sync::Arc;

fn realm() -> ScriptRuntime {
    ScriptRuntime::new(&LoadedPage::from_html(
        "https://example.test/page",
        "<!doctype html><title>Web APIs</title>",
    ))
    .unwrap()
}

fn js(runtime: &mut ScriptRuntime, source: &str, expected: impl Into<Value>) {
    assert_eq!(runtime.eval(source).unwrap(), expected.into(), "{source}");
}

#[test]
fn url_observes_shared_query_updates_and_iterators_are_live() {
    let mut runtime = realm();
    js(
        &mut runtime,
        r#"
        class CustomURL extends URL {}
        var url = new CustomURL('/path?sp=a%20~&k=1&k=2', 'https://exämple.test');
        var params = url.searchParams;
        var iterator = params.entries();
        var result = [];
        result.push(url instanceof CustomURL && url instanceof URL);
        result.push(url.origin);
        result.push(iterator.next().value.join(':'));
        params.delete('k','1');
        result.push(url.search);
        result.push(iterator.next().value.join(':'));
        result.push(iterator.next().done);
        url.search = '?z=3&q=%F0%9F%A6%80';
        result.push(url.searchParams === params);
        params.append('z','4');
        result.push(iterator.next().value.join(':'));
        result.push(Array.from(params.keys()).join(','));
        result.push(URL.canParse('https://example.test','invalid-base'));
        result.push(URL.parse('/profile','https://a.test').pathname);
        result.join('|')
    "#,
        "true|https://xn--exmple-cua.test|sp:a ~|?sp=a+%7E&k=2|k:2|true|true|z:4|z,q,z|false|/profile",
    );
    js(
        &mut runtime,
        r#"
        var pairs = new URLSearchParams([['\ue000', 'one'], ['\ud800\udc00', 'two'], ['\ud800','\u0000']]);
        pairs.sort();
        var input = {};
        Object.defineProperty(input, 'b', {enumerable:true, get:function(){return 'x y'}});
        input.a = 4;
        var record = new URLSearchParams(input);
        var visits = [];
        record.forEach(function(value,key,self){
            visits.push(key + ':' + value);
            if (key === 'b') self.append('c','+');
        });
        pairs.toString() + '|' + visits.join(',') + '|' + record.toString()
    "#,
        "%F0%90%80%80=two&%EE%80%80=one&%EF%BF%BD=%00|b:x y,a:4,c:+|b=x+y&a=4&c=%2B",
    );
}

#[test]
fn fetch_serializes_native_urlsearchparams_and_copies_request_signal() {
    let mut runtime = realm();
    js(
        &mut runtime,
        r#"
        var original = new AbortController();
        var params = new URLSearchParams([['q','a b'], ['q','🦀']]);
        params.toString = function(){ throw Error('wrong conversion'); };
        var request = new Request(new URL('/send', document.baseURI), {
            method: 'POST', body: params, signal: original.signal
        });
        var copy = request.clone();
        var captured = request.signal;
        fetch(request);
        captured === request.signal && captured !== original.signal &&
            copy.signal !== captured && copy.signal instanceof AbortSignal
    "#,
        true,
    );
    let requests = runtime.take_network_requests();
    let (_, request) = &requests[0];
    assert_eq!(request.url.as_str(), "https://example.test/send");
    assert_eq!(
        request.body.as_ref().unwrap().as_slice(),
        b"q=a+b&q=%F0%9F%A6%80"
    );
    assert!(request.headers.iter().any(|(name, value)| {
        name == "content-type" && value == "application/x-www-form-urlencoded;charset=UTF-8"
    }));
}

#[test]
fn abort_marks_dependents_before_dispatch_and_preserves_the_reason() {
    let mut runtime = realm();
    js(
        &mut runtime,
        r#"
        var controller = new AbortController();
        var token = {cause: 'stop'};
        var trace = [];
        var duplicate = AbortSignal.any([controller.signal, controller.signal]);
        controller.signal.addEventListener('abort', event => {
            trace.push('source:' + duplicate.aborted + ':' + (duplicate.reason === token));
            trace.push(event instanceof Event && event.isTrusted && event.currentTarget === controller.signal);
        });
        duplicate.onabort = event => {
            trace.push('dependent:' + (event.target === duplicate));
        };
        var target = new EventTarget();
        target.addEventListener('custom', () => trace.push('removed listener ran'), {signal: duplicate});
        controller.abort(token);
        controller.abort('second');
        target.dispatchEvent(new Event('custom'));
        try { duplicate.throwIfAborted(); } catch (reason) { trace.push(reason === token); }
        trace.push(duplicate instanceof EventTarget);
        const error = AbortSignal.abort().reason;
        trace.push(error.name);
        trace.push(error instanceof DOMException && error instanceof Error && error.code === DOMException.ABORT_ERR);
        trace.join('|')
    "#,
        "source:true:true|true|dependent:true|true|true|AbortError|true",
    );
    // The source's traced listener edge keeps a locally unreferenced dependent
    // observable across a garbage collection.
    runtime
        .eval(
            r#"
        var keep = new AbortController();
        AbortSignal.any([keep.signal]).addEventListener('abort', () => trace.push('retained'));
    "#,
        )
        .unwrap();
    runtime.pump();
    js(
        &mut runtime,
        "keep.abort(); trace[trace.length-1]",
        "retained",
    );
}

#[test]
fn fetch_abort_rejects_locally_and_cancels_a_dispatched_request() {
    let mut runtime = realm();
    js(
        &mut runtime,
        r#"
        var marker = {custom: true};
        var stoppedEarly = false;
        fetch('/never', {signal: AbortSignal.abort(marker)}).catch(e => stoppedEarly = e === marker);
        var queued = new AbortController();
        var stoppedQueued = false;
        fetch('/queued', {signal: queued.signal}).catch(e => stoppedQueued = e === marker);
        queued.abort(marker);
        var running = new AbortController();
        var stoppedRunning = false;
        fetch('/running', {signal: running.signal}).catch(e => stoppedRunning = e === marker);
        false
    "#,
        false,
    );
    let mut requests = runtime.take_network_requests();
    assert_eq!(requests.len(), 1);
    let (id, request) = requests.pop().unwrap();
    assert_eq!(request.url.path(), "/running");
    let cancellation = request.signal.unwrap();
    assert!(!cancellation.is_cancelled());
    runtime.eval("running.abort(marker)").unwrap();
    assert!(cancellation.is_cancelled());
    assert_eq!(runtime.pending_network(), 0);
    // Any completion that raced with the signal belongs to a settled Promise.
    runtime
        .complete_network(id, Ok(WebResponse::opaque(ResponseType::Opaque)))
        .unwrap();
    js(
        &mut runtime,
        "stoppedEarly && stoppedQueued && stoppedRunning",
        true,
    );
}

#[test]
fn buffered_response_and_clone_observe_abort_and_timeout_uses_the_task_queue() {
    let mut runtime = realm();
    runtime
        .eval(
            r#"
        var controller = new AbortController();
        var response;
        var clone;
        fetch('/body', {signal: controller.signal}).then(r => { response = r; clone = r.clone(); });
    "#,
        )
        .unwrap();
    let (id, _) = runtime.take_network_requests().pop().unwrap();
    runtime
        .complete_network(
            id,
            Ok(WebResponse {
                url: "https://example.test/body".into(),
                status: 200,
                status_text: "OK".into(),
                headers: vec![],
                body: Some(Arc::new(b"payload".to_vec())),
                response_type: ResponseType::Basic,
                redirected: false,
            }),
        )
        .unwrap();
    js(
        &mut runtime,
        r#"
        var seen = [];
        var guessed = setTimeout(() => seen.push('cleared'), 0);
        var timeout = AbortSignal.timeout(0);
        clearTimeout(guessed);
        clearTimeout(guessed + 1);
        timeout.addEventListener('abort', () => seen.push(timeout.reason.name), {once:true});
        var both = AbortSignal.any([timeout]);
        var fetchInner = fetch('/timeout', {signal: both}).catch(e => seen.push(e.name));
        controller.abort(42);
        response.text().catch(e => seen.push(e));
        clone.text().catch(e => seen.push(e));
        timeout.aborted
    "#,
        false,
    );
    let options = runtime.take_network_requests();
    assert_eq!(options.len(), 1);
    runtime.pump();
    assert!(options[0].1.signal.as_ref().unwrap().is_cancelled());
    js(
        &mut runtime,
        "seen.join(',')",
        "42,42,TimeoutError,TimeoutError",
    );
    js(
        &mut runtime,
        "response.bodyUsed && clone.bodyUsed && timeout.aborted && both.reason === timeout.reason",
        true,
    );
    js(
        &mut runtime,
        r#"
        var future = AbortSignal.timeout(2147483648);
        var rejected = false;
        try { AbortSignal.timeout(9007199254740992); } catch (error) { rejected = error instanceof TypeError; }
        rejected && !future.aborted
    "#,
        true,
    );
    assert!(
        runtime
            .next_deadline()
            .unwrap()
            .duration_since(std::time::Instant::now())
            .as_secs()
            > 2_000_000
    );
}
