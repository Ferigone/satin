use super::*;
use crate::script::abort::AbortHost;

fn run(source: &str, check: &str, memory: Option<Vec<u8>>) {
    let mut vm = Vm::new();
    let abort = AbortHost::install(&mut vm, None).unwrap();
    let host = StreamHost::install(&mut vm, abort).unwrap();
    if let Some(memory) = memory {
        let len = memory.len();
        let stream = host
            .memory_stream(&mut vm, Arc::new(memory), 0..len)
            .unwrap();
        vm.set_global("memory", stream).unwrap();
    }
    vm.eval(source).unwrap();
    vm.collect_garbage();
    vm.with_task(|vm| {
        vm.drain_jobs(128)?;
        assert!(!vm.has_pending_jobs());
        Ok(())
    })
    .unwrap();
    assert_eq!(vm.take_unhandled_rejections().len(), 0);
    assert_eq!(
        vm.eval(check).unwrap(),
        Value::Bool(true),
        "{:?}",
        vm.get_global("testState").unwrap()
    );
}

#[test]
fn enqueue_fills_ready_byob_views_before_inherited_then_getters() {
    run(
        r#"
          var checked, controller, sawCompleteBatch;
          const stream = new ReadableStream({type:'bytes', start(c) {controller = c;}});
          const reader = stream.getReader({mode:'byob'});
          const view = new Float64Array(new ArrayBuffer(32), 24, 1);
          const first = reader.read(new Uint8Array(16));
          const second = reader.read(view);
          Object.defineProperty(Object.prototype, 'then', {
            configurable: true,
            get: function () {
              if (sawCompleteBatch === undefined && this.value instanceof Uint8Array) {
                sawCompleteBatch = controller.byobRequest === null;
              }
            }
          });
          try {
            controller.enqueue(new Uint8Array(24).fill(66));
          } finally {
            delete Object.prototype.then;
          }
          Promise.all([first, second]).then(([a, b]) => {
            const bytes = new Uint8Array(b.value.buffer, b.value.byteOffset, b.value.byteLength);
            checked = sawCompleteBatch === true && !a.done && !b.done &&
              view.byteLength === 0 && a.value.length === 16 && a.value.every(value => value === 66) &&
              b.value instanceof Float64Array && b.value.byteOffset === 24 &&
              b.value.buffer.byteLength === 32 && bytes.length === 8 && bytes.every(value => value === 66);
          });
        "#,
        "checked === true",
        None,
    );
}

#[test]
fn response_remainder_fills_the_next_byob_view_before_settling() {
    run(
        r#"
          var checked, controller, sawCompleteBatch;
          const stream = new ReadableStream({type:'bytes', start(c) {controller = c;}});
          const reader = stream.getReader({mode:'byob'});
          const first = reader.read(new Uint16Array(2));
          const second = reader.read(new Uint8Array(1));
          const request = controller.byobRequest;
          request.view.set([4,5,6]);
          Object.defineProperty(Object.prototype, 'then', {
            configurable: true,
            get: function () {
              if (sawCompleteBatch === undefined && this.value instanceof Uint16Array) {
                sawCompleteBatch = controller.byobRequest === null;
              }
            }
          });
          try {
            request.respond(3);
          } finally {
            delete Object.prototype.then;
          }
          Promise.all([first,second]).then(([a,b]) => {
            const bytes = new Uint8Array(a.value.buffer, a.value.byteOffset, a.value.byteLength);
            checked = sawCompleteBatch === true && !a.done && !b.done &&
              bytes.join(',') === '4,5' && b.value.join(',') === '6' && request.view === null;
          });
        "#,
        "checked === true",
        None,
    );
}

#[test]
fn closed_byob_batch_releases_its_descriptors_before_settling() {
    run(
        r#"
          var checked, controller, sawCompleteBatch;
          const stream = new ReadableStream({type:'bytes', start(c) {controller = c;}});
          const reader = stream.getReader({mode:'byob'});
          const first = reader.read(new Uint8Array(new ArrayBuffer(4), 2));
          const second = reader.read(new Uint8Array(new ArrayBuffer(5), 3));
          const request = controller.byobRequest;
          controller.close();
          Object.defineProperty(Object.prototype, 'then', {
            configurable: true,
            get: function () {
              if (sawCompleteBatch === undefined && this.value instanceof Uint8Array) {
                sawCompleteBatch = controller.byobRequest === null;
              }
            }
          });
          try {
            request.respond(0);
          } finally {
            delete Object.prototype.then;
          }
          Promise.all([first,second]).then(([a,b]) => {
            checked = sawCompleteBatch === true && a.done && b.done &&
              a.value.byteOffset === 2 && b.value.byteOffset === 3 &&
              a.value.byteLength === 0 && b.value.byteLength === 0 && request.view === null;
          });
        "#,
        "checked === true",
        None,
    );
}

