//! Independent local HTTP boundary tests. No external server, DNS service, or
//! third-party cookie jar supplies the behavior under test. Expectations follow
//! Fetch's credential/CORS/redirect algorithms and RFC6265bis cookie storage.
use rust_browser_engine::{
    Loader,
    cookies::{CookieContext, CookieStore},
    loader::NavigationRequest,
    network::{CredentialsMode, FetchErrorKind, RequestMode, ResponseType, WebRequest},
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
    body: Vec<u8>,
}
impl Request {
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", String::as_str)
    }
}
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}
impl Reply {
    fn ok() -> Self {
        Self {
            status: 200,
            headers: vec![],
            body: "<!doctype html><title>Cookie audit</title><p>done".into(),
        }
    }
    fn redirect(status: u16, target: &str) -> Self {
        Self {
            status,
            headers: vec![("Location".into(), target.into())],
            body: String::new(),
        }
    }
    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    fn cors(self, origin: &str) -> Self {
        self.header("Access-Control-Allow-Origin", origin)
            .header("Access-Control-Allow-Credentials", "true")
    }
}
struct Server {
    url: Url,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(&Request) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !cancelled.load(Ordering::Relaxed) && Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 4096];
                let end = loop {
                    let count = stream.read(&mut chunk).unwrap_or(0);
                    if count == 0 || bytes.len() > 80 * 1024 {
                        break None;
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(index) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                        break Some(index + 4);
                    }
                };
                let Some(end) = end else { continue };
                let header = String::from_utf8_lossy(&bytes[..end]);
                let mut lines = header.lines();
                let mut start = lines.next().unwrap_or_default().split_ascii_whitespace();
                let method = start.next().unwrap_or_default().to_owned();
                let path = start.next().unwrap_or_default().to_owned();
                let headers: BTreeMap<_, _> = lines
                    .filter_map(|line| line.split_once(':'))
                    .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                    .collect();
                let length = headers
                    .get("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if length > 64 * 1024 {
                    continue;
                }
                while bytes.len() < end + length {
                    let count = stream.read(&mut chunk).unwrap_or(0);
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                }
                if bytes.len() < end + length {
                    continue;
                }
                let request = Request {
                    method,
                    path,
                    headers,
                    body: bytes[end..end + length].to_vec(),
                };
                let reply = handler(&request);
                captured.lock().unwrap().push(request);
                let mut response = format!(
                    "HTTP/1.1 {} Audit\r\nContent-Length: {}\r\nContent-Type: text/html;charset=utf-8\r\nConnection: close\r\n",
                    reply.status,
                    reply.body.len(),
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
            worker: Some(worker),
        }
    }
    fn url(&self, path: &str) -> Url {
        self.url.join(path).unwrap()
    }
    fn log(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn seed(store: &CookieStore, url: &Url, pair: &str) {
    assert!(
        store.receive(url, pair.as_bytes(), &CookieContext::subresource(url)),
        "seed rejected: {pair}"
    );
}
fn hidden_cookie_headers(response: &rust_browser_engine::network::WebResponse) {
    assert!(
        !response
            .headers
            .iter()
            .any(|(name, _)| matches!(name.as_str(), "set-cookie" | "set-cookie2"))
    );
}

#[test]
fn navigation_receives_each_cookie_and_sends_redirect_cookies_on_the_next_hop() {
    let server = Server::new(|request| match request.path.as_str() {
        "/start" => Reply::redirect(302, "/final")
            .header("Set-Cookie", "root=one; Path=/")
            .header("Set-Cookie", "secret=two; HttpOnly; Path=/")
            .header("Set-Cookie", "scoped=three; Path=/private")
            .header(
                "Set-Cookie",
                "expired=gone; Expires=Wed, 09 Jun 2021 10:18:14 GMT; Path=/",
            ),
        "/final" => Reply::ok().header("Set-Cookie", "final=four; Path=/"),
        _ => Reply::ok(),
    });
    let store = CookieStore::new();
    let loader = Loader::with_cookies(store.clone()).unwrap();
    let mut previews = Vec::new();
    let page = loader
        .load_navigation_with_preview(
            &NavigationRequest::get(server.url("/start").as_str()).unwrap(),
            &AtomicBool::new(false),
            |page| previews.push(page.url),
        )
        .unwrap();
    assert_eq!(page.url, server.url("/final").as_str());
    assert!(!previews.is_empty());
    let log = server.log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].header("cookie"), "");
    assert_eq!(log[1].header("cookie"), "root=one; secret=two");
    assert_eq!(store.document_cookie(&server.url), "root=one; final=four");
    assert_eq!(
        page.cookies.document_cookie(&server.url),
        store.document_cookie(&server.url)
    );
    let response = loader
        .web_fetch(
            &WebRequest::get(server.url("/private/child"), server.url.clone()),
            &AtomicBool::new(false),
        )
        .unwrap();
    hidden_cookie_headers(&response);
    assert_eq!(
        server.log()[2].header("cookie"),
        "scoped=three; root=one; secret=two; final=four"
    );
    assert_eq!(
        store.document_cookie(&server.url("/private")),
        "scoped=three; root=one; final=four"
    );
}

