use rust_browser_engine::{
    LoadedPage, Loader,
    network::{CancelSignal, FETCH_TIMEOUT, ResponseType, WebResponse},
    script::ScriptRuntime,
};
use rust_browser_javascript::Value;
use std::{
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener},
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

fn serve(handler: impl FnOnce(String) -> Vec<u8> + Send + 'static) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut socket);
        let mut request = String::new();
        while !request.ends_with("\r\n\r\n") {
            assert_ne!(reader.read_line(&mut request).unwrap(), 0);
        }
        socket.write_all(&handler(request)).unwrap();
    });
    (address, server)
}

fn realm(address: &str, html: &str) -> ScriptRuntime {
    ScriptRuntime::new(&LoadedPage::from_html(address, html)).unwrap()
}

#[test]
fn synchronous_text_bootstrap_reads_filtered_response_before_send_returns() {
    let (address, server) = serve(|request| {
        let request = request.to_ascii_lowercase();
        assert!(request.starts_with("get /assets/bootstrap.txt?fresh=1 http/1.1\r\n"));
        assert!(request.contains("\r\nx-requested-with: xmlhttprequest\r\n"));
        assert!(request.contains("\r\naccept: text/plain, */*\r\n"));
        assert!(!request.contains("\r\ncookie:"));
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Type: text/plain; charset=windows-1252\r\n\
          X-Marker: one\r\nX-Marker: two\r\nSet-Cookie: secret=1\r\nConnection: close\r\n\r\ncaf\xe9!"
            .to_vec()
    });
    let mut runtime = realm(
        &format!("http://{address}/page/index.html"),
        "<base href='/assets/'>",
    );
    let actual = runtime
        .eval(
            r#"
            var xhr = new XMLHttpRequest(), states = [];
            xhr.onreadystatechange = function(event) {
                states.push(this.readyState + ':' + (event.target === xhr && event.isTrusted));
            };
            xhr.open('get', 'bootstrap.txt?fresh=1#ignored', false);
            xhr.setRequestHeader('X-Requested-With', 'XMLHttpRequest');
            xhr.setRequestHeader('Accept', ' text/plain ');
            xhr.setRequestHeader('aCcEpT', '*/*');
            xhr.setRequestHeader('Cookie', 'injected=1');
            xhr.send();
            [
                xhr instanceof EventTarget,
                xhr.readyState, xhr.status, xhr.statusText, xhr.responseText, xhr.response,
                xhr.getResponseHeader('X-MARKER'), xhr.getResponseHeader('set-cookie'),
                xhr.getAllResponseHeaders().indexOf('set-cookie') === -1,
                xhr.responseURL.slice(-29), states.join(',')
            ].join('|');
            "#,
        )
        .unwrap();
    assert_eq!(
        actual,
        Value::from(
            "true|4|200|OK|café!|café!|one, two||true|/assets/bootstrap.txt?fresh=1|1:true,4:true"
        )
    );
    server.join().unwrap();
}

#[test]
fn synchronous_cross_origin_requests_use_cors_header_filtering_and_default_credentials() {
    const PAGE: &str = "http://127.0.0.1:1/page";
    let (allowed, allowed_server) = serve(|request| {
        let headers = request.to_ascii_lowercase();
        assert!(headers.contains("\r\norigin: http://127.0.0.1:1\r\n"));
        assert!(!headers.contains("\r\ncookie:"));
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nAccess-Control-Allow-Origin: *\r\n\
          Access-Control-Expose-Headers: X-Public\r\nX-Public: shown\r\nX-Private: hidden\r\n\
          Connection: close\r\n\r\nok"
            .to_vec()
    });
    let (denied, denied_server) = serve(|_| {
        b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret".to_vec()
    });
    let mut runtime = realm(PAGE, "");
    runtime
        .eval(&format!(
            "var allowed = 'http://{allowed}/data', denied = 'http://{denied}/data';"
        ))
        .unwrap();
    let actual = runtime
        .eval(
            r#"
            var xhr = new XMLHttpRequest();
            xhr.open('GET', allowed, false);
            xhr.send();
            var allowedResult = [xhr.status, xhr.responseText, xhr.getResponseHeader('x-public'),
                xhr.getResponseHeader('x-private') === null];
            xhr.open('GET', denied, false);
            try { xhr.send(); } catch (error) { allowedResult.push(error.name); }
            allowedResult.concat([xhr.readyState, xhr.status, xhr.responseText,
                xhr.getAllResponseHeaders()]).join('|');
            "#,
        )
        .unwrap();
    assert_eq!(actual, Value::from("200|ok|shown|true|NetworkError|4|0||"));
    allowed_server.join().unwrap();
    denied_server.join().unwrap();
}

