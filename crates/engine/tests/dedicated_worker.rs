use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;
use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver},
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
            close();
            postMessage({turns, shared, viewSawWrite, scope, buffer, bytes, view, alias: bytes}, [buffer]);
            postMessage({detached: buffer.byteLength === 0 && bytes.byteLength === 0});
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
                replies[1].value.detached
            ].join('|');
            "#,
        )
        .unwrap_or_else(|error| panic!("{error}. {:?}", runtime.diagnostics()));
    assert_eq!(
        transcript,
        Value::from(
            "0|0|true|2|true|startup,first,microtask,second|true|true|true|true|true|true|1|2|2|12,42,8,10|true"
        ),
        "{:?}",
        runtime.diagnostics()
    );
    assert!(runtime.diagnostics().is_empty());
}

#[test]
fn terminating_a_worker_discards_its_queued_window_message() {
    let (mut runtime, wakes) = runtime();
    runtime
        .eval(
            r#"
            var replies = [];
            const url = URL.createObjectURL(new Blob(["onmessage = event => postMessage(event.data);"]));
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
}
