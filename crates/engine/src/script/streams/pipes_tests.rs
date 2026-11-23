use super::{StreamHost, Value, Vm, abort, run};

#[test]
fn pipe_delivers_last_chunk_through_intrinsic_read() {
    run(
        r#"
          var pipeDone, piped = [];
          const source = new ReadableStream({
            start(c) { c.enqueue('last'); c.close(); }
          });
          const destination = new WritableStream({
            write(chunk) { piped.push(chunk); },
            close() { piped.push('closed'); }
          });
          Object.prototype.then = function() { throw new Error('internal then call'); };
          source.pipeTo(destination).then(() => {
            delete Object.prototype.then;
            pipeDone = !source.locked && !destination.locked;
          });
        "#,
        "pipeDone && piped.join(',') === 'last,closed'",
    );
}

#[test]
fn pipe_respects_writable_queue_and_waits_for_abort_actions() {
    run(
        r#"
          var pipeDone;
          const signals = new AbortController();
          const reason = {};
          let sawWrite, endWrite, sawCancel, endCancel;
          const wrote = new Promise(resolve => sawWrite = resolve);
          const canceled = new Promise(resolve => sawCancel = resolve);
          let reads = 0, retained, cancelReason, abortReason;
          const source = new ReadableStream({
            pull(c) { c.enqueue(++reads); },
            cancel(r) {
              cancelReason = r;
              sawCancel();
              return new Promise(resolve => endCancel = resolve);
            }
          }, {highWaterMark: 0});
          const destination = new WritableStream({
            write(chunk) {
              if (chunk !== 1) return;
              sawWrite();
              return new Promise(resolve => endWrite = resolve);
            },
            abort(r) { abortReason = r; }
          }, {highWaterMark: 2});
          const pipe = source.pipeTo(destination, {signal: signals.signal}).catch(error => error);
          (async () => {
            await wrote;
            for (let i = 0; i < 8; i++) await Promise.resolve();
            retained = reads === 2;
            signals.abort(reason);
            retained = retained && cancelReason === undefined && abortReason === undefined;
            endWrite();
            await canceled;
            retained = retained && source.locked && destination.locked;
            endCancel();
            pipeDone = await pipe === reason && retained && cancelReason === reason &&
              abortReason === reason && !source.locked && !destination.locked && reads === 2;
          })();
        "#,
        "pipeDone === true",
    );
}

#[test]
fn pipe_abort_releases_a_pending_read_with_prevent_cancel() {
    run(
        r#"
          var pipeDone;
          const aborter = new AbortController();
          let controller, sawPull, canceled = false;
          const pulling = new Promise(resolve => sawPull = resolve);
          const source = new ReadableStream({
            start(c) { controller = c; },
            pull() { sawPull(); },
            cancel() { canceled = true; }
          }, {highWaterMark: 0});
          const destination = new WritableStream();
          const piped = source.pipeTo(destination, {
            signal: aborter.signal, preventCancel: true, preventAbort: true
          }).catch(error => error);
          (async () => {
            await pulling;
            aborter.abort('stop');
            const result = await piped;
            const released = !source.locked && !destination.locked;
            controller.enqueue('retained');
            const next = await source.getReader().read();
            pipeDone = result === 'stop' && released && !canceled && next.value === 'retained';
          })();
        "#,
        "pipeDone === true",
    );
}

#[test]
fn signal_keeps_abortable_pipe_alive_across_collection() {
    let mut vm = Vm::new();
    let abort_host = abort::AbortHost::install(&mut vm, None).unwrap();
    StreamHost::install(&mut vm, abort_host).unwrap();
    vm.eval(
        r#"
          var aborter = new AbortController(), canceled, aborted, rejected;
          (() => {
            const source = new ReadableStream({
              cancel(reason) { canceled = reason; }
            });
            const destination = new WritableStream({
              abort(reason) { aborted = reason; }
            });
            source.pipeTo(destination, {signal: AbortSignal.any([aborter.signal])})
              .catch(reason => rejected = reason);
          })();
        "#,
    )
    .unwrap();
    vm.with_task(|vm| vm.drain_jobs(128)).unwrap();
    vm.collect_garbage();
    vm.eval("aborter.abort('after GC');").unwrap();
    vm.with_task(|vm| vm.drain_jobs(128)).unwrap();
    assert_eq!(
        vm.eval("canceled === 'after GC' && aborted === canceled && rejected === canceled")
            .unwrap(),
        Value::Bool(true),
    );
    assert!(vm.take_unhandled_rejections().is_empty());
}

#[test]
fn transform_through_composes_start_transform_and_flush() {
    run(
        r#"
          var transformDone, output = [];
          let finishStart;
          const source = new ReadableStream({
            start(c) { c.enqueue(2); c.enqueue(3); c.close(); }
          });
          const transform = new TransformStream({
            start(c) {
              c.enqueue(1);
              return new Promise(resolve => finishStart = resolve);
            },
            transform(chunk, c) { c.enqueue(chunk * 2); },
            flush(c) { c.enqueue(7); }
          });
          const destination = new WritableStream({
            write(chunk) { output.push(chunk); }
          });
          source.pipeThrough(transform).pipeTo(destination).then(() => {
            transformDone = !source.locked && !destination.locked;
          });
          finishStart();
        "#,
        "transformDone && output.join(',') === '1,4,6,7'",
    );
}

#[test]
fn transform_cancel_unblocks_writer_and_awaits_cancel_callback() {
    run(
        r#"
          var transformDone, called = false, acceptCancel, cancelReason;
          const reason = {};
          const transform = new TransformStream({
            transform() { called = true; },
            cancel(input) {
              cancelReason = input;
              return new Promise(resolve => acceptCancel = resolve);
            }
          });
          const writer = transform.writable.getWriter();
          const writes = writer.write('blocked').catch(error => error);
          const closes = writer.closed.catch(error => error);
          (async () => {
            for (let i = 0; i < 4; i++) await Promise.resolve();
            const cancels = transform.readable.cancel(reason);
            const properReason = cancelReason === reason;
            acceptCancel();
            transformDone = await cancels === undefined && await writes === reason &&
              await closes === reason && properReason && !called;
          })();
        "#,
        "transformDone === true",
    );
}

#[test]
fn transform_termination_during_cancel_rejects_with_the_writable_error() {
    run(
        r#"
          var transformDone;
          let controller;
          const transform = new TransformStream({
            start(c) { controller = c; }
          });
          const canceled = transform.readable.cancel('cancel requested').catch(error => error);
          controller.terminate();
          const closed = transform.writable.getWriter().closed.catch(error => error);
          Promise.all([canceled, closed]).then(reasons => {
            transformDone = reasons[0] instanceof TypeError && reasons[1] === reasons[0];
          });
        "#,
        "transformDone === true",
    );
}

#[test]
fn transform_size_callback_can_terminate_while_a_chunk_is_enqueued() {
    run(
        r#"
          var transformDone;
          let controller;
          const transform = new TransformStream({
            start(c) { controller = c; }
          }, undefined, {
            highWaterMark: Infinity,
            size() { controller.terminate(); return 1; }
          });
          const writer = transform.writable.getWriter();
          const closes = writer.closed.catch(error => error);
          (async () => {
            await writer.write('retired while sizing');
            const readable = await transform.readable.getReader().read();
            transformDone = readable.done && await closes instanceof TypeError;
          })();
        "#,
        "transformDone === true",
    );
}
