use hyper::body::{Body, Bytes, Frame};
use rust_browser_engine::{
    LoadedPage,
    network::{FetchError, FetchErrorKind, UploadBody},
    script::ScriptRuntime,
};
use rust_browser_javascript::Value;
use std::{
    future::Future,
    pin::{Pin, pin},
    task::{Context, Poll, Waker},
};

fn runtime() -> ScriptRuntime {
    let page = LoadedPage::from_html(
        "https://upload.test/index.html",
        "<!doctype html><main></main>",
    );
    ScriptRuntime::new(&page).unwrap()
}

fn verified(runtime: &mut ScriptRuntime, expression: &str) {
    assert_eq!(
        runtime.eval(expression).unwrap(),
        Value::Bool(true),
        "{expression}"
    );
}

fn frame(transport: &mut UploadBody) -> Poll<Option<Result<Frame<Bytes>, FetchError>>> {
    Pin::new(transport).poll_frame(&mut Context::from_waker(Waker::noop()))
}

fn deliver_demand(runtime: &mut ScriptRuntime, id: u64, transport: &mut UploadBody) {
    assert!(frame(transport).is_pending());
    let demand = transport.demand();
    let mut next = pin!(demand.next());
    let mut context = Context::from_waker(Waker::noop());
    assert!(next.as_mut().poll(&mut context).is_ready());
    // Further transport polls while this demand is outstanding do not read again.
    assert!(frame(transport).is_pending());
    assert!(pin!(demand.next()).poll(&mut context).is_pending());
    runtime.progress_network_upload(id).unwrap();
}

fn bytes(transport: &mut UploadBody) -> Bytes {
    let Poll::Ready(Some(Ok(frame))) = frame(transport) else {
        panic!("Expected upload data frame");
    };
    frame.into_data().unwrap()
}

#[test]
fn request_duplex_validation_and_body_proxy_preserve_stream_ownership() {
    let mut runtime = runtime();
    verified(
        &mut runtime,
        r#"
        function invalid(options) {
          try { new Request('/upload', options); return false; }
          catch (error) { return error instanceof TypeError; }
        }
        let original = new ReadableStream({start(c) { c.enqueue(new Uint8Array([4, 7])); c.close(); }});
        let getterRuns = 0;
        let request = new Request('/upload', {
          method: 'POST', body: original, get duplex() { getterRuns++; return 'half'; }
        });
        let identity = getterRuns === 1 && request.body === original && request.duplex === 'half' && !original.locked;
        let clone = request.clone();
        let oldBody = clone.body;
        let proxy = new Request(clone);
        var receipt;
        Promise.all([request.bytes(), proxy.bytes()]).then(values => { receipt = [values[0].join(), values[1].join()]; });
        let locked = new ReadableStream();
        locked.getReader();
        identity && oldBody.locked && clone.bodyUsed && proxy.body !== oldBody &&
          invalid({method:'POST', body:new ReadableStream()}) &&
          invalid({method:'POST', body:new ReadableStream(), duplex:'full'}) &&
          invalid({method:'POST', body:new ReadableStream(), duplex:'half', keepalive:true}) &&
          invalid({method:'POST', body:new ReadableStream(), duplex:'half', mode:'no-cors'}) &&
          invalid({method:'GET', body:new ReadableStream(), duplex:'half'}) &&
          invalid({method:'POST', body:locked, duplex:'half'}) &&
          new Request('/buffered', {method:'POST', body:'plain', duplex:'half'}).duplex === 'half'
    "#,
    );
    runtime.pump();
    verified(&mut runtime, "receipt.join('|') === '4,7|4,7'");
}

#[test]
fn a_preaborted_upload_cancels_synchronously_with_the_same_reason() {
    let mut runtime = runtime();
    verified(
        &mut runtime,
        r#"
        const controller = new AbortController();
        const reason = {token:true};
        controller.abort(reason);
        var canceled, rejected;
        let source = new ReadableStream({cancel(cause) { canceled = cause; }}, {highWaterMark:0});
        fetch('/upload', {method:'POST', duplex:'half', body:source, signal:controller.signal})
          .catch(cause => rejected = cause);
        canceled === reason
    "#,
    );
    assert!(runtime.take_network_jobs().is_empty());
    runtime.pump();
    verified(&mut runtime, "rejected === reason");
}

