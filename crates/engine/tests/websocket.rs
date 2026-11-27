use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use ring::digest::{SHA1_FOR_LEGACY_USE_ONLY, digest};
use rust_browser_engine::{Loader, script::ScriptRuntime};
use rust_browser_javascript::Value;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(4));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}

fn request(stream: &mut TcpStream, line: &str) -> BTreeMap<String, String> {
    let mut reader = BufReader::new(stream);
    let mut actual = String::new();
    reader.read_line(&mut actual).unwrap();
    assert_eq!(actual.trim_end(), line);
    let mut headers = BTreeMap::new();
    loop {
        actual.clear();
        assert_ne!(reader.read_line(&mut actual).unwrap(), 0);
        if actual == "\r\n" {
            break;
        }
        let (name, value) = actual.split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }
    headers
}

#[test]
fn page_connects_and_exchanges_websocket_frames_through_its_profile() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let websocket_url = origin.replacen("http", "ws", 1);
    let expected_origin = origin.clone();
    let server = thread::spawn(move || {
        let mut page = accept(&listener);
        request(&mut page, "GET / HTTP/1.1");
        let html = "<!doctype html><p>Socket fixture";
        write!(
            page,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\nContent-Security-Policy: connect-src 'self'\r\nSet-Cookie: socket_profile=1; Path=/; HttpOnly\r\n\r\n{html}",
            html.len()
        )
        .unwrap();
        drop(page);
        let mut transport = accept(&listener);
        let headers = request(&mut transport, "GET /chat HTTP/1.1");
        assert_eq!(headers["origin"], expected_origin);
        assert_eq!(headers["cookie"], "socket_profile=1");
        assert_eq!(headers["sec-websocket-protocol"], "chat-v1");
        assert_eq!(headers["sec-websocket-version"], "13");
        let challenge = format!(
            "{}258EAFA5-E914-47DA-95CA-C5AB0DC85B11",
            headers["sec-websocket-key"]
        );
        let key = STANDARD.encode(digest(&SHA1_FOR_LEGACY_USE_ONLY, challenge.as_bytes()));
        write!(
            transport,
            "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {key}\r\nSec-WebSocket-Protocol: chat-v1\r\n\r\n"
        )
        .unwrap();
        transport.set_nonblocking(true).unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let transport = tokio::net::TcpStream::from_std(transport).unwrap();
                let mut ws = WebSocketStream::from_raw_socket(transport, Role::Server, None).await;
                tokio::time::timeout(Duration::from_secs(8), async {
                    assert_eq!(
                        ws.next().await.unwrap().unwrap(),
                        Message::Text("subscribe".into())
                    );
                    ws.send(Message::Text("first row".into())).await.unwrap();
                    ws.send(Message::Binary(vec![4, 7].into())).await.unwrap();
                    let Message::Close(Some(frame)) = ws.next().await.unwrap().unwrap() else {
                        panic!("expected a close frame");
                    };
                    assert_eq!(u16::from(frame.code), 1000);
                    assert_eq!(frame.reason.as_str(), "done");
                    ws.flush().await.unwrap();
                })
                .await
                .unwrap();
            });
    });
    let loader = Loader::new().unwrap();
    let page = loader
        .load(&format!("{origin}/"), &AtomicBool::new(false))
        .unwrap();
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.eval(r#"
        var transcript = [], done = false;
        var socket = new WebSocket('/chat', ['chat-v1']);
        transcript.push(socket instanceof EventTarget && socket.readyState === WebSocket.CONNECTING);
        try { socket.send('early'); } catch (e) { transcript.push(e.name); }
        socket.binaryType = 'arraybuffer';
        socket.onopen = function(event) {
            transcript.push(event.isTrusted && socket.readyState === WebSocket.OPEN, socket.protocol);
            socket.send('subscribe');
            transcript.push(socket.bufferedAmount);
        };
        socket.addEventListener('message', function(event) {
            transcript.push(event instanceof MessageEvent && event.isTrusted, event.origin);
            if (typeof event.data === 'string') transcript.push(event.data);
            else {
                transcript.push(new Uint8Array(event.data).join('/'), socket.bufferedAmount);
                socket.close(1000, 'done');
            }
        });
        socket.onclose = function(event) {
            transcript.push(event instanceof CloseEvent, event.isTrusted,
                socket.readyState, event.wasClean, event.code, event.reason);
            done = true;
        };
        socket.onerror = function() { transcript.push('error'); };
    "#).unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    while runtime.eval("done").unwrap() != Value::Bool(true) && Instant::now() < deadline {
        runtime.pump();
        thread::sleep(Duration::from_millis(4));
    }
    assert_eq!(
        runtime.eval("transcript.join('|')").unwrap(),
        Value::from(format!(
            "true|InvalidStateError|true|chat-v1|9|true|{websocket_url}|first row|true|{websocket_url}|4/7|0|true|true|3|true|1000|done"
        ))
    );
    assert!(runtime.diagnostics().is_empty());
    server.join().unwrap();
}