#[test]
fn form_navigation_307_and_308_preserve_post_bytes_content_type_and_cookie_updates() {
    for status in [307, 308] {
        let server = Server::new(move |request| {
            if request.path == "/submit" {
                Reply::redirect(status, "/receipt").header("Set-Cookie", "submitted=yes; Path=/")
            } else {
                Reply::ok()
            }
        });
        let store = CookieStore::new();
        seed(&store, &server.url, "session=start; Path=/");
        let loader = Loader::with_cookies(store).unwrap();
        let mut request = NavigationRequest::get(server.url("/submit").as_str()).unwrap();
        let body = b"name=Jan+Kowalski&message=%C5%BC%C3%B3%C5%82w&empty=".to_vec();
        request.method = "POST".into();
        request.body = Some(body.clone());
        request.content_type = Some("application/x-www-form-urlencoded".into());
        request.context = CookieContext::navigation(Some(&server.url), &request.url, "POST");
        request.initiator = Some(server.url.clone());
        let page = loader
            .load_navigation_with_preview(&request, &AtomicBool::new(false), |_| {})
            .unwrap();
        assert_eq!(page.url, server.url("/receipt").as_str());
        let log = server.log();
        assert_eq!(log.len(), 2);
        for hop in &log {
            assert_eq!(hop.method, "POST", "status={status}");
            assert_eq!(hop.body, body);
            assert_eq!(
                hop.header("content-type"),
                "application/x-www-form-urlencoded"
            );
            assert_eq!(
                hop.header("origin"),
                server.url.origin().ascii_serialization()
            );
        }
        assert_eq!(log[1].header("cookie"), "session=start; submitted=yes");
    }
}

#[test]
fn form_navigation_301_302_303_rewrite_post_to_get_and_remove_body_headers() {
    for status in [301, 302, 303] {
        let server = Server::new(move |request| {
            if request.path == "/submit" {
                Reply::redirect(status, "/receipt").header("Set-Cookie", "submitted=yes; Path=/")
            } else {
                Reply::ok()
            }
        });
        let loader = Loader::new().unwrap();
        let mut request = NavigationRequest::get(server.url("/submit").as_str()).unwrap();
        request.method = "POST".into();
        request.body = Some(b"a=1&b=two".to_vec());
        request.content_type = Some("application/x-www-form-urlencoded".into());
        request.context = CookieContext::navigation(Some(&server.url), &request.url, "POST");
        request.initiator = Some(server.url.clone());
        loader
            .load_navigation_with_preview(&request, &AtomicBool::new(false), |_| {})
            .unwrap();
        let log = server.log();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].method, "POST");
        assert_eq!(log[0].body, b"a=1&b=two");
        assert_eq!(log[1].method, "GET", "status={status}");
        assert!(log[1].body.is_empty());
        assert_eq!(log[1].header("content-type"), "");
        assert_eq!(log[1].header("origin"), "");
        assert_eq!(log[1].header("cookie"), "submitted=yes");
    }
}

