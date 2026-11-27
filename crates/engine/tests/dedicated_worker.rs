use rust_browser_engine::{
    LoadedPage,
    script::{ConsoleLevel, ConsoleOrigin, ScriptRuntime},
};
use rust_browser_javascript::Value;
use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

const WORKER_TIMEOUT: Duration = Duration::from_secs(5);

fn runtime() -> (ScriptRuntime, Receiver<()>) {
    let page = LoadedPage::from_html(
        "https://example.test/workers",
        "<!doctype html><title>Dedicated Worker</title>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let (notify, wakes) = mpsc::sync_channel(1);
    runtime.set_worker_wake(Arc::new(move || {
        let _ = notify.try_send(());
    }));
    (runtime, wakes)
}

fn wait_for_worker_close(runtime: &mut ScriptRuntime, wakes: &Receiver<()>) {
    let deadline = Instant::now() + WORKER_TIMEOUT;
    loop {
        runtime.pump();
        if !runtime.has_workers() {
            return;
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| {
                panic!(
                    "waiting for Worker shutdown: {error}. {:?}",
                    runtime.diagnostics()
                );
            });
    }
}

#[test]
fn a_revoked_blob_worker_transfers_shared_views_and_closes_after_its_active_task() {
    let (mut runtime, wakes) = runtime();
    let worker_script = r#"
        const turns = [];
        queueMicrotask(() => turns.push('startup'));
        addEventListener('message', function(event) {
            const input = event.data;
            turns.push(input.phase);
            if (input.phase === 'first') {
                Promise.resolve().then(() => turns.push('microtask'));
                return;
            }
            if (input.phase !== 'second') {
                postMessage('message after close');
                return;
            }
            const buffer = input.buffer;
            const bytes = input.bytes;
            const view = input.view;
            const shared = bytes.buffer === buffer && view.buffer === buffer &&
                input.alias === bytes && bytes.byteOffset === 1 &&
                view.byteOffset === 2 && view.byteLength === 2;
            bytes[0] += 2;
            bytes[1] += 1;
            const viewSawWrite = view.getUint8(0) === 8;
            view.setUint8(1, 10);
            const scope = this === self && self === globalThis &&
                self.name === 'blob-worker' && typeof document === 'undefined' &&
                typeof window === 'undefined' && typeof documentMarker === 'undefined' &&
                location.href === input.address &&
                event instanceof MessageEvent && event.isTrusted && event.target === self;
            setTimeout(() => postMessage('timer after close'), 0);
            close();
            postMessage({turns, shared, viewSawWrite, scope, buffer, bytes, view, alias: bytes}, [buffer]);
            postMessage({detached: buffer.byteLength === 0 && bytes.byteLength === 0});
            queueMicrotask(() => postMessage('close checkpoint'));
        });
    "#;
    let mut source = format!(
        "const workerScript = {};\n",
        serde_json::to_string(worker_script).unwrap()
    );
    source.push_str(
        r##"
        var documentMarker = 23, replies = [], errors = [], stopped = 0;
        const url = URL.createObjectURL(new Blob([workerScript], {type: 'text/javascript'}));
        const address = url + '#fixture';
        const worker = new Worker(address, {name: 'blob-worker'});
        worker.onmessage = event => {
            replies.push({value: event.data, eventShape: event instanceof MessageEvent &&
                event.isTrusted && event.target === worker && event.origin === '' &&
                event.source === null && event.ports.length === 0 && Object.isFrozen(event.ports)});
        };
        worker.onerror = event => errors.push(event.message);
        worker.onmessageerror = event => errors.push(event.type);
        URL.revokeObjectURL(url);

        const stoppedUrl = URL.createObjectURL(new Blob(["postMessage('unexpected'); close();"]));
        const stoppedWorker = new Worker(stoppedUrl);
        stoppedWorker.onmessage = () => stopped++;
        stoppedWorker.onerror = () => stopped++;
        stoppedWorker.terminate();
        URL.revokeObjectURL(stoppedUrl);

        const buffer = new Uint8Array([12, 40, 7, 9]).buffer;
        const bytes = new Uint8Array(buffer, 1, 2);
        const view = new DataView(buffer, 2, 2);
        worker.postMessage({phase: 'first'});
        worker.postMessage({phase: 'second', address, buffer, bytes, view, alias: bytes},
            {transfer: [buffer]});
        worker.postMessage({phase: 'closed'});
        var detached = buffer.byteLength === 0 && bytes.byteLength === 0;
        "##,
    );
    runtime.eval(&source).unwrap();
    wait_for_worker_close(&mut runtime, &wakes);
    let transcript = runtime
        .eval(
            r#"
            const transferred = replies[0].value;
            [
                stopped, errors.length, detached, replies.length, replies.every(item => item.eventShape),
                transferred.turns.join(','), transferred.shared, transferred.viewSawWrite,
                transferred.scope, transferred.bytes.buffer === transferred.buffer,
                transferred.view.buffer === transferred.buffer, transferred.bytes === transferred.alias,
                transferred.bytes.byteOffset, transferred.view.byteOffset, transferred.view.byteLength,
                Array.from(new Uint8Array(transferred.buffer)).join(','),
                replies[1].value.detached, replies[2].value
            ].join('|');
            "#,
        )
        .unwrap_or_else(|error| panic!("{error}. {:?}", runtime.diagnostics()));
    assert_eq!(
        transcript,
        Value::from(
            "0|0|true|3|true|startup,first,microtask,second|true|true|true|true|true|true|1|2|2|12,42,8,10|true|close checkpoint"
        ),
        "{:?}",
        runtime.diagnostics()
    );
    assert!(runtime.diagnostics().is_empty());
}

#[test]
fn dedicated_worker_exposes_its_navigator_and_forwards_console_output() {
    let (mut runtime, wakes) = runtime();
    runtime.eval(r#"
        var result, workerErrors = [];
        const address = URL.createObjectURL(new Blob([`
            const get = Object.getOwnPropertyDescriptor(WorkerGlobalScope.prototype, 'navigator').get;
            let branded = false;
            try { get.call({}); } catch (error) { branded = error instanceof TypeError; }
            console.warn('worker output', {toString() { throw 'unexpected coercion'; }});
            postMessage([
                navigator === self.navigator, navigator instanceof WorkerNavigator, branded,
                navigator.userAgent, navigator.language, navigator.onLine,
                Object.isFrozen(navigator.languages), navigator.hardwareConcurrency
            ]);
            close();
        `]));
        const worker = new Worker(address);
        worker.onmessage = event => { result = event.data; };
        worker.onerror = event => workerErrors.push(event.message);
    "#).unwrap();
    wait_for_worker_close(&mut runtime, &wakes);
    assert_eq!(runtime.eval(r#"
        workerErrors.length === 0 && result.slice(0, 3).every(Boolean) &&
        result[3] === navigator.userAgent && result[4] === navigator.language &&
        result[5] && result[6] && result[7] === navigator.hardwareConcurrency
    "#).unwrap(), Value::Bool(true));
    let console = runtime.console_snapshot();
    assert_eq!(console.records.len(), 1);
    let record = &console.records[0];
    assert_eq!(record.level, ConsoleLevel::Warn);
    assert_eq!(record.origin, ConsoleOrigin::ConsoleApi);
    assert_eq!(record.source.as_deref(), Some("Worker"));
    assert_eq!(record.text, "worker output [object]");
}

#[test]
fn terminating_a_worker_discards_its_queued_window_message() {
    let (mut runtime, wakes) = runtime();
    runtime
        .eval(
            r#"
            var replies = [];
            const url = URL.createObjectURL(new Blob([`
                setTimeout(() => postMessage('late timer'), 60000);
                onmessage = event => postMessage(event.data);
            `]));
            const worker = new Worker(url);
            URL.revokeObjectURL(url);
            worker.onmessage = event => replies.push(event.data);
            worker.postMessage('live');
            "#,
        )
        .unwrap();
    wakes.recv_timeout(WORKER_TIMEOUT).unwrap();
    runtime.pump();
    assert_eq!(
        runtime.eval("replies.join(',')").unwrap(),
        Value::from("live")
    );
    runtime.eval("worker.postMessage('discarded');").unwrap();
    wakes.recv_timeout(WORKER_TIMEOUT).unwrap();
    runtime.eval("worker.terminate();").unwrap();
    runtime.pump();
    assert!(!runtime.has_workers());
    assert_eq!(
        runtime.eval("replies.join(',')").unwrap(),
        Value::from("live")
    );
    assert!(runtime.diagnostics().is_empty());
    drop(runtime);
    assert_eq!(
        wakes.recv_timeout(WORKER_TIMEOUT),
        Err(RecvTimeoutError::Disconnected)
    );
}

#[test]
fn worker_timers_wake_without_an_incoming_message_and_run_checkpoints() {
    let (mut runtime, wakes) = runtime();
    runtime
        .eval(
            r#"
            var replies = [];
            const source = `
                const cancelled = setTimeout(() => postMessage('cancelled'), 0);
                clearInterval(cancelled);
                setTimeout(function(payload, suffix) {
                    postMessage('timeout:' + payload.value + suffix + ':' + (this === self));
                    queueMicrotask(() => postMessage('timeout checkpoint'));
                    let count = 0;
                    const interval = setInterval(() => {
                        const tick = ++count;
                        postMessage('tick ' + tick);
                        Promise.resolve().then(() => {
                            postMessage('tick checkpoint ' + tick);
                            if (tick === 2) {
                                clearTimeout(interval);
                                setTimeout(() => { close(); postMessage('end'); }, 20);
                            }
                        });
                        if (tick === 1) throw Error('interval fixture');
                    }, 0);
                }, 10, {value: 42}, '!');
                queueMicrotask(() => postMessage('startup checkpoint'));
            `;
            const url = URL.createObjectURL(new Blob([source]));
            const worker = new Worker(url);
            URL.revokeObjectURL(url);
            worker.onmessage = event => replies.push(event.data);
            worker.onerror = event => {
                event.preventDefault();
                replies.push(event.message.includes('interval fixture') ? 'caught' : event.message);
            };
            "#,
        )
        .unwrap();
    wait_for_worker_close(&mut runtime, &wakes);
    assert_eq!(
        runtime.eval("replies.join('|')").unwrap(),
        Value::from(
            "startup checkpoint|timeout:42!:true|timeout checkpoint|tick 1|tick checkpoint 1|caught|tick 2|tick checkpoint 2|end"
        ),
        "{:?}",
        runtime.diagnostics()
    );
    assert!(runtime.diagnostics().is_empty());
}

#[test]
fn discarding_the_document_releases_a_parked_worker_and_its_wake_callback() {
    let (mut previous, old_wakes) = runtime();
    previous
        .eval(
            r#"
            var replies = [];
            const url = URL.createObjectURL(new Blob([`
                setTimeout(() => postMessage('abandoned'), 60000);
                postMessage('parked');
            `]));
            const worker = new Worker(url);
            worker.onmessage = event => replies.push(event.data);
            URL.revokeObjectURL(url);
            "#,
        )
        .unwrap();
    old_wakes.recv_timeout(WORKER_TIMEOUT).unwrap();
    previous.pump();
    assert_eq!(previous.eval("replies.join(',')").unwrap(), "parked".into());
    assert!(previous.has_workers());

    // A full navigation drops the old ScriptRuntime. Its connection must
    // release the wake callback instead of retaining it until the timer fires.
    drop(previous);
    assert_eq!(
        old_wakes.recv_timeout(WORKER_TIMEOUT),
        Err(RecvTimeoutError::Disconnected)
    );

    let (mut next, next_wakes) = runtime();
    next.eval(
        r#"
        var replies = [];
        const url = URL.createObjectURL(new Blob(["postMessage('replacement'); close();"]));
        const worker = new Worker(url);
        worker.onmessage = event => replies.push(event.data);
        URL.revokeObjectURL(url);
        "#,
    )
    .unwrap();
    wait_for_worker_close(&mut next, &next_wakes);
    assert_eq!(
        next.eval("replies.join(',')").unwrap(),
        "replacement".into()
    );
}

#[test]
fn installing_a_window_wake_replays_an_already_queued_worker_notice() {
    let page = LoadedPage::from_html("https://example.test/workers", "<!doctype html>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime
        .eval(
            r#"
            var errors = 0;
            const worker = new Worker('blob:https://example.test/unregistered');
            worker.onerror = event => { errors++; event.preventDefault(); };
            "#,
        )
        .unwrap();
    let (notify, wakes) = mpsc::sync_channel(1);
    runtime.set_worker_wake(Arc::new(move || {
        let _ = notify.try_send(());
    }));
    // Unavailable Blob sources report the error during worker construction,
    // before the embedder installs the wake callback.
    assert!(wakes.try_recv().is_ok());
    runtime.pump();
    assert_eq!(runtime.eval("errors").unwrap(), Value::from(1));
    assert!(!runtime.has_workers());
}
