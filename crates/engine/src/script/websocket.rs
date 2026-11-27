use super::{
    Shared, abort, arg, blobs, document_base, event_target, getter, message_ports, webidl,
};
use crate::{
    loader::{ConnectPolicy, LoadedPage, Loader},
    network::websocket::{self, Connection, MAX_MESSAGE, Notification, WakeSlot},
};
use rust_browser_javascript::{JsError, RootId, Value, Vm};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashSet},
    rc::Rc,
    sync::Arc,
};
use tokio::sync::mpsc::error::TryRecvError;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

const MAX_CONNECTIONS: usize = 16;
const MAX_PROTOCOLS: usize = 32;
const CONNECTING: u32 = 0;
const OPEN: u32 = 1;
const CLOSING: u32 = 2;
const CLOSED: u32 = 3;
const CLOSE_FIELDS: u32 = 225;

struct Socket {
    id: u64,
    url: Url,
    ready: Cell<u32>,
    buffered: Cell<usize>,
    protocol: RefCell<String>,
    array_buffer: Cell<bool>,
    root: Cell<Option<RootId>>,
}

struct Entry {
    object: Value,
    connection: Connection,
}

#[derive(Clone, Default)]
pub(super) struct Sockets {
    entries: Rc<RefCell<BTreeMap<u64, Entry>>>,
    next: Rc<Cell<u64>>,
    cursor: Rc<Cell<u64>>,
    wake: WakeSlot,
    meta_policy: Rc<RefCell<ConnectPolicy>>,
}

fn brand(vm: &Vm, value: &Value) -> Result<Rc<Socket>, JsError> {
    vm.host_data::<Socket>(value)
        .ok_or_else(|| JsError::type_error("Illegal WebSocket receiver"))
}

pub(super) fn update_liveness(vm: &mut Vm, object: &Value) {
    let Some(socket) = vm.host_data::<Socket>(object) else {
        return;
    };
    let types: &[&str] = match socket.ready.get() {
        CONNECTING => &["open", "message", "error", "close"],
        OPEN => &["message", "error", "close"],
        CLOSING => &["error", "close"],
        _ => &[],
    };
    let active = socket.ready.get() != CLOSED
        && (socket.buffered.get() != 0 || event_target::has_listener(vm, object, types));
    match (active, socket.root.get()) {
        (true, None) => socket.root.set(Some(vm.root(object.clone()))),
        (false, Some(root)) => {
            socket.root.set(None);
            vm.unroot(root);
        }
        _ => {}
    }
}

#[derive(Clone)]
pub(super) struct SocketEvents {
    events: event_target::Events,
    close: Value,
    messages: message_ports::MessagePorts,
    blobs: blobs::BlobFactory,
}