#[test]
fn synchronous_request_checks_the_document_policy_on_redirect() {
    let foreign = TcpListener::bind("127.0.0.1:0").unwrap();
    let foreign_address = foreign.local_addr().unwrap();
    let (address, server) = serve(move |_| {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{foreign_address}/never\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_bytes()
    });
    let mut runtime = realm(
        &format!("http://{address}/page"),
        r#"<meta http-equiv="Content-Security-Policy" content="connect-src 'self'">"#,
    );
    let actual = runtime
        .eval(
            r#"
            var xhr = new XMLHttpRequest();
            xhr.open('GET', '/redirect', false);
            var result = '';
            try { xhr.send(); } catch(error) { result = error.name; }
            [result, xhr.status, xhr.responseURL].join('|');
            "#,
        )
        .unwrap();
    assert_eq!(actual, Value::from("NetworkError|0|"));
    server.join().unwrap();
    foreign.set_nonblocking(true).unwrap();
    assert_eq!(
        foreign.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn asynchronous_text_uses_fetch_head_and_body_notifications() {
    let (address, server) = serve(|request| {
        assert!(request.starts_with("GET /manifest.json HTTP/1.1\r\n"));
        let body = br#"{"tests":[1,2]}"#;
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        [header.as_bytes(), body].concat()
    });
    let mut runtime = realm(&format!("http://{address}/page"), "");
    let before = runtime
        .eval(
            r#"
            var xhr = new XMLHttpRequest(), states = [], effects = [], tests;
            xhr.onreadystatechange = function() {
                states.push(xhr.readyState + ':' + xhr.status);
                if (xhr.readyState === 4) tests = JSON.parse(xhr.responseText).tests;
            };
            xhr.onloadstart = function(e) { effects.push('start:' + xhr.readyState); };
            xhr.onprogress = function(e) { effects.push('progress:' + e.loaded); };
            xhr.onload = function(e) { effects.push('load:' + (e instanceof ProgressEvent) + ':' +
                e.isTrusted + ':' + e.lengthComputable + ':' + e.total); };
            xhr.onloadend = function() { effects.push('end'); };
            xhr.open('GET', 'manifest.json');
            xhr.send();
            [xhr.readyState, xhr.status, xhr.responseText, effects.join(',')].join('|');
            "#,
        )
        .unwrap();
    assert_eq!(before, Value::from("1|0||start:1"));
    let job = runtime.take_network_jobs().pop().unwrap();
    let loader = Loader::new().unwrap();
    let cancel = CancelSignal::new();
    let (head, transfer) = loader
        .transport_handle()
        .block_on(loader.open_web_fetch(&job.request, &cancel, Instant::now() + FETCH_TIMEOUT))
        .unwrap();
    runtime.complete_fetch(job.id, Ok(head)).unwrap();
    assert_eq!(
        runtime
            .eval("[xhr.readyState,xhr.status,xhr.responseText].join('|')")
            .unwrap(),
        Value::from("2|200|")
    );
    let (notify, wakes) = mpsc::channel();
    loader
        .transport_handle()
        .spawn(transfer.unwrap().run(move || {
            let notify = notify.clone();
            async move {
                let _ = notify.send(());
            }
        }));
    for _ in 0..8 {
        if runtime.pending_network() == 0 {
            break;
        }
        wakes.recv_timeout(Duration::from_secs(5)).unwrap();
        runtime.progress_network(job.id).unwrap();
    }
    assert_eq!(runtime.pending_network(), 0);
    assert_eq!(
        runtime.eval(
            "[states.join(','),tests.join(','),effects[0],effects[effects.length-2],effects[effects.length-1]].join('|')"
        ).unwrap(),
        Value::from("1:0,2:200,3:200,4:200|1,2|start:1|load:true:true:true:15|end")
    );
    server.join().unwrap();
}

#[test]
fn async_abort_cancels_queued_or_in_flight_fetch_and_ignores_stale_results() {
    let mut runtime = realm("https://example.test/page", "");
    runtime
        .eval(
            r#"
            var xhr = new XMLHttpRequest(), events = [];
            xhr.onreadystatechange = function() { events.push('state:' + xhr.readyState); };
            xhr.onabort = function() { events.push('abort:' + xhr.status); };
            xhr.onloadend = function() { events.push('end:' + xhr.responseText); };
            xhr.open('GET', '/queued');
            xhr.send();
            xhr.abort();
            xhr.open('GET', '/old');
            xhr.send();
            "#,
        )
        .unwrap();
    let jobs = runtime.take_network_jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].request.url.path(), "/old");
    runtime
        .eval("xhr.abort(); xhr.open('GET', '/new'); xhr.send();")
        .unwrap();
    assert!(jobs[0].request.signal.as_ref().unwrap().is_cancelled());
    let replacement = runtime.take_network_jobs().pop().unwrap();
    let response = |text: &str| WebResponse {
        url: "https://example.test/new".into(),
        status: 200,
        status_text: "OK".into(),
        headers: vec![],
        body: Some(Arc::new(text.as_bytes().to_vec())),
        response_type: ResponseType::Basic,
        redirected: false,
    };
    runtime
        .complete_network(jobs[0].id, Ok(response("stale")))
        .unwrap();
    assert_eq!(runtime.eval("xhr.readyState").unwrap(), Value::from(1_u32));
    runtime
        .complete_network(replacement.id, Ok(response("new")))
        .unwrap();
    assert_eq!(
        runtime
            .eval("events.join(',')+'|'+xhr.responseText")
            .unwrap(),
        Value::from(
            "state:1,state:4,abort:0,end:,state:1,state:4,abort:0,end:,state:1,state:2,state:3,state:4,end:new|new"
        )
    );
    assert_eq!(runtime.pending_network(), 0);
}

