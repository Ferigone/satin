use rust_browser_engine::{LoadedPage, Loader, script::ScriptRuntime};
use rust_browser_javascript::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, atomic::AtomicBool, mpsc},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(5);
const WASM: &[u8] = &[
    0, 97, 115, 109, 1, 0, 0, 0, 1, 5, 1, 96, 0, 1, 127, 3, 2, 1, 0, 7, 7, 1, 3, 114, 117, 110, 0,
    0, 10, 6, 1, 4, 0, 65, 42, 11,
];

fn reply_script(socket: &mut TcpStream, mime: &str, policy: &str, body: &str) {
    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\n{policy}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
}

fn loopback() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    (listener, origin)
}

fn accept(listener: &TcpListener) -> (TcpStream, String) {
    let (stream, _) = listener.accept().unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(TIMEOUT)).unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        headers.push_str(&line);
    }
    (reader.into_inner(), headers)
}

fn worker(page: &LoadedPage, source: &str) -> (ScriptRuntime, mpsc::Receiver<()>) {
    let mut runtime = ScriptRuntime::new(page).unwrap();
    let (notify, wakes) = mpsc::sync_channel(1);
    runtime.set_worker_wake(Arc::new(move || {
        let _ = notify.try_send(());
    }));
    runtime
        .eval(&format!(
            r#"
            document.cookie = "worker=kept; Path=/";
            var results = [], errors = [];
            var workerURL = URL.createObjectURL(new Blob([{}]));
            var worker = new Worker(workerURL);
            worker.onmessage = event => results.push(event.data);
            worker.onerror = event => errors.push(event.message);
            URL.revokeObjectURL(workerURL);
            "#,
            serde_json::to_string(source).unwrap(),
        ))
        .unwrap();
    (runtime, wakes)
}

fn wait_until(runtime: &mut ScriptRuntime, wakes: &mpsc::Receiver<()>, predicate: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        runtime.pump();
        if runtime.eval(predicate).unwrap() == Value::Bool(true) {
            return;
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| {
                panic!(
                    "{predicate}: {error}; {:?}; {:?}",
                    runtime.eval("JSON.stringify([results, errors])"),
                    runtime.diagnostics(),
                );
            });
    }
}

fn chunk(socket: &mut TcpStream, bytes: &[u8]) {
    write!(socket, "{:x}\r\n", bytes.len()).unwrap();
    socket.write_all(bytes).unwrap();
    socket.write_all(b"\r\n").unwrap();
}

#[test]
fn same_origin_chunked_wasm_exposes_headers_before_eof_and_posts_42() {
    let (listener, origin) = loopback();
    let (continue_body, started) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        let (mut socket, request) = accept(&listener);
        assert!(request.starts_with("GET /module.wasm HTTP/1.1\r\n"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("cookie: worker=kept\r\n")
        );
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/wasm\r\n\
                Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        chunk(&mut socket, &WASM[..11]);
        started.recv_timeout(TIMEOUT).unwrap();
        chunk(&mut socket, &WASM[11..]);
        chunk(&mut socket, &[]);
    });
    let page = LoadedPage::from_html(&format!("{origin}/page"), "<!doctype html>");
    let source = format!(
        r#"
        async function run() {{
            const response = fetch("{origin}/module.wasm");
            response.then(r => postMessage([r.status, r.type, r.headers.get("content-type")].join(":")));
            const {{ instance }} = await WebAssembly.instantiateStreaming(response);
            postMessage(instance.exports.run());
        }}
        run().catch(error => postMessage(error.name + ": " + error.message)).finally(() => close());
        "#,
    );
    let (mut runtime, wakes) = worker(&page, &source);
    wait_until(&mut runtime, &wakes, "results.length === 1");
    assert_eq!(
        runtime.eval("results[0]").unwrap(),
        "200:basic:application/wasm".into()
    );
    continue_body.send(()).unwrap();
    wait_until(&mut runtime, &wakes, "results.length === 2");
    assert_eq!(runtime.eval("results[1]").unwrap(), 42.into());
    assert_eq!(runtime.eval("errors.length").unwrap(), 0.into());
    server.join().unwrap();
}

