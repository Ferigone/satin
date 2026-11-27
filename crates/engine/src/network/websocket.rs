use super::{trustworthy, validate_url};
use crate::{
    cookies::CookieContext,
    loader::{ConnectPolicy, Loader},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use reqwest::{
    StatusCode, Version,
    header::{self, HeaderMap, HeaderValue},
};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::mpsc, time::timeout};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{CloseFrame, Role, WebSocketConfig},
    },
};
use tokio_util::sync::CancellationToken;
use url::Url;

pub(crate) const MAX_MESSAGE: usize = 4 * 1024 * 1024;
const QUEUE: usize = 8;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(12);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

type Waker = Arc<dyn Fn() + Send + Sync>;
pub(crate) type WakeSlot = Arc<Mutex<Option<Waker>>>;

pub(crate) enum Notification {
    Open(String),
    Text(String),
    Binary(Vec<u8>),
    Sent(usize),
    Closed {
        code: u16,
        reason: String,
        clean: bool,
    },
}

pub(crate) struct Connection {
    pub(crate) commands: mpsc::Sender<Message>,
    pub(crate) events: mpsc::Receiver<Notification>,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Connection {
    pub(crate) fn abort(&self) {
        self.cancel.cancel();
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

struct Opening {
    client: Url,
    url: Url,
    protocols: Vec<String>,
    policy: ConnectPolicy,
}

pub(crate) fn connect(
    loader: Option<&Loader>,
    client: Url,
    url: Url,
    protocols: Vec<String>,
    policy: ConnectPolicy,
    wake: WakeSlot,
) -> Connection {
    let (commands, incoming) = mpsc::channel(QUEUE);
    let (outgoing, mut events) = mpsc::channel(QUEUE);
    let cancel = CancellationToken::new();
    let task = loader.map(|loader| {
        let loader = loader.clone();
        let cancelled = cancel.clone();
        let opening = Opening {
            client,
            url,
            protocols,
            policy,
        };
        loader.transport_handle().clone().spawn(async move {
            let end = tokio::select! {
                _ = cancelled.cancelled() => abnormal(),
                result = run(loader, opening, incoming, &outgoing, &wake) => result,
            };
            notify(&outgoing, &wake, end).await;
        })
    });
    if task.is_none() {
        events.close();
    }
    Connection {
        commands,
        events,
        cancel,
        task,
    }
}

async fn notify(queue: &mpsc::Sender<Notification>, wake: &WakeSlot, event: Notification) -> bool {
    if queue.send(event).await.is_err() {
        return false;
    }
    let callback = wake.lock().ok().and_then(|slot| slot.clone());
    if let Some(callback) = callback {
        callback();
    }
    true
}

fn abnormal() -> Notification {
    Notification::Closed {
        code: 1006,
        reason: String::new(),
        clean: false,
    }
}

async fn run(
    loader: Loader,
    opening: Opening,
    mut commands: mpsc::Receiver<Message>,
    events: &mpsc::Sender<Notification>,
    wake: &WakeSlot,
) -> Notification {
    let Ok(Some((transport, protocol))) =
        timeout(HANDSHAKE_TIMEOUT, handshake(&loader, opening)).await
    else {
        return abnormal();
    };
    let config = WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_MESSAGE + 1024)
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let mut socket = WebSocketStream::from_raw_socket(transport, Role::Client, Some(config)).await;
    if !notify(events, wake, Notification::Open(protocol)).await {
        return abnormal();
    }
    let mut close_at = None;
    loop {
        tokio::select! {
            command = commands.recv(), if close_at.is_none() => {
                let Some(command) = command else { return abnormal(); };
                let closing = command.is_close();
                let size = command.len();
                if !matches!(timeout(IO_TIMEOUT, socket.send(command)).await, Ok(Ok(()))) {
                    return abnormal();
                }
                if closing {
                    close_at = Some(tokio::time::Instant::now() + CLOSE_TIMEOUT);
                } else if !notify(events, wake, Notification::Sent(size)).await {
                    return abnormal();
                }
            }
            incoming = socket.next() => {
                let Some(Ok(incoming)) = incoming else { return abnormal(); };
                let message = match incoming {
                    Message::Text(text) => Notification::Text(text.to_string()),
                    Message::Binary(bytes) => Notification::Binary(bytes.to_vec()),
                    Message::Close(frame) => {
                        if close_at.is_none()
                            && !matches!(timeout(IO_TIMEOUT, socket.flush()).await, Ok(Ok(())))
                        {
                            return abnormal();
                        }
                        let (code, reason) = frame.map_or((1005, String::new()), |frame|
                            (u16::from(frame.code), frame.reason.to_string()));
                        return Notification::Closed { code, reason, clean: true };
                    }
                    Message::Ping(_) => {
                        if !matches!(timeout(IO_TIMEOUT, socket.flush()).await, Ok(Ok(()))) {
                            return abnormal();
                        }
                        continue;
                    }
                    _ => continue,
                };
                if !notify(events, wake, message).await {
                    return abnormal();
                }
            }
            _ = async { tokio::time::sleep_until(close_at.unwrap()).await }, if close_at.is_some() => {
                return abnormal();
            }
        }
    }
}

pub(crate) fn closing(code: Option<u16>, reason: String) -> Message {
    Message::Close(code.map(|code| CloseFrame {
        code: code.into(),
        reason: reason.into(),
    }))
}

fn response_token(headers: &HeaderMap, name: header::HeaderName, token: &str) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|value| value.trim().eq_ignore_ascii_case(token))
}

