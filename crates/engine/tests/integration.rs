use rust_browser_engine::{
    Fonts, Layout, LoadedPage, Loader,
    loader::{NavigationPart, NavigationRequest},
    script::DocumentSession,
};
use std::{
    io::{BufRead, BufReader, Cursor, Read, Write},
    net::TcpListener,
    sync::atomic::AtomicBool,
};

#[test]
fn redirect_base_stylesheet_image_and_preview() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    let server = std::thread::spawn(move || {
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0u8; 8192];
            let n = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..n]);
            let route = request.split_whitespace().nth(1).unwrap_or("/");
            let (status,kind,extra,body)=match route{
                "/start"=>("302 Found","text/html","Location: /page/index.html\r\n",vec![]),
                "/page/index.html"=>("200 OK","text/html; charset=utf-8","",br#"<title>Fixture</title><base href="/assets/"><link rel="stylesheet" href="style.css"><p>Hello Rust</p><img src="tiny.png"><a href="next">next</a>"#.to_vec()),
                "/assets/style.css"=>("200 OK","text/css","",b"p {color:#123456} a{display:block}".to_vec()),
                "/assets/tiny.png"=>("200 OK","image/png","",png.clone()),
                _=>panic!("unexpected route {route}"),
            };
            write!(stream,"HTTP/1.1 {status}\r\nContent-Type: {kind}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    let mut preview_count = 0;
    let page = Loader::new()
        .unwrap()
        .load_with_preview(
            &format!("{origin}/start#chapter"),
            &AtomicBool::new(false),
            |page| {
                preview_count += 1;
                assert_eq!(page.title(), "Fixture");
                assert_eq!(page.stylesheets.len(), 1);
                assert!(page.images.is_empty());
            },
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(preview_count, 1);
    assert_eq!(page.url, format!("{origin}/page/index.html#chapter"));
    assert_eq!(
        page.resolve("next").unwrap(),
        format!("{origin}/assets/next")
    );
    assert_eq!(page.images.len(), 1);
    assert_eq!(page.stats.resources, 3);
}

#[test]
fn streamed_http_navigation_keeps_the_document_fragment_off_the_wire() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for _ in 0..7 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut line = String::new();
            let mut reader = BufReader::new(&mut stream);
            reader.read_line(&mut line).unwrap();
            let route = line.split_ascii_whitespace().nth(1).unwrap();
            for header in reader.lines() {
                if header.unwrap().is_empty() {
                    break;
                }
            }
            let response = match route {
                "/section?edition=2" => {
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 0\r\n"
                }
                "/inherit" => {
                    "HTTP/1.1 302 Found\r\nLocation: /section?edition=2\r\nContent-Length: 0\r\n"
                }
                "/replace" => {
                    "HTTP/1.1 302 Found\r\nLocation: /section?edition=2#new\r\nContent-Length: 0\r\n"
                }
                "/clear" => {
                    "HTTP/1.1 302 Found\r\nLocation: /section?edition=2#\r\nContent-Length: 0\r\n"
                }
                _ => panic!("unexpected request target: {route}"),
            };
            write!(stream, "{response}Connection: close\r\n\r\n").unwrap();
        }
    });
    let loader = Loader::new().unwrap();
    let cancel = AtomicBool::new(false);
    for (source, expected_fragment) in [
        ("/section?edition=2#intro", "intro"),
        ("/inherit#intro", "intro"),
        ("/replace#intro", "new"),
        ("/clear#intro", ""),
    ] {
        let request = NavigationRequest::get(&format!("{origin}{source}")).unwrap();
        let mut address = None;
        loader
            .stream_navigation(&request, &cancel, |part| {
                if let NavigationPart::Open(opened) = part {
                    let (page, _) = DocumentSession::start(*opened).unwrap();
                    assert!(address.replace(page.url).is_none());
                }
            })
            .unwrap();
        assert_eq!(
            address.unwrap(),
            format!("{origin}/section?edition=2#{expected_fragment}")
        );
    }
    server.join().unwrap();
}

#[test]
fn layout_reflows_and_hidden_subtree_has_no_links() {
    let fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html(
        "https://example.test/",
        "<style>.hidden{display:none}</style><p>Words wrap onto additional lines when the available width becomes smaller.</p><a href='/visible'>visible</a><div class='hidden'><a href='/hidden'>hidden</a></div>",
    );
    let wide = Layout::build(&page, &fonts, 600.);
    let narrow = Layout::build(&page, &fonts, 160.);
    assert!(narrow.height > wide.height);
    assert!(narrow.links.iter().all(|l| !l.url.ends_with("/hidden")));
    assert!(
        narrow
            .links
            .iter()
            .any(|l| l.url == "https://example.test/visible")
    );
}

#[test]
fn parser_bounds_deep_documents() {
    let text = format!("{}x{}", "<div>".repeat(2000), "</div>".repeat(2000));
    let doc = rust_browser_engine::html::parse(&text);
    assert!(doc.truncated);
    assert!(doc.nodes.len() < 1000);
    let _ = doc.serialize();
}

#[test]
fn cancelled_navigation_does_not_start_request() {
    let result = Loader::new()
        .unwrap()
        .load("http://127.0.0.1:1/", &AtomicBool::new(true));
    assert!(result.unwrap_err().contains("anulowana"));
}