#[test]
fn cross_origin_fetch_uses_the_creator_origin_and_explicit_credentials() {
    // Reserve the creator port until the foreign listener has its own address.
    let (creator_listener, origin) = loopback();
    let (listener, foreign) = loopback();
    assert_ne!(origin, foreign);
    drop(creator_listener);
    let allowed = origin.clone();
    let server = std::thread::spawn(move || {
        for (path, cookie, header) in [
            ("/denied", false, String::new()),
            (
                "/allowed",
                false,
                format!("Access-Control-Allow-Origin: {allowed}\r\n"),
            ),
            (
                "/included",
                true,
                format!(
                    "Access-Control-Allow-Origin: {allowed}\r\nAccess-Control-Allow-Credentials: true\r\n"
                ),
            ),
        ] {
            let (mut socket, request) = accept(&listener);
            assert!(request.starts_with(&format!("GET {path} HTTP/1.1\r\n")));
            let request = request.to_ascii_lowercase();
            assert!(request.contains(&format!("origin: {allowed}\r\n")));
            assert_eq!(request.contains("cookie: worker=kept\r\n"), cookie);
            write!(
                socket,
                "HTTP/1.1 200 OK\r\n{header}Content-Length: 2\r\nConnection: close\r\n\r\nok"
            )
            .unwrap();
        }
    });
    let page = LoadedPage::from_html(&format!("{origin}/page"), "<!doctype html>");
    let source = format!(
        r#"
        async function run() {{
            try {{ await fetch("{foreign}/denied"); postMessage("bad CORS"); }}
            catch (error) {{ postMessage(error.name); }}
            const allowed = await fetch("{foreign}/allowed");
            postMessage(allowed.type + ":" + await allowed.text());
            const included = await fetch("{foreign}/included", {{ credentials: "include" }});
            postMessage(included.type + ":" + await included.text());
        }}
        run().catch(error => postMessage(error.name + ": " + error.message)).finally(() => close());
        "#,
    );
    let (mut runtime, wakes) = worker(&page, &source);
    wait_until(&mut runtime, &wakes, "results.length === 3");
    assert_eq!(
        runtime.eval("results.join('|')").unwrap(),
        "TypeError|cors:ok|cors:ok".into()
    );
    server.join().unwrap();
}

