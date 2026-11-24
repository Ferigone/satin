use super::*;

mod document_write;
mod readiness;
mod wasm_csp;

const PAGE: &str = "https://example.test/path/index.html";

const WORKER_PROBE: &str = r#"
var results = [];
var workerUrl = URL.createObjectURL(new Blob(["postMessage('ran'); close()"]));
function startProbe(label) {
    var worker = new Worker(workerUrl);
    worker.onmessage = function(event) { results.push(label + ':' + event.data); };
    worker.onerror = function(event) {
        event.preventDefault();
        results.push(label + ':blocked');
    };
}
"#;

fn wait_for_worker_probes(runtime: &mut Option<ScriptRuntime>) {
    let realm = runtime.as_mut().unwrap();
    let (wake, signal) = std::sync::mpsc::sync_channel(1);
    realm.set_worker_wake(Arc::new(move || {
        let _ = wake.try_send(());
    }));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        realm.pump();
        if !realm.has_workers() {
            break;
        }
        assert!(
            signal
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_ok()
        );
    }
}

#[test]
fn worker_meta_applies_at_its_parser_position_and_stays_effective() {
    let source = format!(
        r#"<!doctype html><head><script>{WORKER_PROBE}
startProbe('before');
</script>
<meta id="policy" http-equiv="Content-Security-Policy" content="worker-src 'none'">
<meta http-equiv="content-security-policy" content="worker-src blob:">
<script>
document.getElementById('policy').remove();
startProbe('after');
</script></head><body></body>"#
    );
    let (page, _, mut runtime) = start(&source);
    wait_for_worker_probes(&mut runtime);
    assert!(!page.document_policy.allows_blob());
    assert_eq!(
        evaluate(
            &mut runtime,
            "results.length === 2 && results.includes('before:ran') && results.includes('after:blocked')"
        ),
        Value::Bool(true)
    );
}

#[test]
fn worker_meta_requires_the_head_and_is_kept_before_the_realm_exists() {
    let meta = r#"<meta http-equiv="Content-Security-Policy" content="worker-src 'none'">"#;
    for (head, body, expected) in [(meta, "", "probe:blocked"), ("", meta, "probe:ran")] {
        let source = format!(
            "<!doctype html><head>{head}</head><body>{body}<script>{WORKER_PROBE}startProbe('probe');</script></body>"
        );
        let (_, _, mut runtime) = start(&source);
        wait_for_worker_probes(&mut runtime);
        assert_eq!(evaluate(&mut runtime, "results.join('|')"), expected.into());
    }
}

#[test]
fn worker_meta_from_parser_write_applies_before_its_nested_script() {
    let source = format!(
        r#"<!doctype html><head><script>{WORKER_PROBE}
document.write('<meta http-equiv="Content-Security-Policy" content="worker-src &#39;none&#39;">'
    + '<script>startProbe("written");</scr' + 'ipt>');
</script></head><body></body>"#
    );
    let (_, _, mut runtime) = start(&source);
    wait_for_worker_probes(&mut runtime);
    assert_eq!(
        evaluate(&mut runtime, "results.join('|')"),
        "written:blocked".into()
    );
}

#[test]
fn worker_connect_policy_is_snapshotted_at_the_parser_position() {
    let (_, _, mut runtime) = start(
        r#"<!doctype html><head><script>
        var results = [];
        var url = URL.createObjectURL(new Blob([`
            fetch("data:text/plain,ok").then(response => response.text())
                .then(text => postMessage(text), error => postMessage(error.message))
                .finally(() => close());
        `]));
        function probe(label) {
            var thread = new Worker(url);
            thread.onmessage = event => results.push(label + ":" + event.data);
        }
        probe("before");
        document.write('<meta id="policy" http-equiv="Content-Security-Policy" '
            + 'content="connect-src &#39;none&#39;">'
            + '<script>probe("written");</scr' + 'ipt>');
        </script><script>
        document.getElementById("policy").remove();
        probe("after");
        </script></head><body></body>"#,
    );
    let realm = runtime.as_mut().unwrap();
    realm.eval("probe('after parsing')").unwrap();
    wait_for_worker_probes(&mut runtime);
    let blocked = "Content Security Policy blocked this Fetch connection";
    assert_eq!(
        evaluate(&mut runtime, "results.sort().join('|')"),
        format!("after parsing:{blocked}|after:{blocked}|before:ok|written:{blocked}").into(),
    );
}