impl Sockets {
    pub(super) fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        if let Ok(mut slot) = self.wake.lock() {
            *slot = Some(wake);
        }
    }

    pub(super) fn set_meta_policy(&self, policy: &ConnectPolicy) {
        self.meta_policy.replace(policy.clone());
    }

    pub(super) fn sweep(&self, vm: &Vm) {
        self.entries
            .borrow_mut()
            .retain(|_, entry| vm.host_data::<Socket>(&entry.object).is_some());
    }

    pub(super) fn has_tasks(&self) -> bool {
        self.entries
            .borrow()
            .values()
            .any(|entry| !entry.connection.events.is_empty() || entry.connection.events.is_closed())
    }

    pub(super) fn dispatch_next(
        &self,
        vm: &mut Vm,
        delivery: &SocketEvents,
    ) -> Result<bool, JsError> {
        let (id, object, event) = {
            let mut entries = self.entries.borrow_mut();
            let cursor = self.cursor.get();
            let mut take = |(&id, entry): (&u64, &mut Entry)| {
                let event = match entry.connection.events.try_recv() {
                    Ok(event) => event,
                    Err(TryRecvError::Empty) => return None,
                    Err(TryRecvError::Disconnected) => Notification::Closed {
                        code: 1006,
                        reason: String::new(),
                        clean: false,
                    },
                };
                Some((id, entry.object.clone(), event))
            };
            let next = entries
                .range_mut(cursor..)
                .find_map(&mut take)
                .or_else(|| entries.range_mut(..cursor).find_map(take));
            let Some((id, object, event)) = next else {
                return Ok(false);
            };
            if matches!(event, Notification::Closed { .. }) {
                entries.remove(&id);
            }
            self.cursor.set(id + 1);
            (id, object, event)
        };
        let Some(socket) = vm.host_data::<Socket>(&object) else {
            self.entries.borrow_mut().remove(&id);
            return Ok(true);
        };
        match event {
            Notification::Open(protocol) if socket.ready.get() == CONNECTING => {
                socket.ready.set(OPEN);
                socket.protocol.replace(protocol);
                delivery.send_event(vm, &object, "open")?;
            }
            Notification::Text(text) if socket.ready.get() == OPEN => {
                delivery.send_message(vm, &object, &socket, text.into())?;
            }
            Notification::Binary(bytes) if socket.ready.get() == OPEN => {
                let value = if socket.array_buffer.get() {
                    vm.new_array_buffer(bytes)?
                } else {
                    delivery.blobs.from_bytes(vm, bytes, "")?
                };
                delivery.send_message(vm, &object, &socket, value)?;
            }
            Notification::Sent(bytes) => socket
                .buffered
                .set(socket.buffered.get().saturating_sub(bytes)),
            Notification::Closed {
                code,
                reason,
                clean,
            } => {
                socket.ready.set(CLOSED);
                let outcome = (|| {
                    if !clean {
                        delivery.send_event(vm, &object, "error")?;
                    }
                    let event = close_event(
                        vm,
                        &delivery.close,
                        "close".into(),
                        [false; 3],
                        true,
                        CloseFields {
                            code,
                            reason,
                            clean,
                        },
                    )?;
                    delivery.events.dispatch_scheduled(vm, object.clone(), event)?;
                    Ok(())
                })();
                update_liveness(vm, &object);
                outcome?;
            }
            _ => {}
        }
        update_liveness(vm, &object);
        Ok(true)
    }
}

impl SocketEvents {
    fn send_event(&self, vm: &mut Vm, object: &Value, kind: &str) -> Result<(), JsError> {
        let event = self
            .events
            .make_event(vm, None, kind.into(), [false; 3], true)?;
        self.events.dispatch_scheduled(vm, object.clone(), event).map(|_| ())
    }

    fn send_message(
        &self,
        vm: &mut Vm,
        object: &Value,
        socket: &Socket,
        data: Value,
    ) -> Result<(), JsError> {
        let event = self.messages.received_from_network(
            vm,
            data,
            socket.url.origin().ascii_serialization(),
        )?;
        self.events.dispatch_scheduled(vm, object.clone(), event).map(|_| ())
    }
}

fn protocols(vm: &mut Vm, value: Value, exception: &Value) -> Result<Vec<String>, JsError> {
    if value.is_undefined() {
        return Ok(Vec::new());
    }
    let protocols = if value.as_object().is_some() {
        let method = webidl::iterator_method(vm, &value)?;
        if method.is_nullish() {
            vec![webidl::usv_string(vm, &value, 1024)?]
        } else {
            webidl::sequence(vm, value, method, MAX_PROTOCOLS, |vm, name| {
                webidl::usv_string(vm, &name, 1024)
            })?
        }
    } else {
        vec![webidl::usv_string(vm, &value, 1024)?]
    };
    let mut unique = HashSet::new();
    for protocol in &protocols {
        if reqwest::header::HeaderName::from_bytes(protocol.as_bytes()).is_err()
            || !unique.insert(protocol)
        {
            return Err(abort::dom_exception_error(
                vm,
                exception.clone(),
                "SyntaxError",
                "WebSocket subprotocols must be unique HTTP tokens",
            )?);
        }
    }
    Ok(protocols)
}

fn resolved_url(base: &Url, input: &str) -> Option<Url> {
    let mut url = base.join(input).ok()?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        "ws" => "ws",
        "wss" => "wss",
        _ => return None,
    };
    url.set_scheme(scheme).ok()?;
    if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    Some(url)
}

