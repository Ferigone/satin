use super::*;

#[path = "pipes_tests.rs"]
mod pipes;

fn run(source: &str, check: &str) {
    let mut vm = Vm::new();
    let abort = abort::AbortHost::install(&mut vm, None).unwrap();
    StreamHost::install(&mut vm, abort).unwrap();
    vm.eval(source).unwrap();
    vm.collect_garbage();
    vm.with_task(|vm| {
        vm.drain_jobs(128)?;
        assert!(!vm.has_pending_jobs());
        Ok(())
    })
    .unwrap();
    assert_eq!(vm.eval(check).unwrap(), Value::Bool(true));
    assert!(vm.take_unhandled_rejections().is_empty());
}

#[test]
fn stream_constructors_validate_sources_and_keep_subclass_prototypes() {
    run(
        r#"
          class Readable extends ReadableStream {}
          class Writable extends WritableStream {}
          class Transform extends TransformStream {}
          const readable = new Readable({}, null);
          const bytes = new Readable({type: 'bytes'});
          const writable = new Writable();
          const transform = new Transform();
          let sizeReads = 0;
          const strategy = {get size() { sizeReads++; }};
          let rejected = 0;
          for (const Constructor of [Readable, Writable, Transform]) {
            try { new Constructor(null, strategy); }
            catch (error) { if (error instanceof TypeError) rejected++; }
          }
          let invalidType = false;
          try { new Readable({type: null}); }
          catch (error) { invalidType = error instanceof TypeError; }
        "#,
        r#"
          rejected === 3 && sizeReads === 0 && invalidType &&
            readable instanceof Readable && readable instanceof ReadableStream && !readable.locked &&
            bytes instanceof Readable && bytes.getReader({mode: 'byob'}) instanceof ReadableStreamBYOBReader &&
            writable instanceof Writable && writable instanceof WritableStream && !writable.locked &&
            transform instanceof Transform && transform instanceof TransformStream &&
            transform.readable instanceof ReadableStream && transform.writable instanceof WritableStream
        "#,
    );
}

#[test]
fn releasing_reader_rejects_old_reads_and_preserves_future_chunks() {
    run(
        r#"
          var control, readRejected, closeRejected, chunk, ended;
          const stream = new ReadableStream({
            start(c) { control = c; }
          }, {highWaterMark: 0});
          const first = stream.getReader();
          first.read().catch(error => readRejected = error instanceof TypeError);
          first.closed.catch(error => closeRejected = error instanceof TypeError);
          first.releaseLock();
          const unlocked = !stream.locked;
          control.enqueue({value: 8});
          control.close();
          const second = stream.getReader();
          second.read().then(result => chunk = result);
          second.read().then(result => ended = result);
        "#,
        "unlocked && readRejected && closeRejected && chunk.value.value === 8 && !chunk.done && ended.done",
    );
}

#[test]
fn a_closing_queue_is_closed_before_the_last_default_read_settles() {
    run(
        r#"
          var seen = [], results = [];
          for (const type of [undefined, 'bytes']) {
            let control;
            const stream = new ReadableStream({type, start(c) {
              control = c;
              c.enqueue(type === 'bytes' ? new Uint8Array([23]) : 23);
              c.close();
            }}, {highWaterMark: 3});
            const reader = stream.getReader();
            Object.defineProperty(Object.prototype, 'then', {
              configurable: true,
              get: function() { seen.push(control.desiredSize); }
            });
            let read;
            try { read = reader.read(); }
            finally { delete Object.prototype.then; }
            read.then(result => results.push(!result.done && (type === 'bytes' ? result.value[0] : result.value)));
          }
        "#,
        "seen.join(',') === '0,0' && results.join(',') === '23,23'",
    );
}

#[test]
fn a_reentrant_close_stops_cancel_from_calling_the_underlying_source() {
    run(
        r#"
          var observed, result, closed, canceled = false;
          let control;
          const stream = new ReadableStream({
            start(c) { control = c; c.enqueue(41); },
            cancel() { canceled = true; }
          }, {highWaterMark: 3});
          const reader = stream.getReader();
          Object.defineProperty(Object.prototype, 'then', {
            configurable: true,
            get: function() {
              control.close();
              observed = control.desiredSize;
              reader.cancel().then(() => closed = true);
            }
          });
          let read;
          try { read = reader.read(); }
          finally { delete Object.prototype.then; }
          read.then(r => result = r);
        "#,
        "observed === 0 && closed && !canceled && result.value === 41 && !result.done",
    );
}

#[test]
fn cancel_preserves_branch_reason_order_and_awaits_underlying_source() {
    run(
        r#"
          var reasons, allow, leftDone, rightDone;
          const stream = new ReadableStream({
            cancel(reason) {
              reasons = reason;
              return new Promise(resolve => allow = resolve);
            }
          });
          const branches = stream.tee();
          branches[0].cancel('left').then(() => leftDone = true);
          const waitsForRight = reasons === undefined && leftDone === undefined;
          branches[1].cancel('right').then(() => rightDone = true);
          const waitsForSource = leftDone === undefined && rightDone === undefined;
          allow();
        "#,
        "waitsForRight && waitsForSource && leftDone && rightDone && reasons.join(',') === 'left,right'",
    );
}

#[test]
fn iterator_serializes_reads_and_waits_for_cancel_after_releasing_lock() {
    run(
        r#"
          var controller, cancelReason, finishCancel, first, returned, after, unlocked;
          const stream = new ReadableStream({
            start(c) { controller = c; },
            cancel(reason) {
              cancelReason = reason;
              return new Promise(resolve => finishCancel = resolve);
            }
          }, {highWaterMark: 0});
          const iterator = stream.values();
          const writes = [];
          const firstPromise = iterator.next().then(r => { first = r; writes.push('next'); });
          iterator.return('bye').then(r => { returned = r; writes.push('return'); });
          iterator.next().then(r => { after = r; writes.push('done'); });
          const held = stream.locked;
          controller.enqueue(undefined);
          firstPromise.then(async () => {
            while (finishCancel === undefined) await Promise.resolve();
            unlocked = !stream.locked && returned === undefined && after === undefined;
            finishCancel();
          });
        "#,
        "held && unlocked && cancelReason === 'bye' && !first.done && first.value === undefined &&
          returned.value === 'bye' && returned.done && after.done && writes.join(',') === 'next,return,done'",
    );
}

#[test]
fn async_iteration_with_prevent_cancel_preserves_remaining_chunks() {
    run(
        r#"
          var observed, finalChunk, unlocked, canceled = false;
          const stream = new ReadableStream({
            start(c) { c.enqueue(1); c.enqueue(2); c.close(); },
            cancel() { canceled = true; }
          });
          const alias = ReadableStream.prototype[Symbol.asyncIterator] === ReadableStream.prototype.values;
          async function consume() {
            const iterator = stream.values({preventCancel: true});
            for await (const value of iterator) {
              observed = value;
              break;
            }
            unlocked = !stream.locked;
            const reader = stream.getReader();
            finalChunk = await reader.read();
          }
          consume();
        "#,
        "alias && unlocked && !canceled && observed === 1 && !finalChunk.done && finalChunk.value === 2",
    );
}

#[test]
fn closed_async_iterator_releases_its_reader_in_the_read_close_steps() {
    run(
        r#"
          var result, controller;
          const stream = new ReadableStream({start(c) { controller = c; }});
          const iterator = stream.values();
          iterator.next().then(value => result = value);
          controller.close();
          const unlocked = !stream.locked;
        "#,
        "unlocked && result.done && result.value === undefined",
    );
}