#[test]
fn credentials_modes_control_sending_and_receiving_across_origins_and_ports() {
    let target = Server::new(|request| {
        Reply::ok()
            .cors(request.header("origin"))
            .header("Set-Cookie", "received=yes; SameSite=None; Secure; Path=/")
            .header(
                "Access-Control-Expose-Headers",
                "Set-Cookie,Set-Cookie2,X-Visible",
            )
            .header("Set-Cookie2", "hidden=obsolete")
            .header("X-Visible", "yes")
    });
    let client = Server::new(|_| Reply::ok());
    let mut other_host = client.url.clone();
    other_host.set_host(Some("localhost")).unwrap();
    for (case, client_url) in [
        ("same-origin", target.url.clone()),
        ("other-port", client.url.clone()),
        ("other-site", other_host),
    ] {
        for mode in [
            CredentialsMode::Omit,
            CredentialsMode::SameOrigin,
            CredentialsMode::Include,
        ] {
            let store = CookieStore::new();
            seed(
                &store,
                &target.url,
                "seed=present; SameSite=None; Secure; Path=/",
            );
            let loader = Loader::with_cookies(store.clone()).unwrap();
            let mut request = WebRequest::get(target.url("/matrix"), client_url.clone());
            request.credentials = mode;
            let response = loader.web_fetch(&request, &AtomicBool::new(false)).unwrap();
            let allowed = mode == CredentialsMode::Include
                || mode == CredentialsMode::SameOrigin && case == "same-origin";
            let log = target.log();
            assert_eq!(
                log.last().unwrap().header("cookie"),
                if allowed { "seed=present" } else { "" },
                "{case} {mode:?}"
            );
            assert_eq!(
                store.document_cookie(&target.url).contains("received=yes"),
                allowed,
                "{case} {mode:?}"
            );
            assert_eq!(
                response.response_type,
                if case == "same-origin" {
                    ResponseType::Basic
                } else {
                    ResponseType::Cors
                }
            );
            hidden_cookie_headers(&response);
            assert!(
                response
                    .headers
                    .iter()
                    .any(|(name, value)| name == "x-visible" && value == "yes")
            );
        }
    }
    assert!(
        client.log().is_empty(),
        "The client origin must not be contacted by these requests"
    );
}

#[test]
fn credentialed_preflight_omits_cookies_and_cannot_set_them() {
    let client = Server::new(|_| Reply::ok());
    let target = Server::new(|request| {
        let reply = Reply::ok().cors(request.header("origin"));
        if request.method == "OPTIONS" {
            reply
                .header("Access-Control-Allow-Methods", "PUT")
                .header("Access-Control-Allow-Headers", "x-audit")
                .header(
                    "Set-Cookie",
                    "preflight=forbidden; SameSite=None; Secure; Path=/",
                )
        } else {
            reply.header(
                "Set-Cookie",
                "actual=accepted; SameSite=None; Secure; Path=/",
            )
        }
    });
    let store = CookieStore::new();
    seed(
        &store,
        &target.url,
        "seed=present; SameSite=None; Secure; Path=/",
    );
    let loader = Loader::with_cookies(store.clone()).unwrap();
    let mut request = WebRequest::get(target.url("/put"), client.url.clone());
    request.credentials = CredentialsMode::Include;
    request.method = "PUT".into();
    request.headers = vec![("X-Audit".into(), "enabled".into())];
    request.body = Some(Arc::new(b"payload".to_vec()));
    let response = loader.web_fetch(&request, &AtomicBool::new(false)).unwrap();
    hidden_cookie_headers(&response);
    let log = target.log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].method, "OPTIONS");
    assert_eq!(log[0].header("cookie"), "");
    assert!(log[0].body.is_empty());
    assert_eq!(log[0].header("access-control-request-method"), "PUT");
    assert_eq!(log[0].header("access-control-request-headers"), "x-audit");
    assert_eq!(log[1].method, "PUT");
    assert_eq!(log[1].header("cookie"), "seed=present");
    assert_eq!(log[1].body, b"payload");
    assert_eq!(
        store.document_cookie(&target.url),
        "seed=present; actual=accepted"
    );
}