#[test]
fn parser_script_sees_navigation_and_response_timing() {
    let src = r#"<!doctype html><title>Timing</title><script>
        var timing = window.performance.timing;
        var responseDelay = timing.responseStart - timing.navigationStart;
        var validTiming = timing === performance.timing &&
            timing instanceof PerformanceTiming &&
            timing.navigationStart === Math.floor(performance.timeOrigin) &&
            responseDelay === 25 &&
            timing.responseStart <= performance.timeOrigin + performance.now();
    </script>"#;
    let started = Instant::now() - Duration::from_millis(50);
    let (mut page, mut session) = DocumentSession::start(OpenedPage {
        page: LoadedPage::from_html(PAGE, ""),
        input: DocumentInput::Html(src.into()),
        started,
        response_start: started + Duration::from_millis(25),
        http_status: None,
        http_content: Default::default(),
    })
    .unwrap();
    let mut runtime = None;
    advance_ready(&mut page, &mut session, &mut runtime);
    assert_eq!(evaluate(&mut runtime, "validTiming"), Value::Bool(true));
    assert!(runtime.unwrap().diagnostics().is_empty());
}

#[test]
fn parser_records_are_visible_before_preparing_the_following_script() {
    let (_, session, mut runtime) = start(
        r#"<!doctype html><body><script>
var trace=[];
var changed=false;
var watcher=new MutationObserver(function(records) {
    var candidate=document.getElementById('candidate');
    if (candidate && !changed) {
        changed=true;
        trace.push(records.some(function(r){return r.addedNodes.length && r.addedNodes[0]===candidate;}) ? 'parser' : 'missing-parser-record');
        candidate.type='text/javascript';
        candidate.text='trace.push("rewritten");';
    }
});
watcher.observe(document.body,{childList:true,subtree:true});
document.write('<b id="written">written</b>');
trace.push('initial');
</script><p id="later">late</p><script id="candidate" type="application/json">trace.push('old');</script><script>
watcher.disconnect();
trace.push(document.getElementById('written') && document.getElementById('later') ? 'tail' : 'missing');
</script>"#,
    );
    assert!(session.done());
    assert_eq!(
        evaluate(&mut runtime, "trace.join('|')"),
        "initial|parser|rewritten|tail".into()
    );
    assert!(runtime.unwrap().diagnostics().is_empty());
}

#[test]
fn parser_attribute_merge_records_follow_source_order() {
    let (_, session, mut runtime) = start(
        r#"<!doctype html><body data-existing="kept"><script>
var trace=[];
var watcher=new MutationObserver(function(records) {
    for (const r of records) trace.push(r.attributeName+':'+r.oldValue);
});
watcher.observe(document.body,{attributes:true,attributeOldValue:true});
</script><body data-z="first" data-existing="ignored" data-a="second" data-z="duplicate"><script>
trace.push(document.body.getAttribute('data-existing')+'|'+document.body.getAttribute('data-z'));
</script>"#,
    );
    assert!(session.done());
    assert_eq!(
        evaluate(&mut runtime, "trace.join('/')"),
        "data-z:null/data-a:null/kept|first".into()
    );
}

fn start(source: &str) -> (LoadedPage, DocumentSession, Option<ScriptRuntime>) {
    let started = Instant::now();
    let (mut page, mut session) = DocumentSession::start(OpenedPage {
        page: LoadedPage::from_html(PAGE, ""),
        input: DocumentInput::Html(source.into()),
        started,
        response_start: started,
        http_status: None,
        http_content: Default::default(),
    })
    .unwrap();
    let mut runtime = None;
    advance_ready(&mut page, &mut session, &mut runtime);
    (page, session, runtime)
}

fn advance_ready(
    page: &mut LoadedPage,
    session: &mut DocumentSession,
    runtime: &mut Option<ScriptRuntime>,
) {
    for _ in 0..64 {
        if !session.needs_advance(runtime.as_ref()) {
            return;
        }
        session.advance(page, runtime, Viewport::default()).unwrap();
    }
    panic!("runnable session did not reach a wait point");
}