struct OpeningContext {
    document: Shared,
    loader: Option<Loader>,
    policy: ConnectPolicy,
    exception: Value,
}

fn send_payload(vm: &mut Vm, value: Value) -> Result<Message, JsError> {
    if let Some(blob) = vm.host_data::<blobs::Blob>(&value) {
        if blob.bytes().len() > MAX_MESSAGE {
            return Err(super::unsupported(
                "WebSocket message exceeds its byte budget",
            ));
        }
        vm.charge_work(blob.bytes().len())?;
        return Ok(Message::Binary(blob.bytes().to_vec().into()));
    }
    if vm.is_buffer_source(&value) {
        if vm
            .buffer_source_len(&value)?
            .is_some_and(|length| length > MAX_MESSAGE)
        {
            return Err(super::unsupported(
                "WebSocket message exceeds its byte budget",
            ));
        }
        let bytes = vm.copy_buffer_source(&value)?.unwrap_or_default();
        return Ok(Message::Binary(bytes.into()));
    }
    Ok(Message::Text(
        webidl::usv_string(vm, &value, MAX_MESSAGE)?.into(),
    ))
}

fn unsigned_short(vm: &mut Vm, value: &Value) -> Result<u16, JsError> {
    Ok(vm.to_number(value)?.trunc().rem_euclid(65536.) as u16)
}

