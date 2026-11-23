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
    let (notify, wakes) = mpsc::channel();
    loader
        .transport_handle()
        .spawn(transfer.unwrap().run(move || {
            let notify = notify.clone();
            async move {
                let _ = notify.send(());
            }
        }));
    // Deliberately wait for data to be present before delivering the head.
    // Header handling must not consume an already-ready pipe.
    wakes.recv_timeout(Duration::from_secs(5)).unwrap();
    runtime.complete_fetch(job.id, Ok(head)).unwrap();
    assert_eq!(
        runtime
            .eval("[xhr.readyState,xhr.status,xhr.responseText].join('|')")
            .unwrap(),
        Value::from("2|200|")
    );
    runtime.progress_network(job.id).unwrap();
    assert_eq!(runtime.eval("xhr.readyState").unwrap(), Value::from(3));
    runtime.pump_with_network_errors().unwrap();
    for _ in 0..8 {
        if runtime.pending_network() == 0 {
            break;
        }
        wakes.recv_timeout(Duration::from_secs(5)).unwrap();
        runtime.progress_network(job.id).unwrap();
        runtime.pump_with_network_errors().unwrap();
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
    runtime.pump_with_network_errors().unwrap();
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

fn xml_reply(mime: Option<&str>, body: &[u8]) -> Vec<u8> {
    let typed = mime.map_or(String::new(), |value| format!("Content-Type: {value}\r\n"));
    let headers = format!(
        "HTTP/1.1 200 OK\r\n{typed}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    [headers.as_bytes(), body].concat()
}

#[test]
fn response_xml_is_an_inert_same_origin_document_with_case_sensitive_namespaces() {
    let observer = TcpListener::bind("127.0.0.1:0").unwrap();
    let foreign = observer.local_addr().unwrap();
    let (address, server) = serve(move |request| {
        assert!(request.starts_with("GET /payload.xml HTTP/1.1\r\n"));
        let body = format!(
            r#"<?xml version="1.0"?><?xml-stylesheet href="http://{foreign}/never.css"?>
<Root xmlns:p="urn:example" xmlns:q="urn:example" Key="yes" key="no">
  <q:Sec p:ID="abc">T&amp;T</q:Sec>
  <script xmlns="http://www.w3.org/1999/xhtml">window.ran = true;</script>
  <script xmlns="http://www.w3.org/1999/xhtml"
    src="http://{foreign}/never.js"/>
  <img xmlns="http://www.w3.org/1999/xhtml" src="http://{foreign}/never.png"/>
</Root>"#
        );
        xml_reply(
            Some("Application/Example+XML; charset=UTF-8"),
            body.as_bytes(),
        )
    });
    let mut runtime = realm(&format!("http://{address}/page"), "");
    let actual = runtime
        .eval(
            r#"
        var ran = false, xhr = new XMLHttpRequest();
        xhr.open('GET', '/payload.xml', false);
        var beforeSend = xhr.responseXML === null;
        xhr.send();
        var doc = xhr.responseXML, root = doc.documentElement, sec, count = 0;
        root.childNodes.forEach(function(node) {
            if (node.nodeName === 'q:Sec') { sec = node; count++; }
        });
        [
            beforeSend, xhr.status, doc === xhr.responseXML, doc.contentType,
            doc.defaultView === null, doc.nodeType, root.parentNode === doc,
            doc.URL.slice(-12), root.nodeName, root.localName, root.namespaceURI === null,
            root.getAttribute('Key'), root.getAttribute('key'), root.getAttribute('KEY') === null,
            root.getAttribute('xmlns:q'), sec.nodeName, sec.prefix, sec.localName, sec.namespaceURI,
            sec.getAttribute('p:ID'), sec.getAttributeNS('urn:example', 'ID'),
            sec.getAttributeNS('urn:example', 'id') === null, sec.textContent,
            sec.ownerDocument === doc, root.childNodes instanceof NodeList,
            root.childNodes === root.childNodes, root.childNodes.item(1) === sec,
            root.childNodes[1] === sec, count, ran
        ].join('|');
    "#,
        )
        .unwrap();
    assert_eq!(
        actual,
        Value::from(
            "true|200|true|application/example+xml|true|9|true|/payload.xml|Root|Root|true|yes|no|true|urn:example|q:Sec|q|Sec|urn:example|abc|abc|true|T&T|true|true|true|true|true|1|false"
        )
    );
    assert_eq!(runtime.pending_network(), 0);
    assert!(runtime.take_network_jobs().is_empty());
    server.join().unwrap();
    observer.set_nonblocking(true).unwrap();
    assert_eq!(
        observer.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn response_xml_rejects_non_xml_mime_malformed_xml_dtd_and_excessive_input() {
    let observer = TcpListener::bind("127.0.0.1:0").unwrap();
    let foreign = observer.local_addr().unwrap();
    let too_many_attrs = (0..129).map(|i| format!(" a{i}=''")).collect::<String>();
    let too_many_nodes = format!("<root>{}</root>", "<a/>".repeat(4096));
    let too_many_bytes = format!("<root>{}</root>", "x".repeat(256 * 1024));
    let cases: Vec<(Option<&str>, Vec<u8>)> =
        vec![
        (Some("text/plain"), b"<Root/>".to_vec()),
        (Some("text/html"), b"<Root/>".to_vec()),
        (Some("application/xml"), b"<Root></root>".to_vec()),
        (Some("application/xml"), b"<p:Root/>".to_vec()),
        (Some("application/xml"), format!(
            r#"<!DOCTYPE Root [<!ENTITY e SYSTEM "http://{foreign}/never">]><Root>&e;</Root>"#
        ).into_bytes()),
        (Some("application/xml"), format!(
            r#"<!DOCTYPE Root SYSTEM "http://{foreign}/never"><Root/>"#
        ).into_bytes()),
        (None, format!("{}{}", "<a>".repeat(65), "</a>".repeat(65)).into_bytes()),
        (None, format!("<root{too_many_attrs}/>").into_bytes()),
        (None, too_many_nodes.into_bytes()),
        (None, too_many_bytes.into_bytes()),
    ];
    for (mime, body) in cases {
        let (address, server) = serve(move |_| xml_reply(mime, &body));
        let mut runtime = realm(&format!("http://{address}/page"), "");
        assert_eq!(
            runtime
                .eval(
                    r#"
                var xhr = new XMLHttpRequest();
                xhr.open('GET', '/payload', false);
                xhr.send();
                [xhr.readyState, xhr.status,
                    xhr.responseXML === null, xhr.responseXML === null, xhr.responseText.length > 0
                ].join('|');
                "#
                )
                .unwrap(),
            Value::from("4|200|true|true|true"),
            "{mime:?}",
        );
        assert!(runtime.take_network_jobs().is_empty());
        server.join().unwrap();
    }
    observer.set_nonblocking(true).unwrap();
    assert_eq!(
        observer.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn response_xml_uses_default_xml_mime_and_utf16_byte_order_mark() {
    let mut body = vec![0xff, 0xfe];
    for unit in "<?xml version='1.0' encoding='UTF-16'?><Root>é</Root>".encode_utf16() {
        body.extend_from_slice(&unit.to_le_bytes());
    }
    let (address, server) = serve(move |_| xml_reply(None, &body));
    let mut runtime = realm(&format!("http://{address}/page"), "");
    assert_eq!(
        runtime
            .eval(
                r#"
            var xhr = new XMLHttpRequest();
            xhr.open('GET', '/payload', false);
            xhr.send();
            var doc = xhr.responseXML;
            var result = [doc.contentType, doc.characterSet, doc.documentElement.textContent];
            xhr.open('GET', '/reset');
            result.concat([xhr.responseXML === null, xhr.responseText]).join('|');
            "#
            )
            .unwrap(),
        Value::from("text/xml|UTF-16LE|é|true|"),
    );
    server.join().unwrap();
}

#[test]
fn response_xml_remains_null_for_an_allowed_cross_origin_xml_response() {
    let (address, server) = serve(|request| {
        assert!(
            request
                .to_ascii_lowercase()
                .contains("\r\norigin: http://127.0.0.1:1\r\n")
        );
        b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Type: application/xml\r\n\
          Content-Length: 7\r\nConnection: close\r\n\r\n<Root/>"
            .to_vec()
    });
    let mut runtime = realm("http://127.0.0.1:1/page", "");
    assert_eq!(
        runtime.eval(&format!(
            "var xhr = new XMLHttpRequest(); xhr.open('GET', 'http://{address}/payload.xml', false); xhr.send(); [xhr.status,xhr.responseText,xhr.responseXML===null].join('|');"
        )).unwrap(),
        Value::from("200|<Root/>|true")
    );
    server.join().unwrap();
}

#[test]
fn response_xml_only_materializes_at_done_on_async_completion() {
    let mut runtime = realm("https://example.test/page", "");
    runtime
        .eval(
            r#"
        var xhr = new XMLHttpRequest(), states = [];
        xhr.onreadystatechange = function() {
            states.push(xhr.readyState === 4 ? xhr.responseXML.documentElement.nodeName :
                xhr.responseXML === null);
        };
        xhr.open('GET', '/payload.xml');
        xhr.send();
    "#,
        )
        .unwrap();
    let job = runtime.take_network_jobs().pop().unwrap();
    runtime
        .complete_network(
            job.id,
            Ok(WebResponse {
                url: "https://example.test/payload.xml".into(),
                status: 200,
                status_text: "OK".into(),
                headers: vec![("content-type".into(), "application/xml".into())],
                body: Some(Arc::new(b"<CaseSensitive/>".to_vec())),
                response_type: ResponseType::Basic,
                redirected: false,
            }),
        )
        .unwrap();
    runtime.pump_with_network_errors().unwrap();
    assert_eq!(
        runtime.eval("states.join('|')").unwrap(),
        Value::from("true|true|true|CaseSensitive"),
    );
}

fn buffered_response(path: &str, body: &[u8]) -> WebResponse {
    WebResponse {
        url: format!("https://example.test{path}"),
        status: 200,
        status_text: "OK".into(),
        headers: vec![("content-type".into(), "text/plain".into())],
        body: Some(Arc::new(body.to_vec())),
        response_type: ResponseType::Basic,
        redirected: false,
    }
}

#[test]
fn xhr_headers_body_and_eof_have_separate_tasks_and_microtask_checkpoints() {
    let mut runtime = realm("https://example.test/page", "");
    runtime
        .eval(
            r#"
        var xhr = new XMLHttpRequest(), trace = [];
        xhr.onreadystatechange = function() {
            var state = xhr.readyState;
            trace.push('S' + state);
            if (state > 1) {
                // Each real task needs 600k work units, which fits by
                // itself but two stages must not share that budget.
                'x'.repeat(300000);
                Promise.resolve().then(function() {
                    trace.push('M' + state + '@' + xhr.readyState);
                });
            }
        };
        xhr.onload = function() { trace.push('load:' + xhr.responseText); };
        xhr.onloadend = function() { trace.push('end'); };
        xhr.open('GET', '/phases');
        xhr.send();
    "#,
        )
        .unwrap();
    let id = runtime.take_network_jobs().pop().unwrap().id;
    runtime
        .complete_network(id, Ok(buffered_response("/phases", b"abc")))
        .unwrap();
    assert_eq!(
        runtime.eval("trace.join(',')").unwrap(),
        Value::from("S1,S2,M2@2")
    );
    assert_eq!(runtime.pending_network(), 1);
    assert!(runtime.next_deadline().is_some());
    runtime.pump_with_network_errors().unwrap();
    assert_eq!(
        runtime.eval("trace.join(',')").unwrap(),
        Value::from("S1,S2,M2@2,S3,M3@3,S4,load:abc,end,M4@4"),
    );
    assert_eq!(runtime.pending_network(), 0);
}

#[test]
fn xhr_done_and_load_callbacks_still_share_one_fuel_budget() {
    let mut runtime = realm("https://example.test/page", "");
    runtime
        .eval(
            r#"
        var xhr = new XMLHttpRequest(), trace = [];
        xhr.onreadystatechange = function() {
            if (xhr.readyState === 4) {
                'x'.repeat(300000);
                trace.push('done');
            }
        };
        xhr.onload = function() { 'x'.repeat(300000); trace.push('unexpected load'); };
        xhr.onloadend = function() { trace.push('unexpected end'); };
        xhr.open('GET', '/end');
        xhr.send();
    "#,
        )
        .unwrap();
    let id = runtime.take_network_jobs().pop().unwrap().id;
    runtime
        .complete_network(id, Ok(buffered_response("/end", b"abc")))
        .unwrap();
    let error = runtime.pump_with_network_errors().unwrap_err();
    assert_eq!(
        error.kind,
        rust_browser_javascript::ErrorKind::ExecutionLimit
    );
    assert_eq!(
        runtime
            .eval("[xhr.readyState,trace.join(',')].join('|')")
            .unwrap(),
        Value::from("4|done")
    );
    assert_eq!(runtime.pending_network(), 0);
}

#[test]
fn xhr_body_checkpoint_can_abort_queued_eof_and_reopen_without_stale_load() {
    let mut runtime = realm("https://example.test/page", "");
    runtime
        .eval(
            r#"
        var xhr = new XMLHttpRequest(), trace = [], first = true;
        xhr.onreadystatechange = function() {
            if (xhr.readyState === 3 && first) {
                first = false;
                Promise.resolve().then(function() {
                    trace.push('cancel@' + xhr.readyState);
                    xhr.abort();
                    xhr.open('GET', '/new');
                    xhr.send();
                });
            }
        };
        xhr.onabort = function() { trace.push('abort@' + xhr.readyState); };
        xhr.onload = function() { trace.push('load:' + xhr.responseText); };
        xhr.open('GET', '/old');
        xhr.send();
    "#,
        )
        .unwrap();
    let old = runtime.take_network_jobs().pop().unwrap();
    runtime
        .complete_network(old.id, Ok(buffered_response("/old", b"stale")))
        .unwrap();
    runtime.pump_with_network_errors().unwrap();
    assert!(old.request.signal.as_ref().unwrap().is_cancelled());
    assert_eq!(
        runtime.eval("trace.join(',')").unwrap(),
        Value::from("cancel@3,abort@4")
    );
    assert_eq!(runtime.pending_network(), 1);
    let new = runtime.take_network_jobs().pop().unwrap();
    runtime.progress_network(old.id).unwrap();
    runtime
        .complete_network(old.id, Ok(buffered_response("/old", b"stale")))
        .unwrap();
    runtime
        .complete_network(new.id, Ok(buffered_response("/new", b"fresh")))
        .unwrap();
    runtime.pump_with_network_errors().unwrap();
    assert_eq!(
        runtime.eval("trace.join(',')").unwrap(),
        Value::from("cancel@3,abort@4,load:fresh")
    );
    assert_eq!(runtime.pending_network(), 0);
}

#[test]
fn xhr_header_checkpoint_cancel_drops_buffered_data_before_loading() {
    let mut runtime = realm("https://example.test/page", "");
    runtime
        .eval(
            r#"
            var xhr = new XMLHttpRequest(), trace = [];
            xhr.onreadystatechange = function() {
                trace.push(xhr.readyState);
                if (xhr.readyState === 2) {
                    Promise.resolve().then(function() { xhr.abort(); });
                }
            };
            xhr.onabort = function() { trace.push('abort'); };
            xhr.onload = function() { trace.push('unexpected load'); };
            xhr.open('GET', '/header-abort');
            xhr.send();
            "#,
        )
        .unwrap();
    let job = runtime.take_network_jobs().pop().unwrap();
    runtime
        .complete_network(job.id, Ok(buffered_response("/header-abort", b"unused")))
        .unwrap();
    runtime.progress_network(job.id).unwrap();
    runtime.pump_with_network_errors().unwrap();
    assert!(job.request.signal.as_ref().unwrap().is_cancelled());
    assert_eq!(runtime.pending_network(), 0);
    assert_eq!(
        runtime
            .eval("[trace.join(','),xhr.readyState,xhr.responseText].join('|')")
            .unwrap(),
        Value::from("1,2,4,abort|0|"),
    );
}

#[test]
fn xhr_null_body_completes_with_headers_but_an_empty_body_stream_closes_later() {
    for has_body in [false, true] {
        let mut runtime = realm("https://example.test/page", "");
        runtime
            .eval(
                r#"
                var xhr = new XMLHttpRequest(), states = [];
                xhr.onreadystatechange = function() { states.push(xhr.readyState); };
                xhr.open('GET', '/empty');
                xhr.send();
                "#,
            )
            .unwrap();
        let id = runtime.take_network_jobs().pop().unwrap().id;
        let mut response = buffered_response("/empty", b"");
        if !has_body {
            response.body = None;
            response.status = 204;
        }
        runtime.complete_network(id, Ok(response)).unwrap();
        assert_eq!(
            runtime.eval("states.join(',')").unwrap(),
            Value::from(if has_body { "1,2" } else { "1,2,4" }),
        );
        runtime.pump_with_network_errors().unwrap();
        assert_eq!(
            runtime.eval("states.join(',')").unwrap(),
            Value::from("1,2,4")
        );
        assert_eq!(runtime.pending_network(), 0);
    }
}

#[test]
fn xhr_waits_for_transport_without_scheduling_reads_of_an_idle_pipe() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release, wait) = mpsc::channel();
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
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\na")
            .unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        socket.write_all(b"b").unwrap();
    });
    let mut runtime = realm(&format!("http://{address}/page"), "");
    runtime
        .eval("var xhr=new XMLHttpRequest();xhr.open('GET','/slow-body');xhr.send();")
        .unwrap();
    let job = runtime.take_network_jobs().pop().unwrap();
    let loader = Loader::new().unwrap();
    let cancel = CancelSignal::new();
    let (head, transfer) = loader
        .transport_handle()
        .block_on(loader.open_web_fetch(&job.request, &cancel, Instant::now() + FETCH_TIMEOUT))
        .unwrap();
    let (notify, received) = mpsc::channel();
    loader
        .transport_handle()
        .spawn(transfer.unwrap().run(move || {
            let notify = notify.clone();
            async move {
                let _ = notify.send(());
            }
        }));
    runtime.complete_fetch(job.id, Ok(head)).unwrap();
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    runtime.progress_network(job.id).unwrap();
    assert_eq!(
        runtime
            .eval("[xhr.readyState,xhr.responseText].join('|')")
            .unwrap(),
        Value::from("3|a")
    );
    assert!(runtime.next_deadline().is_none());
    assert_eq!(runtime.pending_network(), 1);
    release.send(()).unwrap();
    for _ in 0..4 {
        if runtime.pending_network() == 0 {
            break;
        }
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        runtime.progress_network(job.id).unwrap();
        runtime.pump_with_network_errors().unwrap();
    }
    assert_eq!(runtime.pending_network(), 0);
    assert_eq!(
        runtime
            .eval("[xhr.readyState,xhr.responseText].join('|')")
            .unwrap(),
        Value::from("4|ab")
    );
    server.join().unwrap();
}

#[test]
fn response_xml_preserves_long_qualified_names_and_traces_retained_child_lists() {
    let (address, server) = serve(|_| {
        // Longer than u16::MAX in UTF-8, and that offset falls inside é.
        let name = "é".repeat(32768);
        let body = format!(
            r#"<Top><Child {name}="full" �="replacement"><![CDATA[<!DOCTYPE Root>]]></Child></Top>"#
        );
        xml_reply(Some("application/xml"), body.as_bytes())
    });
    let mut runtime = realm(&format!("http://{address}/page"), "");
    runtime
        .eval(
            r#"
        var xhr = new XMLHttpRequest();
        xhr.open('GET', '/payload.xml', false);
        xhr.send();
        var saved = xhr.responseXML.documentElement.childNodes;
        xhr.open('GET', '/reset');
    "#,
        )
        .unwrap();
    // The XHR no longer roots this response. Here only its child list is live.
    runtime.cancel_pending_network().unwrap();
    let actual = runtime
        .eval(
            r#"
        var child = saved[0], name = 'é'.repeat(32768);
        [
            xhr.responseXML === null, child.nodeName,
            child.ownerDocument.documentElement.childNodes === saved,
            child.getAttribute(name), child.getAttribute(name.slice(1)) === null,
            child.getAttribute('\uFFFD'), child.getAttribute('\uD800') === null,
            child.getAttributeNS(null, '\uD800') === null, child.textContent
        ].join('|');
    "#,
        )
        .unwrap();
    assert_eq!(
        actual,
        Value::from("true|Child|true|full|true|replacement|true|true|<!DOCTYPE Root>"),
    );
    server.join().unwrap();
}

#[test]
fn response_xml_and_default_text_follow_xml_mime_encoding_precedence() {
    let cases = [
        (
            "text/xml",
            "<Root>é</Root>".as_bytes(),
            "text/xml|UTF-8|é|é",
        ),
        (
            "application/xml",
            b"<?xml version='1.0' encoding='windows-1252'?><Root>\xe9</Root>",
            "application/xml|windows-1252|é|é",
        ),
        (
            "application/xml; charset=windows-1252",
            b"<?xml version='1.0' encoding='utf-8'?><Root>\xe9</Root>",
            "application/xml|windows-1252|é|é",
        ),
        (
            "application/xml; charset=windows-1252",
            "\u{FEFF}<Root>é</Root>".as_bytes(),
            "application/xml|UTF-8|é|é",
        ),
        (
            r#"text/plain; charset=windows-1252; note="x, y", application/xml"#,
            "<Root>é</Root>".as_bytes(),
            "application/xml|UTF-8|é|é",
        ),
        (
            r#"text/xml; charset=windows-1252, text/xml; note="x, y""#,
            b"<Root>\xe9</Root>",
            "text/xml|windows-1252|é|é",
        ),
        (
            "application/xml, text/plain",
            "<Root>é</Root>".as_bytes(),
            "null|é",
        ),
        ("application/xml", b"<Root>\xff</Root>", "null|�"),
    ];
    for (mime, body, expected) in cases {
        let (address, server) = serve(move |_| xml_reply(Some(mime), body));
        let mut runtime = realm(&format!("http://{address}/page"), "");
        assert_eq!(
            runtime.eval(r#"
                var xhr = new XMLHttpRequest();
                xhr.open('GET', '/payload', false);
                xhr.send();
                var doc = xhr.responseXML;
                var text = xhr.responseText.slice(-8, -7);
                doc === null ? 'null|' + text :
                    [doc.contentType, doc.characterSet, doc.documentElement.textContent, text].join('|');
            "#).unwrap(),
            Value::from(expected),
            "{mime}",
        );
        server.join().unwrap();
    }
}
