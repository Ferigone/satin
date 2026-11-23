use super::*;
use crate::{
    LoadedPage, Loader,
    network::{CancelSignal, FetchTransfer, StreamingResponse},
    script::ScriptRuntime,
};
use http_body_util::{BodyExt, Full};
use hyper::{
    Method, Request, Response, StatusCode, Version, body::Incoming, server::conn::http2,
    service::service_fn,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rust_browser_javascript::Value;
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    crypto::ring,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use std::{
    convert::Infallible,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_rustls::TlsAcceptor;
use url::Url;

const CA: &[u8] = include_bytes!("../../../tests/fixtures/fetch-upload/ca.pem");
const CERT: &[u8] = include_bytes!("../../../tests/fixtures/fetch-upload/localhost.pem");
const KEY: &[u8] = include_bytes!("../../../tests/fixtures/fetch-upload/localhost-key.pem");
const WAIT: Duration = Duration::from_secs(8);

#[derive(Debug)]
enum Event {
    Headers(Method, String),
    Data(Vec<u8>),
    End,
    Reset,
}

struct Peer {
    url: Url,
    events: mpsc::Receiver<Event>,
    read_gate: Arc<Notify>,
    listener: JoinHandle<()>,
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

impl Peer {
    async fn new() -> Self {
        let config = Arc::new(ring::default_provider());
        let mut tls = ServerConfig::builder_with_provider(config)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(CERT).unwrap()],
                PrivateKeyDer::from_pem_slice(KEY).unwrap(),
            )
            .unwrap();
        tls.alpn_protocols = vec![b"h2".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = Url::parse(&format!("https://{}/", listener.local_addr().unwrap())).unwrap();
        let (events, receive) = mpsc::channel(24);
        let read_gate = Arc::new(Notify::new());
        let peer_gate = read_gate.clone();
        let listener = tokio::spawn(async move {
            while let Ok((io, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let events = events.clone();
                let read_gate = peer_gate.clone();
                tokio::spawn(async move {
                    let tls = acceptor.accept(io).await.unwrap();
                    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
                    let _ = http2::Builder::new(TokioExecutor::new())
                        .initial_stream_window_size(16_384)
                        .serve_connection(
                            TokioIo::new(tls),
                            service_fn(move |request| {
                                respond(request, events.clone(), read_gate.clone())
                            }),
                        )
                        .await;
                });
            }
        });
        Self {
            url,
            events: receive,
            read_gate,
            listener,
        }
    }
}

async fn receive_body(mut incoming: Incoming, events: mpsc::Sender<Event>) {
    while let Some(frame) = incoming.frame().await {
        match frame {
            Ok(frame) => {
                if let Ok(bytes) = frame.into_data()
                    && !bytes.is_empty()
                {
                    let _ = events.send(Event::Data(bytes.to_vec())).await;
                }
            }
            Err(_) => {
                let _ = events.send(Event::Reset).await;
                return;
            }
        }
    }
    let _ = events.send(Event::End).await;
}

async fn respond(
    request: Request<Incoming>,
    events: mpsc::Sender<Event>,
    read_gate: Arc<Notify>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    assert_eq!(request.version(), Version::HTTP_2);
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let origin = request.headers().get("origin").cloned();
    let mut response = Response::builder();
    if let Some(origin) = origin {
        response = response.header("access-control-allow-origin", origin);
    }
    events
        .send(Event::Headers(method.clone(), path.clone()))
        .await
        .unwrap();
    if method == Method::OPTIONS {
        assert_eq!(
            request
                .headers()
                .get("access-control-request-method")
                .unwrap(),
            "POST"
        );
        if path == "/forbid" {
            return Ok(Response::builder()
                .status(403)
                .body(Full::default())
                .unwrap());
        }
        return Ok(response
            .status(204)
            .header("access-control-allow-methods", "POST")
            .body(Full::default())
            .unwrap());
    }
    if path == "/early" {
        tokio::spawn(receive_body(request.into_body(), events));
        return Ok(response.status(204).body(Full::default()).unwrap());
    }
    if path == "/pressure" {
        read_gate.notified().await;
    }
    receive_body(request.into_body(), events).await;
    let response = match path.as_str() {
        "/redirect302" => response.status(302).header("location", "/land"),
        "/redirect303" => response.status(303).header("location", "/land"),
        "/authenticate" => response
            .status(401)
            .header("www-authenticate", "Basic realm=\"upload\""),
        "/not-retried" => response.status(421),
        _ => response.status(204),
    };
    Ok(response.body(Full::default()).unwrap())
}

fn client() -> Loader {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(CA).unwrap())
        .unwrap();
    let tls = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut loader = Loader::new().unwrap();
    loader.fetch_client = Loader::fetch_client_with_tls(tls).unwrap();
    loader
}

fn runtime(page: &Url) -> ScriptRuntime {
    ScriptRuntime::new(&LoadedPage::from_html(
        page.as_str(),
        "<!doctype html><main></main>",
    ))
    .unwrap()
}

async fn run_upload(
    loader: &Loader,
    realm: &mut ScriptRuntime,
    job: FetchJob,
) -> Result<(StreamingResponse, Option<FetchTransfer>), FetchError> {
    let generation = CancelSignal::new();
    let demand = job.upload.as_ref().unwrap().demand();
    let opened = loader.open_web_fetch_with_upload(
        &job.request,
        job.upload,
        &generation,
        Instant::now() + WAIT,
    );
    tokio::pin!(opened);
    loop {
        tokio::select! {
            biased;
            result = &mut opened => return result,
            () = demand.next() => realm.progress_network_upload(job.id).unwrap(),
        }
    }
}

#[tokio::test]
async fn tls_http2_upload_delivers_data_before_producer_eof_and_guards_half_duplex() {
    let mut peer = Peer::new().await;
    let loader = client();
    let mut realm = runtime(&peer.url);
    realm
        .eval(
            r#"
        var resume, pulls = 0;
        const gate = new Promise(resolve => resume = resolve);
        const body = new ReadableStream({async pull(c) {
          if (++pulls === 1) { c.enqueue(new Uint8Array([11, 12, 13])); return; }
          await gate;
          c.enqueue(new Uint8Array([14, 15, 16]));
          c.close();
        }}, {highWaterMark:0});
        var status;
        fetch('/early', {method:'POST', duplex:'half', body}).then(r => status = r.status);
    "#,
        )
        .unwrap();
    let job = realm.take_network_jobs().pop().unwrap();
    let id = job.id;
    let demand = job.upload.as_ref().unwrap().demand();
    let generation = CancelSignal::new();
    let fetch = loader.open_web_fetch_with_upload(
        &job.request,
        job.upload,
        &generation,
        Instant::now() + WAIT,
    );
    tokio::pin!(fetch);
    let mut gated = false;
    let mut first_piece = false;
    let hold_early = tokio::time::sleep(WAIT);
    tokio::pin!(hold_early);
    let response = loop {
        tokio::select! {
            biased;
            result = &mut fetch => {
                assert!(gated, "Exposed HTTP/2 response headers before source EOF");
                break result.unwrap();
            }
            () = demand.next() => realm.progress_network_upload(id).unwrap(),
            event = peer.events.recv(), if !gated => match event.unwrap() {
                Event::Headers(method, path) => {
                    assert_eq!(method, Method::POST);
                    assert_eq!(path, "/early");
                }
                Event::Data(bytes) => {
                    assert_eq!(bytes, [11, 12, 13]);
                    assert_eq!(realm.eval("status").unwrap(), Value::Undefined);
                    first_piece = true;
                    hold_early.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(25));
                }
                other => panic!("Unexpected pre-EOF event {other:?}"),
            },
            () = &mut hold_early, if !gated => {
                assert!(first_piece, "Server did not receive first upload bytes before EOF");
                gated = true;
                realm.eval("resume()").unwrap();
                realm.pump();
            }
        }
    };
    assert!(response.1.is_none());
    realm.complete_fetch(id, Ok(response.0)).unwrap();
    assert_eq!(realm.eval("status").unwrap(), Value::Number(204.));
    let tail = tokio::time::timeout(WAIT, async {
        let mut tail = Vec::new();
        loop {
            match peer.events.recv().await.unwrap() {
                Event::Data(bytes) => tail.extend(bytes),
                Event::End => return tail,
                other => panic!("Unexpected event {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(tail, [14, 15, 16]);
}

#[tokio::test]
async fn tls_http2_upload_forces_preflight_and_never_replays_a_one_shot_body() {
    let mut peer = Peer::new().await;
    let loader = client();
    let mut other = peer.url.clone();
    other
        .set_port(Some(if other.port() == Some(8443) {
            8444
        } else {
            8443
        }))
        .unwrap();
    for (path, expected, preflight, credentials) in [
        ("/forbid", None, true, "same-origin"),
        ("/cors", Some(StatusCode::NO_CONTENT), true, "same-origin"),
        ("/redirect302", None, false, "same-origin"),
        (
            "/redirect303",
            Some(StatusCode::NO_CONTENT),
            false,
            "same-origin",
        ),
        (
            "/not-retried",
            Some(StatusCode::MISDIRECTED_REQUEST),
            false,
            "same-origin",
        ),
        ("/authenticate", None, false, "same-origin"),
        (
            "/authenticate",
            Some(StatusCode::UNAUTHORIZED),
            false,
            "omit",
        ),
    ] {
        let mut realm = runtime(if preflight { &other } else { &peer.url });
        let target = peer.url.join(path).unwrap();
        realm.eval(&format!(r#"
           var pulls = 0;
           const body = new ReadableStream({{pull(c) {{
             pulls++;
             c.enqueue(new Uint8Array([77]));
             c.close();
           }}}}, {{highWaterMark:0}});
           fetch('{target}', {{method:'POST', duplex:'half', body, credentials:'{credentials}'}}).catch(() => {{}});
        "#)).unwrap();
        let job = realm.take_network_jobs().pop().unwrap();
        let result = run_upload(&loader, &mut realm, job).await;
        assert_eq!(
            result.as_ref().ok().map(|(head, _)| head.metadata.status),
            expected.map(|status| status.as_u16()),
            "{path}"
        );
        let observations: Vec<_> = std::iter::from_fn(|| peer.events.try_recv().ok()).collect();
        let methods: Vec<_> = observations
            .iter()
            .filter_map(|event| match event {
                Event::Headers(method, path) => Some((method.as_str(), path.as_str())),
                _ => None,
            })
            .collect();
        match path {
            "/forbid" => {
                assert_eq!(methods, [("OPTIONS", path)]);
                assert_eq!(realm.eval("pulls").unwrap(), Value::Number(0.));
            }
            "/cors" => assert_eq!(methods, [("OPTIONS", path), ("POST", path)]),
            "/redirect303" => assert_eq!(methods, [("POST", path), ("GET", "/land")]),
            _ => assert_eq!(methods, [("POST", path)]),
        }
    }
}

#[tokio::test]
async fn tls_http2_receive_window_limits_author_pulls_before_the_peer_reads() {
    let mut peer = Peer::new().await;
    let loader = client();
    let mut realm = runtime(&peer.url);
    realm
        .eval(
            r#"
        var pulls = 0;
        const bytes = new Uint8Array(65536);
        const body = new ReadableStream({pull(c) {
          bytes[0] = ++pulls;
          c.enqueue(bytes);
          if (pulls === 12) c.close();
        }}, {highWaterMark:0});
        fetch('/pressure', {method:'POST', duplex:'half', body}).catch(() => {});
    "#,
        )
        .unwrap();
    let job = realm.take_network_jobs().pop().unwrap();
    let generation = CancelSignal::new();
    let demand = job.upload.as_ref().unwrap().demand();
    let fetch = loader.open_web_fetch_with_upload(
        &job.request,
        job.upload,
        &generation,
        Instant::now() + WAIT,
    );
    tokio::pin!(fetch);
    let timer = tokio::time::sleep(WAIT);
    tokio::pin!(timer);
    let mut received_headers = false;
    let mut receiving = false;
    let mut received_end = false;
    let mut received_bytes = 0;
    let mut chunk_heads = Vec::new();
    loop {
        tokio::select! {
            biased;
            result = &mut fetch => {
                assert_eq!(result.unwrap().0.metadata.status, 204);
                while let Ok(event) = peer.events.try_recv() {
                    match event {
                        Event::Data(bytes) => check_pressure_data(&bytes, &mut received_bytes, &mut chunk_heads),
                        Event::End => received_end = true,
                        other => panic!("Unexpected final event: {other:?}"),
                    }
                }
                break;
            }
            () = demand.next() => realm.progress_network_upload(job.id).unwrap(),
            event = peer.events.recv(), if !received_end => match event.unwrap() {
                Event::Headers(method, path) => {
                    assert_eq!(method, Method::POST);
                    assert_eq!(path, "/pressure");
                    assert!(!received_headers);
                    received_headers = true;
                    timer.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(80));
                }
                Event::Data(bytes) => {
                    assert!(receiving);
                    check_pressure_data(&bytes, &mut received_bytes, &mut chunk_heads);
                }
                Event::End => received_end = true,
                other => panic!("Unexpected event while receiving: {other:?}"),
            },
            () = &mut timer, if received_headers && !receiving => {
                let pulls = realm.eval("pulls").unwrap();
                assert!(matches!(pulls, Value::Number(value) if (1.0..=2.0).contains(&value)), "Unexpected pull count while peer withheld flow-control credit: {pulls:?}");
                receiving = true;
                peer.read_gate.notify_one();
            }
        }
    }
    assert!(received_end);
    assert_eq!(received_bytes, 12 * 65536);
    assert_eq!(chunk_heads, (1..=12).collect::<Vec<u8>>());
}

fn check_pressure_data(bytes: &[u8], received_bytes: &mut usize, chunk_heads: &mut Vec<u8>) {
    let next_start = received_bytes.next_multiple_of(65536) - *received_bytes;
    for index in (next_start..bytes.len()).step_by(65536) {
        chunk_heads.push(bytes[index]);
    }
    *received_bytes += bytes.len();
}

#[tokio::test]
async fn aborting_an_active_tls_http2_upload_resets_the_stream_and_cancels_the_source() {
    let mut peer = Peer::new().await;
    let loader = client();
    let mut realm = runtime(&peer.url);
    realm
        .eval(
            r#"
        const controller = new AbortController(), reason = { abortUpload: true };
        var canceled, rejected, pulls = 0;
        const body = new ReadableStream({pull(c) {
          if (++pulls === 1) c.enqueue(new Uint8Array([29]));
          return new Promise(() => {});
        }, cancel(value) { canceled = value; }}, {highWaterMark:0});
        fetch('/hold', {method:'POST', duplex:'half', body, signal:controller.signal})
          .catch(value => rejected = value);
    "#,
        )
        .unwrap();
    let job = realm.take_network_jobs().pop().unwrap();
    let id = job.id;
    let generation = CancelSignal::new();
    let demand = job.upload.as_ref().unwrap().demand();
    let fetch = loader.open_web_fetch_with_upload(
        &job.request,
        job.upload,
        &generation,
        Instant::now() + WAIT,
    );
    tokio::pin!(fetch);
    let mut abort_sent = false;
    loop {
        tokio::select! {
            biased;
            result = &mut fetch => {
                assert!(abort_sent && result.is_err());
                break;
            }
            () = demand.next() => realm.progress_network_upload(id).unwrap(),
            event = peer.events.recv(), if !abort_sent => match event.unwrap() {
                Event::Headers(method, path) => {
                    assert_eq!(method, Method::POST);
                    assert_eq!(path, "/hold");
                }
                Event::Data(bytes) => {
                    assert_eq!(bytes, [29]);
                    abort_sent = true;
                    realm.eval("controller.abort(reason)").unwrap();
                    realm.pump();
                }
                other => panic!("Unexpected pre-abort event {other:?}"),
            }
        }
    }
    assert!(matches!(
        tokio::time::timeout(WAIT, peer.events.recv())
            .await
            .unwrap(),
        Some(Event::Reset)
    ));
    assert_eq!(realm.pending_network(), 0);
    assert_eq!(
        realm
            .eval("canceled === reason && rejected === reason")
            .unwrap(),
        Value::Bool(true)
    );
}

#[tokio::test]
#[ignore = "run with tools/verify-fetch-upload.mjs and its pinned TLS loopback server"]
async fn original_http2_upload_wpt() {
    use std::{env, fs, path::PathBuf};

    let root = PathBuf::from(env::var_os("RUST_BROWSER_UPLOAD_WPT_ROOT").unwrap());
    let url = Url::parse(&env::var("RUST_BROWSER_UPLOAD_WPT_URL").unwrap()).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    let page = LoadedPage::from_html(
        url.as_str(),
        "<!doctype html><meta charset=utf-8><pre id=wpt-receipt></pre>",
    );
    let mut realm = ScriptRuntime::new(&page).unwrap();
    let harness = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/fixtures/web-streams/testharness.js");
    for path in [
        harness,
        root.join("fetch/api/resources/utils.js"),
        root.join("common/utils.js"),
        root.join("common/get-host-info.sub.js"),
        root.join("fetch/api/basic/request-upload.h2.any.js"),
    ] {
        let source = fs::read_to_string(&path).unwrap();
        realm
            .eval(&source)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
    realm.eval("runStreamsTests()").unwrap();
    let loader = client();
    let deadline = Instant::now() + Duration::from_secs(15);
    let finished =
        r#"document.getElementById('wpt-receipt').getAttribute('data-complete') === 'yes'"#;
    while !realm.eval(finished).unwrap().is_truthy() {
        assert!(Instant::now() < deadline, "Pinned upload suite timed out");
        for job in realm.take_network_jobs() {
            replay_fetch(&loader, &mut realm, job).await;
        }
        let next = realm.next_deadline().expect("No upload test tasks remain");
        tokio::time::sleep_until(next.min(deadline).into()).await;
        realm.pump();
    }
    assert!(realm.diagnostics().is_empty(), "{:?}", realm.diagnostics());
    assert_eq!(realm.pending_network(), 0);
    let receipt = realm
        .eval("document.getElementById('wpt-receipt').textContent")
        .unwrap();
    println!(
        "UPLOAD_WPT_RECEIPT {}",
        receipt.as_string().unwrap().to_string_lossy()
    );
}

async fn replay_fetch(loader: &Loader, realm: &mut ScriptRuntime, job: FetchJob) {
    let id = job.id;
    let result = if job.upload.is_some() {
        run_upload(loader, realm, job).await
    } else {
        loader
            .open_web_fetch(&job.request, &CancelSignal::new(), Instant::now() + WAIT)
            .await
    };
    let mut body = None;
    let head = result.map(|(head, transfer)| {
        body = transfer;
        head
    });
    realm.complete_fetch(id, head).unwrap();
    if let Some(body) = body {
        let (events, mut ready) = mpsc::channel(2);
        let receive = body.run(|| async {
            events.send(()).await.unwrap();
        });
        tokio::pin!(receive);
        loop {
            tokio::select! {
                biased;
                () = &mut receive => break,
                _ = ready.recv() => realm.progress_network(id).unwrap(),
            }
        }
        realm.progress_network(id).unwrap();
    }
}
