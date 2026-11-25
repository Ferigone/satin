use rust_browser_engine::{LoadedPage, Loader, script::ScriptRuntime};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

#[test]
fn fatal_script_or_lifecycle_task_discards_queued_microtasks() {
    for source in [
        "queueMicrotask(function(){document.title='ran after fatal';});'x'.repeat(67108865);",
        "document.addEventListener('DOMContentLoaded',function(){queueMicrotask(function(){document.title='ran after fatal';});'x'.repeat(67108865);});",
        "window.onload=function(){queueMicrotask(function(){document.title='ran after fatal';});'x'.repeat(67108865);};",
    ] {
        let mut page = LoadedPage::from_html(
            "https://example.test/",
            &format!("<!doctype html><title>Before</title><script>{source}</script>"),
        );
        let mut runtime = ScriptRuntime::for_page(&mut page).unwrap().unwrap();
        runtime.pump();
        runtime.snapshot(&mut page);
        assert_eq!(page.title(), "Before");
        assert!(!runtime.diagnostics().is_empty());
        runtime.eval("document.title='next task works';").unwrap();
        runtime.snapshot(&mut page);
        assert_eq!(page.title(), "next task works");
    }
}
impl Server {
    fn new(html: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let thread = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !cancelled.load(Ordering::Relaxed) && Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                // Accepted sockets can inherit nonblocking mode on Windows.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_millis(500)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_millis(500)))
                    .unwrap();
                let mut bytes = [0; 8192];
                let mut count = 0;
                while count < bytes.len() {
                    let read = stream.read(&mut bytes[count..]).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    count += read;
                    if bytes[..count]
                        .windows(4)
                        .any(|window| window == b"\r\n\r\n")
                    {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&bytes[..count]);
                let path = request.split_whitespace().nth(1).unwrap_or("");
                let (status, mime, nosniff, body) = match path {
                    "/" => (200, "text/html; charset=utf-8", false, html),
                    "/first.js" => (200, "text/javascript", false, "var order='a';"),
                    "/defer.js" => (
                        200,
                        "application/javascript",
                        false,
                        "order+='d';document.title=order;",
                    ),
                    "/missing.js" => (404, "text/javascript", false, "var bad='404 executed';"),
                    "/nosniff.js" => (200, "text/plain", true, "var bad='nosniff executed';"),
                    "/image.js" => (200, "image/png", false, "var bad='image executed';"),
                    _ => (404, "text/plain", false, "Not found"),
                };
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Type: {mime}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    if nosniff {
                        "X-Content-Type-Options: nosniff\r\n"
                    } else {
                        ""
                    },
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            url,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn external_classic_and_deferred_scripts_execute_in_document_order() {
    let server = Server::new(
        "<!doctype html><script src='first.js'></script><script>order+='i';</script><script defer src='defer.js'></script><p>Ready</p>",
    );
    let mut page = Loader::new()
        .unwrap()
        .load(&server.url, &AtomicBool::new(false))
        .unwrap();
    let runtime = ScriptRuntime::for_page(&mut page).unwrap().unwrap();
    assert_eq!(page.title(), "aid");
    assert_eq!(runtime.executed_scripts(), 3);
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
}

#[test]
fn failed_status_and_blocked_script_mime_never_execute() {
    let server = Server::new(
        "<!doctype html><script src='missing.js'></script><script src='nosniff.js'></script><script src='image.js'></script><script>document.title=typeof bad;</script>",
    );
    let mut page = Loader::new()
        .unwrap()
        .load(&server.url, &AtomicBool::new(false))
        .unwrap();
    let runtime = ScriptRuntime::for_page(&mut page).unwrap().unwrap();
    assert_eq!(page.title(), "undefined");
    assert_eq!(runtime.executed_scripts(), 1);
    assert_eq!(runtime.diagnostics().len(), 3);
    assert_eq!(page.stats.skipped_resources, 3);
}
