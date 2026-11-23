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
