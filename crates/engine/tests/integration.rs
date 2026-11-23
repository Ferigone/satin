use rust_browser_engine::{Fonts, Layout, LoadedPage, Loader};
use std::{
    io::{Cursor, Read, Write},
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
            &format!("{origin}/start"),
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
    assert_eq!(page.url, format!("{origin}/page/index.html"));
    assert_eq!(
        page.resolve("next").unwrap(),
        format!("{origin}/assets/next")
    );
    assert_eq!(page.images.len(), 1);
    assert_eq!(page.stats.resources, 3);
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