pub(super) fn install(
    vm: &mut Vm,
    document: &Shared,
    events: event_target::Events,
    messages: message_ports::MessagePorts,
    blobs: blobs::BlobFactory,
    page: &LoadedPage,
) -> Result<(Sockets, SocketEvents), JsError> {
    let sockets = Sockets::default();
    sockets.set_meta_policy(&page.meta_connect_policy);
    let prototype = vm.new_object_with_prototype(events.target_proto.clone())?;
    let exception = vm.get_global("DOMException")?;
    let opening = OpeningContext {
        document: document.clone(),
        loader: page.fetch_loader.clone(),
        policy: page.connect_policy.clone(),
        exception: exception.clone(),
    };
    let host = sockets.clone();
    let proto = prototype.clone();
    let constructor = webidl::interface(
        vm,
        prototype.clone(),
        "WebSocket",
        1,
        move |vm, _, args, target| {
            let target = target.ok_or_else(|| JsError::type_error("WebSocket requires new"))?;
            webidl::required(args, 1, "WebSocket")?;
            let input = webidl::usv_string(vm, &args[0], 64 * 1024)?;
            let names = protocols(vm, arg(args, 1), &opening.exception)?;
            let (base, client) = {
                let state = opening.document.borrow();
                (document_base(&state), state.url.clone())
            };
            let Some(url) = resolved_url(&base, &input) else {
                return Err(abort::dom_exception_error(
                    vm,
                    opening.exception.clone(),
                    "SyntaxError",
                    "Invalid WebSocket URL",
                )?);
            };
            host.sweep(vm);
            if host.entries.borrow().len() >= MAX_CONNECTIONS {
                return Err(abort::dom_exception_error(
                    vm,
                    opening.exception.clone(),
                    "QuotaExceededError",
                    "Too many WebSocket connections",
                )?);
            }
            let object = webidl::instance(vm, target, &proto)?;
            event_target::attach(vm, object.clone())?;
            let id = host.next.get();
            host.next.set(id + 1);
            vm.set_host_data(
                object.clone(),
                Rc::new(Socket {
                    id,
                    url: url.clone(),
                    ready: Cell::new(CONNECTING),
                    buffered: Cell::new(0),
                    protocol: RefCell::new(String::new()),
                    array_buffer: Cell::new(false),
                    root: Cell::new(None),
                }),
            )?;
            let meta = opening
                .document
                .borrow()
                .parser
                .upgrade()
                .map(|parser| parser.borrow().connect_policy().clone())
                .unwrap_or_else(|| host.meta_policy.borrow().clone());
            let policy = opening.policy.intersect(&meta);
            let connection = websocket::connect(
                opening.loader.as_ref(),
                client,
                url,
                names,
                policy,
                host.wake.clone(),
            );
            host.entries.borrow_mut().insert(
                id,
                Entry {
                    object: object.clone(),
                    connection,
                },
            );
            Ok(object)
        },
    )?;
    for (name, value) in [
        ("CONNECTING", CONNECTING),
        ("OPEN", OPEN),
        ("CLOSING", CLOSING),
        ("CLOSED", CLOSED),
    ] {
        vm.define_data_property(constructor.clone(), name, value.into(), false, true, false)?;
        vm.define_data_property(prototype.clone(), name, value.into(), false, true, false)?;
    }
    getter(vm, prototype.clone(), "url", |vm, this, _| {
        Ok(brand(vm, &this)?.url.as_str().into())
    })?;
    getter(vm, prototype.clone(), "readyState", |vm, this, _| {
        Ok(brand(vm, &this)?.ready.get().into())
    })?;
    getter(vm, prototype.clone(), "bufferedAmount", |vm, this, _| {
        Ok(Value::Number(brand(vm, &this)?.buffered.get() as f64))
    })?;
    getter(vm, prototype.clone(), "protocol", |vm, this, _| {
        Ok(brand(vm, &this)?.protocol.borrow().as_str().into())
    })?;
    getter(vm, prototype.clone(), "extensions", |vm, this, _| {
        brand(vm, &this)?;
        Ok("".into())
    })?;
    let get = vm.new_native_function("get binaryType", |vm, this, _| {
        Ok(if brand(vm, &this)?.array_buffer.get() {
            "arraybuffer"
        } else {
            "blob"
        }
        .into())
    });
    let set = vm.new_native_function("set binaryType", |vm, this, args| {
        let socket = brand(vm, &this)?;
        match vm.to_string(&arg(args, 0))?.to_string_lossy().as_str() {
            "arraybuffer" => socket.array_buffer.set(true),
            "blob" => socket.array_buffer.set(false),
            _ => {}
        }
        Ok(Value::Undefined)
    });
    vm.define_accessor(prototype.clone(), "binaryType", Some(get), Some(set))?;
    for kind in ["open", "message", "error", "close"] {
        event_target::install_handler(
            vm,
            prototype.clone(),
            kind,
            |vm, value| brand(vm, value).map(|_| ()),
            |_, _| Ok(()),
        )?;
    }
    let host = sockets.clone();
    let send_error = exception.clone();
    webidl::operation(vm, prototype.clone(), "send", 1, move |vm, this, args| {
        let socket = brand(vm, &this)?;
        webidl::required(args, 1, "WebSocket.send")?;
        let message = send_payload(vm, args[0].clone())?;
        if socket.ready.get() == CONNECTING {
            return Err(abort::dom_exception_error(
                vm,
                send_error.clone(),
                "InvalidStateError",
                "WebSocket is still connecting",
            )?);
        }
        socket
            .buffered
            .set(socket.buffered.get().saturating_add(message.len()));
        if socket.ready.get() == OPEN
            && let Some(entry) = host.entries.borrow().get(&socket.id)
            && entry.connection.commands.try_send(message).is_err()
        {
            socket.ready.set(CLOSING);
            entry.connection.abort();
        }
        update_liveness(vm, &this);
        Ok(Value::Undefined)
    })?;
    let host = sockets.clone();
    webidl::operation(vm, prototype, "close", 0, move |vm, this, args| {
        let socket = brand(vm, &this)?;
        let code = args
            .first()
            .filter(|value| !value.is_undefined())
            .map(|value| unsigned_short(vm, value))
            .transpose()?;
        let reason = if let Some(value) = args.get(1).filter(|value| !value.is_undefined()) {
            webidl::usv_string(vm, value, MAX_MESSAGE)?
        } else {
            String::new()
        };
        if code.is_some_and(|code| code != 1000 && !(3000..=4999).contains(&code)) {
            return Err(abort::dom_exception_error(
                vm,
                exception.clone(),
                "InvalidAccessError",
                "Invalid WebSocket close code",
            )?);
        }
        if reason.len() > 123 {
            return Err(abort::dom_exception_error(
                vm,
                exception.clone(),
                "SyntaxError",
                "WebSocket close reason exceeds 123 bytes",
            )?);
        }
        let state = socket.ready.get();
        if state < CLOSING {
            socket.ready.set(CLOSING);
            if let Some(entry) = host.entries.borrow().get(&socket.id)
                && (state == CONNECTING
                    || entry
                        .connection
                        .commands
                        .try_send(websocket::closing(code, reason))
                        .is_err())
            {
                entry.connection.abort();
            }
        }
        update_liveness(vm, &this);
        Ok(Value::Undefined)
    })?;
    let close = install_close(vm)?;
    vm.root(close.clone());
    Ok((
        sockets,
        SocketEvents {
            events,
            close,
            messages,
            blobs,
        },
    ))
}

