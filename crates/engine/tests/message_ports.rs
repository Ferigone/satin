use rust_browser_engine::{LoadedPage, script::ScriptRuntime};

fn runtime() -> ScriptRuntime {
    ScriptRuntime::new(&LoadedPage::from_html(
        "https://example.test/",
        "<p>Ports</p>",
    ))
    .unwrap()
}

fn yes(runtime: &mut ScriptRuntime, source: &str) {
    assert_eq!(
        runtime.eval(source).unwrap().as_bool(),
        Some(true),
        "{source}"
    );
}

#[test]
fn disabled_ports_and_microtask_checkpoints_preserve_task_order() {
    let mut browser = runtime();
    yes(
        &mut browser,
        r#"
        const first = new MessageChannel(), second = new MessageChannel(), log = [];
        const receive = label => event => {
            log.push(label+event.data);
            queueMicrotask(() => log.push('microtask'));
        };
        first.port2.addEventListener('message', receive('a'));
        first.port2.onmessageerror = () => log.push('messageerror');
        second.port2.onmessage = receive('b');
        first.port1.postMessage(0);
        second.port1.postMessage(1);
        Promise.resolve().then(() => log.push('initial'));
        log.length === 0
    "#,
    );
    browser.pump();
    yes(&mut browser, "log.join() === 'initial,b1,microtask'");
    yes(
        &mut browser,
        r#"
        first.port2.onmessage = null;
        second.port1.postMessage(2);
        first.port1.postMessage(3);
        log.length === 3 && first.port2.onmessage === null
    "#,
    );
    browser.pump();
    yes(
        &mut browser,
        "log.join() === 'initial,b1,microtask,a0,microtask,b2,microtask,a3,microtask'",
    );
}

#[test]
fn message_event_brands_properties_and_event_handler_slots() {
    let mut browser = runtime();
    yes(
        &mut browser,
        r#"
        const c = new MessageChannel(), order = [];
        const defaults = new MessageEvent('synthetic');
        const sample = new MessageEvent('custom', {
            bubbles: true, composed: true, data: undefined, origin: '\ud800',
            lastEventId: '\ud800', source: c.port1, ports: [c.port2]
        });
        const before = sample.bubbles && sample.composed && !sample.isTrusted && sample.data === null &&
          sample.origin.charCodeAt(0) === 65533 && sample.lastEventId.charCodeAt(0) === 55296 &&
          sample.source === c.port1 && Object.isFrozen(sample.ports) && sample.ports[0] === c.port2;
        sample.initMessageEvent('next', false, true, 17);
        let illegal = 0;
        for (const run of [
            () => MessageChannel(), () => new MessagePort(),
            () => MessagePort.prototype.start.call({}),
            () => Object.getOwnPropertyDescriptor(MessageChannel.prototype, 'port1').get.call({}),
            () => new MessageEvent('x', {source: {}}),
            () => new MessageEvent('x', {ports: [Object.create(MessagePort.prototype)]}),
            () => defaults.initMessageEvent()
        ]) {
            try { run(); } catch (error) { if (error instanceof TypeError) illegal++; }
        }
        c.port2.addEventListener('message', () => order.push('listener-before'));
        c.port2.onmessage = () => order.push('replaced');
        c.port2.onmessageerror = () => order.push('error-handler');
        c.port2.addEventListener('message', () => order.push('listener-after'));
        c.port2.onmessage = event => {
            order.push(event instanceof MessageEvent && event instanceof Event && event.isTrusted &&
              event.target === c.port2 && event.currentTarget === c.port2 && event.source === null &&
              event.origin === '' && event.lastEventId === '' && event.data === 'data' &&
              event.ports.length === 0 && Object.isFrozen(event.ports));
        };
        c.port2.dispatchEvent(new MessageEvent('messageerror'));
        c.port1.postMessage('data');
        before && sample.composed && !sample.bubbles && sample.cancelable && sample.type === 'next' &&
          sample.data === 17 && sample.source === null && !sample.ports.length && illegal === 7 &&
          defaults.data === null && defaults.origin === '' && defaults.source === null &&
          Object.getPrototypeOf(MessageEvent) === Event && c.port2 instanceof EventTarget &&
          order.join() === 'error-handler'
    "#,
    );
    browser.pump();
    yes(
        &mut browser,
        "order.join() === 'error-handler,listener-before,true,listener-after'",
    );
}