#[test]
fn enforced_csp_from_navigation_blocks_initial_and_redirected_connections() {
    let (listener, origin) = loopback();
    let (blocked, blocked_origin) = loopback();
    let target = blocked_origin.clone();
    let server = std::thread::spawn(move || {
        let (mut page, request) = accept(&listener);
        assert!(request.starts_with("GET /page HTTP/1.1\r\n"));
        page.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 15\r\n\
              Content-Security-Policy: worker-src blob:; default-src 'none'; connect-src 'self'\r\n\
              Connection: close\r\n\r\n<!doctype html>",
        )
        .unwrap();
        let (mut redirect, request) = accept(&listener);
        assert!(request.starts_with("GET /redirect HTTP/1.1\r\n"));
        write!(
            redirect,
            "HTTP/1.1 302 Found\r\nLocation: {target}/blocked\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
    });
    let loader = Loader::new().unwrap();
    let page = loader
        .load(&format!("{origin}/page"), &AtomicBool::new(false))
        .unwrap();
    let source = format!(
        r#"
        async function run() {{
            for (const url of ["{blocked_origin}/direct", "{origin}/redirect"]) {{
                try {{ await fetch(url, {{ mode: "no-cors" }}); postMessage("resolved"); }}
                catch (error) {{ postMessage(error.name + ": " + error.message); }}
            }}
        }}
        run().finally(() => close());
        "#,
    );
    let (mut runtime, wakes) = worker(&page, &source);
    wait_until(&mut runtime, &wakes, "results.length === 2");
    assert_eq!(
        runtime.eval("results.join('|')").unwrap(),
        concat!(
            "TypeError: Content Security Policy blocked this Fetch connection|",
            "TypeError: Content Security Policy blocked this Fetch connection",
        )
        .into()
    );
    blocked.set_nonblocking(true).unwrap();
    assert_eq!(
        blocked.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    server.join().unwrap();
}

#[test]
fn termination_cancels_incomplete_worker_body_without_delivering_stale_results() {
    let (listener, origin) = loopback();
    let (client_closed, observed_close) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        let (mut socket, request) = accept(&listener);
        assert!(request.starts_with("GET /pending HTTP/1.1\r\n"));
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        chunk(&mut socket, b"first");
        // The server deliberately does not send a final chunk. Termination
        // must interrupt the transport, not just suppress a JS message.
        let closed = match socket.read(&mut [0; 1]) {
            Ok(0) => true,
            Err(error) => matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ),
            Ok(_) => false,
        };
        client_closed.send(closed).unwrap();
    });
    let page = LoadedPage::from_html(&format!("{origin}/page"), "<!doctype html>");
    let source = format!(
        r#"
        (async () => {{
            const response = await fetch("{origin}/pending");
            postMessage("headers");
            try {{ await response.arrayBuffer(); postMessage("finished"); }}
            catch (error) {{ postMessage(error.name); }}
        }})();
        "#,
    );
    let (mut runtime, wakes) = worker(&page, &source);
    wait_until(&mut runtime, &wakes, "results.join() === 'headers'");
    runtime.eval("worker.terminate()").unwrap();
    assert!(observed_close.recv_timeout(TIMEOUT).unwrap());
    runtime.pump();
    assert!(!runtime.has_workers());
    assert_eq!(runtime.eval("results.join()").unwrap(), "headers".into());
    server.join().unwrap();
}

#[test]
fn worker_fetches_and_parses_json_larger_than_the_standalone_vm_string_limit() {
    let (listener, origin) = loopback();
    let length = 9 * 1024 * 1024;
    let server = std::thread::spawn(move || {
        let (mut socket, request) = accept(&listener);
        assert!(request.starts_with("GET /data.json "));
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n\"", length + 2).unwrap();
        let block = [b'x'; 32 * 1024];
        for _ in 0..length / block.len() {
            socket.write_all(&block).unwrap();
        }
        socket.write_all(b"\"").unwrap();
    });
    let page = LoadedPage::from_html(&format!("{origin}/page"), "<!doctype html>");
    let src = format!(
        r#"fetch("{origin}/data.json").then(r => r.json()).then(s => postMessage(s.length)).catch(e => postMessage(e.message));"#
    );
    let (mut runtime, wakes) = worker(&page, &src);
    wait_until(&mut runtime, &wakes, "results.length === 1");
    assert_eq!(runtime.eval("results[0]").unwrap(), Value::from(length));
    server.join().unwrap();
}

