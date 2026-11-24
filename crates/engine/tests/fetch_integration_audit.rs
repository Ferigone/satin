//! Independent boundary checks for queued Fetch, reentrant getters and DOM jobs.
use rust_browser_engine::{
    LoadedPage, Loader,
    network::{FetchError, FetchErrorKind, ResponseType, WebRequest, WebResponse},
    script::ScriptRuntime,
};
use rust_browser_javascript::Value;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex, atomic::AtomicBool},
    thread,
    time::{Duration, Instant},
};
use url::Url;

fn realm() -> (LoadedPage, ScriptRuntime) {
    let page = LoadedPage::from_html(
        "https://audit.test/base/index.html",
        "<!doctype html><main id='output'></main>",
    );
    let runtime = ScriptRuntime::new(&page).unwrap();
    (page, runtime)
}
fn reply(url: &str, body: &str) -> WebResponse {
    WebResponse {
        url: url.into(),
        status: 200,
        status_text: "OK".into(),
        headers: vec![("content-type".into(), "text/plain;charset=UTF-8".into())],
        body: Some(Arc::new(body.as_bytes().to_vec())),
        response_type: ResponseType::Basic,
        redirected: false,
    }
}

#[test]
fn reentrant_request_getter_cannot_exceed_pending_quota() {
    let (_, mut runtime) = realm();
    runtime.eval("var rejected=0;var options={};Object.defineProperty(options,'method',{get:function(){for(var i=0;i<16;i++)fetch('/inner').catch(function(){rejected++;});return 'GET';}});fetch('/outer',options).catch(function(){rejected++;});").unwrap();
    runtime.pump();
    assert_eq!(runtime.pending_network(), 16);
    let requests = runtime.take_network_requests();
    assert_eq!(requests.len(), 16);
    assert_eq!(runtime.eval("rejected").unwrap(), Value::Number(1.));
    for (id, _) in requests {
        runtime
            .complete_network(id, Err(FetchError::new(FetchErrorKind::Network, "offline")))
            .unwrap();
    }
    assert_eq!(runtime.pending_network(), 0);
    assert_eq!(runtime.eval("rejected").unwrap(), Value::Number(17.));
}

