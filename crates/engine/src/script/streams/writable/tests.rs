use super::*;

fn setup() -> Vm {
    let mut vm = Vm::new();
    let abort = abort::AbortHost::install(&mut vm, None).unwrap();
    Host::install(&mut vm, abort).unwrap();
    vm
}

fn drain(vm: &mut Vm) {
    vm.collect_garbage();
    vm.with_task(|vm| {
        vm.drain_jobs(200)?;
        assert!(!vm.has_pending_jobs());
        Ok(())
    })
    .unwrap();
    assert!(vm.take_unhandled_rejections().is_empty());
}

fn run(source: &str, check: &str) {
    let mut vm = setup();
    vm.eval(source).unwrap();
    drain(&mut vm);
    assert_eq!(vm.eval(check).unwrap(), Value::Bool(true));
}

#[test]
fn waits_for_start_and_serializes_sink_calls_under_backpressure() {
    run(
        r#"
        const calls = [];
        let start, next, firstDone, secondDone, ready, finished;
        const sink = {
          start(c) {
            calls.push(c.signal instanceof AbortSignal && this === sink ? 'start' : 'wrong');
            return new Promise(resolve => start = resolve);
          },
          write(chunk, c) {
            calls.push(chunk.value);
            return chunk.value === 1 ? new Promise(resolve => next = resolve) : undefined;
          },
          close() { calls.push('close'); }
        };
        const stream = new WritableStream(sink, {highWaterMark: 2, size: v => v.size});
        const writer = stream.getWriter();
        const originalReady = writer.ready;
        writer.write({value: 1, size: 2}).then(() => firstDone = true);
        writer.write({value: 2, size: 1}).then(() => secondDone = true);
        const backpressured = writer.desiredSize === -1 && writer.ready !== originalReady;
        writer.ready.then(() => ready = true);
        const expectedBeforeStart = calls.join(',') === 'start';
        start();
        Promise.resolve().then(() => Promise.resolve()).then(() => {
          if (calls.join(',') !== 'start,1' || ready || secondDone) throw new Error('ordering');
          next();
        });
        writer.closed.then(() => finished = true);
        Promise.resolve().then(() => Promise.resolve()).then(() => writer.ready).then(() => writer.close());
        "#,
        "backpressured && expectedBeforeStart && firstDone && secondDone && ready && finished && calls.join(',') === 'start,1,2,close' && writer.desiredSize === 0",
    );
}