#[test]
fn failed_cors_still_processes_credentialed_set_cookie_without_exposing_response() {
    let target = Server::new(|_| {
        Reply::ok()
            .header("Access-Control-Allow-Origin", "*")
            .header("Access-Control-Allow-Credentials", "true")
            .header(
                "Set-Cookie",
                "network=accepted; SameSite=None; Secure; HttpOnly; Path=/",
            )
    });
    let store = CookieStore::new();
    seed(
        &store,
        &target.url,
        "seed=present; SameSite=None; Secure; Path=/",
    );
    let loader = Loader::with_cookies(store.clone()).unwrap();
    let mut request = WebRequest::get(
        target.url("/denied"),
        Url::parse("http://localhost:45678/").unwrap(),
    );
    request.credentials = CredentialsMode::Include;
    let error = loader
        .web_fetch(&request, &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(error.kind, FetchErrorKind::Cors);
    assert_eq!(target.log()[0].header("cookie"), "seed=present");
    assert_eq!(store.document_cookie(&target.url), "seed=present");
    assert_eq!(
        store.header(&target.url, &CookieContext::subresource(&target.url)),
        b"seed=present; network=accepted"
    );
}

#[test]
fn no_cors_include_has_opaque_response_but_credential_cookie_side_effects() {
    let target = Server::new(|_| {
        Reply::ok().header("Set-Cookie", "opaque=stored; SameSite=None; Secure; Path=/")
    });
    for mode in [
        CredentialsMode::Include,
        CredentialsMode::SameOrigin,
        CredentialsMode::Omit,
    ] {
        let store = CookieStore::new();
        seed(
            &store,
            &target.url,
            "seed=present; SameSite=None; Secure; Path=/",
        );
        let loader = Loader::with_cookies(store.clone()).unwrap();
        let mut request = WebRequest::get(
            target.url("/opaque"),
            Url::parse("http://localhost:45678/").unwrap(),
        );
        request.mode = RequestMode::NoCors;
        request.credentials = mode;
        let response = loader.web_fetch(&request, &AtomicBool::new(false)).unwrap();
        assert_eq!(response.response_type, ResponseType::Opaque);
        assert!(response.headers.is_empty());
        assert!(response.body.is_none());
        assert!(response.url.is_empty());
        assert_eq!(
            target.log().last().unwrap().header("cookie"),
            if mode == CredentialsMode::Include {
                "seed=present"
            } else {
                ""
            }
        );
        assert_eq!(
            store.document_cookie(&target.url).contains("opaque=stored"),
            mode == CredentialsMode::Include
        );
    }
}

#[test]
fn same_site_cookie_policy_distinguishes_cross_site_fetch_and_top_level_navigation() {
    let target = Server::new(|request| {
        if request.path == "/post" {
            Reply::redirect(303, "/receipt")
        } else {
            Reply::ok().cors(request.header("origin"))
        }
    });
    let client = Url::parse("http://localhost:45678/").unwrap();
    let store = CookieStore::new();
    for pair in [
        "strict=one; SameSite=Strict; Path=/",
        "lax=two; SameSite=Lax; Path=/",
        "none=three; SameSite=None; Secure; Path=/",
    ] {
        seed(&store, &target.url, pair);
    }
    let loader = Loader::with_cookies(store).unwrap();
    let mut fetch = WebRequest::get(target.url("/fetch"), client.clone());
    fetch.credentials = CredentialsMode::Include;
    loader.web_fetch(&fetch, &AtomicBool::new(false)).unwrap();
    assert_eq!(target.log()[0].header("cookie"), "none=three");

    let mut get = NavigationRequest::get(target.url("/get").as_str()).unwrap();
    get.initiator = Some(client.clone());
    get.context = CookieContext::navigation(Some(&client), &get.url, "GET");
    loader
        .load_navigation_with_preview(&get, &AtomicBool::new(false), |_| {})
        .unwrap();
    assert_eq!(target.log()[1].header("cookie"), "lax=two; none=three");

    let mut post = NavigationRequest::get(target.url("/post").as_str()).unwrap();
    post.method = "POST".into();
    post.body = Some(b"message=hello".to_vec());
    post.content_type = Some("application/x-www-form-urlencoded".into());
    post.initiator = Some(client.clone());
    post.context = CookieContext::navigation(Some(&client), &post.url, "POST");
    loader
        .load_navigation_with_preview(&post, &AtomicBool::new(false), |_| {})
        .unwrap();
    let log = target.log();
    assert_eq!(log.len(), 4);
    assert_eq!(log[2].method, "POST");
    assert_eq!(log[2].header("cookie"), "none=three");
    assert_eq!(log[3].method, "GET");
    assert_eq!(log[3].header("cookie"), "lax=two; none=three");
}

#[test]
fn redirect_origin_taint_suppresses_same_origin_credentials_after_return() {
    let return_to = Arc::new(Mutex::new(String::new()));
    let final_url = return_to.clone();
    let middle = Server::new(move |request| {
        Reply::redirect(302, &final_url.lock().unwrap())
            .cors(request.header("origin"))
            .header("Set-Cookie", "middle=stored; Path=/")
    });
    let middle_url = middle.url("/middle").to_string();
    let start = Server::new(move |request| {
        if request.path == "/start" {
            Reply::redirect(302, &middle_url).header("Set-Cookie", "first=stored; Path=/")
        } else {
            Reply::ok()
                .cors(request.header("origin"))
                .header("Set-Cookie", "last=stored; Path=/")
        }
    });
    *return_to.lock().unwrap() = start.url("/final").to_string();
    for mode in [
        CredentialsMode::SameOrigin,
        CredentialsMode::Include,
        CredentialsMode::Omit,
    ] {
        let store = CookieStore::new();
        seed(&store, &start.url, "seed=present; Path=/");
        let loader = Loader::with_cookies(store.clone()).unwrap();
        let mut request = WebRequest::get(start.url("/start"), start.url.clone());
        request.credentials = mode;
        let response = loader.web_fetch(&request, &AtomicBool::new(false)).unwrap();
        assert!(response.redirected);
        assert_eq!(response.response_type, ResponseType::Cors);
        hidden_cookie_headers(&response);
        let first_log = start.log();
        let middle_log = middle.log();
        let first = &first_log[first_log.len() - 2];
        let final_hop = first_log.last().unwrap();
        let intermediate = middle_log.last().unwrap();
        assert_eq!(
            first.header("cookie"),
            if mode == CredentialsMode::Omit {
                ""
            } else {
                "seed=present"
            }
        );
        assert_eq!(
            intermediate.header("origin"),
            start.url.origin().ascii_serialization()
        );
        assert_eq!(final_hop.header("origin"), "null");
        if mode == CredentialsMode::Include {
            assert_eq!(intermediate.header("cookie"), "seed=present; first=stored");
            assert_eq!(
                final_hop.header("cookie"),
                "seed=present; first=stored; middle=stored"
            );
            assert_eq!(
                store.document_cookie(&start.url),
                "seed=present; first=stored; middle=stored; last=stored"
            );
        } else {
            assert_eq!(intermediate.header("cookie"), "");
            assert_eq!(final_hop.header("cookie"), "");
            assert_eq!(
                store.document_cookie(&start.url),
                if mode == CredentialsMode::SameOrigin {
                    "seed=present; first=stored"
                } else {
                    "seed=present"
                }
            );
        }
    }
}

#[test]
fn cross_site_redirect_round_trip_keeps_strict_cookies_out_of_the_return_hop() {
    let return_to = Arc::new(Mutex::new(String::new()));
    let final_url = return_to.clone();
    let middle = Server::new(move |request| {
        Reply::redirect(302, &final_url.lock().unwrap()).cors(request.header("origin"))
    });
    // localhost is explicitly pinned to loopback by the production transport;
    // the change from an IP host makes this a real cross-site redirect.
    let mut middle_url = middle.url("/middle");
    middle_url.set_host(Some("localhost")).unwrap();
    let start = Server::new(move |request| {
        if request.path == "/start" {
            Reply::redirect(302, middle_url.as_str())
        } else {
            Reply::ok().cors(request.header("origin"))
        }
    });
    *return_to.lock().unwrap() = start.url("/final").to_string();
    let store = CookieStore::new();
    for pair in [
        "strict=one; SameSite=Strict; Path=/",
        "lax=two; SameSite=Lax; Path=/",
        "none=three; SameSite=None; Secure; Path=/",
    ] {
        seed(&store, &start.url, pair);
    }
    let loader = Loader::with_cookies(store).unwrap();
    let mut request = WebRequest::get(start.url("/start"), start.url.clone());
    request.credentials = CredentialsMode::Include;
    loader.web_fetch(&request, &AtomicBool::new(false)).unwrap();
    let log = start.log();
    assert_eq!(log[0].header("cookie"), "strict=one; lax=two; none=three");
    assert_eq!(middle.log()[0].header("cookie"), "");
    assert_eq!(log[1].header("cookie"), "none=three");
    assert_eq!(log[1].header("origin"), "null");

    loader
        .load_navigation_with_preview(
            &NavigationRequest::get(start.url("/start").as_str()).unwrap(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    let log = start.log();
    assert_eq!(log[2].header("cookie"), "strict=one; lax=two; none=three");
    assert_eq!(middle.log()[1].header("cookie"), "");
    assert_eq!(log[3].header("cookie"), "lax=two; none=three");
}

#[test]
fn public_cookie_storage_enforces_psl_prefixes_http_only_partition_and_persistence() {
    let store = CookieStore::new();
    let origin = Url::parse("https://a.github.io/account/index").unwrap();
    let context = CookieContext::subresource(&origin);
    assert!(!store.receive(&origin, b"bad=1; Domain=github.io", &context));
    assert!(!store.receive(&origin, b"bad=1; Domain=other.github.io", &context));
    assert!(!store.receive(
        &origin,
        b"__Host-bad=1; Secure; Path=/; Domain=a.github.io",
        &context
    ));
    assert!(!store.receive(&origin, b"__SeCuRe-bad=1", &context));
    assert!(store.receive(
        &origin,
        b"__Host-Http-secret=1; Secure; HttpOnly; Path=/; Max-Age=3600",
        &context
    ));
    assert!(!store.set_document_cookie(&origin, "__Host-Http-secret=2; Secure; HttpOnly; Path=/"));
    assert!(!store.set_document_cookie(&origin, "__Host-Http-secret=; Secure; Path=/; Max-Age=0"));
    assert_eq!(store.document_cookie(&origin), "");
    assert!(store.receive(&origin, b"session=1; Path=/", &context));
    let tracker = Url::parse("https://tracker.test/").unwrap();
    let other = CookieContext::subresource(&Url::parse("https://b.github.io/").unwrap());
    assert!(store.receive(
        &tracker,
        b"partition=a; Secure; SameSite=None; Partitioned; Max-Age=3600",
        &context
    ));
    assert!(store.receive(
        &tracker,
        b"partition=b; Secure; SameSite=None; Partitioned; Max-Age=3600",
        &other
    ));
    assert_eq!(store.header(&tracker, &context), b"partition=a");
    assert_eq!(store.header(&tracker, &other), b"partition=b");
    let restored = CookieStore::new();
    restored
        .restore_persistent(&store.export_persistent().unwrap())
        .unwrap();
    assert_eq!(restored.header(&origin, &context), b"__Host-Http-secret=1");
    assert_eq!(restored.document_cookie(&origin), "");
    assert_eq!(restored.header(&tracker, &context), b"partition=a");
    assert_eq!(restored.header(&tracker, &other), b"partition=b");
}

#[test]
fn persistent_nameless_protected_pairs_are_rejected_atomically() {
    let origin = Url::parse("https://example.test/").unwrap();
    let store = CookieStore::new();
    seed(&store, &origin, "ordinary=one; Max-Age=3600; Path=/");
    let mut encoded: serde_json::Value =
        serde_json::from_slice(&store.export_persistent().unwrap()).unwrap();
    encoded["cookies"][0]["name"] = serde_json::json!("");
    for value in [
        "__Host-forged=evil",
        "__Secure-forged=evil",
        "__Http-forged=evil",
    ] {
        encoded["cookies"][0]["value"] = serde_json::json!(value);
        let before = store.header(&origin, &CookieContext::subresource(&origin));
        assert!(
            store
                .restore_persistent(&serde_json::to_vec(&encoded).unwrap())
                .is_err(),
            "restored protected nameless value: {value}"
        );
        assert_eq!(
            store.header(&origin, &CookieContext::subresource(&origin)),
            before
        );
    }
}

#[test]
fn persistent_partition_site_is_validated_and_counted_in_the_byte_budget() {
    let origin = Url::parse("https://tracker.test/").unwrap();
    let context = CookieContext::subresource(&Url::parse("https://client.test/").unwrap());
    let store = CookieStore::new();
    assert!(store.receive(
        &origin,
        b"partition=one; Max-Age=3600; SameSite=None; Secure; Partitioned",
        &context
    ));
    let baseline = store.export_persistent().unwrap();
    let mut encoded: serde_json::Value = serde_json::from_slice(&baseline).unwrap();
    for domain in ["not a domain".to_owned(), "a".repeat(16 * 1024 * 1024 + 1)] {
        encoded["cookies"][0]["partition"]["domain"] = serde_json::json!(domain);
        assert!(
            store
                .restore_persistent(&serde_json::to_vec(&encoded).unwrap())
                .is_err(),
            "invalid/over-budget partition site accepted"
        );
        assert_eq!(store.header(&origin, &context), b"partition=one");
    }
}

#[test]
fn persistent_cookie_restore_cannot_bypass_the_per_domain_count_quota() {
    let origin = Url::parse("https://example.test/").unwrap();
    let context = CookieContext::subresource(&origin);
    let store = CookieStore::new();
    seed(&store, &origin, "existing=untouched; Max-Age=3600; Path=/");
    let mut encoded: serde_json::Value =
        serde_json::from_slice(&store.export_persistent().unwrap()).unwrap();
    let mut cookies = Vec::new();
    for index in 0..181 {
        let mut cookie = encoded["cookies"][0].clone();
        cookie["name"] = serde_json::json!(format!("cookie{index}"));
        cookies.push(cookie);
    }
    encoded["cookies"] = serde_json::Value::Array(cookies);
    assert!(
        store
            .restore_persistent(&serde_json::to_vec(&encoded).unwrap())
            .is_err()
    );
    assert_eq!(store.len(), 1);
    assert_eq!(store.header(&origin, &context), b"existing=untouched");
}

#[test]
fn initially_cross_site_redirect_to_client_does_not_restore_same_site_cookies() {
    let destination = Server::new(|_| Reply::ok().cors("null"));
    let mut client = destination.url.clone();
    client.set_host(Some("localhost")).unwrap();
    let target = client.clone();
    let origin = client.origin().ascii_serialization();
    let middle = Server::new(move |_| Reply::redirect(302, target.as_str()).cors(&origin));
    let store = CookieStore::new();
    for value in [
        "strict=1; SameSite=Strict; Path=/",
        "lax=1; SameSite=Lax; Path=/",
        "none=1; SameSite=None; Secure; Path=/",
    ] {
        seed(&store, &client, value);
    }
    let loader = Loader::with_cookies(store).unwrap();
    let mut request = WebRequest::get(middle.url.clone(), client.clone());
    request.credentials = CredentialsMode::Include;
    loader.web_fetch(&request, &AtomicBool::new(false)).unwrap();
    let fetched = destination
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .header("cookie")
        .to_owned();
    assert_eq!(fetched, "none=1");
    let request = NavigationRequest {
        url: middle.url.clone(),
        method: "GET".into(),
        body: None,
        content_type: None,
        context: CookieContext::navigation(Some(&client), &middle.url, "GET"),
        initiator: Some(client),
    };
    loader
        .load_navigation_with_preview(&request, &AtomicBool::new(false), |_| {})
        .unwrap();
    let navigated = destination
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .header("cookie")
        .to_owned();
    assert_eq!(navigated, "lax=1; none=1");
}
#[test]
fn cancellation_while_waiting_for_headers_cannot_commit_late_cookies() {
    for fetch_api in [true, false] {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let server = Server::new(move |_| {
            flag.store(true, Ordering::Relaxed);
            Reply::ok().header("Set-Cookie", "late=must-not-commit; Max-Age=3600; Path=/")
        });
        let store = CookieStore::new();
        let loader = Loader::with_cookies(store.clone()).unwrap();
        if fetch_api {
            let error = loader
                .web_fetch(
                    &WebRequest::get(server.url.clone(), server.url.clone()),
                    &cancel,
                )
                .unwrap_err();
            assert_eq!(error.kind, FetchErrorKind::Aborted);
        } else {
            assert!(loader.load(server.url.as_str(), &cancel).is_err());
        }
        assert!(store.is_empty());
    }
}

#[test]
fn remote_file_base_cannot_authorize_local_script_or_stylesheet_reads() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/local-boundary-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("fixture.js"),
        "window.localBoundaryExecuted=true;",
    )
    .unwrap();
    std::fs::write(directory.join("fixture.css"), "body{color:red}").unwrap();
    let base = Url::from_directory_path(directory.canonicalize().unwrap()).unwrap();
    let html = format!(
        "<!doctype html><base href='{base}'><link rel=stylesheet href='fixture.css'><script src='fixture.js'></script><p>Remote document"
    );
    let server = Server::new(move |_| Reply {
        status: 200,
        headers: vec![],
        body: html.clone(),
    });
    let mut page = Loader::new()
        .unwrap()
        .load(server.url.as_str(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(page.scripts.len(), 1);
    assert!(page.scripts[0].source.is_none());
    assert!(page.linked_stylesheets.is_empty());
    let mut runtime = rust_browser_engine::script::ScriptRuntime::for_page(&mut page)
        .unwrap()
        .unwrap();
    assert_eq!(
        runtime.eval("typeof localBoundaryExecuted;").unwrap(),
        rust_browser_javascript::Value::from("undefined")
    );
    std::fs::remove_file(directory.join("fixture.js")).unwrap();
    std::fs::remove_file(directory.join("fixture.css")).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn https_subresource_policy_checks_initial_and_redirect_target_before_network() {
    let client = Url::parse("https://secure.example.test/").unwrap();
    let untrusted = Url::parse("http://untrusted.example.test/script.js").unwrap();
    let loader = Loader::new().unwrap();
    let error = loader
        .load_navigation_with_preview(
            &NavigationRequest::subresource(untrusted.clone(), &client),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
    assert!(error.contains("Mixed content"), "{error}");
    let server = Server::new(move |_| Reply::redirect(302, untrusted.as_str()));
    let error = loader
        .load_navigation_with_preview(
            &NavigationRequest::subresource(server.url.clone(), &client),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
    assert!(error.contains("Mixed content"), "{error}");
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}