fn complete(
    page: &mut LoadedPage,
    session: &mut DocumentSession,
    runtime: &mut Option<ScriptRuntime>,
    id: u64,
    source: Option<(&str, &str)>,
) {
    let result = source
        .map(|(url, source)| DocumentResource {
            url: Url::parse(PAGE).unwrap().join(url).unwrap(),
            transferred: source.len(),
            body: ResourceBody::Text(source.into()),
            origin_clean: true,
        })
        .ok_or_else(|| "Fixture request failed".into());
    session
        .complete_resource(id, result, page, runtime, Viewport::default())
        .unwrap();
    advance_ready(page, session, runtime);
}

fn evaluate(runtime: &mut Option<ScriptRuntime>, expression: &str) -> Value {
    runtime.as_mut().unwrap().eval(expression).unwrap()
}

#[test]
fn detached_images_use_document_requests_and_ignore_replaced_sources() {
    let (mut page, mut session, mut runtime) = start(
        r#"<base href="/assets/"><script>
        var image = new Image();
        var events = [];
        image.onload = function() {
            events.push('load:' + image.naturalWidth + 'x' + image.naturalHeight);
        };
        image.onerror = function() { events.push('error'); };
        image.src = 'first.png';
        </script>"#,
    );
    let first = session.next_resource().expect("detached image request");
    assert_eq!(
        first.request.url.as_str(),
        "https://example.test/assets/first.png"
    );
    assert_eq!(evaluate(&mut runtime, "image.complete"), Value::Bool(false));
    evaluate(&mut runtime, "image.src = 'second.png'");
    advance_ready(&mut page, &mut session, &mut runtime);
    let second = session.next_resource().expect("new image request");
    assert_eq!(
        second.request.url.as_str(),
        "https://example.test/assets/second.png"
    );

    for (request, width) in [(first, 3), (second, 7)] {
        let id = request.id;
        session
            .complete_resource(
                id,
                Ok(DocumentResource {
                    url: request.request.url,
                    body: ResourceBody::Image(Arc::new(image::RgbaImage::new(width, 2))),
                    transferred: 12,
                    origin_clean: true,
                }),
                &mut page,
                &mut runtime,
                Viewport::default(),
            )
            .unwrap();
        advance_ready(&mut page, &mut session, &mut runtime);
    }
    assert_eq!(
        evaluate(&mut runtime, "events.join('|')"),
        "load:7x2".into()
    );
    assert_eq!(
        evaluate(&mut runtime, "image.complete && image.width === 7"),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate(&mut runtime, "image.currentSrc"),
        "https://example.test/assets/second.png".into()
    );
    assert!(runtime.unwrap().diagnostics().is_empty());
}