#[test]
fn copies_native_bytes_into_the_visible_range_and_preserves_the_view_kind() {
    run(
        r#"
          var checked;
          async function test() {
            const reader = memory.getReader({mode: 'byob'});
            const buffer = new ArrayBuffer(8);
            const original = new DataView(buffer, 3, 2);
            const pending = reader.read(original, {min: 2});
            const detached = buffer.byteLength === 0;
            const first = await pending;
            const second = await reader.read(new Uint8Array(4));
            const end = await reader.read(new Uint8Array(new ArrayBuffer(7), 4));
            checked = detached && first.value instanceof DataView && first.value.byteOffset === 3 &&
              first.value.buffer.byteLength === 8 && first.value.getUint8(0) === 1 && first.value.getUint8(1) === 2 &&
              !first.done && second.value.length === 3 && second.value[0] === 3 && second.value[2] === 5 &&
              end.done && end.value.length === 0 && end.value.byteOffset === 4;
          }
          test();
        "#,
        "checked === true",
        Some(vec![1, 2, 3, 4, 5]),
    );
}

#[test]
fn byob_requests_commit_aligned_elements_and_queue_their_remainder() {
    run(
        r#"
          var checked, firstView, previousRequest, requests = 0;
          const stream = new ReadableStream({
            type: 'bytes',
            pull(controller) {
              const request = controller.byobRequest;
              requests++;
              if (requests === 1) {
                firstView = request.view;
                request.view[0] = 7;
                previousRequest = request;
                request.respond(1);
              } else if (requests === 2) {
                request.view[0] = 9;
                request.view[1] = 11;
                request.respond(2);
              } else {
                controller.close();
                request.respond(0);
              }
            }
          });
          async function test() {
            const reader = new ReadableStreamBYOBReader(stream);
            const first = await reader.read(new Uint16Array(new ArrayBuffer(8), 2, 2));
            const remainder = await reader.read(new DataView(new ArrayBuffer(4), 1, 3));
            const end = await reader.read(new Uint8Array(1));
            checked = firstView.byteLength === 0 && previousRequest.view === null &&
              first.value instanceof Uint16Array && first.value.byteOffset === 2 && first.value.length === 1 &&
              first.value[0] === 2311 && remainder.value.getUint8(0) === 11 && !remainder.done &&
              end.value.length === 0 && end.done && requests === 3;
          }
          test();
        "#,
        "checked === true",
        None,
    );
}

#[test]
fn default_reader_autoallocates_and_enqueued_views_keep_their_buffer_range() {
    run(
        r#"
          var checked, enqueued, oldView, controllerCheck, pullCount = 0;
          const stream = new ReadableStream({
            type: 'bytes', autoAllocateChunkSize: 2,
            pull(controller) {
              const request = controller.byobRequest;
              controllerCheck = controller instanceof ReadableByteStreamController;
              pullCount++;
              if (pullCount === 1) {
                oldView = request.view;
                enqueued = new Uint8Array(new ArrayBuffer(8), 3, 3);
                enqueued[1] = 17;
                controller.enqueue(enqueued);
              } else {
                request.view[1] = 12;
                const replacement = new Uint8Array(request.view.buffer.transfer(), 0, 2);
                request.respondWithNewView(replacement);
                controller.close();
              }
            }
          });
          async function test() {
            const reader = stream.getReader();
            const first = await reader.read();
            const second = await reader.read();
            const end = await reader.read();
            checked = controllerCheck && oldView.byteLength === 0 && enqueued.byteLength === 0 &&
              first.value.length === 3 && first.value.byteOffset === 3 && first.value.buffer.byteLength === 8 &&
              first.value[1] === 17 && second.value.length === 2 && second.value[1] === 12 &&
              end.done && end.value === undefined;
          }
          test();
        "#,
        "checked === true",
        None,
    );
}