#[test]
fn demand_snapshots_author_bytes_and_sends_bounded_frames_before_the_next_pull() {
    let mut runtime = runtime();
    verified(
        &mut runtime,
        r#"
        var pulls = 0;
        const body = new ReadableStream({pull(controller) {
          pulls++;
          if (pulls === 1) {
            const input = new Uint8Array(70000).fill(77);
            controller.enqueue(input);
            input.fill(9);
          } else {
            controller.close();
          }
        }}, {highWaterMark:0});
        const request = new Request('/upload', {method:'POST', duplex:'half', body});
        fetch(request).catch(() => {});
        request.bodyUsed && body.locked && pulls === 0
    "#,
    );
    assert!(runtime.take_network_requests().is_empty());
    let mut job = runtime.take_network_jobs().pop().unwrap();
    let mut transport = job.upload.take().unwrap();
    for length in [32768, 32768, 4464] {
        deliver_demand(&mut runtime, job.id, &mut transport);
        let payload = bytes(&mut transport);
        assert_eq!(payload.len(), length);
        assert!(payload.iter().all(|&byte| byte == 77));
        verified(&mut runtime, "pulls === 1");
    }
    deliver_demand(&mut runtime, job.id, &mut transport);
    assert!(matches!(frame(&mut transport), Poll::Ready(None)));
    verified(&mut runtime, "pulls === 2");
}

#[test]
fn cancel_and_invalid_chunk_errors_drop_the_transport_and_keep_reason_identity() {
    let mut runtime = runtime();
    runtime
        .eval(
            r#"
        const controller = new AbortController();
        const reason = {token:1};
        var canceled, rejected, pulls = 0;
        let body = new ReadableStream({pull() { pulls++; return new Promise(() => {}); },
          cancel(cause) { canceled = cause; }
        }, {highWaterMark:0});
        fetch('/upload', {method:'POST', duplex:'half', body, signal:controller.signal})
          .catch(cause => rejected = cause);
    "#,
        )
        .unwrap();
    let mut job = runtime.take_network_jobs().pop().unwrap();
    let mut transport = job.upload.take().unwrap();
    deliver_demand(&mut runtime, job.id, &mut transport);
    runtime.eval("controller.abort(reason)").unwrap();
    runtime.pump();
    verified(
        &mut runtime,
        "pulls === 1 && rejected === reason && canceled === reason",
    );
    assert!(job.request.signal.unwrap().is_cancelled());
    assert!(matches!(frame(&mut transport), Poll::Ready(Some(Err(_)))));
    assert_eq!(runtime.pending_network(), 0);

    runtime
        .eval(
            r#"
        var failed, stopped;
        let bad = new ReadableStream({pull(c) { c.enqueue(new Uint16Array(1)); },
          cancel(cause) { stopped = cause; }
        }, {highWaterMark:0});
        fetch('/invalid', {method:'POST', duplex:'half', body:bad}).catch(cause => failed = cause);
    "#,
        )
        .unwrap();
    let mut job = runtime.take_network_jobs().pop().unwrap();
    let mut transport = job.upload.take().unwrap();
    deliver_demand(&mut runtime, job.id, &mut transport);
    let Poll::Ready(Some(Err(error))) = frame(&mut transport) else {
        panic!("Expected invalid chunk failure");
    };
    assert_eq!(error.kind, FetchErrorKind::Network);
    runtime.complete_fetch(job.id, Err(error)).unwrap();
    verified(
        &mut runtime,
        "failed instanceof TypeError && stopped === failed",
    );
}

#[test]
fn an_empty_or_detached_source_chunk_does_not_end_the_upload() {
    let mut runtime = runtime();
    runtime
        .eval(
            r#"
      var pulls = 0;
      const body = new ReadableStream({pull(c) {
        pulls++;
        if (pulls === 1) {
          const bytes = new Uint8Array([1, 2]);
          bytes.buffer.transfer();
          c.enqueue(bytes);
        } else if (pulls === 2) {
          c.enqueue(new Uint8Array([3]));
          c.close();
        }
      }}, {highWaterMark:0});
      fetch('/upload', {method:'POST', duplex:'half', body}).catch(() => {});
    "#,
        )
        .unwrap();
    let mut job = runtime.take_network_jobs().pop().unwrap();
    let mut transport = job.upload.take().unwrap();
    deliver_demand(&mut runtime, job.id, &mut transport);
    deliver_demand(&mut runtime, job.id, &mut transport);
    assert_eq!(&bytes(&mut transport)[..], &[3]);
    deliver_demand(&mut runtime, job.id, &mut transport);
    assert!(matches!(frame(&mut transport), Poll::Ready(None)));
}