#[test]
fn inline_post_connection_is_synchronous_and_preserves_parser_and_fragment_state() {
    let (_, session, mut runtime) = start(
        r#"<!doctype html><template id="template"><script>log.push('parsed-template')</script></template>
<script id="blank"></script><script id="space"> </script>
<div id="sink"></div><div id="replacement"></div><script id="outer">
var log = [];
var parentScript = document.currentScript;
var beforeWrite = document.createElement('script');
beforeWrite.text = 'log.push(document.currentScript===beforeWrite ? "inline" : "bad"); document.write("<p id=written>from script</p>");';
document.body.appendChild(beforeWrite);
log.push(document.getElementById('written') && document.currentScript===parentScript ? 'restored' : 'bad');
beforeWrite.remove();
document.body.insertBefore(beforeWrite, document.body.firstChild);
document.body.appendChild(beforeWrite.cloneNode(true));

var parsed = document.getElementById('template');
document.body.appendChild(parsed.content.cloneNode(true));
var ending = '</scr'+'ipt>';
var sink = document.getElementById('sink');
sink.innerHTML = '<script>log.push("bad")'+ending+'<template><script>log.push("bad")'+ending+'</template>';
document.body.appendChild(sink.firstChild.cloneNode(true));
document.body.appendChild(sink.lastChild.content.cloneNode(true));
sink.insertAdjacentHTML('afterbegin', '<script>log.push("bad")'+ending);
document.getElementById('replacement').outerHTML = '<script>log.push("bad")'+ending;

var blank = document.getElementById('blank');
log.push(blank.async===true ? 'retryable' : 'bad');
blank.text = 'log.push("blank")';
document.getElementById('space').text = 'log.push("bad")';
var script = document.createElement('script');
document.body.appendChild(script);
script.innerHTML = 'log.push("raw-html")';

var later = document.createElement('script');
var child = document.createTextNode('');
later.appendChild(child);
var wrapper = document.createElement('span');
wrapper.textContent = 'log.push("bad")';
later.appendChild(wrapper);
document.body.appendChild(later);
child.data = 'log.push("direct-data")';

var first = document.createElement('script');
var second = document.createElement('script');
second.id = 'second';
first.text = 'log.push(document.getElementById("second")===second ? "both-inserted" : "bad");second.remove();';
second.text = 'log.push("reinserted")';
document.body.append(first, second);
log.push('after-append');
document.body.appendChild(second);

var t = document.createElement('template');
var fragment = document.createDocumentFragment();
var detached = document.createElement('script');
detached.textContent = 'log.push("fragment")';
fragment.appendChild(detached);
t.content.appendChild(fragment);
document.body.appendChild(t.content.cloneNode(true));
</script>"#,
    );
    assert!(session.done());
    assert!(session.parser.is_none());
    assert!(session.next_deadline().is_none());
    assert_eq!(
        evaluate(&mut runtime, "log.join('|')"),
        "inline|restored|parsed-template|retryable|blank|raw-html|direct-data|both-inserted|after-append|reinserted|fragment".into()
    );
    assert_eq!(
        evaluate(&mut runtime, "document.currentScript"),
        Value::Null
    );
    assert!(runtime.as_ref().unwrap().diagnostics().is_empty());
}

#[test]
fn resource_transfer_gets_a_fresh_deadline_without_resetting_queue_wait() {
    let mut queue = ResourceQueue::new();
    let queued_at = Instant::now();
    let queue_deadline = queued_at + DOCUMENT_RESOURCE_QUEUE_TIMEOUT;
    let client = Arc::new(Url::parse("https://example.test/").unwrap());
    let request = resource_request(
        client.join("app.js").unwrap(),
        &client,
        ResourceKind::ClassicScript,
        None,
        None,
        queue_deadline,
    );
    queue.queue(request, Consumer::Classic(NodeId(1))).unwrap();
    assert_eq!(queue.next_deadline(), Some(queue_deadline));

    let dispatch_at = queued_at + Duration::from_secs(20);
    let load = queue
        .take(dispatch_at)
        .expect("still eligible after queueing");
    let transfer_deadline = dispatch_at + DOCUMENT_RESOURCE_TIMEOUT;
    assert_eq!(load.request.deadline, transfer_deadline);
    assert_eq!(queue.next_deadline(), Some(transfer_deadline));

    queue.retry(load);
    let retry_at = dispatch_at + Duration::from_secs(1);
    let load = queue.take(retry_at).expect("retry before original cutoff");
    assert_eq!(load.request.deadline, retry_at + DOCUMENT_RESOURCE_TIMEOUT);
    queue.retry(load);
    assert!(queue.take(queue_deadline).is_none());
    assert_eq!(queue.pending.get(&1).unwrap().1, queue_deadline);
}