async fn handshake(loader: &Loader, opening: Opening) -> Option<(reqwest::Upgraded, String)> {
    let Opening {
        client,
        mut url,
        protocols,
        policy,
    } = opening;
    let scheme = if url.scheme() == "wss" {
        "https"
    } else {
        "http"
    };
    url.set_scheme(scheme).ok()?;
    if validate_url(&url).is_err()
        || !policy.allows(&client, &url, false)
        || (client.scheme() == "https" && !trustworthy(&url))
    {
        return None;
    }
    let mut nonce = [0; 16];
    SystemRandom::new().fill(&mut nonce).ok()?;
    let key = STANDARD.encode(nonce);
    let mut challenge = key.clone();
    challenge.push_str("258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    // The RFC 6455 upgrade digest is a protocol challenge, independent of TLS.
    let accept = STANDARD.encode(digest::digest(
        &digest::SHA1_FOR_LEGACY_USE_ONLY,
        challenge.as_bytes(),
    ));
    let mut context = CookieContext::subresource(&client);
    context.redirect_to(&url, "GET");
    let mut request = loader
        .websocket_client
        .get(url.clone())
        .version(Version::HTTP_11)
        .header(header::UPGRADE, "websocket")
        .header(header::CONNECTION, "Upgrade")
        .header(header::ORIGIN, client.origin().ascii_serialization())
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::PRAGMA, "no-cache")
        .header("sec-websocket-key", key)
        .header("sec-websocket-version", "13");
    if !protocols.is_empty() {
        request = request.header("sec-websocket-protocol", protocols.join(", "));
    }
    let cookies = loader.cookies.header(&url, &context);
    if !cookies.is_empty() {
        request = request.header(header::COOKIE, HeaderValue::from_bytes(&cookies).ok()?);
    }
    let response = request.send().await.ok()?;
    let headers = response.headers();
    if response.status() != StatusCode::SWITCHING_PROTOCOLS
        || !response_token(headers, header::UPGRADE, "websocket")
        || !response_token(headers, header::CONNECTION, "upgrade")
        || single_value(headers, "sec-websocket-accept") != Some(Some(accept.as_str()))
        || headers.contains_key("sec-websocket-extensions")
    {
        return None;
    }
    let protocol = match single_value(headers, "sec-websocket-protocol")? {
        Some(value) if protocols.iter().any(|protocol| protocol == value) => value.to_owned(),
        None if protocols.is_empty() => String::new(),
        _ => return None,
    };
    for value in headers.get_all(header::SET_COOKIE) {
        loader.cookies.receive(&url, value.as_bytes(), &context);
    }
    Some((response.upgrade().await.ok()?, protocol))
}

fn single_value<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<Option<&'a str>> {
    let mut values = headers.get_all(name).iter();
    let value = match values.next() {
        Some(value) => Some(value.to_str().ok()?),
        None => None,
    };
    values.next().is_none().then_some(value)
}