#[test]
fn tee_lends_the_requesting_branch_buffer_and_reuses_its_prefix() {
    run(
        r#"
          var checked, controller, pulls = 0;
          const stream = new ReadableStream({
            type: 'bytes',
            start(value) { controller = value; },
            pull() { pulls++; }
          });
          async function test() {
            const branches = stream.tee();
            const first = branches[0].getReader({mode: 'byob'});
            const second = branches[1].getReader({mode: 'byob'});
            await Promise.resolve();
            const neverPulled = pulls === 0;
            const firstBacking = new Uint8Array([55, 0, 0, 0, 56]);
            const firstRead = first.read(new Uint8Array(firstBacking.buffer, 1, 3), {min: 3});
            const secondRead = second.read(new Uint8Array(1));
            const firstRequest = controller.byobRequest;
            const offsets = pulls === 1 && firstRequest.view.byteOffset === 1 &&
              firstRequest.view.byteLength === 3 && firstRequest.view.buffer.byteLength === 5;
            firstRequest.view[0] = 1;
            firstRequest.respond(1);
            const secondResult = await secondRead;
            const nextRequest = controller.byobRequest;
            const preserved = pulls === 2 && nextRequest.view.byteOffset === 2 &&
              nextRequest.view.byteLength === 2 && new Uint8Array(nextRequest.view.buffer)[1] === 1;
            nextRequest.view[0] = 2;
            nextRequest.view[1] = 3;
            nextRequest.respond(2);
            controller.close();
            const firstResult = await firstRead;
            const tail = await second.read(new Uint8Array(2));
            const end = await first.read(new Uint8Array(1));
            checked = neverPulled && offsets && preserved && firstRequest.view === null &&
              firstBacking.byteLength === 0 && firstResult.value[0] === 1 &&
              firstResult.value[2] === 3 && new Uint8Array(firstResult.value.buffer)[4] === 56 &&
              secondResult.value[0] === 1 && tail.value[0] === 2 && tail.value[1] === 3 &&
              end.done && end.value.byteLength === 0;
          }
          test();
        "#,
        "checked === true",
        None,
    );
}

#[test]
fn nested_byte_tees_restore_the_borrowed_buffer_at_end_of_stream() {
    run(
        r#"
          var checked, controller, testState;
          const stream = new ReadableStream({type: 'bytes', start(value) { controller = value; }});
          async function test() {
            const outer = stream.tee();
            const inner = outer[0].tee();
            const reader = inner[1].getReader({mode: 'byob'});
            const sibling = inner[0].getReader();
            const pending = reader.read(new Uint8Array(new ArrayBuffer(8), 2, 4));
            const request = controller.byobRequest;
            const correctOffset = request.view.byteOffset === 2 && request.view.byteLength === 4;
            request.view[0] = 19;
            request.respond(1);
            testState = 'responded ' + correctOffset;
            const first = await pending;
            testState += ', first = ' + first.done + ' ' + first.value[0];
            const siblingResult = await sibling.read();
            testState += ', sibling = ' + siblingResult.done + ' ' + siblingResult.value[0];
            const closing = reader.read(new Uint8Array(new ArrayBuffer(6), 3, 1));
            const finalRequest = controller.byobRequest;
            controller.close();
            finalRequest.respond(0);
            testState += ', requested close';
            const ended = await closing;
            testState += ', ended = ' + ended.done + ' ' + ended.value.byteOffset + ' ' +
              ended.value.byteLength + ' ' + ended.value.buffer.byteLength;
            checked = correctOffset && first.value[0] === 19 && siblingResult.value[0] === 19 &&
              !first.done && ended.done && ended.value.byteOffset === 3 &&
              ended.value.byteLength === 0 && ended.value.buffer.byteLength === 6;
          }
          test();
        "#,
        "checked === true",
        None,
    );
}

#[test]
fn release_keeps_partial_source_bytes_and_cancel_closes_byob_reads() {
    run(
        r#"
          var checked, controller, cancelled;
          const stream = new ReadableStream({
            type: 'bytes',
            start(value) { controller = value; },
            cancel(reason) { cancelled = reason; }
          });
          async function test() {
            const first = stream.getReader({mode: 'byob'});
            const rejected = first.read(new Uint16Array(2)).catch(error => error instanceof TypeError);
            controller.byobRequest.view[0] = 5;
            controller.byobRequest.respond(1);
            first.releaseLock();
            const reader = stream.getReader({mode: 'byob'});
            const next = reader.read(new Uint8Array(2));
            controller.byobRequest.view[0] = 6;
            controller.byobRequest.respond(1);
            const bytes = await next;
            const pending = reader.read(new Uint8Array(1));
            const request = controller.byobRequest;
            await reader.cancel('finished');
            const end = await pending;
            checked = await rejected && bytes.value.length === 2 && bytes.value[0] === 5 && bytes.value[1] === 6 &&
              end.done && end.value === undefined && cancelled === 'finished' && request.view === null;
          }
          test();
        "#,
        "checked === true",
        None,
    );
}
