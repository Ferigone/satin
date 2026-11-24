use super::*;
use rust_browser_engine::{
    cookies::CookieContext,
    layout::Viewport,
    network::{FetchErrorKind, WebRequest},
    script::DocumentSession,
};
use std::{
    io::{BufRead, BufReader, ErrorKind, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};
use url::Url;

const WAIT: Duration = Duration::from_secs(5);

fn listener() -> TcpListener {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

fn accept(listener: &TcpListener, deadline: Instant) -> (String, TcpStream) {
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("The expected HTTP connection did not arrive: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.set_write_timeout(Some(WAIT)).unwrap();
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let path = line.split_ascii_whitespace().nth(1).unwrap().to_owned();
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
    }
    (path, stream)
}

fn navigation(
    listener: &TcpListener,
    path: &str,
    tab: u64,
    navigation: u64,
    cancel: CancelSignal,
) -> Job {
    let request =
        NavigationRequest::get(&format!("http://{}{path}", listener.local_addr().unwrap()))
            .unwrap();
    Job {
        tab,
        navigation,
        task: Task::Navigate(Box::new(request)),
        cancel,
    }
}

fn open_body(stream: &mut TcpStream, prefix: &str, headers: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nTransfer-Encoding: chunked\r\n{headers}\r\n{:x}\r\n{prefix}\r\n",
        prefix.len(),
    ).unwrap();
    stream.flush().unwrap();
}

fn finish_body(stream: &mut TcpStream, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    ).unwrap();
    stream.flush().unwrap();
}

#[test]
fn cancel_during_headers_and_body_releases_navigation_slots() {
    let server = listener();
    let (out, mut rx) = mpsc::channel(16);
    let workers = Workers::new(CookieStore::new(), out, || {}).unwrap();
    let headers = CancelSignal::new();
    let body = CancelSignal::new();
    workers
        .submit(navigation(&server, "/headers", 1, 1, headers.clone()))
        .unwrap();
    workers
        .submit(navigation(&server, "/body", 2, 1, body.clone()))
        .unwrap();
    let mut old_connections = Vec::new();
    for _ in 0..2 {
        let (path, mut stream) = accept(&server, Instant::now() + WAIT);
        match path.as_str() {
            "/headers" => {}
            "/body" => open_body(
                &mut stream,
                "<!doctype html><title>Body is open</title>",
                "",
            ),
            _ => panic!("Unexpected initial request {path}"),
        }
        old_connections.push(stream);
    }
    let end = Instant::now() + WAIT;
    loop {
        let message = workers
            .receive_until(&mut rx, end)
            .expect("The streamed prefix arrived");
        assert_eq!((message.tab, message.navigation), (2, 1));
        if matches!(message.payload, Payload::Navigation(NavigationPart::Text(text, _)) if text.contains("Body is open"))
        {
            break;
        }
    }
    headers.cancel();
    body.cancel();
    workers
        .submit(navigation(
            &server,
            "/replacement",
            1,
            2,
            CancelSignal::new(),
        ))
        .unwrap();
    let (path, mut replacement) = accept(&server, Instant::now() + WAIT);
    assert_eq!(path, "/replacement");
    finish_body(
        &mut replacement,
        "<!doctype html><title>Replacement</title>",
    );
    let end = Instant::now() + WAIT;
    loop {
        let message = workers
            .receive_until(&mut rx, end)
            .expect("The replacement finished");
        assert_eq!((message.tab, message.navigation), (1, 2));
        match message.payload {
            Payload::Navigation(NavigationPart::Finish) => break,
            Payload::Navigation(_) => {}
            Payload::NavigationError(error) => panic!("{error}"),
            _ => panic!("Unexpected result for replacement navigation"),
        }
    }
    // Both old HTTP responses remained incomplete while the replacement ran.
    drop(old_connections);
}