#[test]
fn async_default_bypasses_ordered_scripts_and_failure_releases_the_front() {
    let (mut page, mut session, mut runtime) = start(
        r#"<!doctype html><script>
var log = [];
document.addEventListener('DOMContentLoaded', function () { log.push('dcl'); });
window.addEventListener('load', function () { log.push('window-load'); });
</script><body></body>"#,
    );
    assert!(session.done());
    assert!(session.next_deadline().is_none());
    let old_deadline = Instant::now() - Duration::from_secs(30);
    session.started = old_deadline;
    evaluate(
        &mut runtime,
        r#"var first = document.createElement('script');
var second = document.createElement('script');
var fast = document.createElement('script');
first.async = false;
first.src = 'first.js';
first.onerror = function () { log.push(this===first ? 'first-error' : 'bad'); };
second.setAttribute('async', '');
second.removeAttribute('async');
log.push(!first.async && !second.async && fast.async ? 'async-flags' : 'bad');
second.src = 'second.js';
second.onload = function () { log.push(this===second ? 'second-load' : 'bad'); };
fast.onload = function () { log.push(this===fast ? 'fast-load' : 'bad'); };
document.body.replaceChildren(first, second, fast);
fast.setAttribute('src', 'fast.js');
fast.remove();
fast.src = 'never-fetch.js';"#,
    );
    assert!(runtime.as_ref().unwrap().has_queued_script_tasks());
    advance_ready(&mut page, &mut session, &mut runtime);
    let first = session.next_resource().unwrap();
    let second = session.next_resource().unwrap();
    let fast = session.next_resource().unwrap();
    assert_eq!(first.request.url.path(), "/path/first.js");
    assert_eq!(second.request.url.path(), "/path/second.js");
    assert_eq!(fast.request.url.path(), "/path/fast.js");
    assert!(fast.request.deadline > Instant::now());
    assert!(session.next_resource().is_none());
    assert!(!session.done());
    complete(
        &mut page,
        &mut session,
        &mut runtime,
        second.id,
        Some((
            "second.js",
            "log.push(document.currentScript===second ? 'second' : 'bad');",
        )),
    );
    assert_eq!(
        evaluate(&mut runtime, "log.join('|')"),
        "dcl|window-load|async-flags".into()
    );
    complete(
        &mut page,
        &mut session,
        &mut runtime,
        fast.id,
        Some((
            "fast.js",
            "document.write('<p id=unwanted>unwanted</p>'); log.push(document.currentScript===fast ? 'fast' : 'bad');",
        )),
    );
    assert_eq!(
        evaluate(&mut runtime, "log.join('|')"),
        "dcl|window-load|async-flags|fast|fast-load".into()
    );
    assert_eq!(
        evaluate(&mut runtime, "document.getElementById('unwanted')"),
        Value::Null
    );
    complete(&mut page, &mut session, &mut runtime, first.id, None);
    assert_eq!(
        evaluate(&mut runtime, "log.join('|')"),
        "dcl|window-load|async-flags|fast|fast-load|first-error|second|second-load".into()
    );
    assert!(session.done());
    assert!(session.next_deadline().is_none());
}

#[test]
fn dynamic_module_reuses_the_navigation_graph_and_delays_load_only() {
    let (mut page, mut session, mut runtime) = start(
        r#"<!doctype html><script type="importmap">{"imports":{"leaf":"./leaf.js"}}</script>
<script>
var log = [];
window.addEventListener('load', function () { log.push('window-load'); });
document.addEventListener('DOMContentLoaded', function () { log.push('dcl'); });
window.addEventListener('DOMContentLoaded', function () {
    var s = document.createElement('script');
    s.type = 'module';
    s.src = 'entry.js';
    s.onload = function () { log.push('module-load'); };
    document.body.appendChild(s);
});
</script><body></body>"#,
    );
    assert!(!session.loaded());
    assert_eq!(evaluate(&mut runtime, "log.join('|')"), "dcl".into());
    let entry = session.next_resource().unwrap();
    assert_eq!(entry.request.url.path(), "/path/entry.js");
    complete(
        &mut page,
        &mut session,
        &mut runtime,
        entry.id,
        Some((
            "redirect/final.js",
            "import { value } from 'leaf'; log.push(document.currentScript===null ? value : 'bad');",
        )),
    );
    let leaf = session.next_resource().unwrap();
    assert_eq!(leaf.request.url.path(), "/path/leaf.js");
    complete(
        &mut page,
        &mut session,
        &mut runtime,
        leaf.id,
        Some(("leaf.js", "export const value = 'module';")),
    );
    assert!(session.done());
    assert_eq!(
        evaluate(&mut runtime, "log.join('|')"),
        "dcl|module|module-load|window-load".into()
    );
    assert!(runtime.as_ref().unwrap().diagnostics().is_empty());
}