#[test]
fn asynchronous_cors_denial_reaches_done_and_fires_error_without_exposing_response() {
    let (address, server) = serve(|_| {
        b"HTTP/1.1 200 OK\r\nX-Secret: token\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret"
            .to_vec()
    });
    let mut runtime = realm("http://127.0.0.1:1/page", "");
    runtime
        .eval(&format!(
            r#"
            var xhr = new XMLHttpRequest(), events = [];
            xhr.onreadystatechange = function() {{ events.push(xhr.readyState); }};
            xhr.onerror = function(e) {{ events.push(e.type + ':' + e.loaded); }};
            xhr.onload = function() {{ events.push('load'); }};
            xhr.onloadend = function() {{ events.push('end'); }};
            xhr.open('GET', 'http://{address}/other-origin');
            xhr.send();
            "#,
        ))
        .unwrap();
    let job = runtime.take_network_jobs().pop().unwrap();
    let loader = Loader::new().unwrap();
    let cancel = CancelSignal::new();
    let result = loader.transport_handle().block_on(loader.open_web_fetch(
        &job.request,
        &cancel,
        Instant::now() + FETCH_TIMEOUT,
    ));
    let Err(error) = result else {
        panic!("the shared Fetch transport should deny the response");
    };
    runtime.complete_fetch(job.id, Err(error)).unwrap();
    assert_eq!(
        runtime
            .eval("[events.join(','),xhr.status,xhr.responseText,xhr.getResponseHeader('x-secret')].join('|')")
            .unwrap(),
        Value::from("1,4,error:0,end|0||")
    );
    assert_eq!(runtime.pending_network(), 0);
    server.join().unwrap();
}
