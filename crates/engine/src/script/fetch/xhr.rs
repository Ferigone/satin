//! Window XMLHttpRequest text loading over the existing Fetch transport.
use super::{Shared, byte_string, typed, usv};
use crate::{
    Loader,
    loader::{ConnectPolicy, LoadedPage},
    network::{
        self, CancelSignal, FETCH_BODY_LIMIT, FETCH_HEADER_LIMIT, FETCH_TIMEOUT, FetchError,
        FetchErrorKind, WebRequest, WebResponse,
    },
    script::{abort, arg, event_target, getter, unsupported, webidl},
};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};
use std::{
    cell::{OnceCell, RefCell},
    rc::{Rc, Weak},
    sync::{Arc, mpsc},
    time::Instant,
};

const TEXT_SLOT: u32 = 0;
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
    request: Option<WebRequest>,
    response: Option<WebResponse>,
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
        }) + 256
    }

    fn can_send(&self) -> bool {
        self.ready == Ready::Opened && self.request.is_some()
    }
}

struct Host {
    fetch: Shared,
    loader: OnceCell<Result<Loader, String>>,
    cookies: crate::cookies::CookieStore,
    headers: ConnectPolicy,
    events: event_target::Events,
    exception: Value,
    live: RefCell<Vec<Weak<RefCell<Request>>>>,
}

impl Host {
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

fn response_text(vm: &mut Vm, response: &WebResponse) -> Result<Value, JsError> {
    let Some(body) = &response.body else {
        return Ok("".into());
    };
    vm.charge_work(body.len())?;
    vm.ensure_string_allocation(body.len())?;
    let encoding = response
        .headers
        .iter()
        .find(|(name, _)| name == "content-type")
        .and_then(|(_, value)| crate::mime::MimeType::parse(value))
        .and_then(|mime| {
            mime.parameter("charset")
                .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        })
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(body);
    vm.finish_string(text.encode_utf16().collect())
}

pub(super) fn install(vm: &mut Vm, fetch: &Shared, page: &LoadedPage) -> Result<(), JsError> {
    let exception = vm.get_global("DOMException")?;
    vm.root(exception.clone());
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
        events: fetch.borrow().abort.events().clone(),
        exception,
        live: RefCell::default(),
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
        let reference = usv(vm, arg(args, 1), FETCH_HEADER_LIMIT)?;
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
        let state_changed = data.borrow().ready != Ready::Opened;
        let replacement = Request {
            ready: Ready::Opened,
            synchronous,
            request: Some(request),
            response: None,
        };
        context.budget(vm, Some(&data), replacement.metadata_size())?;
        vm.set_host_slot(xhr.clone(), TEXT_SLOT, "".into())?;
        *data.borrow_mut() = replacement;
        if state_changed {
            context.fire(vm, xhr, "readystatechange")?;
        }
        Ok(Value::Undefined)
    })?;
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
        let mut request = {
            let data = data.borrow();
            if !data.can_send() {
                return Err(context.exception(
                    vm,
                    "InvalidStateError",
                    "XMLHttpRequest is not open for send",
                ));
            }
            if !data.synchronous {
                return Err(unsupported(
                    "Asynchronous XMLHttpRequest is not implemented",
                ));
            }
            data.request.clone().expect("open request")
        };
        let body = arg(args, 0);
        if !body.is_nullish() && !matches!(request.method.as_str(), "GET" | "HEAD") {
            // This first stage implements text bodies, not the other XHR BodyInit types.
            if body.as_string().is_none() {
                return Err(unsupported(
                    "Only XMLHttpRequest text request bodies are implemented",
                ));
            }
            let body = usv(vm, body, FETCH_BODY_LIMIT)?;
            if !request
                .headers
                .iter()
                .any(|(name, _)| name == "content-type")
            {
                network::append_header(
                    &mut request.headers,
                    "content-type",
                    "text/plain;charset=UTF-8",
                )
                .map_err(super::map_error)?;
            }
            request.body = Some(Arc::new(body.into_bytes()));
        }
        request.connect_policy = Some(context.policy());
        data.borrow_mut().request = None;
        let response = context.sync_fetch(request);
        data.borrow_mut().ready = Ready::Done;
        let mut response = response.map_err(|_| {
            context.exception(vm, "NetworkError", "XMLHttpRequest transport failed")
        })?;
        let text = response_text(vm, &response)?;
        response.body = None;
        context.budget(
            vm,
            None,
            super::bytes(&response.headers)
                + response.url.capacity()
                + response.status_text.capacity(),
        )?;
        vm.set_host_slot(xhr.clone(), TEXT_SLOT, text)?;
        data.borrow_mut().response = Some(response);
        context.fire(vm, xhr, "readystatechange")?;
        Ok(Value::Undefined)
    })?;
    webidl::operation(vm, prototype.clone(), "abort", 0, |vm, xhr, _| {
        let data = receiver(vm, &xhr)?;
        if data.borrow().ready == Ready::Done {
            *data.borrow_mut() = Request::default();
            vm.set_host_slot(xhr, TEXT_SLOT, "".into())?;
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
                _ => vm.host_slot(&xhr, TEXT_SLOT).unwrap_or_else(|| "".into()),
            })
        })?;
    }
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
    event_target::install_handler(
        vm,
        prototype,
        "readystatechange",
        |vm, xhr| receiver(vm, xhr).map(|_| ()),
        |_, _| Ok(()),
    )
}