#[test]
fn recursive_inline_insertions_stop_and_restore_current_script_on_one_mib_stack() {
    std::thread::Builder::new()
        .name("bounded-dynamic-inline".into())
        .stack_size(1024 * 1024)
        .spawn(|| {
            let (_, _, mut runtime) = start("<!doctype html><body><script>var count=0;</script>");
            let error = runtime
                .as_mut()
                .unwrap()
                .eval(
                    r#"var recursiveSource = "count++; var s=document.createElement('script'); s.text=recursiveSource; document.body.appendChild(s);";
var first=document.createElement('script'); first.text=recursiveSource; document.body.appendChild(first);"#,
                )
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::ExecutionLimit);
            assert!(error.message.contains("Nested dynamic inline script"), "{error}");
            assert_eq!(evaluate(&mut runtime, "document.currentScript"), Value::Null);
            assert_eq!(
                evaluate(&mut runtime, "var next=document.createElement('script');next.text='count=99';document.body.appendChild(next);count"),
                99.into()
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn import_uses_a_delayed_classic_functions_response_url_and_waits_for_tla_once() {
    let (mut page, mut session, mut runtime) =
        start("<!doctype html><script src='start.js'></script>");
    let classic = session.next_resource().unwrap();
    complete(
        &mut page,
        &mut session,
        &mut runtime,
        classic.id,
        Some((
            "/scripts/final.js",
            "var log=[]; function delayedImport(){return import('./later.mjs');}",
        )),
    );
    assert!(session.done());
    evaluate(
        &mut runtime,
        r#"var base = document.createElement('base'); base.href='/unrelated/';
document.head.appendChild(base);
var first=delayedImport(), second=delayedImport();
Promise.all([first,second]).then(function(values) {
    log.push(values[0]===values[1] ? values[0].value : 'different namespaces');
});"#,
    );
    advance_ready(&mut page, &mut session, &mut runtime);
    let module = session.next_resource().unwrap();
    assert_eq!(module.request.url.path(), "/scripts/later.mjs");
    assert_eq!(module.request.cors, Some(CredentialsMode::SameOrigin));
    assert!(session.next_resource().is_none());
    complete(
        &mut page,
        &mut session,
        &mut runtime,
        module.id,
        Some((
            "/redirected/later.mjs",
            "log.push('start'); export const value=await new Promise(function(resolve){window.release=resolve;}); log.push('finish');",
        )),
    );
    assert_eq!(evaluate(&mut runtime, "log.join('|')"), "start".into());
    assert!(session.next_deadline().is_none());
    assert!(!session.needs_advance(runtime.as_ref()));
    evaluate(&mut runtime, "release('answer');");
    runtime.as_mut().unwrap().pump();
    assert_eq!(
        evaluate(&mut runtime, "log.join('|')"),
        "start|finish|answer".into()
    );
    assert!(runtime.as_ref().unwrap().diagnostics().is_empty());
    assert!(!session.needs_advance(runtime.as_ref()));
}

#[test]
fn a_module_function_retains_credentials_for_its_later_import_and_rejects_with_the_reason() {
    let (mut page, mut session, mut runtime) = start(
        r#"<!doctype html><script>
var reason = { code: 'test' };
var imported;
</script><script type="module" crossorigin="use-credentials" src="https://other.test/root.mjs"></script>"#,
    );
    let root = session.next_resource().unwrap();
    complete(
        &mut page,
        &mut session,
        &mut runtime,
        root.id,
        Some((
            "https://other.test/final/root.mjs",
            "window.imported = function(){return import('./failing.mjs');};",
        )),
    );
    assert!(session.done());
    evaluate(
        &mut runtime,
        "var gotReason=false; imported().catch(function(error){gotReason=error===reason;});",
    );
    advance_ready(&mut page, &mut session, &mut runtime);
    let dependency = session.next_resource().unwrap();
    assert_eq!(
        dependency.request.url.as_str(),
        "https://other.test/final/failing.mjs"
    );
    assert_eq!(dependency.request.cors, Some(CredentialsMode::Include));
    complete(
        &mut page,
        &mut session,
        &mut runtime,
        dependency.id,
        Some((
            "https://other.test/final/failing.mjs",
            "await Promise.reject(reason);",
        )),
    );
    assert_eq!(evaluate(&mut runtime, "gotReason"), true.into());
    assert!(runtime.as_ref().unwrap().diagnostics().is_empty());
}
