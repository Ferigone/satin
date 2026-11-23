use rust_browser_engine::{
    Loader,
    network::{
        CredentialsMode, FetchErrorKind, RedirectMode, RequestMode, ResponseType, WebRequest,
    },
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use url::Url;

#[derive(Clone, Debug)]
struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}
impl Reply {
    fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            headers: vec![],
            body: body.into(),
        }
    }
    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}
struct Server {
    url: Url,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(&Request) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let requests = Arc::new(Mutex::new(vec![]));
        let captured = requests.clone();
        let thread = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !cancelled.load(Ordering::Relaxed) && Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(stream) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_millis(1000)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_millis(1000)))
                    .unwrap();
                let mut bytes = vec![];
                let mut chunk = [0; 4096];
                let header_end = loop {
                    let count = stream.read(&mut chunk).unwrap_or(0);
                    if count == 0 {
                        break None;
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(at) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        break Some(at + 4);
                    }
                    if bytes.len() > 80 * 1024 {
                        break None;
                    }
                };
                let Some(end) = header_end else { continue };
                let header = String::from_utf8_lossy(&bytes[..end]);
                let mut lines = header.lines();
                let mut start = lines.next().unwrap_or("").split_whitespace();
                let method = start.next().unwrap_or("").to_owned();
                let path = start.next().unwrap_or("").to_owned();
                let headers = lines
                    .filter_map(|line| line.split_once(':'))
                    .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                    .collect::<BTreeMap<_, _>>();
                let length = headers
                    .get("content-length")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0)
                    .min(1024 * 1024);
                while bytes.len() < end + length {
                    let count = stream.read(&mut chunk).unwrap_or(0);
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let request = Request {
                    method,
                    path,
                    headers,
                    body: String::from_utf8_lossy(&bytes[end..]).into_owned(),
                };
                let reply = handler(&request);
                captured.lock().unwrap().push(request);
                let mut response = format!(
                    "HTTP/1.1 {} Custom Reason\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status,
                    reply.body.len()
                );
                for (name, value) in reply.headers {
                    response.push_str(&format!("{name}: {value}\r\n"));
                }
                response.push_str("\r\n");
                response.push_str(&reply.body);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            url,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    fn request(&self, path: &str) -> WebRequest {
        WebRequest::get(self.url.join(path).unwrap(), self.url.clone())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn same_origin_status_body_and_headers_are_real_not_loader_resource_rules() {
    let server = Server::new(|_| Reply {
        status: 404,
        headers: vec![
            ("Content-Type".into(), "text/plain".into()),
            ("X-Visible".into(), "yes".into()),
            ("Set-Cookie".into(), "secret=test".into()),
        ],
        body: "Missing".into(),
    });
    let response = Loader::new()
        .unwrap()
        .web_fetch(&server.request("/missing"), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(response.status, 404);
    assert_eq!(response.status_text, "Custom Reason");
    assert_eq!(response.response_type, ResponseType::Basic);
    assert_eq!(response.body.as_ref().unwrap().as_slice(), b"Missing");
    assert!(response.headers.iter().any(|(name, _)| name == "x-visible"));
    assert!(
        !response
            .headers
            .iter()
            .any(|(name, _)| name == "set-cookie")
    );
}
#[test]
fn cors_denied_response_is_not_exposed_and_no_cors_is_opaque() {
    let server = Server::new(|_| Reply::ok("secret").header("X-Hidden", "yes"));
    let mut request = server.request("/data");
    request.client_url = Url::parse("https://client.test/").unwrap();
    let loader = Loader::new().unwrap();
    assert_eq!(
        loader
            .web_fetch(&request, &AtomicBool::new(false))
            .unwrap_err()
            .kind,
        FetchErrorKind::Cors
    );
    request.mode = RequestMode::NoCors;
    let response = loader.web_fetch(&request, &AtomicBool::new(false)).unwrap();
    assert_eq!(response.response_type, ResponseType::Opaque);
    assert_eq!(response.status, 0);
    assert!(response.headers.is_empty());
    assert!(response.body.is_none());
    assert!(response.url.is_empty());
    request.mode = RequestMode::SameOrigin;
    let count = server.requests.lock().unwrap().len();
    assert!(loader.web_fetch(&request, &AtomicBool::new(false)).is_err());
    assert_eq!(server.requests.lock().unwrap().len(), count);
}
#[test]
fn preflight_allows_the_real_request_and_only_exposed_response_headers() {
    let server = Server::new(|request| {
        if request.method == "OPTIONS" {
            Reply::ok("")
                .header("Access-Control-Allow-Origin", "http://client.test")
                .header("Access-Control-Allow-Methods", "PUT")
                .header("Access-Control-Allow-Headers", "x-custom,content-type")
        } else {
            Reply::ok("done")
                .header("Access-Control-Allow-Origin", "http://client.test")
                .header("Content-Type", "text/plain")
                .header("Access-Control-Expose-Headers", "X-Visible")
                .header("X-Visible", "yes")
                .header("X-Hidden", "no")
        }
    });
    let mut request = server.request("/api");
    request.client_url = Url::parse("http://client.test/").unwrap();
    request.method = "PUT".into();
    request.headers = vec![
        ("X-Custom".into(), "value".into()),
        ("Content-Type".into(), "application/json".into()),
    ];
    request.body = Some(Arc::new(b"{\"x\":1}".to_vec()));
    let response = Loader::new()
        .unwrap()
        .web_fetch(&request, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(response.response_type, ResponseType::Cors);
    assert!(response.headers.iter().any(|(name, _)| name == "x-visible"));
    assert!(!response.headers.iter().any(|(name, _)| name == "x-hidden"));
    let log = server.requests.lock().unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].method, "OPTIONS");
    assert_eq!(
        log[0].headers["access-control-request-headers"],
        "content-type,x-custom"
    );
    assert_eq!(log[1].method, "PUT");
    assert_eq!(log[1].body, "{\"x\":1}");
}
#[test]
fn failed_preflight_never_sends_side_effect_request_and_wildcard_does_not_allow_authorization() {
    let server = Server::new(|_| {
        Reply::ok("")
            .header("Access-Control-Allow-Origin", "*")
            .header("Access-Control-Allow-Methods", "*")
            .header("Access-Control-Allow-Headers", "*")
    });
    let mut request = server.request("/api");
    request.client_url = Url::parse("http://client.test/").unwrap();
    request.method = "DELETE".into();
    request.headers = vec![("Authorization".into(), "Bearer test".into())];
    assert_eq!(
        Loader::new()
            .unwrap()
            .web_fetch(&request, &AtomicBool::new(false))
            .unwrap_err()
            .kind,
        FetchErrorKind::Cors
    );
    let log = server.requests.lock().unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].method, "OPTIONS");
}
#[test]
fn redirects_rewrite_post_and_manual_mode_hides_location() {
    let server = Server::new(|request| {
        if request.path == "/redirect" {
            Reply {
                status: 302,
                headers: vec![("Location".into(), "/final".into())],
                body: String::new(),
            }
        } else {
            Reply::ok("final")
        }
    });
    let loader = Loader::new().unwrap();
    let mut request = server.request("/redirect");
    request.method = "POST".into();
    request.headers = vec![("Content-Type".into(), "text/plain".into())];
    request.body = Some(Arc::new(b"post data".to_vec()));
    let response = loader.web_fetch(&request, &AtomicBool::new(false)).unwrap();
    assert!(response.redirected);
    assert_eq!(response.url, server.url.join("/final").unwrap().as_str());
    let log = server.requests.lock().unwrap();
    assert_eq!(log[0].method, "POST");
    assert_eq!(log[1].method, "GET");
    assert!(log[1].body.is_empty());
    assert!(!log[1].headers.contains_key("content-type"));
    drop(log);
    request.redirect = RedirectMode::Manual;
    let response = loader.web_fetch(&request, &AtomicBool::new(false)).unwrap();
    assert_eq!(response.response_type, ResponseType::OpaqueRedirect);
    assert!(response.url.is_empty());
    assert!(response.headers.is_empty());
}
#[test]
fn credentials_require_exact_origin_and_permission_not_wildcard() {
    let server = Server::new(|_| Reply::ok("body").header("Access-Control-Allow-Origin", "*"));
    let mut request = server.request("/");
    request.client_url = Url::parse("https://client.test/").unwrap();
    request.credentials = CredentialsMode::Include;
    assert_eq!(
        Loader::new()
            .unwrap()
            .web_fetch(&request, &AtomicBool::new(false))
            .unwrap_err()
            .kind,
        FetchErrorKind::Cors
    );
}
#[test]
fn cancellation_and_forbidden_body_fail_before_any_network_request() {
    let server = Server::new(|_| Reply::ok("body"));
    let loader = Loader::new().unwrap();
    let mut request = server.request("/");
    assert_eq!(
        loader
            .web_fetch(&request, &AtomicBool::new(true))
            .unwrap_err()
            .kind,
        FetchErrorKind::Aborted
    );
    request.body = Some(Arc::new(vec![]));
    assert_eq!(
        loader
            .web_fetch(&request, &AtomicBool::new(false))
            .unwrap_err()
            .kind,
        FetchErrorKind::Invalid
    );
    assert!(server.requests.lock().unwrap().is_empty());
}
#[test]
fn active_mixed_content_is_rejected_before_dns_or_http() {
    let request = WebRequest::get(
        Url::parse("http://insecure.invalid/data").unwrap(),
        Url::parse("https://client.test/").unwrap(),
    );
    let error = Loader::new()
        .unwrap()
        .web_fetch(&request, &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(error.kind, FetchErrorKind::Network);
    assert!(error.message.contains("Mixed content"));
}