struct CloseFields {
    code: u16,
    reason: String,
    clean: bool,
}

fn close_event(
    vm: &mut Vm,
    prototype: &Value,
    kind: rust_browser_javascript::JsString,
    flags: [bool; 3],
    trusted: bool,
    fields: CloseFields,
) -> Result<Value, JsError> {
    let object = vm.new_object_with_prototype(prototype.clone())?;
    init_close(vm, object, kind, flags, trusted, fields)
}

fn init_close(
    vm: &mut Vm,
    object: Value,
    kind: rust_browser_javascript::JsString,
    flags: [bool; 3],
    trusted: bool,
    fields: CloseFields,
) -> Result<Value, JsError> {
    let event = event_target::init_event(vm, object, kind, flags, trusted)?;
    let data = vm.new_object_with_prototype(Value::Null)?;
    vm.set_host_data(data.clone(), Rc::new(fields))?;
    vm.set_host_slot(event.clone(), CLOSE_FIELDS, data)?;
    Ok(event)
}

fn close_fields(vm: &Vm, value: &Value) -> Result<Rc<CloseFields>, JsError> {
    vm.host_slot(value, CLOSE_FIELDS)
        .and_then(|data| vm.host_data::<CloseFields>(&data))
        .ok_or_else(|| JsError::type_error("Illegal CloseEvent receiver"))
}

fn install_close(vm: &mut Vm) -> Result<Value, JsError> {
    let event = vm.get_global("Event")?;
    let event_prototype = vm.get_property(event, "prototype")?;
    let prototype = vm.new_object_with_prototype(event_prototype)?;
    let proto = prototype.clone();
    webidl::interface(
        vm,
        prototype.clone(),
        "CloseEvent",
        1,
        move |vm, _, args, target| {
            let target = target.ok_or_else(|| JsError::type_error("CloseEvent requires new"))?;
            webidl::required(args, 1, "CloseEvent")?;
            let kind = event_target::kind(vm, &args[0])?;
            let options = arg(args, 1);
            let flags = event_target::flags(vm, &options)?;
            let code = event_target::option(vm, &options, "code")?;
            let code = unsigned_short(vm, &code)?;
            let reason = event_target::option(vm, &options, "reason")?;
            let reason = if reason.is_undefined() {
                String::new()
            } else {
                webidl::usv_string(vm, &reason, MAX_MESSAGE)?
            };
            let clean = event_target::option(vm, &options, "wasClean")?.is_truthy();
            let object = webidl::instance(vm, target, &proto)?;
            init_close(
                vm,
                object,
                kind,
                flags,
                false,
                CloseFields {
                    code,
                    reason,
                    clean,
                },
            )
        },
    )?;
    getter(vm, prototype.clone(), "code", |vm, this, _| {
        Ok(u32::from(close_fields(vm, &this)?.code).into())
    })?;
    getter(vm, prototype.clone(), "reason", |vm, this, _| {
        Ok(close_fields(vm, &this)?.reason.as_str().into())
    })?;
    getter(vm, prototype.clone(), "wasClean", |vm, this, _| {
        Ok(close_fields(vm, &this)?.clean.into())
    })?;
    Ok(prototype)
}