#[test]
fn header_argument_conversion_and_foreach_can_reenter_same_headers() {
    let (_, mut runtime) = realm();
    runtime.eval("var headers=new Headers();var key={toString:function(){headers.set('a','inside');return 'b';}};headers.set(key,'outside');var seen='';headers.forEach(function(value,name){seen+=name;if(name==='a')headers.set('c','new');});").unwrap();
    assert_eq!(
        runtime
            .eval("headers.get('a')==='inside' && headers.get('b')==='outside' && seen==='abc'")
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn fetch_resolution_then_getter_can_reenter_fetch_and_dom() {
    let (_, mut runtime) = realm();
    runtime.eval("var thenReads=0;Object.defineProperty(Response.prototype,'then',{get:function(){thenReads++;fetch('/nested');document.getElementById('output').textContent=this.headers.get('content-type');return undefined;}});var answer;fetch('/first').then(function(response){return response.text();}).then(function(text){answer=text;});").unwrap();
    let (id, request) = runtime.take_network_requests().remove(0);
    runtime
        .complete_network(id, Ok(reply(request.url.as_str(), "reply")))
        .unwrap();
    assert_eq!(
        runtime.eval("answer==='reply' && thenReads===1").unwrap(),
        Value::Bool(true)
    );
    let nested = runtime.take_network_requests();
    assert_eq!(nested.len(), 1);
    assert_eq!(nested[0].1.url.as_str(), "https://audit.test/nested");
    assert_eq!(runtime.pending_network(), 1);
}

#[test]
fn response_html_jobs_preserve_template_inertness_and_fragment_invariants() {
    let (mut page, mut runtime) = realm();
    runtime.eval("var count=0;var failure='';var template=document.createElement('template');fetch('/markup').then(function(response){return response.text();}).then(function(markup){template.innerHTML=markup;document.getElementById('output').replaceChildren(template.content.cloneNode(true));}).catch(function(error){failure=error.name+':'+error.message;});").unwrap();
    let (id, request) = runtime.take_network_requests().remove(0);
    runtime
        .complete_network(
            id,
            Ok(reply(
                request.url.as_str(),
                "<b id='injected'>Fetched</b><script>count=99</script>",
            )),
        )
        .unwrap();
    assert_eq!(runtime.eval("failure").unwrap(), Value::from(""));
    assert_eq!(runtime.eval("document.getElementById('injected').textContent==='Fetched' && count===0 && template.content.nodeType===11 && template.content.parentNode===null").unwrap(),Value::Bool(true));
    runtime.snapshot(&mut page);
    for id in page.document.connected_nodes() {
        for &child in &page.document.nodes[id.0].children {
            assert_eq!(page.document.nodes[child.0].parent, Some(id));
        }
    }
}

#[test]
fn request_transfer_clone_and_body_reader_jobs_are_single_use() {
    let (_, mut runtime) = realm();
    runtime.eval("var first=new Request('/post',{method:'POST',body:'payload'});var moved=new Request(first);var cloned=moved.clone();var values=[];moved.text().then(function(text){values.push(text);});cloned.text().then(function(text){values.push(text);});moved.text().catch(function(error){values.push(error instanceof TypeError);});").unwrap();
    assert_eq!(
        runtime
            .eval("first.bodyUsed && moved.bodyUsed && cloned.bodyUsed && values.length===0")
            .unwrap(),
        Value::Bool(true)
    );
    runtime.pump();
    assert_eq!(
        runtime.eval("values.join(',')").unwrap(),
        Value::from("payload,payload,true")
    );
}

#[test]
fn late_unknown_and_duplicate_completions_do_not_settle_other_requests() {
    let (_, mut runtime) = realm();
    runtime.eval("var result='pending';fetch('/first').then(function(response){return response.text();}).then(function(text){result=text;});").unwrap();
    let (id, request) = runtime.take_network_requests().remove(0);
    runtime
        .complete_network(id + 10, Ok(reply(request.url.as_str(), "wrong")))
        .unwrap();
    assert_eq!(runtime.pending_network(), 1);
    assert_eq!(runtime.eval("result").unwrap(), Value::from("pending"));
    runtime
        .complete_network(id, Ok(reply(request.url.as_str(), "right")))
        .unwrap();
    runtime
        .complete_network(id, Ok(reply(request.url.as_str(), "duplicate")))
        .unwrap();
    assert_eq!(runtime.pending_network(), 0);
    assert_eq!(runtime.eval("result").unwrap(), Value::from("right"));
    assert!(runtime.next_deadline().is_none());
}

#[test]
fn completed_network_headers_are_immutable_but_clones_have_independent_body_usage() {
    let (_, mut runtime) = realm();
    runtime
        .eval("var response;fetch('/one').then(function(value){response=value;});")
        .unwrap();
    let (id, request) = runtime.take_network_requests().remove(0);
    runtime
        .complete_network(id, Ok(reply(request.url.as_str(), "body")))
        .unwrap();
    assert_eq!(runtime.eval("var blocked=false;try{response.headers.set('x','new');}catch(error){blocked=error instanceof TypeError;}var cloned=response.clone();var bodies=[];response.text().then(function(value){bodies.push(value);});cloned.text().then(function(value){bodies.push(value);});blocked && response.headers===response.headers").unwrap(),Value::Bool(true));
    runtime.pump();
    assert_eq!(
        runtime.eval("bodies.join(',')").unwrap(),
        Value::from("body,body")
    );
}

fn read_headers(stream: &mut TcpStream) -> String {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 1);
        bytes.push(byte[0]);
        assert!(bytes.len() < 16 * 1024);
    }
    String::from_utf8(bytes).unwrap()
}
fn server(
    listener: TcpListener,
    replies: Vec<String>,
) -> (Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    listener.set_nonblocking(true).unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let read = captured.clone();
    let worker = thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(5);
        for reply in replies {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < end, "audit server deadline");
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            read.lock().unwrap().push(read_headers(&mut stream));
            stream.write_all(reply.as_bytes()).unwrap();
        }
    });
    (captured, worker)
}
#[test]
fn cors_redirect_strips_authorization_taints_origin_and_filters_response_headers() {
    let first = TcpListener::bind("127.0.0.1:0").unwrap();
    let next = TcpListener::bind("127.0.0.1:0").unwrap();
    let first_url = format!("http://{}/start", first.local_addr().unwrap());
    let next_url = format!("http://{}/final", next.local_addr().unwrap());
    let preflight="HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: http://client.test\r\nAccess-Control-Allow-Headers: authorization\r\nAccess-Control-Allow-Methods: GET\r\nConnection: close\r\n\r\n".into();
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: {next_url}\r\nAccess-Control-Allow-Origin: http://client.test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let (first_seen, first_thread) = server(first, vec![preflight, redirect]);
    let (next_seen,next_thread)=server(next,vec!["HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: null\r\nAccess-Control-Expose-Headers: x-visible\r\nX-Visible: yes\r\nX-Secret: hidden\r\nSet-Cookie: secret=value\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".into()]);
    let mut request = WebRequest::get(
        Url::parse(&first_url).unwrap(),
        Url::parse("http://client.test/page").unwrap(),
    );
    request
        .headers
        .push(("authorization".into(), "Bearer audit-only".into()));
    let response = Loader::new()
        .unwrap()
        .web_fetch(&request, &AtomicBool::new(false))
        .unwrap();
    first_thread.join().unwrap();
    next_thread.join().unwrap();
    assert_eq!(response.response_type, ResponseType::Cors);
    assert!(response.redirected);
    assert_eq!(response.body.unwrap().as_slice(), b"ok");
    assert!(
        response
            .headers
            .iter()
            .any(|(name, value)| name == "x-visible" && value == "yes")
    );
    assert!(
        !response
            .headers
            .iter()
            .any(|(name, _)| name == "x-secret" || name == "set-cookie")
    );
    let first = first_seen.lock().unwrap();
    assert!(first[0].starts_with("OPTIONS "));
    assert!(!first[0].to_ascii_lowercase().contains("\r\nauthorization:"));
    assert!(
        first[1]
            .to_ascii_lowercase()
            .contains("\r\nauthorization: bearer audit-only\r\n")
    );
    let next = next_seen.lock().unwrap();
    assert!(!next[0].to_ascii_lowercase().contains("\r\nauthorization:"));
    assert!(
        next[0]
            .to_ascii_lowercase()
            .contains("\r\norigin: null\r\n")
    );
}

