//! Synthetic owner-clock controls: no sleeping, sockets, or timer polling.
use crate::{
    LoadedPage,
    network::{FetchError, FetchErrorKind, ResponseType, WebResponse},
    script::ScriptRuntime,
};
use rust_browser_javascript::Value;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn realm() -> ScriptRuntime {
    let runtime =
        ScriptRuntime::new(&LoadedPage::from_html("https://example.test/page", "")).unwrap();
    set_now(&runtime, Instant::now());
    runtime
}

fn set_now(runtime: &ScriptRuntime, now: Instant) {
    runtime.fetch.xhr.as_ref().unwrap().test_now.set(Some(now));
}

fn ms(delta: u64) -> Duration {
    Duration::from_millis(delta)
}

fn step_at(runtime: &mut ScriptRuntime, now: Instant) {
    set_now(runtime, now);
    runtime
        .network_task(crate::script::fetch::FetchHost::run_next_xhr_task, false)
        .unwrap();
}

fn response(path: &str, text: &str) -> WebResponse {
    WebResponse {
        url: format!("https://example.test{path}"),
        status: 200,
        status_text: "OK".into(),
        headers: vec![("content-type".into(), "text/plain".into())],
        body: Some(Arc::new(text.as_bytes().to_vec())),
        response_type: ResponseType::Basic,
        redirected: false,
    }
}

#[test]
fn timeout_setter_retains_the_original_start_and_zero_disables_an_armed_deadline() {
    let mut rt = realm();
    rt.eval(
        "var xhr=new XMLHttpRequest(); xhr.timeout=300000;\
         xhr.onloadstart=function(){xhr.timeout=600000;};\
         xhr.open('GET','/wait'); xhr.send();",
    )
    .unwrap();
    let job = rt.take_network_jobs().pop().unwrap();
    let first = rt.fetch.xhr_deadline().unwrap();
    assert_eq!(rt.eval("xhr.timeout").unwrap(), Value::from(600_000));
    assert_eq!(rt.next_deadline(), Some(first));
    rt.eval("xhr.timeout=900000;").unwrap();
    assert_eq!(rt.next_deadline(), Some(first + ms(300_000)));
    step_at(&mut rt, first);
    assert_eq!(rt.pending_network(), 1);
    assert!(!job.request.signal.as_ref().unwrap().is_cancelled());
    rt.eval("xhr.timeout=700000;").unwrap();
    let shortened = first + ms(100_000);
    assert_eq!(rt.fetch.xhr_deadline(), Some(shortened));
    rt.eval("xhr.timeout=0;").unwrap();
    assert!(rt.fetch.xhr_deadline().is_none());
    step_at(&mut rt, shortened + ms(1));
    assert_eq!(rt.pending_network(), 1);

    rt.eval("xhr.timeout=800000;").unwrap();
    let rearmed = first + ms(200_000);
    assert_eq!(rt.fetch.xhr_deadline(), Some(rearmed));
    assert!(!rt.fetch.has_queued_xhr_tasks());
    set_now(&rt, rearmed - ms(1));
    rt.pump_with_network_errors().unwrap();
    assert_eq!(rt.eval("xhr.readyState").unwrap(), Value::from(1));
    set_now(&rt, rearmed);
    rt.pump_with_network_errors().unwrap();
    assert_eq!(rt.eval("xhr.readyState").unwrap(), Value::from(4));
    assert!(job.request.signal.as_ref().unwrap().is_cancelled());
    assert_eq!(rt.pending_network(), 0);
}

