use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
};

#[test]
fn http_metadata_redacts_url_and_excludes_non_http() {
    let mut request = stylesheet("https://example.test/", None);
    let observation = HttpResourceObservation::capture(
        &request,
        "https://user:pass@[::1]:8443/style.css?secret=yes#private",
        200,
        Some(0),
        None,
    )
    .unwrap();
    assert_eq!(observation.url, "https://[::1]:8443/style.css");
    request.url = Url::parse("data:text/css,body{}").unwrap();
    assert!(
        HttpResourceObservation::capture(&request, request.url.as_str(), 200, Some(6), None)
            .is_none()
    );
}

#[test]
fn observes_buffered_http_even_when_resource_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let fixture = std::thread::spawn(move || {
        for (path, reply) in [
            (
                "/redirect",
                concat!(
                    "302 Found\r\nLocation: /missing?secret=yes#private\r\n",
                    "Content-Length: 0\r\nConnection: close\r\n\r\n",
                ),
            ),
            (
                "/missing?secret=yes",
                "404 Not Found\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad",
            ),
            (
                "/wrong-type",
                concat!(
                    "200 OK\r\nContent-Type: text/plain\r\n",
                    "Content-Length: 3\r\nConnection: close\r\n\r\nbad",
                ),
            ),
            (
                "/truncated",
                "200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbad",
            ),
        ] {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = String::new();
            let mut reader = BufReader::new(&mut socket);
            reader.read_line(&mut request).unwrap();
            assert_eq!(request.split_ascii_whitespace().nth(1), Some(path));
            for line in reader.lines() {
                if line.unwrap().is_empty() {
                    break;
                }
            }
            write!(socket, "HTTP/1.1 {reply}").unwrap();
        }
    });

    let loader = Loader::new().unwrap();
    let cancel = CancelSignal::new();
    let observe = |path, cors| {
        let request = stylesheet(&format!("{base}{path}"), cors);
        loader.transport_handle().block_on(
            loader.load_document_resource_observed_cancellable(&request, &cancel),
        )
    };
    let (result, observation) = observe("/redirect", None);
    assert!(result.unwrap_err().starts_with("HTTP 404"));
    let observation = observation.unwrap();
    assert_eq!(observation.method, "GET");
    assert_eq!(observation.kind, ResourceKind::Stylesheet);
    assert_eq!(observation.url, format!("{base}/missing"));
    assert_eq!(observation.status, 404);
    assert_eq!(observation.body_bytes, Some(3));
    assert!(observation.headers_at.unwrap() <= observation.observed_at);

    let (result, observation) = observe("/wrong-type", Some(CredentialsMode::SameOrigin));
    assert!(result.unwrap_err().contains("text/css"));
    let observation = observation.unwrap();
    assert_eq!(observation.status, 200);
    assert_eq!(observation.body_bytes, Some(3));
    assert_eq!(observation.headers_at, None);

    let (result, observation) = observe("/truncated", None);
    assert!(result.is_err());
    assert!(observation.is_none());
    fixture.join().unwrap();
}

fn stylesheet(url: &str, cors: Option<CredentialsMode>) -> ResourceRequest {
    let url = Url::parse(url).unwrap();
    ResourceRequest {
        client: Arc::new(url.join("/index").unwrap()),
        url,
        kind: ResourceKind::Stylesheet,
        cors,
        integrity: None,
        deadline: Instant::now() + Duration::from_secs(3),
    }
}

#[test]
fn linked_style_bundles_use_the_resource_budget_after_decompression() {
    use flate2::{Compression, write::GzEncoder};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let rule = "main { display: grid }";
    let fixture = std::thread::spawn(move || {
        for size in [RESOURCE_LIMIT, RESOURCE_LIMIT + 1] {
            let mut gzip = GzEncoder::new(Vec::new(), Compression::fast());
            gzip.write_all(&vec![b' '; size - rule.len()]).unwrap();
            gzip.write_all(rule.as_bytes()).unwrap();
            let body = gzip.finish().unwrap();
            let (mut socket, _) = listener.accept().unwrap();
            for line in BufReader::new(&socket).lines() {
                if line.unwrap().is_empty() {
                    break;
                }
            }
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            socket.write_all(&body).unwrap();
        }
    });

    let loader = Loader::new().unwrap();
    let fetch = |path| {
        loader.load_document_resource(
            &stylesheet(&format!("{base}/{path}"), None),
            &AtomicBool::new(false),
        )
    };
    let response = fetch("bundle.css").unwrap();
    assert_eq!(response.transferred, RESOURCE_LIMIT);
    assert!(matches!(response.body, ResourceBody::Text(source) if source.ends_with(rule)));
    assert!(fetch("oversize.css").is_err());
    fixture.join().unwrap();
}