#[test]
fn abort_signal_precedes_abort_callback_and_preserves_pending_write_result() {
    let mut vm = setup();
    vm.eval(
        r#"
        const error = {};
        let signal, unblock, abortFinish, abortCalls = 0, eventCalls = 0;
        let firstDone, secondFailed, closeFailed, pendingAbort, closed;
        const stream = new WritableStream({
          start(c) { signal = c.signal; signal.addEventListener('abort', () => eventCalls++); },
          write() { return new Promise(resolve => unblock = resolve); },
          abort(reason) {
            if (reason === error && signal.reason === error && signal.aborted) abortCalls++;
            return new Promise(resolve => abortFinish = resolve);
          }
        });
        const writer = stream.getWriter();
        writer.write(1).then(() => firstDone = true);
        writer.write(2).catch(reason => secondFailed = reason === error);
        writer.closed.catch(reason => closed = reason === error);
        "#,
    )
    .unwrap();
    drain(&mut vm);
    vm.eval(
        r#"
        const aborting = writer.abort(error);
        const sameAbort = writer.abort('ignored') === aborting;
        aborting.then(() => pendingAbort = false);
        pendingAbort = true;
        writer.close().catch(reason => closeFailed = reason === error);
        const before = signal.aborted && eventCalls === 1 && abortCalls === 0 && writer.desiredSize === null;
        unblock();
        "#,
    )
    .unwrap();
    drain(&mut vm);
    assert_eq!(
        vm.eval("before && sameAbort && firstDone && secondFailed && pendingAbort && abortCalls === 1 && !closed && !closeFailed").unwrap(),
        Value::Bool(true)
    );
    vm.eval("abortFinish()").unwrap();
    drain(&mut vm);
    assert_eq!(
        vm.eval("!pendingAbort && closed && closeFailed").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn release_rejects_status_promises_and_keeps_writes_for_a_new_writer() {
    run(
        r#"
        const calls = [];
        let releaseReady, releaseClosed, previous, current, done;
        const stream = new WritableStream({write(v) { calls.push(v.v); }});
        const writer = new WritableStreamDefaultWriter(stream);
        writer.write({v: 1}).then(() => previous = true);
        const ready = writer.ready;
        const closed = writer.closed;
        ready.catch(e => releaseReady = e);
        closed.catch(e => releaseClosed = e);
        writer.releaseLock();
        const same = writer.ready === ready && writer.closed === closed && !stream.locked;
        const other = stream.getWriter();
        other.write({v: 2}).then(() => current = true);
        other.close().then(() => done = true);
        "#,
        "same && releaseReady === releaseClosed && releaseReady instanceof TypeError && previous && current && done && calls.join(',') === '1,2'",
    );
}

#[test]
fn rejects_on_bad_size_and_rechecks_lock_after_reentrant_sizing() {
    run(
        r#"
        let released, bad, closed, unexpected;
        const silent = {write() { unexpected = true; }};
        const a = new WritableStream(silent, {size() { writer.releaseLock(); return 1; }});
        const writer = a.getWriter();
        writer.write(1).catch(error => released = error instanceof TypeError);
        const b = new WritableStream(silent, {size() { return Infinity; }});
        const broken = b.getWriter();
        broken.write('bad').catch(error => bad = error);
        broken.closed.catch(error => closed = error);
        "#,
        "released && !a.locked && bad === closed && bad instanceof RangeError && broken.desiredSize === null && !unexpected",
    );
}

#[test]
fn handles_close_and_abort_racing_while_that_close_is_in_flight() {
    let mut vm = setup();
    vm.eval(
        r#"
        let signal, finish, abortCalls = 0, closed, aborted, writerClosed;
        const writer = new WritableStream({
          start(c) { signal = c.signal; },
          close() { return new Promise(resolve => finish = resolve); },
          abort() { abortCalls++; }
        }).getWriter();
        writer.close().then(() => closed = true);
        writer.closed.then(() => writerClosed = true);
        "#,
    )
    .unwrap();
    drain(&mut vm);
    vm.eval("writer.abort('stop').then(() => aborted = true); finish();")
        .unwrap();
    drain(&mut vm);
    assert_eq!(
        vm.eval("signal.aborted && signal.reason === 'stop' && closed && writerClosed && aborted && abortCalls === 0 && writer.desiredSize === 0").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn a_pending_write_retains_its_stream_and_chunk_across_collection() {
    let mut vm = setup();
    vm.eval(
        r#"
        let resume, seen, written;
        function write() {
          const stream = new WritableStream({
            start() { return new Promise(resolve => resume = resolve); },
            write(chunk) { seen = chunk.value; }
          });
          return stream.getWriter().write({value: 'alive'});
        }
        const pending = write();
        "#,
    )
    .unwrap();
    drain(&mut vm);
    vm.eval("pending.then(() => written = true); resume();")
        .unwrap();
    drain(&mut vm);
    assert_eq!(
        vm.eval("written && seen === 'alive'").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn constructor_converts_strategy_before_sink_and_keeps_thrown_values() {
    run(
        r#"
        const strategyFailure = {};
        const sinkFailure = {};
        let strategyError, sinkError, reads = 0;
        const sink = Object.defineProperty({}, 'start', {get: function() {
          reads++;
          throw sinkFailure;
        }});
        const strategy = Object.defineProperty({}, 'size', {get: function() {
          throw strategyFailure;
        }});
        try { new WritableStream(sink, strategy); } catch (error) { strategyError = error; }
        const strategyFirst = reads === 0;
        try { new WritableStream(sink); } catch (error) { sinkError = error; }
        "#,
        "strategyFirst && reads === 1 && strategyError === strategyFailure && sinkError === sinkFailure",
    );
}

#[test]
fn brands_writable_interfaces_and_keeps_standard_strategy_sizes() {
    run(
        r#"
        let rejected, badCall = false, illegalController = false, got;
        WritableStream.prototype.close.call({}).catch(e => rejected = e instanceof TypeError);
        try { new WritableStreamDefaultController(); } catch(e) { illegalController = e instanceof TypeError; }
        try { WritableStream.prototype.getWriter.call({}); } catch(e) { badCall = e instanceof TypeError; }
        const count = new CountQueuingStrategy({highWaterMark: 4});
        const bytes = new ByteLengthQueuingStrategy({highWaterMark: 2});
        const writer = new WritableStream({write(v) { got = v.byteLength; }}, bytes).getWriter();
        writer.write(new Uint8Array([1, 2]));
        const exact = writer.desiredSize === 0;
        "#,
        "got === 2 && exact && rejected && badCall && illegalController && count.size === new CountQueuingStrategy({highWaterMark: 3}).size && count.size() === 1 && count.highWaterMark === 4 && bytes.size.length === 1",
    );
}
