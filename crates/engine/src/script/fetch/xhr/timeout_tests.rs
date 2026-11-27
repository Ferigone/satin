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
fn send_accepts_a_blob_slice_and_keeps_its_content_type() {
    let mut rt = realm();
    rt.eval(r#"
        const xhr = new XMLHttpRequest();
        xhr.open("POST", "/echo");
        xhr.send(new Blob(["-measure-"]).slice(1, 8, "text/plain"));
    "#).unwrap();
    let job = rt.take_network_jobs().pop().unwrap();
    assert_eq!(job.request.body.as_deref().unwrap().as_slice(), b"measure");
    assert!(job.request.headers.iter().any(|(name, value)|
        name == "content-type" && value == "text/plain"));
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
fn timeout_idl_unsigned_long_truncates_then_wraps_and_normalizes_zero() {
    let mut rt = realm();
    assert_eq!(
        rt.eval(
            r#"
            var xhr=new XMLHttpRequest(), checked=0, failures=[];
            var cases=[
                [0.99,0], [17.99,17], [-0.99,0], [-1.99,4294967295],
                [4294967295.75,4294967295], [4294967296.75,0],
                [4294967297.75,1], [-4294967297.75,4294967295],
                [NaN,0], [Infinity,0], [-Infinity,0], [-0,0],
                [undefined,0], ['12.75',12]
            ];
            for (var i=0;i<cases.length;i++) {
                xhr.timeout=cases[i][0];
                var actual=xhr.timeout, expected=cases[i][1];
                if (actual===expected && (expected!==0 || 1/actual===Infinity)) checked++;
                else failures.push(i+':'+actual);
            }
            [checked,failures.join(',')].join('|');
            "#,
        )
        .unwrap(),
        Value::from("14|"),
    );
}

#[test]
fn timeout_idl_setter_checks_receiver_then_coerces_then_checks_current_mode() {
    let mut rt = realm();
    assert_eq!(
        rt.eval(
            r#"
            var xhr=new XMLHttpRequest(), trace=[], marker={};
            var setter=Object.getOwnPropertyDescriptor(XMLHttpRequest.prototype,'timeout').set;
            var zero={valueOf:function(){trace.push('coerced'); return -0.5;}};
            xhr.open('GET','/sync',false);
            try { setter.call({},zero); } catch(e) { trace.push('receiver:'+e.name); }
            try {
                xhr.timeout={valueOf:function(){trace.push('throwing'); throw marker;}};
            } catch(e) { trace.push('original:'+(e===marker)); }
            try { xhr.timeout=zero; } catch(e) { trace.push(e.name); }
            trace.push(xhr.timeout);

            // Coercion may reopen this XHR. The setter tests the resulting mode.
            xhr.timeout={valueOf:function(){
                xhr.open('GET','/async');
                return 7.9;
            }};
            trace.push(xhr.timeout);
            xhr.timeout=0;
            try {
                xhr.timeout={valueOf:function(){
                    xhr.open('GET','/sync-again',false);
                    return 3;
                }};
            } catch(e) { trace.push(e.name); }
            trace.push(xhr.timeout);
            trace.join('|');
            "#,
        )
        .unwrap(),
        Value::from(
            "receiver:TypeError|throwing|original:true|coerced|InvalidAccessError|0|7|InvalidAccessError|0"
        ),
    );
}

#[test]
fn timeout_idl_open_validates_arguments_before_rejecting_a_nonzero_sync_timeout() {
    let mut rt = realm();
    assert_eq!(
        rt.eval(
            r#"
            var xhr=new XMLHttpRequest(), trace=[], states=[], marker={};
            xhr.onreadystatechange=function(){states.push(xhr.readyState);};
            xhr.timeout=-1.9;
            try { xhr.open('bad method','/sync',false); } catch(e) { trace.push(e.name); }
            try { xhr.open('GET','http://[',false); } catch(e) { trace.push(e.name); }
            try {
                xhr.open('GET',{toString:function(){throw marker;}},false);
            } catch(e) { trace.push('original:'+(e===marker)); }
            try { xhr.open('GET','/sync',false); } catch(e) { trace.push(e.name); }
            trace.push(xhr.readyState,xhr.timeout,states.length);

            // Omission means async; explicit undefined is converted to false.
            xhr.open('GET','/async');
            try { xhr.open('GET','/sync',undefined); } catch(e) { trace.push(e.name); }
            trace.push(xhr.readyState,xhr.timeout);
            xhr.timeout=-0.99; // The failed open left the previous async mode intact.
            xhr.open('GET','/sync',false);
            try { xhr.timeout=NaN; } catch(e) { trace.push(e.name); }
            trace.push(xhr.readyState,xhr.timeout,states.join(','));
            trace.join('|');
            "#,
        )
        .unwrap(),
        Value::from(
            "SyntaxError|SyntaxError|original:true|InvalidAccessError|0|4294967295|0|InvalidAccessError|1|4294967295|InvalidAccessError|1|0|1"
        ),
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

#[test]
fn deadline_boundary_queued_eof_retires_or_moves_behind_unaffected_fifo_work() {
    for (at_deadline, mailbox_first) in [(false, true), (true, false), (true, true)] {
        let mut rt = realm();
        rt.eval(
            r#"
            var trace=[];
            function send(label, timeout) {
                var xhr=new XMLHttpRequest();
                xhr.onreadystatechange=function(){trace.push(label+xhr.readyState);};
                xhr.onload=function(){trace.push(label+':load:'+xhr.responseText);};
                xhr.ontimeout=function(){trace.push(label+':timeout:'+xhr.status);};
                xhr.onloadend=function(){trace.push(label+':end');};
                xhr.open('GET','/'+label); xhr.timeout=timeout; xhr.send();
            }
            send('a',1000); send('b',0);
            "#,
        )
        .unwrap();
        let jobs = rt.take_network_jobs();
        assert_eq!(jobs.len(), 2);
        let [a, b] = [jobs[0].id, jobs[1].id];
        let deadline = rt.fetch.xhr_deadline().unwrap();
        rt.complete_network(a, Ok(response("/a", "A"))).unwrap();
        rt.complete_network(b, Ok(response("/b", "B"))).unwrap();
        step_at(&mut rt, deadline - ms(2)); // A read; queue [B read, A EOF].
        step_at(&mut rt, deadline - ms(2)); // B read; queue [A EOF, B EOF].
        assert_eq!(
            rt.eval("trace.join(',')").unwrap(),
            Value::from("a1,b1,a2,b2,a3,b3")
        );

        let boundary = if at_deadline {
            deadline
        } else {
            deadline - ms(1)
        };
        set_now(&rt, boundary);
        if mailbox_first {
            // A progress wake cannot steal the local continuation. At the
            // deadline it latches timeout, but still does not dispatch DONE.
            rt.progress_network(a).unwrap();
            assert_eq!(rt.pending_network(), 2);
            assert_eq!(rt.eval("trace.length").unwrap(), Value::from(6));
            assert_eq!(
                jobs[0].request.signal.as_ref().unwrap().is_cancelled(),
                at_deadline
            );
        }
        step_at(&mut rt, boundary);
        assert_eq!(rt.pending_network(), 1);
        assert_eq!(
            rt.eval("trace.slice(6).join(',')").unwrap(),
            Value::from(if at_deadline {
                "b4,b:load:B,b:end"
            } else {
                "a4,a:load:A,a:end"
            })
        );
        step_at(&mut rt, deadline);
        assert_eq!(
            rt.eval("trace.slice(6).join(',')").unwrap(),
            Value::from(if at_deadline {
                "b4,b:load:B,b:end,a4,a:timeout:0,a:end"
            } else {
                "a4,a:load:A,a:end,b4,b:load:B,b:end"
            })
        );
        assert_eq!(rt.pending_network(), 0);
        assert_eq!(
            jobs[0].request.signal.as_ref().unwrap().is_cancelled(),
            at_deadline
        );
        assert!(!jobs[1].request.signal.as_ref().unwrap().is_cancelled());
    }
}

#[test]
fn deadline_boundary_mailbox_error_is_accepted_only_before_the_author_deadline() {
    for at_deadline in [false, true] {
        let mut rt = realm();
        rt.eval(
            r#"
            var xhr=new XMLHttpRequest(), trace=[];
            xhr.onreadystatechange=function(){trace.push('s'+xhr.readyState);};
            xhr.onerror=function(){trace.push('error:'+xhr.status);};
            xhr.ontimeout=function(){trace.push('timeout:'+xhr.status);};
            xhr.onloadend=function(){trace.push('end');};
            xhr.open('GET','/failure'); xhr.timeout=1000; xhr.send();
            "#,
        )
        .unwrap();
        let job = rt.take_network_jobs().pop().unwrap();
        let deadline = rt.fetch.xhr_deadline().unwrap();
        set_now(
            &rt,
            if at_deadline {
                deadline
            } else {
                deadline - ms(1)
            },
        );
        let error = || FetchError::new(FetchErrorKind::Network, "Fetch deadline exceeded");
        rt.complete_network(job.id, Err(error())).unwrap();
        assert_eq!(
            rt.eval("trace.join(',')").unwrap(),
            Value::from(if at_deadline {
                "s1"
            } else {
                "s1,s4,error:0,end"
            })
        );
        assert_eq!(rt.fetch.has_queued_xhr_tasks(), at_deadline);

        set_now(&rt, deadline);
        rt.complete_network(job.id, Err(error())).unwrap(); // Latched or stale.
        rt.pump_with_network_errors().unwrap();
        assert_eq!(rt.pending_network(), 0);
        assert_eq!(
            rt.eval("trace.join(',')").unwrap(),
            Value::from(if at_deadline {
                "s1,s4,timeout:0,end"
            } else {
                "s1,s4,error:0,end"
            })
        );
    }
}
