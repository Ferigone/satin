//! Window XMLHttpRequest over the existing Fetch transport.
mod async_load;
mod progress_event;
#[cfg(test)]
mod timeout_tests;
mod xml_document;
pub(in crate::script) use xml_document::xml_node_type;

use super::{Shared, byte_string, typed, usv};
use crate::{
    Loader,
    loader::{ConnectPolicy, LoadedPage},
    network::{
        self, CancelSignal, FETCH_BODY_LIMIT, FETCH_TIMEOUT, FetchError,
        FetchErrorKind, WebRequest, WebResponse,
    },
    script::{abort, arg, event_target, getter, unsupported, webidl},
};
pub(super) use async_load::Pending;
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};
use std::{
    cell::{OnceCell, RefCell},
    rc::{Rc, Weak},
    sync::{Arc, mpsc},
    time::Instant,
};

const TEXT_SLOT: u32 = 0;
const XML_SLOT: u32 = 1;
const LIVE_METADATA: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Default, PartialEq)]
#[repr(u8)]
enum Ready {
    #[default]
    Unsent,
    Opened,
    HeadersReceived,
    Loading,
    Done,
}

#[derive(Default)]
struct Request {
    ready: Ready,
    synchronous: bool,
    timeout_ms: u32,
    request: Option<WebRequest>,
    response: Option<WebResponse>,
    pending: Option<u64>,
    generation: u64,
    xml: Option<xml_document::Source>,
}

impl Request {
    fn metadata_size(&self) -> usize {
        self.request.as_ref().map_or(0, |request| {
            request.url.as_str().len()
                + request.client_url.as_str().len()
                + request.method.len()
                + super::bytes(&request.headers)
        }) + self.response.as_ref().map_or(0, |response| {
            super::bytes(&response.headers)
                + response.url.capacity()
                + response.status_text.capacity()
        }) + self.xml.as_ref().map_or(0, xml_document::Source::size)
            + 256
    }

    fn can_send(&self) -> bool {
        self.ready == Ready::Opened && self.request.is_some()
    }
}

pub(super) struct Host {
    fetch: Shared,
    loader: OnceCell<Result<Loader, String>>,
    cookies: crate::cookies::CookieStore,
    headers: ConnectPolicy,
    events: event_target::Events,
    exception: Value,
    progress_proto: Value,
    xml: xml_document::Host,
    live: RefCell<Vec<Weak<RefCell<Request>>>>,
    #[cfg(test)]
    test_now: std::cell::Cell<Option<Instant>>,
}