#[test]
fn import_scripts_uses_the_worker_global_and_obeys_response_policy() {
    let (same_server, origin) = loopback();
    let (foreign_server, foreign) = loopback();
    let same = std::thread::spawn(move || {
        let (mut socket, headers) = accept(&same_server);
        assert!(headers.contains("worker=kept"));
        reply_script(&mut socket, "text/javascript", "", "var imported = ['first'];");
    });
    let other = std::thread::spawn(move || {
        for (path, mime, policy, script) in [
            ("/next.js", "application/javascript", "", "imported.push('second');"),
            ("/wrong.js", "text/plain", "", "imported.push('wrong MIME');"),
            ("/corp.js", "text/javascript", "Cross-Origin-Resource-Policy: same-origin\r\n", "imported.push('wrong origin');"),
            ("/throws.js", "text/javascript", "", "throw new Error('foreign message');"),
        ] {
            let (mut socket, headers) = accept(&foreign_server);
            assert!(headers.starts_with(&format!("GET {path} ")));
            assert!(!headers.contains("worker=kept"));
            reply_script(&mut socket, mime, policy, script);
        }
    });
    let page = LoadedPage::from_html(&origin, "<!doctype html>");
    let script = format!(r#"
        queueMicrotask(() => postMessage('checkpoint'));
        importScripts();
        importScripts('{origin}/first.js', '{foreign}/next.js');
        for (const path of ['wrong.js','corp.js','throws.js']) {{
            try {{ importScripts('{foreign}/' + path); }}
            catch (error) {{ imported.push(error.name); }}
        }}
        postMessage(imported.join(','));
        close();
    "#);
    let (mut runtime, wakes) = worker(&page, &script);
    wait_until(&mut runtime, &wakes, "results.length === 2");
    assert_eq!(runtime.eval("results.join('|')").unwrap(),
        "first,second,NetworkError,NetworkError,NetworkError|checkpoint".into());
    same.join().unwrap();
    other.join().unwrap();
}

#[test]
fn browser_and_worker_base64_use_binary_strings_and_reject_invalid_input() {
    let page = LoadedPage::from_html("https://example.test/", "<!doctype html>");
    let checks = r#"
        (() => {
            let rejected = 0;
            for (const test of [() => btoa("\u0100"), () => atob("A===")]) {
                try { test(); } catch (error) { if (error.name === "InvalidCharacterError") rejected++; }
            }
            try { btoa(); } catch (error) { if (error instanceof TypeError) rejected++; }
            return btoa("\u0000\u00ff") === "AP8=" &&
                atob(" A P8\n").charCodeAt(1) === 255 && rejected === 3;
        })()
    "#;
    let (mut runtime, wakes) = worker(&page, &format!("postMessage({checks}); close();"));
    wait_until(&mut runtime, &wakes, "results.length === 1");
    assert_eq!(runtime.eval("results[0]").unwrap(), Value::Bool(true));
    assert_eq!(runtime.eval(checks).unwrap(), Value::Bool(true));
}

#[test]
fn secure_blob_worker_can_generate_random_values_and_digest() {
    let page = LoadedPage::from_html("https://example.test/", "<!doctype html>");
    let source = r#"
        const array = new Uint32Array(4);
        const alive = isSecureContext && self.crypto === crypto &&
            crypto.getRandomValues(array) === array && array.some(value => value !== 0) &&
            crypto.randomUUID().charAt(14) === '4';
        crypto.subtle.digest('SHA-256', new Uint8Array([97, 98, 99])).then(out => {
            postMessage(alive && new Uint8Array(out).toHex() ===
                'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
            close();
        });
    "#;
    let (mut runtime, wakes) = worker(&page, source);
    wait_until(&mut runtime, &wakes, "results.length === 1");
    assert_eq!(runtime.eval("results[0]").unwrap(), Value::Bool(true));
}

#[test]
fn imported_scripts_check_csp_on_redirect_destinations() {
    let (same_server, origin) = loopback();
    let (foreign_server, foreign) = loopback();
    let target = foreign.clone();
    let server = std::thread::spawn(move || {
        let (mut socket, headers) = accept(&same_server);
        assert!(headers.starts_with("GET /redirect "));
        write!(socket, "HTTP/1.1 302 Found\r\nLocation: {target}/foreign.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let page = LoadedPage::from_html(&origin,
        r#"<meta http-equiv="Content-Security-Policy" content="worker-src blob:; script-src 'none'; script-src-elem 'self'">"#);
    let script = format!(r#"
        try {{ importScripts('{origin}/redirect'); postMessage('allowed'); }}
        catch (error) {{ postMessage(error.name); }}
        close();
    "#);
    let (mut runtime, wakes) = worker(&page, &script);
    wait_until(&mut runtime, &wakes, "results.length === 1");
    assert_eq!(runtime.eval("results[0]").unwrap(), "NetworkError".into());
    foreign_server.set_nonblocking(true).unwrap();
    assert_eq!(foreign_server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    server.join().unwrap();
}