#[test]
fn transfer_preserves_graph_aliases_and_moves_an_enabled_backlog_to_a_disabled_port() {
    let mut browser = runtime();
    yes(
        &mut browser,
        r#"
        const payload = new MessageChannel(), router = new MessageChannel();
        const buffer = new Uint8Array([19,23]), log = [];
        payload.port2.onmessage = () => log.push('old listener');
        payload.port1.postMessage('first');
        const graph = {port: payload.port2, view: buffer, alias: buffer.buffer, n: 9007199254740993n};
        graph.self = graph;
        let port;
        router.port2.onmessage = event => {
            const data = event.data;
            port = event.ports[0];
            log.push(data.self === data && data.port === port && data.view.buffer === data.alias &&
              data.view.join() === '19,23' && data.n === 9007199254740993n &&
              event.ports.length === 1 && Object.isFrozen(event.ports));
            port.addEventListener('message', message => log.push(message.data));
        };
        router.port1.postMessage(graph, {transfer: [buffer.buffer, payload.port2]});
        payload.port1.postMessage('second');
        payload.port2.postMessage('old sender');
        graph.n = 0n;
        buffer.byteLength === 0
    "#,
    );
    browser.pump();
    yes(
        &mut browser,
        "log.join() === 'true' && port !== payload.port2",
    );
    browser.eval("port.start()").unwrap();
    browser.pump();
    yes(&mut browser, "log.join() === 'true,first,second'");
}

#[test]
fn transfer_validation_rechecks_after_getters_and_discards_doomed_delivery() {
    let mut browser = runtime();
    yes(
        &mut browser,
        r#"
        const carrier = new MessageChannel(), side = new MessageChannel(), log = [];
        const buffer = new ArrayBuffer(8);
        const getter = {get field() { log.push('visited'); side.port1.close(); return 1; }};
        let invalid = 0;
        for (const run of [
            () => carrier.port1.postMessage(getter, [carrier.port1]),
            () => structuredClone(getter, {transfer: [buffer, buffer]}),
            () => structuredClone(getter, {transfer: [buffer, side.port1]}),
            () => structuredClone(getter, {transfer: [side.port1]}),
            () => structuredClone(side.port2)
        ]) {
            try { run(); } catch (error) {
                if (error instanceof DOMException && error.name === 'DataCloneError') invalid++;
            }
        }
        carrier.port2.onmessage = () => log.push('unexpected');
        carrier.port1.postMessage('doomed', {transfer: [carrier.port2]});
        carrier.port1.postMessage('later');
        const attached = !buffer.detached;
        carrier.port2.postMessage(42, [buffer]);
        invalid === 5 && attached && buffer.detached && log.join() === 'visited,visited'
    "#,
    );
    browser.pump();
    yes(&mut browser, "log.join() === 'visited,visited'");
}

#[test]
fn nested_posts_clone_synchronously_and_detached_senders_keep_conversion_errors() {
    let mut browser = runtime();
    yes(
        &mut browser,
        r#"
        const c = new MessageChannel(), rows = [];
        c.port2.onmessage = event => rows.push(event.data);
        const input = {get n() { c.port1.postMessage('nested'); return 7; }};
        const buffer = new ArrayBuffer(8);
        let lookups = 0;
        const options = { get [Symbol.iterator]() {
            lookups++;
            return [buffer][Symbol.iterator].bind([buffer]);
        }, get transfer() { throw 'wrong overload'; } };
        c.port1.postMessage(input, options);
        c.port1.close();
        let conversionError = false;
        try { c.port1.postMessage(Symbol()); }
        catch (error) { conversionError = error.name === 'DataCloneError'; }
        lookups === 1 && conversionError && buffer.detached && rows.length === 0
    "#,
    );
    browser.pump();
    yes(
        &mut browser,
        "rows.length === 2 && rows[0] === 'nested' && rows[1].n === 7",
    );
}

#[test]
fn queue_quota_rejects_before_transfer_and_recovers_as_packets_dispatch() {
    let mut browser = runtime();
    browser
        .eval(
            "var channel = new MessageChannel(), buffer = new ArrayBuffer(8);
             for (let index = 0; index < 256; index++) channel.port1.postMessage(index);",
        )
        .unwrap();
    let error = browser
        .eval("channel.port1.postMessage(0, [buffer])")
        .unwrap_err();
    assert_eq!(error.kind.name(), "HeapLimit");
    yes(&mut browser, "!buffer.detached");
    browser.eval("channel.port2.start()").unwrap();
    browser.pump();
    yes(
        &mut browser,
        "channel.port1.postMessage(0, [buffer]); buffer.detached",
    );
}