#[test]
fn block_link_padding_is_clickable() {
    let fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html(
        "https://example.test/",
        // The HTML5 parser now creates the implicit body with its UA margin.
        // Reset that margin so these coordinates isolate the link's padding.
        "<!doctype html><style>body{margin:0}</style><a href='/next' style='display:block;width:200px;padding:20px'>go</a>",
    );
    let layout = Layout::build(&page, &fonts, 400.);
    assert_eq!(layout.link_at(2., 2.), Some("https://example.test/next"));
    assert_eq!(layout.link_at(235., 2.), Some("https://example.test/next"));
    assert!(layout.link_at(250., 2.).is_none());
}

#[test]
fn raw_json_navigation_streams_a_large_minified_value_and_shapes_only_visible_rows() {
    use rust_browser_engine::{DrawCommand, layout::Viewport};
    use std::sync::mpsc;
    use std::time::Duration;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/document", listener.local_addr().unwrap());
    let (first_seen, begin_remaining) = mpsc::channel::<()>();
    let total = 64 * 1024 * 1024;
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut header = String::new();
        let mut reader = BufReader::new(&socket);
        while reader.read_line(&mut header).unwrap() != 0 && !header.ends_with("\r\n\r\n") {}
        let prefix = b"[\"<script>\",\"";
        let suffix = b"\",\n\"end\"]";
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/vendor.example+json\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n").unwrap();
        socket.write_all(prefix).unwrap();
        begin_remaining
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let block = [b'x'; 32768];
        let mut left = total - prefix.len() - suffix.len();
        while left > 0 {
            let n = left.min(block.len());
            socket.write_all(&block[..n]).unwrap();
            left -= n;
        }
        socket.write_all(suffix).unwrap();
    });
    let loader = Loader::new().unwrap();
    let mut doc = None;
    let mut first_seen = Some(first_seen);
    let mut bytes = 0;
    let mut eof = false;
    loader
        .stream_navigation(
            &NavigationRequest::get(&address).unwrap(),
            &AtomicBool::new(false),
            |part| match part {
                NavigationPart::Open(opened) => {
                    doc = Some(DocumentSession::start(*opened).unwrap())
                }
                NavigationPart::Text(text, n) => {
                    let (_, session) = doc.as_mut().unwrap();
                    session.feed(&text).unwrap();
                    bytes += n;
                    if let Some(tx) = first_seen.take() {
                        tx.send(()).unwrap();
                    }
                }
                NavigationPart::Finish => {
                    doc.as_mut().unwrap().1.finish_input().unwrap();
                    eof = true;
                }
            },
        )
        .unwrap();
    server.join().unwrap();
    assert!(eof);
    assert_eq!(bytes, total);
    let (page, mut session) = doc.unwrap();
    let fonts = Fonts::new().unwrap();
    let viewport = Viewport {
        width: 400.,
        height: 95.,
        ..Default::default()
    };
    let mut realm = None;
    let mut page = page;
    session.advance(&mut page, &mut realm, viewport).unwrap();
    assert!(realm.is_none());
    assert_eq!(page.document.nodes.len(), 1);
    let first = Layout::build_with_viewport(&page, &fonts, viewport);
    assert!(first.height > 1_000_000.);
    assert!(first.commands.len() <= 8);
    let end = Layout::build_with_viewport(
        &page,
        &fonts,
        Viewport {
            scroll_y: first.height - viewport.height,
            ..viewport
        },
    );
    let expected = fonts.shape("\"end\"]", 14., false, false, true);
    assert!(end.commands.len() <= 8);
    assert!(end.commands.iter().any(|cmd| matches!(cmd,DrawCommand::Text {run,..} if run.glyphs.iter().map(|g| g.id).eq(expected.glyphs.iter().map(|g| g.id)))));
}

#[test]
fn raw_json_size_limit_counts_decompressed_bytes_for_chunked_gzip() {
    use flate2::{Compression, write::GzEncoder};
    use rust_browser_engine::network::FETCH_RESPONSE_BODY_LIMIT;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/large", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(&stream);
        let mut headers = String::new();
        while reader.read_line(&mut headers).unwrap() != 0 && !headers.ends_with("\r\n\r\n") {}
        let mut zip = GzEncoder::new(Vec::new(), Compression::fast());
        let block = [b' '; 65536];
        for _ in 0..FETCH_RESPONSE_BODY_LIMIT / block.len() {
            zip.write_all(&block).unwrap();
        }
        zip.write_all(b"0").unwrap();
        let compressed = zip.finish().unwrap();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",compressed.len()).unwrap();
        stream.write_all(&compressed).unwrap();
        stream.write_all(b"\r\n0\r\n\r\n").unwrap();
    });
    let mut received = 0;
    let mut finished = false;
    let result = Loader::new().unwrap().stream_navigation(
        &NavigationRequest::get(&address).unwrap(),
        &AtomicBool::new(false),
        |part| match part {
            NavigationPart::Text(_, bytes) => received += bytes,
            NavigationPart::Finish => finished = true,
            NavigationPart::Open(_) => {}
        },
    );
    server.join().unwrap();
    assert!(
        result
            .unwrap_err()
            .contains(&FETCH_RESPONSE_BODY_LIMIT.to_string())
    );
    assert!(received <= FETCH_RESPONSE_BODY_LIMIT);
    assert!(!finished);
}
