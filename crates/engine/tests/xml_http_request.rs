use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;
use std::{
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener},
    thread::{self, JoinHandle},
    time::Duration,
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