#[test]
fn timeout_during_loading_dispatches_done_timeout_loadend_once() {
    let mut rt = realm();
    rt.eval(
        r#"
        var xhr=new XMLHttpRequest(), trace=[];
        xhr.onreadystatechange=function(){trace.push('s'+xhr.readyState);};
        xhr.onload=function(){trace.push('unwanted load');};
        xhr.onerror=function(){trace.push('unwanted error');};
        xhr.onabort=function(){trace.push('unwanted abort');};
        function end(event) {
            trace.push([event.type,xhr.readyState,xhr.status,event.loaded,event.total,
                event.lengthComputable,event.isTrusted,xhr.responseText,xhr.responseURL,
                xhr.getAllResponseHeaders(),xhr.statusText,xhr.responseXML===null].join(':'));
        }
        xhr.ontimeout=end; xhr.onloadend=end;
        xhr.open('GET','/body'); xhr.timeout=300000; xhr.send();
    "#,
    )
    .unwrap();
    let job = rt.take_network_jobs().pop().unwrap();
    let deadline = rt.fetch.xhr_deadline().unwrap();
    rt.complete_network(job.id, Ok(response("/body", "partial")))
        .unwrap();
    step_at(&mut rt, deadline - ms(1));
    assert_eq!(rt.eval("xhr.responseText").unwrap(), Value::from("partial"));
    assert_eq!(rt.pending_network(), 1);

    set_now(&rt, deadline);
    rt.pump_with_network_errors().unwrap();
    rt.progress_network(job.id).unwrap();
    rt.complete_network(job.id, Ok(response("/body", "stale")))
        .unwrap();
    step_at(&mut rt, deadline + ms(1));
    assert!(job.request.signal.as_ref().unwrap().is_cancelled());
    assert!(rt.fetch.xhr_deadline().is_none());
    assert_eq!(rt.pending_network(), 0);
    assert_eq!(
        rt.eval("trace.join(',')").unwrap(),
        Value::from(
            "s1,s2,s3,s4,timeout:4:0:0:0:false:true:::::true,loadend:4:0:0:0:false:true:::::true"
        ),
    );
}

#[test]
fn timeout_cannot_be_rescinded_after_expiry_and_abort_reopen_retires_its_id() {
    let mut rt = realm();
    rt.eval(
        r#"
        var xhr=new XMLHttpRequest(), trace=[];
        xhr.ontimeout=function(){trace.push('timeout');};
        xhr.onabort=function(){trace.push('abort:'+xhr.readyState);};
        xhr.onload=function(){trace.push('load:'+xhr.responseText);};
        xhr.onloadend=function(){trace.push('end:'+xhr.status);};
        xhr.open('GET','/old'); xhr.timeout=300000; xhr.send();
    "#,
    )
    .unwrap();
    let old = rt.take_network_jobs().pop().unwrap();
    let deadline = rt.fetch.xhr_deadline().unwrap();
    // Exercise the same setter scheduling path with an explicit clock:
    // attempting to disable after expiry latches/cancels but runs no JS.
    set_now(&rt, deadline);
    rt.eval("xhr.timeout=0;").unwrap();
    assert!(old.request.signal.as_ref().unwrap().is_cancelled());
    assert!(rt.fetch.has_queued_xhr_tasks());
    assert_eq!(
        rt.eval("[xhr.readyState,trace.length].join('|')").unwrap(),
        Value::from("1|0")
    );
    rt.eval("xhr.abort(); xhr.open('GET','/new'); xhr.timeout=600000; xhr.send();")
        .unwrap();
    let new = rt.take_network_jobs().pop().unwrap();
    assert_ne!(old.id, new.id);
    rt.complete_network(old.id, Ok(response("/old", "stale")))
        .unwrap();
    rt.progress_network(old.id).unwrap();
    step_at(&mut rt, deadline + ms(1));
    assert_eq!(rt.pending_network(), 1);

    let new_deadline = rt.fetch.xhr_deadline().unwrap();
    rt.complete_network(new.id, Ok(response("/new", "fresh")))
        .unwrap();
    step_at(&mut rt, new_deadline - ms(2));
    step_at(&mut rt, new_deadline - ms(1));
    step_at(&mut rt, new_deadline + ms(1));
    assert!(!new.request.signal.as_ref().unwrap().is_cancelled());
    assert_eq!(rt.pending_network(), 0);
    assert_eq!(
        rt.eval("trace.join(',')").unwrap(),
        Value::from("abort:4,end:0,load:fresh,end:200"),
    );
}

#[test]
fn timeout_idl_defaults_conversion_and_synchronous_window_restrictions() {
    let mut rt = realm();
    assert_eq!(
        rt.eval(
            r#"
            var xhr=new XMLHttpRequest(), result=[xhr.timeout,xhr.ontimeout===null];
            xhr.timeout=-1; result.push(xhr.timeout);
            try { xhr.open('GET','/sync',false); } catch (error) { result.push(error.name); }
            xhr.open('GET','/async'); result.push(xhr.timeout,xhr.readyState);
            xhr.timeout=NaN; xhr.open('GET','/sync',false);
            try { xhr.timeout=0; } catch (error) { result.push(error.name); }
            result.join('|');
        "#
        )
        .unwrap(),
        Value::from("0|true|4294967295|InvalidAccessError|4294967295|1|InvalidAccessError"),
    );
}