impl Host {
    fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = self.test_now.get() {
            return now;
        }
        Instant::now()
    }

    fn exception(&self, vm: &mut Vm, name: &str, message: &str) -> JsError {
        abort::dom_exception_error(vm, self.exception.clone(), name, message)
            .unwrap_or_else(|error| error)
    }

    fn policy(&self) -> ConnectPolicy {
        let parser = self
            .fetch
            .borrow()
            .document
            .upgrade()
            .and_then(|state| state.borrow().parser.upgrade());
        if let Some(parser) = parser {
            self.headers.intersect(parser.borrow().connect_policy())
        } else {
            self.headers.intersect(&self.fetch.borrow().xhr_meta_policy)
        }
    }

    fn budget(
        &self,
        vm: &mut Vm,
        replacing: Option<&Rc<RefCell<Request>>>,
        extra: usize,
    ) -> Result<(), JsError> {
        let mut live = self.live.borrow_mut();
        vm.charge_work(live.len() + 1)?;
        let mut total = extra;
        live.retain(|object| {
            let Some(object) = object.upgrade() else {
                return false;
            };
            if replacing.is_none_or(|old| !Rc::ptr_eq(old, &object)) {
                total = total.saturating_add(object.borrow().metadata_size());
            }
            true
        });
        if total > LIVE_METADATA {
            Err(JsError::new(
                ErrorKind::HeapLimit,
                "XHR metadata budget exceeded",
            ))
        } else {
            Ok(())
        }
    }

    fn fire(&self, vm: &mut Vm, xhr: Value, kind: &str) -> Result<(), JsError> {
        let event = vm.new_object_with_prototype(self.events.event_proto.clone())?;
        let event = event_target::init_event(vm, event, kind.into(), [false; 3], true)?;
        self.events.dispatch(vm, xhr, event).map(|_| ())
    }

    fn fire_progress(
        &self,
        vm: &mut Vm,
        xhr: Value,
        kind: &str,
        loaded: usize,
        total: usize,
    ) -> Result<(), JsError> {
        let event = vm.new_object_with_prototype(self.progress_proto.clone())?;
        let event = progress_event::init(
            vm,
            event,
            kind.into(),
            [false; 3],
            true,
            [Value::Bool(total != 0), loaded.into(), total.into()],
        )?;
        self.events.dispatch(vm, xhr, event).map(|_| ())
    }

    fn success_events(
        &self,
        vm: &mut Vm,
        xhr: Value,
        loaded: usize,
        total: usize,
    ) -> Result<(), JsError> {
        self.fire(vm, xhr.clone(), "readystatechange")?;
        self.fire_progress(vm, xhr.clone(), "load", loaded, total)?;
        self.fire_progress(vm, xhr, "loadend", loaded, total)
    }

    fn xml_source(
        &self,
        response: &WebResponse,
        raw: Arc<Vec<u8>>,
    ) -> Option<xml_document::Source> {
        xml_document::Source::new(response, &self.fetch.borrow().client, raw)
    }

    fn sync_fetch(&self, request: WebRequest) -> Result<WebResponse, FetchError> {
        let error = |message: String| FetchError::new(FetchErrorKind::Network, message);
        let loader = self
            .loader
            .get_or_init(|| Loader::with_cookies(self.cookies.clone()))
            .as_ref()
            .map_err(|message| error(message.clone()))?;
        let deadline = Instant::now() + FETCH_TIMEOUT;
        let cancel = CancelSignal::new();
        let signal = cancel.clone();
        let loader = loader.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        // Wait only for the transport: never pump the JS event loop during send().
        let transport = loader.transport_handle().clone();
        transport.spawn(async move {
            let result = loader
                .web_fetch_cancellable(&request, &signal, deadline)
                .await;
            let _ = sender.send(result);
        });
        receiver.recv_timeout(FETCH_TIMEOUT).unwrap_or_else(|_| {
            cancel.cancel();
            Err(error(
                "Synchronous XMLHttpRequest did not finish before its deadline".into(),
            ))
        })
    }
}

fn receiver(vm: &Vm, xhr: &Value) -> Result<Rc<RefCell<Request>>, JsError> {
    typed(vm, xhr, "XMLHttpRequest")
}

fn response_text(vm: &mut Vm, response: &WebResponse, body: &[u8]) -> Result<Value, JsError> {
    if body.is_empty() {
        return Ok("".into());
    }
    vm.charge_buffer_work(body.len())?;
    vm.ensure_string_allocation(body.len())?;
    let encoding = xml_document::fallback_text_encoding(response, body);
    let (text, _, _) = encoding.decode(body);
    vm.finish_buffer_string(text.encode_utf16().collect())
}

fn content_length(response: &WebResponse) -> usize {
    response
        .headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0)
}

fn response_size(response: &WebResponse) -> usize {
    super::bytes(&response.headers) + response.url.capacity() + response.status_text.capacity()
}

