use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;
use std::time::{Duration, Instant};

fn runtime() -> ScriptRuntime {
    ScriptRuntime::new(&LoadedPage::from_html(
        "https://example.test/clock",
        "<!doctype html><title>Clock</title>",
    ))
    .unwrap()
}

fn run_until(runtime: &mut ScriptRuntime, expression: &str) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        runtime.pump();
        if runtime.eval(expression).unwrap() == Value::Bool(true) {
            return;
        }
        let next = runtime.next_deadline().expect("callback still pending");
        assert!(
            next <= deadline,
            "{expression}: {:?}",
            runtime.diagnostics()
        );
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}

#[test]
fn intervals_cancel_during_callbacks_and_string_timers_share_window_scope() {
    let mut runtime = runtime();
    runtime
        .eval(
            r#"
      var log = [], counter = 0, stringTick = 0;
      const timer = setInterval(function(arg){
        log.push(arg + (++counter));
        queueMicrotask(() => log.push('micro'));
        if (counter === 3) clearTimeout(timer);
      }, 0, 'tick');
      const doomed = setTimeout(() => log.push('bad'), 0);
      clearInterval(doomed);
      setTimeout('stringTick = 11', 0);
    "#,
        )
        .unwrap();
    run_until(&mut runtime, "counter === 3 && stringTick === 11");
    assert_eq!(
        runtime.eval("log.join(',')").unwrap(),
        Value::from("tick1,micro,tick2,micro,tick3,micro")
    );
    assert!(runtime.next_deadline().is_none());
    assert!(runtime.diagnostics().is_empty());
}

#[test]
fn frame_callbacks_share_timestamps_cancel_within_batches_and_wait_for_foreground() {
    let mut runtime = runtime();
    runtime.set_foreground(false);
    runtime
        .eval(
            r#"
      var trace = [], stamps = [];
      requestAnimationFrame(t => {
        trace.push('first');
        stamps.push(t);
        cancelAnimationFrame(removed);
        queueMicrotask(() => trace.push('micro'));
        requestAnimationFrame(t => { trace.push('next'); stamps.push(t); });
      });
      const removed = requestAnimationFrame(() => trace.push('cancelled'));
      requestAnimationFrame(t => { trace.push('second'); stamps.push(t); });
    "#,
        )
        .unwrap();
    runtime.pump();
    assert_eq!(runtime.eval("trace.length").unwrap(), Value::from(0));
    assert!(runtime.next_deadline().is_none());
    runtime.set_foreground(true);
    run_until(&mut runtime, "stamps.length === 3");
    assert_eq!(
        runtime
            .eval(
                r#"
      trace.join(',') === 'first,micro,second,next' &&
      stamps[0] === stamps[1] && stamps[2] > stamps[0] &&
      performance instanceof Performance && performance instanceof EventTarget &&
      performance.now() >= stamps[2] &&
      Math.abs(Date.now() - performance.timeOrigin - performance.now()) < 1000
    "#
            )
            .unwrap(),
        Value::Bool(true)
    );
    assert!(runtime.next_deadline().is_none());
    assert!(runtime.diagnostics().is_empty());
}

#[test]
fn user_timing_marks_measures_and_clears_ordered_entries() {
    let mut runtime = runtime();
    assert_eq!(
        runtime
            .eval(
                r#"
            const before = performance.now();
            const boot = performance.mark('boot');
            const source = { nested: { count: 1 } };
            const first = performance.mark('checkpoint', { startTime: 8, detail: source });
            const early = performance.mark('checkpoint', { startTime: 2 });
            const last = performance.mark('ready', { startTime: 14 });
            const standalone = new PerformanceMark('standalone', { startTime: 5 });
            source.nested.count = 99;
            const elapsed = performance.measure('elapsed', 'checkpoint', 'ready');
            const ranged = performance.measure('ranged', { end: 16, duration: 4 });
            const navigation = performance.measure('navigation', 'navigationStart');
            let found = performance.getEntriesByName('checkpoint', 'mark');
            const ordered = performance.getEntries().every((item, index, entries) =>
                index === 0 || item.startTime >= entries[index - 1].startTime);
            performance.clearMarks('checkpoint');
            performance.clearMeasures('elapsed');
            ordered && boot.startTime >= before && boot.startTime <= performance.now() &&
                first instanceof PerformanceMark && first instanceof PerformanceEntry &&
                elapsed instanceof PerformanceMeasure && elapsed instanceof PerformanceEntry &&
                Object.getPrototypeOf(PerformanceMark) === PerformanceEntry &&
                Object.getPrototypeOf(PerformanceMeasure) === PerformanceEntry &&
                standalone.detail === null && standalone.duration === 0 &&
                performance.getEntriesByName('standalone').length === 0 &&
                found.length === 2 && found[0] === early && found[1] === first &&
                first.detail !== source && first.detail.nested.count === 1 &&
                first.toJSON().entryType === 'mark' && first.toJSON().startTime === 8 &&
                elapsed.name === 'elapsed' && elapsed.startTime === 8 && elapsed.duration === 6 &&
                ranged.startTime === 12 && ranged.duration === 4 && navigation.startTime === 0 &&
                performance.getEntriesByName('checkpoint').length === 0 &&
                performance.getEntriesByType('mark').length === 2 &&
                performance.getEntriesByType('measure').length === 2 && last.detail === null
        "#
            )
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn user_timing_validates_operations_and_uses_the_intrinsic_clone() {
    let mut runtime = runtime();
    assert_eq!(
        runtime.eval(r#"
            function throws(fn, name) {
                try { fn(); } catch (error) { return error.name === name; }
                return false;
            }
            structuredClone = null;
            const original = { mutable: 1 };
            const measurement = performance.measure('measure', { start: 0, end: 1, detail: original });
            const mark = performance.mark('reentrant', { detail: {
                get value() { performance.clearMeasures(); return 20; }
            } });
            measurement.detail.mutable === 1 && measurement.detail !== original &&
                mark.detail.value === 20 && performance.getEntriesByType('measure').length === 0 &&
                throws(() => performance.mark('navigationStart'), 'SyntaxError') &&
                throws(() => performance.mark('negative', { startTime: -1 }), 'TypeError') &&
                throws(() => performance.mark('function', { detail: () => {} }), 'DataCloneError') &&
                throws(() => performance.measure('missing', 'not-marked'), 'SyntaxError') &&
                throws(() => performance.measure('invalid', { duration: 3 }), 'TypeError') &&
                throws(() => performance.measure('invalid', { start: 0, end: 7, duration: 3 }), 'TypeError') &&
                throws(() => Performance.prototype.mark.call({}, 'bad receiver'), 'TypeError') &&
                throws(() => new PerformanceMeasure(), 'TypeError') &&
                performance.getEntriesByType('mark').length === 1
        "#).unwrap(),
        Value::Bool(true)
    );
}