#[test]
fn shortening_below_elapsed_time_defers_timeout_and_reopen_can_retire_it() {
    for reopen in [false, true] {
        let mut rt = realm();
        rt.eval(
            r#"
            var xhr=new XMLHttpRequest(), trace=[];
            xhr.ontimeout=function(){trace.push('timeout:'+xhr.readyState);};
            xhr.onabort=function(){trace.push('abort');};
            xhr.open('GET','/a'); xhr.timeout=1000; xhr.send();
        "#,
        )
        .unwrap();
        let old = rt.take_network_jobs().pop().unwrap();
        let now = rt.fetch.xhr_deadline().unwrap() - ms(500);
        set_now(&rt, now);
        rt.eval("xhr.timeout=100; xhr.timeout=2000;").unwrap();
        assert_eq!(rt.eval("xhr.readyState").unwrap(), Value::from(1));
        assert_eq!(rt.eval("trace.length").unwrap(), Value::from(0));
        assert!(old.request.signal.as_ref().unwrap().is_cancelled());

        if reopen {
            rt.eval("xhr.open('GET','/b'); xhr.send();").unwrap();
            let new = rt.take_network_jobs().pop().unwrap();
            assert_eq!(rt.next_deadline(), Some(now + ms(2000)));
            rt.complete_network(old.id, Ok(response("/a", "stale")))
                .unwrap();
            rt.complete_network(new.id, Ok(response("/b", "ok")))
                .unwrap();
        }
        rt.pump_with_network_errors().unwrap();
        assert_eq!(rt.pending_network(), 0);
        assert_eq!(
            rt.eval("[trace.join(','),xhr.readyState,xhr.responseText].join('|')")
                .unwrap(),
            Value::from(if reopen { "|4|ok" } else { "timeout:4|4|" }),
        );
    }
}

#[test]
fn timeout_is_distinct_from_a_transport_deadline_before_the_author_deadline() {
    let mut rt = realm();
    rt.eval(
        r#"
        var xhr=new XMLHttpRequest(), trace=[];
        xhr.ontimeout=function(){trace.push('timeout');};
        xhr.onerror=function(){trace.push('error:'+xhr.readyState);};
        xhr.onloadend=function(){trace.push('end:'+xhr.status);};
        xhr.open('GET','/safety'); xhr.timeout=300000; xhr.send();
    "#,
    )
    .unwrap();
    let job = rt.take_network_jobs().pop().unwrap();
    let deadline = rt.fetch.xhr_deadline().unwrap();
    set_now(&rt, deadline - ms(1));
    rt.complete_network(
        job.id,
        Err(FetchError::new(
            FetchErrorKind::Network,
            "Fetch deadline exceeded",
        )),
    )
    .unwrap();
    step_at(&mut rt, deadline);
    assert_eq!(rt.pending_network(), 0);
    assert_eq!(
        rt.eval("trace.join(',')").unwrap(),
        Value::from("error:4,end:0")
    );
}

#[test]
fn a_late_mailbox_head_defers_timeout_without_delivering_headers() {
    let mut rt = realm();
    rt.eval(
        r#"
        var xhr=new XMLHttpRequest(), trace=[];
        xhr.onreadystatechange=function(){trace.push('s'+xhr.readyState);};
        xhr.ontimeout=function(){trace.push('timeout');};
        xhr.onloadend=function(){trace.push('end');};
        xhr.open('GET','/head'); xhr.timeout=1000; xhr.send();
    "#,
    )
    .unwrap();
    let job = rt.take_network_jobs().pop().unwrap();
    let deadline = rt.fetch.xhr_deadline().unwrap();
    set_now(&rt, deadline);
    rt.complete_network(job.id, Ok(response("/head", "late")))
        .unwrap();
    rt.progress_network(job.id).unwrap();
    assert!(job.request.signal.as_ref().unwrap().is_cancelled());
    assert_eq!(rt.eval("trace.join(',')").unwrap(), Value::from("s1"));
    rt.pump_with_network_errors().unwrap();
    assert_eq!(rt.pending_network(), 0);
    assert_eq!(
        rt.eval("trace.join(',')").unwrap(),
        Value::from("s1,s4,timeout,end")
    );
}