pub(super) fn install(vm: &mut Vm, fetch: &Shared, page: &LoadedPage) -> Result<Rc<Host>, JsError> {
    let exception = vm.get_global("DOMException")?;
    vm.root(exception.clone());
    let events = fetch.borrow().abort.events().clone();
    let progress_proto = progress_event::install(vm, &events.event_proto)?;
    vm.root(progress_proto.clone());
    let xml = xml_document::Host::install(vm)?;
    let host = Rc::new(Host {
        fetch: fetch.clone(),
        loader: page
            .fetch_loader
            .clone()
            .map(|mut loader| {
                loader.cookies = page.cookies.clone();
                OnceCell::from(Ok(loader))
            })
            .unwrap_or_default(),
        cookies: page.cookies.clone(),
        headers: page.connect_policy.clone(),
        events,
        exception,
        progress_proto,
        xml,
        live: RefCell::default(),
        #[cfg(test)]
        test_now: Default::default(),
    });
    let prototype = vm.new_object_with_prototype(host.events.target_proto.clone())?;
    let intrinsic = prototype.clone();
    let context = host.clone();
    let constructor = webidl::interface(
        vm,
        prototype.clone(),
        "XMLHttpRequest",
        0,
        move |vm, _, _, target| {
            let target =
                target.ok_or_else(|| JsError::type_error("XMLHttpRequest requires new"))?;
            let xhr = webidl::instance(vm, target, &intrinsic)?;
            let request = Rc::new(RefCell::new(Request::default()));
            context.budget(vm, None, request.borrow().metadata_size())?;
            event_target::attach(vm, xhr.clone())?;
            vm.set_host_data(xhr.clone(), request.clone())?;
            context.live.borrow_mut().push(Rc::downgrade(&request));
            Ok(xhr)
        },
    )?;
    let context = host.clone();
    webidl::operation(vm, prototype.clone(), "open", 2, move |vm, xhr, args| {
        let data = receiver(vm, &xhr)?;
        webidl::required(args, 2, "XMLHttpRequest.open")?;
        let method = byte_string(vm, arg(args, 0))?;
        let reference = super::request_url(vm, arg(args, 1), "XMLHttpRequest URL")?;
        let synchronous = args.get(2).is_some_and(|value| !value.is_truthy());
        if ["CONNECT", "TRACE", "TRACK"]
            .iter()
            .any(|name| method.eq_ignore_ascii_case(name))
        {
            return Err(context.exception(vm, "SecurityError", "Forbidden XMLHttpRequest method"));
        }
        let method = network::normalize_method(&method)
            .map_err(|_| context.exception(vm, "SyntaxError", "Invalid XMLHttpRequest method"))?;
        let (base, resolver, client) = {
            let fetch = context.fetch.borrow();
            (
                fetch.base.clone(),
                fetch.base_resolver.clone(),
                fetch.client.clone(),
            )
        };
        let mut url = resolver
            .map_or(base, |resolver| resolver())
            .join(&reference)
            .map_err(|_| context.exception(vm, "SyntaxError", "Invalid XMLHttpRequest URL"))?;
        url.set_fragment(None);
        if !url.username().is_empty()
            || url.password().is_some()
            || !arg(args, 3).is_nullish()
            || !arg(args, 4).is_nullish()
        {
            return Err(unsupported(
                "XMLHttpRequest URL credentials are not implemented",
            ));
        }
        let mut request = WebRequest::get(url, client);
        request.method = method;
        let (state_changed, generation, pending, timeout_ms) = {
            let data = data.borrow();
            (
                data.ready != Ready::Opened,
                data.generation.wrapping_add(1),
                data.pending,
                data.timeout_ms,
            )
        };
        if synchronous && timeout_ms != 0 {
            return Err(context.exception(
                vm,
                "InvalidAccessError",
                "Synchronous Window XMLHttpRequest cannot use an author timeout",
            ));
        }
        let replacement = Request {
            ready: Ready::Opened,
            synchronous,
            timeout_ms,
            request: Some(request),
            generation,
            ..Request::default()
        };
        context.budget(vm, Some(&data), replacement.metadata_size())?;
        vm.set_host_slot(xhr.clone(), TEXT_SLOT, "".into())?;
        vm.set_host_slot(xhr.clone(), XML_SLOT, Value::Undefined)?;
        if let Some(id) = pending {
            context.discard(vm, id);
        }
        *data.borrow_mut() = replacement;
        if state_changed {
            context.fire(vm, xhr, "readystatechange")?;
        }
        Ok(Value::Undefined)
    })?;
    let get_timeout = vm.new_native_function("get timeout", |vm, xhr, _| {
        Ok(receiver(vm, &xhr)?.borrow().timeout_ms.into())
    });
    let context = host.clone();
    let set_timeout = vm.new_native_function("set timeout", move |vm, xhr, args| {
        let data = receiver(vm, &xhr)?;
        let number = vm.to_number(&arg(args, 0))?;
        let timeout_ms = if number.is_finite() {
            number.trunc().rem_euclid(4294967296.0) as u32
        } else {
            0
        };
        if data.borrow().synchronous {
            return Err(context.exception(
                vm,
                "InvalidAccessError",
                "Cannot set timeout on a synchronous Window XMLHttpRequest",
            ));
        }
        let pending = {
            let mut data = data.borrow_mut();
            data.timeout_ms = timeout_ms;
            data.pending
        };
        if let Some(id) = pending {
            context.update_timeout(id, timeout_ms, context.now());
        }
        Ok(Value::Undefined)
    });
    vm.define_data_property(set_timeout.clone(), "length", 1.into(), false, false, true)?;
    vm.define_accessor(
        prototype.clone(),
        "timeout",
        Some(get_timeout),
        Some(set_timeout),
    )?;
    let context = host.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "setRequestHeader",
        2,
        move |vm, xhr, args| {
            let data = receiver(vm, &xhr)?;
            webidl::required(args, 2, "XMLHttpRequest.setRequestHeader")?;
            let name = byte_string(vm, arg(args, 0))?;
            let value = byte_string(vm, arg(args, 1))?;
            if !data.borrow().can_send() {
                return Err(context.exception(
                    vm,
                    "InvalidStateError",
                    "XMLHttpRequest is not open for headers",
                ));
            }
            let (name, value) = network::normalize_header(&name, &value).map_err(|_| {
                context.exception(vm, "SyntaxError", "Invalid XMLHttpRequest header")
            })?;
            if !network::forbidden_request_header(&name, &value) {
                context.budget(vm, None, name.len() + value.len() + 128)?;
                let mut data = data.borrow_mut();
                let request = data.request.as_mut().expect("open request");
                network::append_header(&mut request.headers, &name, &value)
                    .map_err(super::map_error)?;
            }
            Ok(Value::Undefined)
        },
    )?;
    let context = host.clone();
    webidl::operation(vm, prototype.clone(), "send", 0, move |vm, xhr, args| {
        let data = receiver(vm, &xhr)?;
        let (mut request, synchronous) = {
            let data = data.borrow();
            if !data.can_send() {
                return Err(context.exception(
                    vm,
                    "InvalidStateError",
                    "XMLHttpRequest is not open for send",
                ));
            }
            (
                data.request.clone().expect("open request"),
                data.synchronous,
            )
        };
        let body = arg(args, 0);
        if !body.is_nullish() && !matches!(request.method.as_str(), "GET" | "HEAD") {
            if vm.is_buffer_source(&body) {
                let length = vm.fixed_buffer_source_bytes(&body)?.len();
                if length > FETCH_BODY_LIMIT {
                    return Err(JsError::type_error("XMLHttpRequest body exceeds byte limit"));
                }
                vm.charge_buffer_work(length)?;
                request.body = Some(Arc::new(vm.fixed_buffer_source_bytes(&body)?.to_vec()));
            } else if let Some(blob) = crate::script::blobs::get(vm, &body) {
                let (bytes, range) = blob.shared_bytes();
                if range.len() > FETCH_BODY_LIMIT {
                    return Err(JsError::type_error("XMLHttpRequest body exceeds byte limit"));
                }
                if !blob.mime.is_empty() {
                    super::add_content_type(&mut request.headers, Some(&blob.mime))?;
                }
                request.body = Some(if range.len() == bytes.len() {
                    bytes
                } else {
                    vm.charge_buffer_work(range.len())?;
                    Arc::new(bytes[range].to_vec())
                });
            } else {
                if body.as_string().is_none() {
                    return Err(unsupported("XMLHttpRequest request body type is not implemented"));
                }
                let body = usv(vm, body, FETCH_BODY_LIMIT, "XMLHttpRequest body")?;
                super::add_content_type(&mut request.headers, Some("text/plain;charset=UTF-8"))?;
                request.body = Some(Arc::new(body.into_bytes()));
            }
        }
        request.connect_policy = Some(context.policy());
        if !synchronous {
            context.queue(vm, xhr, &data, request)?;
            return Ok(Value::Undefined);
        }
        data.borrow_mut().request = None;
        let response = context.sync_fetch(request);
        data.borrow_mut().ready = Ready::Done;
        let mut response = response.map_err(|_| {
            context.exception(vm, "NetworkError", "XMLHttpRequest transport failed")
        })?;
        let body = response.body.take();
        let bytes = body.as_deref().map_or(&[][..], Vec::as_slice);
        let text = response_text(vm, &response, bytes)?;
        let loaded = bytes.len();
        let total = content_length(&response);
        let xml = body.and_then(|body| context.xml_source(&response, body));
        context.budget(
            vm,
            None,
            response_size(&response) + xml.as_ref().map_or(0, xml_document::Source::size),
        )?;
        vm.set_host_slot(xhr.clone(), TEXT_SLOT, text)?;
        {
            let mut data = data.borrow_mut();
            data.response = Some(response);
            data.xml = xml;
        }
        context.success_events(vm, xhr, loaded, total)?;
        Ok(Value::Undefined)
    })?;
    let context = host.clone();
    webidl::operation(vm, prototype.clone(), "abort", 0, move |vm, xhr, _| {
        let data = receiver(vm, &xhr)?;
        let (id, generation) = {
            let data = data.borrow();
            (data.pending, data.generation)
        };
        if let Some(id) = id {
            context.fail(vm, id, "abort")?;
        }
        let still_done = {
            let data = data.borrow();
            data.ready == Ready::Done && data.generation == generation
        };
        if still_done {
            let mut data = data.borrow_mut();
            data.ready = Ready::Unsent;
            data.response = None;
            data.xml = None;
            vm.set_host_slot(xhr.clone(), TEXT_SLOT, "".into())?;
            vm.set_host_slot(xhr, XML_SLOT, Value::Undefined)?;
        }
        Ok(Value::Undefined)
    })?;
    webidl::operation(
        vm,
        prototype.clone(),
        "getResponseHeader",
        1,
        |vm, xhr, args| {
            let data = receiver(vm, &xhr)?;
            webidl::required(args, 1, "XMLHttpRequest.getResponseHeader")?;
            let name = byte_string(vm, arg(args, 0))?;
            let data = data.borrow();
            let values = data
                .response
                .iter()
                .flat_map(|r| &r.headers)
                .filter(|(key, _)| key.eq_ignore_ascii_case(&name))
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>();
            if values.is_empty() {
                Ok(Value::Null)
            } else {
                vm.finish_string(values.join(", ").encode_utf16().collect())
            }
        },
    )?;
    webidl::operation(
        vm,
        prototype.clone(),
        "getAllResponseHeaders",
        0,
        |vm, xhr, _| {
            let data = receiver(vm, &xhr)?;
            let data = data.borrow();
            let mut headers = data
                .response
                .iter()
                .flat_map(|r| &r.headers)
                .collect::<Vec<_>>();
            headers.sort_by_key(|(name, _)| name);
            let mut serialized = String::new();
            for (name, value) in headers {
                serialized.push_str(name);
                serialized.push_str(": ");
                serialized.push_str(value);
                serialized.push_str("\r\n");
            }
            vm.finish_string(serialized.encode_utf16().collect())
        },
    )?;
    for name in [
        "readyState",
        "status",
        "statusText",
        "responseURL",
        "response",
        "responseText",
    ] {
        let context = host.clone();
        getter(vm, prototype.clone(), name, move |vm, xhr, _| {
            let data = receiver(vm, &xhr)?;
            let data = data.borrow();
            Ok(match name {
                "readyState" => (data.ready as u32).into(),
                "status" => (data.response.as_ref().map_or(0, |r| r.status) as u32).into(),
                "statusText" => vm.finish_string(
                    data.response
                        .as_ref()
                        .map_or("", |r| &r.status_text)
                        .encode_utf16()
                        .collect(),
                )?,
                "responseURL" => vm.finish_string(
                    data.response
                        .as_ref()
                        .map_or("", |r| &r.url)
                        .encode_utf16()
                        .collect(),
                )?,
                _ if data.ready == Ready::Loading => context.partial_text(vm, &data)?,
                _ => vm.host_slot(&xhr, TEXT_SLOT).unwrap_or_else(|| "".into()),
            })
        })?;
    }
    let context = host.clone();
    getter(vm, prototype.clone(), "responseXML", move |vm, xhr, _| {
        let data = receiver(vm, &xhr)?;
        if data.borrow().ready != Ready::Done {
            return Ok(Value::Null);
        }
        if let Some(cached) = vm
            .host_slot(&xhr, XML_SLOT)
            .filter(|value| !value.is_undefined())
        {
            return Ok(cached);
        }
        let source = data.borrow_mut().xml.take();
        let value = if let Some(source) = source {
            let data = data.borrow();
            let url = data
                .response
                .as_ref()
                .expect("XML has response metadata")
                .url
                .as_str();
            context.xml.parse(vm, &source, url)?
        } else {
            Value::Null
        };
        vm.set_host_slot(xhr, XML_SLOT, value.clone())?;
        Ok(value)
    })?;
    for (name, state) in [
        ("UNSENT", Ready::Unsent),
        ("OPENED", Ready::Opened),
        ("HEADERS_RECEIVED", Ready::HeadersReceived),
        ("LOADING", Ready::Loading),
        ("DONE", Ready::Done),
    ] {
        for object in [prototype.clone(), constructor.clone()] {
            vm.define_data_property(object, name, (state as u32).into(), false, true, false)?;
        }
    }
    for kind in [
        "readystatechange",
        "loadstart",
        "progress",
        "abort",
        "error",
        "timeout",
        "load",
        "loadend",
    ] {
        event_target::install_handler(
            vm,
            prototype.clone(),
            kind,
            |vm, xhr| receiver(vm, xhr).map(|_| ()),
            |_, _| Ok(()),
        )?;
    }
    Ok(host)
}