#[test]
fn request_abort_is_reported_without_canceling_the_page() {
    let server = listener();
    let cookies = CookieStore::new();
    let (out, mut rx) = mpsc::channel(16);
    let (workers, mut events) = Workers::new_observed(cookies.clone(), out, || {}).unwrap();
    let client = Url::parse(&format!("http://{}/", server.local_addr().unwrap())).unwrap();
    let generation = CancelSignal::new();
    let headers = CancelSignal::new();
    let body = CancelSignal::new();
    let submit = |id, path, signal| {
        let mut request = WebRequest::get(client.join(path).unwrap(), client.clone());
        request.signal = signal;
        workers
            .submit(Job {
                tab: 1,
                navigation: 1,
                task: Task::Fetch(Box::new(FetchJob {
                    id,
                    request,
                    upload: None,
                })),
                cancel: generation.clone(),
            })
            .unwrap();
    };
    submit(1, "headers", Some(headers.clone()));
    submit(2, "body?keep=secret#private", Some(body.clone()));
    let mut old_connections = Vec::new();
    for _ in 0..2 {
        let (path, mut stream) = accept(&server, Instant::now() + WAIT);
        match path.as_str() {
            "/headers" => {}
            "/body?keep=secret" => open_body(
                &mut stream,
                "The rest of this body never arrives.",
                "Set-Cookie: started=1; Path=/\r\n",
            ),
            _ => panic!("Unexpected Fetch request {path}"),
        }
        old_connections.push(stream);
    }
    let end = Instant::now() + WAIT;
    let context = CookieContext::subresource(&client);
    while cookies.header(&client, &context) != b"started=1" {
        assert!(
            Instant::now() < end,
            "Fetch received the body response headers"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let message = workers
        .receive_until(&mut rx, end)
        .expect("Fetch resolves while the server is holding the rest of the body");
    let Payload::Fetch {
        id: 2,
        result: Ok(head),
    } = message.payload
    else {
        panic!("Expected the open response's headers before either request was aborted");
    };
    assert_eq!(head.metadata.status, 200);
    assert!(head.metadata.body.is_none());
    let mut transfer = head.transfer.unwrap();
    let read = |transfer: &mut rust_browser_engine::network::FetchBody,
                rx: &mut mpsc::Receiver<Message>| {
        let end = Instant::now() + WAIT;
        loop {
            if let std::task::Poll::Ready(value) = transfer.read() {
                break value;
            }
            let progress = workers.receive_until(rx, end).expect("Fetch body progress");
            assert!(matches!(progress.payload, Payload::FetchBodyProgress(_)));
            transfer.acknowledge();
        }
    };
    assert!(!read(&mut transfer, &mut rx).unwrap().unwrap().is_empty());
    headers.cancel();
    body.cancel();
    let mut header_abort = false;
    loop {
        let message = workers
            .receive_until(&mut rx, end)
            .expect("Fetch abort result arrived");
        match message.payload {
            Payload::Fetch {
                id: 1,
                result: Err(error),
            } => {
                assert_eq!(error.kind, FetchErrorKind::Aborted);
                header_abort = true;
            }
            Payload::FetchBodyProgress(2) => transfer.acknowledge(),
            _ => panic!("Expected a per-request abort"),
        }
        if let Some(result) = transfer.outcome()
            && header_abort
        {
            assert_eq!(result.unwrap_err().kind, FetchErrorKind::Aborted);
            break;
        }
    }
    assert!(!generation.is_cancelled());
    let mut aborted = Vec::new();
    let mut streaming = Vec::new();
    while let Some(event) = events.try_recv() {
        assert_eq!((event.key.tab, event.key.navigation), (1, 1));
        match event.key.request {
            Request::Fetch(1) => aborted.push(event.phase),
            Request::Fetch(2) => streaming.push(event.phase),
            _ => panic!("Unexpected observation: {event:?}"),
        }
    }
    assert_eq!(
        aborted,
        [
            Phase::Started {
                method: Some("GET".into()),
                url: client.join("headers").unwrap().to_string(),
                resource_type: None,
            },
            Phase::Cancelled,
        ]
    );
    assert!(
        matches!(streaming.as_slice(), [
        Phase::Started { method: Some(method), url, resource_type: None },
        Phase::Response { status: Some(200), headers_at: Some(_), .. },
        Phase::Cancelled
    ] if method == "GET" && url == client.join("body").unwrap().as_str()),
        "The transfer reported the per-request abort: {streaming:?}"
    );
    submit(3, "healthy", None);
    let (path, mut healthy) = accept(&server, Instant::now() + WAIT);
    assert_eq!(path, "/healthy");
    finish_body(&mut healthy, "OK");
    let end = Instant::now() + WAIT;
    let message = loop {
        let message = workers
            .receive_until(&mut rx, end)
            .expect("The next Fetch finished");
        if matches!(message.payload, Payload::Fetch { .. }) {
            break message;
        }
    };
    let Payload::Fetch {
        id: 3,
        result: Ok(head),
    } = message.payload
    else {
        panic!("The unrelated request must receive its headers");
    };
    assert_eq!(head.metadata.status, 200);
    let mut transfer = head.transfer.unwrap();
    assert_eq!(read(&mut transfer, &mut rx).unwrap().unwrap(), b"OK");
    assert!(read(&mut transfer, &mut rx).unwrap().is_none());
    let healthy: Vec<_> = std::iter::from_fn(|| events.try_recv())
        .filter(|e| e.key.request == Request::Fetch(3))
        .collect();
    assert!(
        matches!(healthy.as_slice(), [start, head, end] if
            matches!(&start.phase, Phase::Started {method:Some(m), ..} if m=="GET") &&
            matches!(&head.phase, Phase::Response {status: Some(200), details, ..} if details.mime.as_deref()==Some("text/html") && details.declared_bytes==Some(2)) &&
            end.phase == Phase::Finished { body_bytes: Some(2) } &&
            start.started == head.started && head.started == end.started && head.elapsed <= end.elapsed
        ),
        "{healthy:?}"
    );
    drop(old_connections);
}

#[test]
fn redirect_and_full_result_queue_share_the_navigation_deadline() {
    let server = listener();
    let (out, mut rx) = mpsc::channel(2);
    let loader = Loader::new().unwrap();
    let handle = loader.transport_handle().clone();
    let deadline = Instant::now() + Duration::from_secs(3);
    let (observer, mut events) = devtools::channel();
    let job = QueuedJob {
        job: navigation(&server, "/redirect", 1, 1, CancelSignal::new()),
        queued: deadline - NAVIGATION_TIMEOUT,
    };
    let worker = handle.spawn(async move {
        run_job(
            job,
            &loader,
            &out,
            &Arc::new(|| {}),
            &Arc::new(Semaphore::new(FETCH_TRANSFERS)),
            &CancelSignal::new(),
            Some(&observer),
        )
        .await
    });
    let (path, mut redirect) = accept(&server, deadline);
    assert_eq!(path, "/redirect");
    thread::sleep(Duration::from_millis(1250));
    redirect.write_all(b"HTTP/1.1 302 Found\r\nLocation: /stream\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    redirect.flush().unwrap();
    let (path, mut stream) = accept(&server, deadline);
    assert_eq!(path, "/stream");
    open_body(
        &mut stream,
        "<!doctype html><title>Blocked in the result queue</title>",
        "",
    );
    handle.block_on(async {
        tokio::time::timeout_at((deadline + Duration::from_millis(750)).into(), worker)
            .await
            .expect("The original deadline applies after redirect and while backpressured")
            .unwrap();
    });
    let open = rx
        .try_recv()
        .expect("The Open part consumed the sole streaming slot");
    assert!(matches!(
        open.payload,
        Payload::Navigation(NavigationPart::Open(_))
    ));
    let error = rx
        .try_recv()
        .expect("The terminal error had its own reserved slot");
    assert!(
        matches!(error.payload, Payload::NavigationError(error) if error.contains("deadline exceeded"))
    );
    assert!(rx.try_recv().is_err());
    let key = Key {
        tab: 1,
        navigation: 1,
        request: Request::Navigate,
    };
    let started = events.try_recv().unwrap();
    assert_eq!(started.key, key);
    assert_eq!(
        started.phase,
        Phase::Started {
            method: Some("GET".into()),
            url: format!("http://{}/redirect", server.local_addr().unwrap()),
            resource_type: None,
        }
    );
    let opened = events.try_recv().unwrap();
    assert_eq!(opened.key, key);
    let Phase::Response {
        status,
        headers_at: Some(headers_at),
        details,
    } = opened.phase
    else {
        panic!("Expected real navigation headers");
    };
    assert_eq!(status, Some(200));
    assert_eq!(details.mime.as_deref(), Some("text/html"));
    assert!(headers_at.duration_since(opened.started) <= opened.elapsed);
    assert_eq!(
        details.final_url,
        Some(format!("http://{}/stream", server.local_addr().unwrap()))
    );
    let failed = events.try_recv().unwrap();
    assert_eq!(failed.key, key);
    assert_eq!(failed.phase, Phase::Failed);
    assert!(failed.elapsed >= opened.elapsed);
    assert!(events.try_recv().is_none());
}

#[test]
fn successful_document_resource_uses_its_existing_id_and_sanitized_final_url() {
    let server = listener();
    let (out, mut rx) = mpsc::channel(16);
    let (workers, mut events) = Workers::new_observed(CookieStore::new(), out, || {}).unwrap();
    let cancel = CancelSignal::new();
    workers
        .submit(navigation(&server, "/index", 12, 7, cancel.clone()))
        .unwrap();
    let (path, mut stream) = accept(&server, Instant::now() + WAIT);
    assert_eq!(path, "/index");
    finish_body(
        &mut stream,
        "<!doctype html><link rel=stylesheet href='/style.css?private=1#fragment'>",
    );
    let end = Instant::now() + WAIT;
    let open = workers.receive_until(&mut rx, end).unwrap();
    let Payload::Navigation(NavigationPart::Open(open)) = open.payload else {
        panic!("Expected the initial document");
    };
    let (mut page, mut session) = DocumentSession::start(*open).unwrap();
    loop {
        match workers.receive_until(&mut rx, end).unwrap().payload {
            Payload::Navigation(NavigationPart::Text(text, _)) => session.feed(&text).unwrap(),
            Payload::Navigation(NavigationPart::Finish) => break,
            _ => panic!("Expected streamed document input"),
        }
    }
    session.finish_input().unwrap();
    let mut runtime = None;
    let load = loop {
        session
            .advance(&mut page, &mut runtime, Viewport::default())
            .unwrap();
        if let Some(load) = session.next_resource() {
            break load;
        }
        assert!(Instant::now() < end, "No stylesheet discovered");
    };
    let id = load.id;
    workers
        .submit(Job {
            tab: 12,
            navigation: 7,
            task: Task::Resource(Box::new(load)),
            cancel,
        })
        .unwrap();
    let (path, mut stream) = accept(&server, Instant::now() + WAIT);
    assert_eq!(path, "/style.css?private=1");
    stream
        .write_all(b"HTTP/1.1 302 Found\r\nLocation: /final.css?private=2\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .unwrap();
    stream.flush().unwrap();
    let (path, mut stream) = accept(&server, Instant::now() + WAIT);
    assert_eq!(path, "/final.css?private=2");
    stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .unwrap();
    stream.flush().unwrap();
    let result = workers
        .receive_until(&mut rx, Instant::now() + WAIT)
        .unwrap();
    assert_eq!((result.tab, result.navigation), (12, 7));
    let Payload::Resource {
        id: result_id,
        result: Ok(resource),
    } = result.payload
    else {
        panic!("Expected the unchanged stylesheet result");
    };
    assert_eq!(result_id, id);
    assert_eq!(resource.url.query(), Some("private=2"));
    let resource_events: Vec<_> = std::iter::from_fn(|| events.try_recv())
        .filter(|event| event.key.request == Request::Resource(id))
        .collect();
    assert_eq!(resource_events.len(), 3);
    assert!(
        resource_events
            .iter()
            .all(|event| (event.key.tab, event.key.navigation) == (12, 7))
    );
    assert!(
        matches!(&resource_events[0].phase, Phase::Started { url, resource_type: Some("Stylesheet"), .. }
        if url == &format!("http://{}/style.css", server.local_addr().unwrap()))
    );
    let Phase::Response {
        status: Some(200),
        details,
        ..
    } = &resource_events[1].phase
    else {
        panic!("missing HTTP response");
    };
    assert_eq!(
        details.final_url,
        Some(format!("http://{}/final.css", server.local_addr().unwrap()))
    );
    assert_eq!(details.body_bytes, Some(0));
    assert_eq!(details.mime.as_deref(), Some("text/css"));
    assert_eq!(details.declared_bytes, Some(0));
    assert_eq!(
        resource_events[2].phase,
        Phase::Finished {
            body_bytes: Some(0)
        }
    );
}

#[test]
fn a_full_metadata_queue_does_not_delay_the_page_fetch_result() {
    let server = listener();
    let (out, mut rx) = mpsc::channel(16);
    let (observer, events) = devtools::channel();
    let client = Url::parse(&format!("http://{}/", server.local_addr().unwrap())).unwrap();
    for id in 0..devtools::CAPACITY {
        observer
            .begin(
                Key {
                    tab: 1,
                    navigation: 1,
                    request: Request::Fetch(id as u64),
                },
                "GET",
                &client,
                None,
            )
            .unwrap();
    }
    let workers = Workers::with_observer(CookieStore::new(), out, || {}, Some(observer)).unwrap();
    let fetch_id = devtools::CAPACITY as u64;
    workers
        .submit(Job {
            tab: 3,
            navigation: 9,
            task: Task::Fetch(Box::new(FetchJob {
                id: fetch_id,
                request: WebRequest::get(client.join("missing").unwrap(), client),
                upload: None,
            })),
            cancel: CancelSignal::new(),
        })
        .unwrap();
    let (path, mut stream) = accept(&server, Instant::now() + WAIT);
    assert_eq!(path, "/missing");
    stream
        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .unwrap();
    stream.flush().unwrap();
    let result = workers
        .receive_until(&mut rx, Instant::now() + WAIT)
        .unwrap();
    assert_eq!((result.tab, result.navigation), (3, 9));
    let Payload::Fetch {
        id,
        result: Ok(head),
    } = result.payload
    else {
        panic!("Expected the Fetch headers despite observer overflow");
    };
    assert_eq!(id, fetch_id);
    assert_eq!(head.metadata.status, 404);
    assert_eq!(events.take_dropped(), 2);
}