#[test]
fn native_fetch_consumers_accept_the_maximum_decoded_body() {
    let page = LoadedPage::from_html("https://example.test/", "");
    for method in ["text", "json", "blob", "arrayBuffer"] {
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        runtime
            .eval(&format!(
                "var result; fetch('/large').then(r => r.{method}()).then(v => result = v);"
            ))
            .unwrap();
        let id = runtime.take_network_jobs().pop().unwrap().id;
        let len = rust_browser_engine::network::FETCH_RESPONSE_BODY_LIMIT;
        let mut bytes = vec![b' '; len];
        let suffix = br#"{"ok":true}"#;
        bytes[len - suffix.len()..].copy_from_slice(suffix);
        let response = WebResponse {
            url: "https://example.test/large".into(),
            status: 200,
            status_text: "OK".into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: Some(std::sync::Arc::new(bytes)),
            response_type: ResponseType::Basic,
            redirected: false,
        };
        runtime
            .complete_network(id, Ok(response))
            .unwrap_or_else(|e| panic!("{method}: {e}"));
        runtime.pump_with_network_errors().unwrap();
        let assertion = match method {
            "text" => "result.length === 67108864 && result.charAt(result.length - 1) === '}'",
            "json" => "result.ok === true",
            "blob" => "result.size === 67108864 && result.type === 'application/json'",
            _ => "result.byteLength === 67108864",
        };
        assert_eq!(
            runtime.eval(assertion).unwrap(),
            Value::Bool(true),
            "{method}"
        );
    }
}
