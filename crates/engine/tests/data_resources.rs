use rust_browser_engine::{
    LoadedPage, Loader,
    network::{CredentialsMode, FetchErrorKind, RequestMode, ResponseType, WebRequest},
    normalize_url,
    script::ScriptRuntime,
};
use std::sync::{Arc, atomic::AtomicBool};
use url::Url;
fn encoded(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|b| format!("%{b:02X}"))
        .collect()
}

#[test]
fn data_fetch_is_readable_in_all_modes_without_network_or_cookies() {
    let loader = Loader::new().unwrap();
    let client = Url::parse("https://example.test/").unwrap();
    for mode in [
        RequestMode::Cors,
        RequestMode::NoCors,
        RequestMode::SameOrigin,
    ] {
        let mut request = WebRequest::get(
            Url::parse("data:text/plain;charset=utf-8;base64,xbw=#fragment").unwrap(),
            client.clone(),
        );
        request.mode = mode;
        request.credentials = CredentialsMode::Include;
        let result = loader.web_fetch(&request, &AtomicBool::new(false)).unwrap();
        assert_eq!(result.response_type, ResponseType::Basic);
        assert_eq!(result.status, 200);
        assert_eq!(result.body.unwrap().as_slice(), "ż".as_bytes());
        assert!(!result.url.contains('#'));
        assert_eq!(
            result.headers,
            [("content-type".into(), "text/plain;charset=utf-8".into())]
        );
    }
    let mut request = WebRequest::get(Url::parse("data:,body").unwrap(), client);
    request.method = "POST".into();
    request.body = Some(Arc::new(b"request".to_vec()));
    assert_eq!(
        loader
            .web_fetch(&request, &AtomicBool::new(false))
            .unwrap()
            .body
            .unwrap()
            .as_slice(),
        b"body"
    );
    request.method = "HEAD".into();
    request.body = None;
    assert!(
        loader
            .web_fetch(&request, &AtomicBool::new(false))
            .unwrap()
            .body
            .is_none()
    );
    assert_eq!(
        loader
            .web_fetch(&request, &AtomicBool::new(true))
            .unwrap_err()
            .kind,
        FetchErrorKind::Aborted
    );
}
#[test]
fn script_fetch_accepts_a_large_inline_body_with_separate_network_url_bounds() {
    let page = LoadedPage::from_html("https://example.test/", "<!doctype html>");
    let loader = Loader::new().unwrap();
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.eval(r#"
        var result = [];
        fetch('data:application/octet-stream;base64,' + 'YWJj'.repeat(87500))
            .then(r => r.arrayBuffer())
            .then(b => result.push(b.byteLength, new Uint8Array(b)[b.byteLength - 1]));
    "#).unwrap();
    let requests = runtime.take_network_requests();
    assert_eq!(requests.len(), 1);
    let (id, request) = &requests[0];
    assert_eq!(request.url.scheme(), "data");
    let response = loader.web_fetch(request, &AtomicBool::new(false));
    runtime.complete_network(*id, response).unwrap();
    assert_eq!(
        runtime.eval("result.join(',')").unwrap(),
        rust_browser_javascript::Value::from("262500,99")
    );
    assert_eq!(
        runtime.eval("new Request('https://example.test/?' + 'x'.repeat(350000))")
            .unwrap_err().kind,
        rust_browser_javascript::ErrorKind::TypeError,
    );
    assert!(runtime.take_network_requests().is_empty());
}

#[test]
fn data_navigation_styles_scripts_and_svg_images_use_the_real_loader() {
    let svg = "<svg xmlns='http://www.w3.org/2000/svg' width='40' height='30'><rect width='40' height='30' fill='green'/></svg>";
    let script = "document.title='Data pipeline OK';";
    let source = format!(
        "<!doctype html><meta charset=utf-8><link rel=stylesheet href='data:text/css,{}'><img src='data:image/svg+xml,{}'><script src='data:text/javascript,{}'></script>",
        encoded("body{color:green}"),
        encoded(svg),
        encoded(script)
    );
    let uri = format!("data:text/html;charset=utf-8,{}", encoded(&source));
    assert_eq!(normalize_url(&uri).unwrap(), uri);
    let mut page = Loader::new()
        .unwrap()
        .load(&uri, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(page.stylesheets.len(), 1);
    assert_eq!(page.svg_images.len(), 1);
    assert_eq!(page.scripts.len(), 1);
    let runtime = ScriptRuntime::for_page(&mut page).unwrap().unwrap();
    assert_eq!(page.title(), "Data pipeline OK");
    assert!(runtime.diagnostics().is_empty());
    let main_svg = format!("data:image/svg+xml,{}", encoded(svg));
    let page = Loader::new()
        .unwrap()
        .load(&main_svg, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        page.document.namespace(
            page.document
                .connected_nodes()
                .find(|&n| page.document.tag(n) == "svg")
                .unwrap()
        ),
        rust_browser_engine::dom::Namespace::Svg
    );
}
#[test]
fn remote_base_cannot_turn_a_data_document_into_a_file_authority() {
    let page = LoadedPage::from_html(
        "data:text/html,test",
        "<base href='file:///C:/secret/'><script src='private.js'></script>",
    );
    assert_ne!(page.url, "file:///C:/secret/");
    // The public network Fetch path deliberately has no file capability.
    let request = WebRequest::get(
        Url::parse("file:///C:/secret/private.js").unwrap(),
        Url::parse(&page.url).unwrap(),
    );
    assert_eq!(
        Loader::new()
            .unwrap()
            .web_fetch(&request, &AtomicBool::new(false))
            .unwrap_err()
            .kind,
        FetchErrorKind::Unsupported
    );
}

#[test]
fn a_remote_initiator_cannot_open_a_local_file_as_a_document() {
    // Read an existing harmless fixture for the browser-initiated control.
    // Never probe a Windows network share to verify this policy.
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/pages/modules.html");
    let local = Url::from_file_path(file).unwrap();
    let loader = Loader::new().unwrap();
    let cancel = AtomicBool::new(false);
    let mut request = rust_browser_engine::loader::NavigationRequest::get(local.as_str()).unwrap();
    assert!(loader.open_navigation(&request, &cancel).is_ok());
    request.initiator = Some(Url::parse("https://example.test/").unwrap());
    let error = loader
        .open_navigation(&request, &cancel)
        .err()
        .expect("remote file navigation must be denied");
    assert!(error.contains("lokalnych plików"), "{error}");
}
