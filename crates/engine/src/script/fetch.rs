//! Fetch objects use traced host slots for streams, headers, and signals.
mod blob_url;
mod upload;
mod wasm_streaming;
mod xhr;
use super::{
    abort, arg, blobs, fatal, form_data, getter, method, streams::StreamHost, text, unsupported,
    url_api,
};
use crate::{
    loader::LoadedPage,
    network::{
        self, CancelSignal, CredentialsMode, FETCH_BODY_LIMIT, FETCH_HEADER_LIMIT, FetchBody,
        FetchError, FetchErrorKind, FetchJob, HeaderList, RedirectMode, RequestMode, ResponseType,
        StreamingResponse, WebRequest, WebResponse,
    },
};
use rust_browser_javascript::{ErrorKind, JsError, JsString, RootId, Value, Vm};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    rc::{Rc, Weak},
    sync::{Arc, Weak as ArcWeak},
};
use url::Url;

const LIVE_BYTES: usize = 16 * 1024 * 1024;
const PENDING: usize = 16;
const HEADERS_SLOT: u32 = 0;
const SIGNAL_SLOT: u32 = 1;
const BODY_SLOT: u32 = 2;
type Shared = Rc<RefCell<State>>;
#[derive(Clone, Copy, PartialEq)]
enum Guard {
    None,
    Request,
    NoCors,
    Response,
    Immutable,
}
struct HeadersData {
    list: HeaderList,
    guard: Guard,
}
#[derive(Clone)]
struct RequestData {
    request: WebRequest,
    headers: Rc<RefCell<HeadersData>>,
    streaming: bool,
    blob: Option<Rc<blobs::Blob>>,
}

enum RequestBody {
    Buffered(Option<Value>),
    Stream { value: Value, inherited: bool },
}

enum ResponseBody {
    Network(FetchBody),
    Buffered(Option<Arc<Vec<u8>>>),
    Blob(Rc<blobs::Blob>, Range<usize>),
}

#[derive(Clone)]
struct ResponseData {
    response: WebResponse,
    headers: Rc<RefCell<HeadersData>>,
}
struct Pending {
    promise: RootId,
    resolve: RootId,
    reject: RootId,
    signal: RootId,
    cancel: CancelSignal,
    _abort: abort::Subscription,
    metadata: usize,
    upload: Option<upload::Upload>,
    blob: Option<Result<(WebResponse, ResponseBody), FetchError>>,
}
struct State {
    byte_limit: usize,
    base: Url,
    base_resolver: Option<Rc<dyn Fn() -> Url>>,
    document: Weak<RefCell<super::State>>,
    xhr_meta_policy: crate::loader::ConnectPolicy,
    client: Url,
    headers_proto: RootId,
    request_proto: RootId,
    response_proto: RootId,
    abort: abort::AbortHost,
    streams: StreamHost,
    blobs: blobs::BlobFactory,
    form_data: Option<form_data::FormDataHost>,
    next: u64,
    pending: BTreeMap<u64, Pending>,
    pending_xhr: BTreeMap<u64, xhr::Pending>,
    ready: Vec<FetchJob>,
    bodies: Vec<ArcWeak<Vec<u8>>>,
    headers: Vec<Weak<RefCell<HeadersData>>>,
    requests: Vec<Weak<RefCell<RequestData>>>,
    responses: Vec<Weak<RefCell<ResponseData>>>,
}
pub(super) struct FetchHost {
    state: Shared,
    xhr: Option<Rc<xhr::Host>>,
}

impl State {
    fn pending_requests(&self) -> usize {
        self.pending.len() + self.pending_xhr.len()
    }

    fn next_id(&mut self) -> Result<u64, JsError> {
        let id = self.next;
        self.next = id
            .checked_add(1)
            .ok_or_else(|| JsError::type_error("Fetch request identifier exhausted"))?;
        Ok(id)
    }
}

fn validate_request(request: &mut WebRequest) -> Result<(), JsError> {
    if request.url.scheme() == "blob" {
        request.validate_metadata()
    } else {
        request.validate()
    }
    .map_err(map_error)
}

fn typed<T: 'static>(vm: &Vm, value: &Value, name: &str) -> Result<Rc<RefCell<T>>, JsError> {
    vm.host_data::<RefCell<T>>(value)
        .ok_or_else(|| JsError::type_error(format!("Illegal {name} receiver")))
}
fn map_error(error: FetchError) -> JsError {
    match error.kind {
        FetchErrorKind::Unsupported => unsupported(error.message),
        _ => JsError::type_error(error.message),
    }
}
fn bytes(list: &HeaderList) -> usize {
    list.iter()
        .map(|(n, v)| n.capacity() + v.capacity() + 64)
        .sum()
}
fn budget(vm: &mut Vm, state: &Shared, extra: usize) -> Result<(), JsError> {
    let mut state = state.borrow_mut();
    vm.charge_work(
        state.bodies.len() + state.headers.len() + state.requests.len() + state.responses.len() + 1,
    )?;
    state.bodies.retain(|v| v.strong_count() > 0);
    state.headers.retain(|v| v.strong_count() > 0);
    state.requests.retain(|v| v.strong_count() > 0);
    state.responses.retain(|v| v.strong_count() > 0);
    let mut total = state.pending.values().map(|p| p.metadata).sum::<usize>();
    total += state
        .pending_xhr
        .values()
        .map(xhr::Pending::size)
        .sum::<usize>();
    let mut seen = BTreeSet::new();
    for body in &state.bodies {
        if let Some(body) = body.upgrade()
            && seen.insert(Arc::as_ptr(&body) as usize)
        {
            total = total.saturating_add(body.capacity() + 64);
        }
    }
    for headers in &state.headers {
        if let Some(headers) = headers.upgrade() {
            total = total.saturating_add(bytes(&headers.borrow().list) + 128);
        }
    }
    for request in &state.requests {
        if let Some(request) = request.upgrade() {
            let request = request.borrow();
            total = total.saturating_add(
                request.request.url.as_str().len()
                    + request.request.client_url.as_str().len()
                    + request.request.method.len()
                    + 512,
            );
        }
    }
    for response in &state.responses {
        if let Some(response) = response.upgrade() {
            let response = response.borrow();
            total = total.saturating_add(
                response.response.url.capacity() + response.response.status_text.capacity() + 512,
            );
        }
    }
    if total.saturating_add(extra) > state.byte_limit {
        return Err(JsError::new(
            ErrorKind::HeapLimit,
            "Fetch live request/response data exceeds 16 MiB",
        ));
    }
    Ok(())
}
fn byte_string(vm: &mut Vm, value: Value) -> Result<String, JsError> {
    let s = vm.to_string(&value)?;
    if s.len() > FETCH_HEADER_LIMIT {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "HTTP ByteString exceeds alpha length limit",
        ));
    }
    vm.charge_work(s.len())?;
    if s.as_utf16().iter().any(|u| *u > 255) {
        return Err(JsError::type_error(
            "HTTP ByteString contains a code unit above 255",
        ));
    }
    Ok(s.as_utf16().iter().map(|u| char::from(*u as u8)).collect())
}
fn usv(vm: &mut Vm, value: Value, limit: usize) -> Result<String, JsError> {
    let s = vm.to_string(&value)?;
    vm.charge_work(s.len())?;
    let size = char::decode_utf16(s.as_utf16().iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER).len_utf8())
        .sum::<usize>();
    if size > limit {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Fetch string exceeds alpha byte limit",
        ));
    }
    Ok(s.to_string_lossy())
}
fn dictionary(value: Value) -> Result<Value, JsError> {
    if value.is_nullish() {
        Ok(Value::Undefined)
    } else if value.as_object().is_some() {
        Ok(value)
    } else {
        Err(JsError::type_error("Fetch init must be an object"))
    }
}
fn field(vm: &mut Vm, init: &Value, name: &str) -> Result<Value, JsError> {
    if init.is_undefined() {
        Ok(Value::Undefined)
    } else {
        vm.get_property(init.clone(), name)
    }
}
fn header_guard(guard: Guard, name: &str, value: &str) -> bool {
    match guard {
        Guard::Request => !network::forbidden_request_header(name, value),
        Guard::NoCors => {
            !network::forbidden_request_header(name, value)
                && matches!(
                    name,
                    "accept" | "accept-language" | "content-language" | "content-type"
                )
                && network::safelisted_header(name, value)
        }
        Guard::Response => !matches!(name, "set-cookie" | "set-cookie2"),
        _ => true,
    }
}
fn append(list: &mut HeaderList, name: &str, value: &str) -> Result<(), JsError> {
    if name == "set-cookie" {
        if list.len() >= 128
            || bytes(list).saturating_add(name.len() + value.len()) > FETCH_HEADER_LIMIT
        {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Header list exceeds alpha limit",
            ));
        }
        list.push((name.into(), value.into()));
        Ok(())
    } else {
        network::append_header(list, name, value).map_err(map_error)
    }
}
fn parse_headers(vm: &mut Vm, input: Value, guard: Guard) -> Result<HeaderList, JsError> {
    let mut list = Vec::new();
    if input.is_undefined() {
        return Ok(list);
    }
    let entries = if let Some(headers) = vm.host_data::<RefCell<HeadersData>>(&input) {
        headers.borrow().list.clone()
    } else if vm.is_array(&input) {
        let length = vm
            .get_property(input.clone(), "length")?
            .as_number()
            .unwrap_or(0.0) as usize;
        if length > 128 {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Header count exceeds alpha limit",
            ));
        }
        let mut entries = Vec::new();
        for i in 0..length {
            vm.charge_work(1)?;
            let pair = vm.get_property(input.clone(), &i.to_string())?;
            if !vm.is_array(&pair) {
                return Err(unsupported(
                    "Headers sequence entries currently require arrays",
                ));
            }
            if vm.get_property(pair.clone(), "length")?.as_number() != Some(2.0) {
                return Err(JsError::type_error(
                    "Headers sequence entry must have exactly two values",
                ));
            }
            let name = vm.get_property(pair.clone(), "0")?;
            let value = vm.get_property(pair, "1")?;
            entries.push((byte_string(vm, name)?, byte_string(vm, value)?));
        }
        entries
    } else {
        if input.as_object().is_none() {
            return Err(JsError::type_error(
                "Headers init must be an object or sequence",
            ));
        }
        let mut entries = Vec::new();
        for key in vm.enumerable_own_keys(input.clone())? {
            let name = byte_string(vm, Value::String(key.clone()))?;
            let value = vm.get_property(input.clone(), &key.to_string_lossy())?;
            entries.push((name, byte_string(vm, value)?));
            if entries.len() > 128 {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Header count exceeds alpha limit",
                ));
            }
        }
        entries
    };
    for (name, value) in entries {
        let (name, value) = network::normalize_header(&name, &value).map_err(map_error)?;
        if header_guard(guard, &name, &value) {
            append(&mut list, &name, &value)?;
        }
    }
    Ok(list)
}
fn headers_data(
    vm: &mut Vm,
    state: &Shared,
    list: HeaderList,
    guard: Guard,
) -> Result<Rc<RefCell<HeadersData>>, JsError> {
    budget(vm, state, bytes(&list) + 128)?;
    let data = Rc::new(RefCell::new(HeadersData { list, guard }));
    state.borrow_mut().headers.push(Rc::downgrade(&data));
    Ok(data)
}
fn headers_object(
    vm: &mut Vm,
    state: &Shared,
    data: Rc<RefCell<HeadersData>>,
) -> Result<Value, JsError> {
    let root = state.borrow().headers_proto;
    let object = vm.new_object_with_prototype(
        vm.root_value(root)
            .ok_or_else(|| JsError::type_error("Headers prototype is missing"))?,
    )?;
    vm.set_host_data(object.clone(), data)?;
    Ok(object)
}
fn request_object(
    vm: &mut Vm,
    state: &Shared,
    mut data: RequestData,
    signal: Value,
    body: Value,
) -> Result<Value, JsError> {
    data.request.headers = Vec::new();
    budget(
        vm,
        state,
        data.request.url.as_str().len()
            + data.request.client_url.as_str().len()
            + data.request.method.len()
            + 512,
    )?;
    let headers = headers_object(vm, state, data.headers.clone())?;
    let root = state.borrow().request_proto;
    let object = vm.new_object_with_prototype(vm.root_value(root).unwrap())?;
    let data = Rc::new(RefCell::new(data));
    state.borrow_mut().requests.push(Rc::downgrade(&data));
    vm.set_host_data(object.clone(), data)?;
    vm.set_host_slot(object.clone(), HEADERS_SLOT, headers)?;
    vm.set_host_slot(object.clone(), SIGNAL_SLOT, signal)?;
    vm.set_host_slot(object.clone(), BODY_SLOT, body)?;
    Ok(object)
}
fn response_object(
    vm: &mut Vm,
    state: &Shared,
    mut data: ResponseData,
    body: Value,
) -> Result<Value, JsError> {
    data.response.headers = Vec::new();
    data.response.body = None;
    budget(
        vm,
        state,
        data.response.url.len() + data.response.status_text.len() + 512,
    )?;
    let headers = headers_object(vm, state, data.headers.clone())?;
    let root = state.borrow().response_proto;
    let object = vm.new_object_with_prototype(vm.root_value(root).unwrap())?;
    let data = Rc::new(RefCell::new(data));
    state.borrow_mut().responses.push(Rc::downgrade(&data));
    vm.set_host_data(object.clone(), data)?;
    vm.set_host_slot(object.clone(), HEADERS_SLOT, headers)?;
    vm.set_host_slot(object.clone(), BODY_SLOT, body)?;
    Ok(object)
}

fn memory_stream(
    vm: &mut Vm,
    streams: &StreamHost,
    bytes: Option<Arc<Vec<u8>>>,
) -> Result<Value, JsError> {
    match bytes {
        Some(bytes) => {
            let length = bytes.len();
            streams.memory_stream(vm, bytes, 0..length)
        }
        None => Ok(Value::Null),
    }
}

struct BodyInput {
    bytes: Option<Arc<Vec<u8>>>,
    content_type: Option<String>,
}
fn body_input(vm: &mut Vm, state: &Shared, value: Value) -> Result<BodyInput, JsError> {
    if value.is_nullish() {
        return Ok(BodyInput {
            bytes: None,
            content_type: None,
        });
    }
    let (bytes, content_type) = if vm.is_buffer_source(&value) {
        let len = vm.fixed_buffer_source_bytes(&value)?.len();
        if len > FETCH_BODY_LIMIT {
            return Err(JsError::type_error("Fetch body exceeds 4 MiB"));
        }
        vm.charge_work(len)?;
        (vm.fixed_buffer_source_bytes(&value)?.to_vec(), None)
    } else if let Some(body) = form_data::body_init(vm, &value)? {
        body
    } else if let Some(form) = url_api::form_body(vm, &value)? {
        (
            form.into_bytes(),
            Some("application/x-www-form-urlencoded;charset=UTF-8".into()),
        )
    } else {
        (
            usv(vm, value, FETCH_BODY_LIMIT)?.into_bytes(),
            Some("text/plain;charset=UTF-8".into()),
        )
    };
    if bytes.len() > FETCH_BODY_LIMIT {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Fetch body exceeds 4 MiB",
        ));
    }
    budget(vm, state, bytes.capacity() + 64)?;
    let body = Arc::new(bytes);
    state.borrow_mut().bodies.push(Arc::downgrade(&body));
    Ok(BodyInput {
        bytes: Some(body),
        content_type,
    })
}

fn add_content_type(headers: &mut HeaderList, content_type: Option<&str>) -> Result<(), JsError> {
    if let Some(content_type) = content_type
        && !headers.iter().any(|(name, _)| name == "content-type")
    {
        append(headers, "content-type", content_type)?;
    }
    Ok(())
}

fn request_init(
    vm: &mut Vm,
    state: &Shared,
    input: Value,
    init: Value,
) -> Result<(RequestData, Value, RequestBody), JsError> {
    let init = dictionary(init)?;
    for name in [
        "cache",
        "integrity",
        "referrer",
        "referrerPolicy",
        "window",
        "priority",
        "attributionReporting",
        "browsingTopics",
    ] {
        if !field(vm, &init, name)?.is_undefined() {
            return Err(unsupported(format!(
                "Request init '{name}' is not implemented"
            )));
        }
    }
    let duplex = field(vm, &init, "duplex")?;
    let has_duplex = !duplex.is_undefined();
    if has_duplex && text(vm, duplex)? != "half" {
        return Err(JsError::type_error("Invalid Request duplex"));
    }
    let keepalive = field(vm, &init, "keepalive")?.is_truthy();
    let original = vm.host_data::<RefCell<RequestData>>(&input);
    let initial_signal = original
        .as_ref()
        .and_then(|_| vm.host_slot(&input, SIGNAL_SLOT));
    let signal = field(vm, &init, "signal")?;
    let source_signal = if signal.is_undefined() {
        initial_signal
    } else if signal == Value::Null {
        None
    } else {
        abort::require_signal(vm, &signal)?;
        Some(signal)
    };
    let mut request = if let Some(original) = &original {
        original.borrow().request.clone()
    } else {
        let value = usv(vm, input.clone(), FETCH_HEADER_LIMIT)?;
        let (base, resolver, client) = {
            let state = state.borrow();
            (
                state.base.clone(),
                state.base_resolver.clone(),
                state.client.clone(),
            )
        };
        let base = resolver.map_or(base, |resolver| resolver());
        WebRequest::get(
            base.join(&value)
                .map_err(|_| JsError::type_error("Invalid Fetch URL"))?,
            client,
        )
    };
    if request.url.scheme() != "blob" {
        request.url.set_fragment(None);
    }
    let method = field(vm, &init, "method")?;
    if !method.is_undefined() {
        request.method = network::normalize_method(&byte_string(vm, method)?).map_err(map_error)?;
    }
    let mode = field(vm, &init, "mode")?;
    if !mode.is_undefined() {
        request.mode = match text(vm, mode)?.as_str() {
            "cors" => RequestMode::Cors,
            "same-origin" => RequestMode::SameOrigin,
            "no-cors" => RequestMode::NoCors,
            _ => return Err(JsError::type_error("Invalid Request mode")),
        };
    }
    let credentials = field(vm, &init, "credentials")?;
    if !credentials.is_undefined() {
        request.credentials = match text(vm, credentials)?.as_str() {
            "omit" => CredentialsMode::Omit,
            "same-origin" => CredentialsMode::SameOrigin,
            "include" => CredentialsMode::Include,
            _ => return Err(JsError::type_error("Invalid Request credentials")),
        };
    }
    let redirect = field(vm, &init, "redirect")?;
    if !redirect.is_undefined() {
        request.redirect = match text(vm, redirect)?.as_str() {
            "follow" => RedirectMode::Follow,
            "error" => RedirectMode::Error,
            "manual" => RedirectMode::Manual,
            _ => return Err(JsError::type_error("Invalid Request redirect mode")),
        };
    }
    let guard = if request.mode == RequestMode::NoCors {
        Guard::NoCors
    } else {
        Guard::Request
    };
    let headers = field(vm, &init, "headers")?;
    request.headers = if headers.is_undefined() {
        if let Some(original) = &original {
            original.borrow().headers.borrow().list.clone()
        } else {
            Vec::new()
        }
    } else {
        parse_headers(vm, headers, guard)?
    };
    let body = field(vm, &init, "body")?;
    let streams = state.borrow().streams.clone();
    let body = if streams.is_stream(vm, &body) {
        if !has_duplex || keepalive {
            return Err(JsError::type_error(
                "A streaming body requires duplex half and disallows keepalive",
            ));
        }
        if streams.unusable(vm, &body) {
            return Err(JsError::type_error(
                "Request body has already been used or locked",
            ));
        }
        request.body = None;
        RequestBody::Stream {
            value: body,
            inherited: false,
        }
    } else if !body.is_nullish() {
        let body = body_input(vm, state, body)?;
        request.body = body.bytes;
        add_content_type(&mut request.headers, body.content_type.as_deref())?;
        RequestBody::Buffered(None)
    } else {
        let inherited = vm
            .host_slot(&input, BODY_SLOT)
            .filter(|body| original.is_some() && !body.is_nullish());
        if inherited
            .as_ref()
            .is_some_and(|body| streams.unusable(vm, body))
        {
            return Err(JsError::type_error(
                "Request body has already been used or locked",
            ));
        }
        match inherited {
            Some(value)
                if original
                    .as_ref()
                    .is_some_and(|original| original.borrow().streaming) =>
            {
                RequestBody::Stream {
                    value,
                    inherited: true,
                }
            }
            inherited => RequestBody::Buffered(inherited),
        }
    };
    validate_request(&mut request)?;
    let streaming = matches!(body, RequestBody::Stream { .. });
    if streaming {
        request.validate_streaming_upload().map_err(map_error)?;
        if keepalive {
            return Err(JsError::type_error("A streaming body disallows keepalive"));
        }
    } else if keepalive {
        return Err(unsupported("Request init 'keepalive' is not implemented"));
    }
    let headers = headers_data(vm, state, request.headers.clone(), guard)?;
    let abort = state.borrow().abort.clone();
    let signal = abort.dependent(vm, source_signal.as_slice())?;
    // Parsing a URL captures the current entry. A Request can continue to use
    // that Blob after revocation, while a fresh parse must look it up again.
    let blob = if let Some(original) = original {
        original.borrow().blob.clone()
    } else if request.url.scheme() == "blob" {
        state
            .borrow()
            .document
            .upgrade()
            .and_then(|document| document.borrow().media.resolve_blob(vm, &request.url))
    } else {
        None
    };
    Ok((
        RequestData {
            request,
            headers,
            streaming,
            blob,
        },
        signal,
        body,
    ))
}
fn response_init(
    vm: &mut Vm,
    state: &Shared,
    body: Value,
    init: Value,
) -> Result<(ResponseData, Value), JsError> {
    let init = dictionary(init)?;
    let status = field(vm, &init, "status")?;
    let status = if status.is_undefined() {
        200.0
    } else {
        vm.to_number(&status)?
    };
    let status = if status.is_finite() {
        status.trunc().rem_euclid(65536.0) as u16
    } else {
        0
    };
    if !(200..=599).contains(&status) {
        return Err(JsError::new(
            ErrorKind::RangeError,
            "Response status must be between 200 and 599",
        ));
    }
    let status_text = field(vm, &init, "statusText")?;
    let status_text = if status_text.is_undefined() {
        String::new()
    } else {
        byte_string(vm, status_text)?
    };
    if status_text
        .chars()
        .any(|c| matches!(c, '\r' | '\n' | '\0' | '\u{7f}') || ((c as u32) < 32 && c != '\t'))
    {
        return Err(JsError::type_error("Invalid Response statusText"));
    }
    if !body.is_nullish() && matches!(status, 204 | 205 | 304) {
        return Err(JsError::type_error("Null-body status cannot have a body"));
    }
    let streams = state.borrow().streams.clone();
    let (body, content_type) = if streams.is_stream(vm, &body) {
        if streams.unusable(vm, &body) {
            return Err(JsError::type_error(
                "Response body stream has already been used or locked",
            ));
        }
        (body, None)
    } else {
        let body = body_input(vm, state, body)?;
        (memory_stream(vm, &streams, body.bytes)?, body.content_type)
    };
    let headers = field(vm, &init, "headers")?;
    let mut list = parse_headers(vm, headers, Guard::Response)?;
    add_content_type(&mut list, content_type.as_deref())?;
    let headers = headers_data(vm, state, list, Guard::Response)?;
    Ok((
        ResponseData {
            response: WebResponse {
                url: String::new(),
                status,
                status_text,
                headers: Vec::new(),
                body: None,
                response_type: ResponseType::Default,
                redirected: false,
            },
            headers,
        },
        body,
    ))
}
fn rejection(vm: &mut Vm, error: JsError) -> Result<Value, JsError> {
    if fatal(&error) {
        return Err(error);
    }
    if let Some(value) = error.thrown {
        Ok(value)
    } else {
        vm.error_value(&if error.kind == ErrorKind::UnsupportedFeature {
            JsError::type_error(error.message)
        } else {
            error
        })
    }
}

fn reject(vm: &mut Vm, reject: Value, error: JsError) -> Result<(), JsError> {
    let value = rejection(vm, error)?;
    vm.call(reject, Value::Undefined, &[value])?;
    Ok(())
}
impl FetchHost {
    pub(super) fn set_document(&self, document: &super::Shared) {
        self.state.borrow_mut().document = Rc::downgrade(document);
    }

    pub(super) fn set_xhr_meta_policy(&self, policy: &crate::loader::ConnectPolicy) {
        self.state.borrow_mut().xhr_meta_policy = policy.clone();
    }

    pub(super) fn set_base_resolver(&self, resolver: Rc<dyn Fn() -> Url>) {
        self.state.borrow_mut().base_resolver = Some(resolver);
    }
    pub(super) fn set_form_data(&self, form_data: form_data::FormDataHost) {
        self.state.borrow_mut().form_data = Some(form_data);
    }

    pub(super) fn install(
        vm: &mut Vm,
        page: &LoadedPage,
        abort: abort::AbortHost,
        streams: StreamHost,
        blobs: blobs::BlobFactory,
    ) -> Result<Self, JsError> {
        let client = Url::parse(&page.url).unwrap_or_else(|_| page.base.clone());
        let mut host =
            Self::install_with_urls(vm, page.base.clone(), client, abort, streams, blobs)?;
        host.set_xhr_meta_policy(&page.meta_connect_policy);
        host.xhr = Some(xhr::install(vm, &host.state, page)?);
        Ok(host)
    }

    pub(super) fn install_with_urls(
        vm: &mut Vm,
        base: Url,
        client: Url,
        abort: abort::AbortHost,
        streams: StreamHost,
        blobs: blobs::BlobFactory,
    ) -> Result<Self, JsError> {
        let hp = vm.new_object();
        let rp = vm.new_object();
        let sp = vm.new_object();
        let state = Rc::new(RefCell::new(State {
            byte_limit: LIVE_BYTES,
            base,
            base_resolver: None,
            document: Weak::new(),
            xhr_meta_policy: Default::default(),
            client,
            headers_proto: vm.root(hp.clone()),
            request_proto: vm.root(rp.clone()),
            response_proto: vm.root(sp.clone()),
            abort,
            streams,
            blobs,
            form_data: None,
            next: 1,
            pending: BTreeMap::new(),
            pending_xhr: BTreeMap::new(),
            ready: Vec::new(),
            bodies: Vec::new(),
            headers: Vec::new(),
            requests: Vec::new(),
            responses: Vec::new(),
        }));
        install_headers(vm, &state, hp.clone())?;
        install_request(vm, &state, rp.clone())?;
        install_response(vm, &state, sp.clone())?;
        wasm_streaming::install(vm, &sp)?;
        let shared = state.clone();
        let fetch = vm.new_native_function("fetch", move |vm, _, args| {
            let capability = vm.new_promise_capability()?;
            let result = (|| {
                if args.is_empty() {
                    return Err(JsError::type_error("fetch requires an input"));
                }
                if shared.borrow().pending_requests() >= PENDING {
                    return Err(JsError::type_error("Fetch pending request limit reached"));
                }
                let (data, signal, body) = request_init(vm, &shared, arg(args, 0), arg(args, 1))?;
                let streams = shared.borrow().streams.clone();
                if let RequestBody::Buffered(Some(body)) = &body {
                    streams.transfer_request(vm, body);
                }
                if let Some(reason) = abort::reason(vm, &signal) {
                    vm.call(
                        capability.reject.clone(),
                        Value::Undefined,
                        std::slice::from_ref(&reason),
                    )?;
                    if let RequestBody::Stream { value, .. } = &body {
                        streams.cancel_upload(vm, value, reason)?;
                    }
                    return Ok(());
                }
                let mut request = data.request;
                request.headers = data.headers.borrow().list.clone();
                validate_request(&mut request)?;
                let metadata = bytes(&request.headers)
                    + request.url.as_str().len()
                    + request.client_url.as_str().len()
                    + request.method.len()
                    + 512;
                budget(vm, &shared, metadata)?;
                // Init getters can reenter fetch and fill the queue.
                if shared.borrow().pending_requests() >= PENDING {
                    return Err(JsError::type_error("Fetch pending request limit reached"));
                }
                let id = shared.borrow_mut().next_id()?;
                let owner = Rc::downgrade(&shared);
                let cancel = CancelSignal::new();
                request.signal = Some(cancel.clone());
                let subscription = abort::subscribe(vm, &signal, move |vm, reason| {
                    if let Some(state) = owner.upgrade() {
                        abort_pending(vm, &state, id, reason)?;
                    }
                    Ok(())
                })?;
                let (upload, transport) = match body {
                    RequestBody::Stream { value, inherited }
                        if !matches!(request.url.scheme(), "data" | "blob") =>
                    {
                        let (upload, transport) =
                            upload::Upload::start(vm, &streams, value, inherited)?;
                        (Some(upload), Some(transport))
                    }
                    RequestBody::Stream {
                        value,
                        inherited: true,
                    } => {
                        streams.lock_upload(vm, &value, true)?;
                        (None, None)
                    }
                    _ => (None, None),
                };
                let (job, blob) = if request.url.scheme() == "blob" {
                    blob_url::queue(vm, &shared, id)?;
                    (None, Some(blob_url::response(&request, data.blob)))
                } else {
                    (
                        Some(FetchJob {
                            id,
                            request,
                            upload: transport,
                        }),
                        None,
                    )
                };
                let mut state = shared.borrow_mut();
                state.pending.insert(
                    id,
                    Pending {
                        promise: vm.root(capability.promise.clone()),
                        resolve: vm.root(capability.resolve.clone()),
                        reject: vm.root(capability.reject.clone()),
                        signal: vm.root(signal),
                        cancel,
                        _abort: subscription,
                        metadata,
                        upload,
                        blob,
                    },
                );
                if let Some(job) = job {
                    state.ready.push(job);
                }
                Ok(())
            })();
            if let Err(error) = result {
                reject(vm, capability.reject, error)?;
            }
            Ok(capability.promise)
        });
        vm.set_global("fetch", fetch)?;
        Ok(Self { state, xhr: None })
    }
    pub(super) fn take_requests(&self) -> Vec<(u64, WebRequest)> {
        let mut state = self.state.borrow_mut();
        let mut requests = Vec::new();
        for job in std::mem::take(&mut state.ready) {
            if job.upload.is_some() {
                state.ready.push(job);
            } else {
                requests.push((job.id, job.request));
            }
        }
        requests
    }
    pub(super) fn take_jobs(&self) -> Vec<FetchJob> {
        std::mem::take(&mut self.state.borrow_mut().ready)
    }
    pub(super) fn has_queued_requests(&self) -> bool {
        !self.state.borrow().ready.is_empty()
    }
    pub(super) fn has_pending_request(&self, id: u64) -> bool {
        let state = self.state.borrow();
        state.pending.contains_key(&id) || state.pending_xhr.contains_key(&id)
    }
    pub(super) fn pending_count(&self) -> usize {
        let state = self.state.borrow();
        state.pending_requests() + usize::from(state.streams.has_network_waiters())
    }
    pub(super) fn progress(&self, vm: &mut Vm, id: u64) -> Result<(), JsError> {
        let is_xhr = self.state.borrow().pending_xhr.contains_key(&id);
        if let Some(xhr) = &self.xhr
            && is_xhr
        {
            return xhr.progress(vm, id);
        }
        let streams = self.state.borrow().streams.clone();
        streams.progress(vm, id)
    }
    pub(super) fn progress_upload(&self, vm: &mut Vm, id: u64) -> Result<(), JsError> {
        upload::demand(vm, &self.state, id)
    }
    pub(super) fn cancel_all(&self, vm: &mut Vm) -> Result<(), JsError> {
        let ids = self
            .state
            .borrow()
            .pending
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let (abort, streams) = {
            let state = self.state.borrow();
            (state.abort.clone(), state.streams.clone())
        };
        let reason = abort.default_reason(vm, Value::Undefined)?;
        let mut error = self.xhr.as_ref().and_then(|xhr| xhr.cancel_all(vm).err());
        for id in ids {
            if let Err(failed) = abort_pending(vm, &self.state, id, reason.clone()) {
                error.get_or_insert(failed);
            }
        }
        let stream_result = streams.stop(vm, reason);
        error.map_or(stream_result, Err)
    }
    pub(super) fn complete(
        &self,
        vm: &mut Vm,
        id: u64,
        result: Result<StreamingResponse, FetchError>,
    ) -> Result<(), JsError> {
        let is_xhr = self.state.borrow().pending_xhr.contains_key(&id);
        if let Some(xhr) = &self.xhr
            && is_xhr
        {
            return xhr.complete(vm, id, result);
        }
        let result = result.map(
            |StreamingResponse {
                 mut metadata,
                 transfer,
             }| {
                let body = match transfer {
                    Some(transfer) => ResponseBody::Network(transfer),
                    None => ResponseBody::Buffered(metadata.body.take()),
                };
                (metadata, body)
            },
        );
        self.complete_response(vm, id, result)
    }

    fn complete_response(
        &self,
        vm: &mut Vm,
        id: u64,
        result: Result<(WebResponse, ResponseBody), FetchError>,
    ) -> Result<(), JsError> {
        let Some(mut pending) = self.state.borrow_mut().pending.remove(&id) else {
            return Ok(());
        };
        let result = (|| {
            let resolve = vm
                .root_value(pending.resolve)
                .ok_or_else(|| JsError::type_error("Fetch resolving function is missing"))?;
            let reject_function = vm
                .root_value(pending.reject)
                .ok_or_else(|| JsError::type_error("Fetch rejecting function is missing"))?;
            match result {
                Ok((mut response, body)) => {
                    response.headers.retain(|(name, _)| {
                        !matches!(
                            name.to_ascii_lowercase().as_str(),
                            "set-cookie" | "set-cookie2"
                        )
                    });
                    let result = (|| {
                        if response.url.len() > FETCH_HEADER_LIMIT
                            || response.status_text.len() > FETCH_HEADER_LIMIT
                            || bytes(&response.headers) > FETCH_HEADER_LIMIT * 2
                        {
                            return Err(JsError::type_error(
                                "Fetch response metadata exceeds alpha limit",
                            ));
                        }
                        // Keep the existing Fetch budget for network buffers.
                        // Local Blob backings are bounded by the Blob and
                        // ReadableStream stores and need no buffered copy.
                        if let ResponseBody::Buffered(Some(body)) = &body {
                            if body.len() > FETCH_BODY_LIMIT {
                                return Err(JsError::type_error(
                                    "Fetch response body exceeds 4 MiB",
                                ));
                            }
                            let known = self
                                .state
                                .borrow()
                                .bodies
                                .iter()
                                .any(|old| old.as_ptr() == Arc::as_ptr(body));
                            budget(
                                vm,
                                &self.state,
                                if known { 0 } else { body.capacity() + 64 },
                            )?;
                            if !known {
                                self.state.borrow_mut().bodies.push(Arc::downgrade(body));
                            }
                        }
                        let headers = headers_data(
                            vm,
                            &self.state,
                            response.headers.clone(),
                            Guard::Immutable,
                        )?;
                        let streams = self.state.borrow().streams.clone();
                        let signal = vm.root_value(pending.signal).unwrap();
                        let body = match body {
                            ResponseBody::Network(transfer) => streams.network_stream(
                                vm,
                                id,
                                transfer,
                                signal,
                                pending.cancel.clone(),
                            )?,
                            ResponseBody::Buffered(bytes) => {
                                let body = memory_stream(vm, &streams, bytes)?;
                                if !body.is_nullish() {
                                    streams.watch_abort(
                                        vm,
                                        &body,
                                        signal,
                                        pending.cancel.clone(),
                                    )?;
                                }
                                body
                            }
                            ResponseBody::Blob(blob, range) => {
                                let (bytes, backing) = blob.shared_bytes();
                                let start = backing.start;
                                let range = start + range.start..start + range.end;
                                let body = streams.memory_stream(vm, bytes, range)?;
                                streams.watch_abort(vm, &body, signal, pending.cancel.clone())?;
                                body
                            }
                        };
                        response_object(vm, &self.state, ResponseData { response, headers }, body)
                    })();
                    match result {
                        Ok(response) => {
                            vm.call(resolve, Value::Undefined, &[response])?;
                        }
                        Err(error) => reject(vm, reject_function, error)?,
                    }
                }
                Err(error) => {
                    let reason = rejection(vm, map_error(error))?;
                    let reject = vm.call(
                        reject_function,
                        Value::Undefined,
                        std::slice::from_ref(&reason),
                    );
                    let stopped = pending.stop_upload(vm, &self.state, reason);
                    reject?;
                    stopped?;
                }
            }
            Ok(())
        })();
        pending.release(vm);
        result
    }
}

impl Pending {
    fn stop_upload(&mut self, vm: &mut Vm, state: &Shared, reason: Value) -> Result<(), JsError> {
        if let Some(upload) = &mut self.upload {
            let streams = state.borrow().streams.clone();
            upload.cancel(vm, &streams, reason)?;
        }
        Ok(())
    }

    fn release(self, vm: &mut Vm) {
        for root in [self.promise, self.resolve, self.reject, self.signal] {
            vm.unroot(root);
        }
        if let Some(upload) = self.upload {
            upload.release(vm);
        }
    }
}

fn abort_pending(vm: &mut Vm, state: &Shared, id: u64, reason: Value) -> Result<(), JsError> {
    let pending = {
        let mut state = state.borrow_mut();
        state.ready.retain(|ready| ready.id != id);
        state.pending.remove(&id)
    };
    let Some(mut pending) = pending else {
        return Ok(());
    };
    pending.cancel.cancel();
    let result = if let Some(reject) = vm.root_value(pending.reject) {
        vm.call(reject, Value::Undefined, std::slice::from_ref(&reason))
            .map(|_| ())
    } else {
        Err(JsError::new(
            ErrorKind::InternalError,
            "Fetch rejecting function is missing",
        ))
    };
    let stopped = pending.stop_upload(vm, state, reason);
    pending.release(vm);
    result.and(stopped)
}

fn constructor(
    vm: &mut Vm,
    name: &str,
    prototype: Value,
    callback: impl Fn(&mut Vm, Value, &[Value], bool) -> Result<Value, JsError> + 'static,
) -> Result<Value, JsError> {
    let constructor = vm.new_native_constructor(name, callback);
    vm.define_data_property(
        constructor.clone(),
        "prototype",
        prototype.clone(),
        false,
        false,
        false,
    )?;
    vm.define_data_property(
        prototype,
        "constructor",
        constructor.clone(),
        true,
        false,
        true,
    )?;
    vm.set_global(name, constructor.clone())?;
    Ok(constructor)
}
fn install_headers(vm: &mut Vm, state: &Shared, prototype: Value) -> Result<(), JsError> {
    let shared = state.clone();
    constructor(vm, "Headers", prototype.clone(), move |vm, _, args, new| {
        if !new {
            return Err(JsError::type_error("Headers requires new"));
        }
        let list = parse_headers(vm, arg(args, 0), Guard::None)?;
        let data = headers_data(vm, &shared, list, Guard::None)?;
        headers_object(vm, &shared, data)
    })?;
    for name in ["get", "has", "delete", "set", "append"] {
        let shared = state.clone();
        method(vm, prototype.clone(), name, move |vm, this, args| {
            let data = typed::<HeadersData>(vm, &this, "Headers")?;
            if args.is_empty() || matches!(name, "set" | "append") && args.len() < 2 {
                return Err(JsError::type_error("Not enough Headers method arguments"));
            }
            let key = byte_string(vm, arg(args, 0))?;
            let value = if matches!(name, "set" | "append") {
                byte_string(vm, arg(args, 1))?
            } else {
                String::new()
            };
            let (key, value) = network::normalize_header(&key, &value).map_err(map_error)?;
            let (guard, old) = {
                let data = data.borrow();
                (data.guard, data.list.clone())
            };
            vm.charge_work(old.len() + bytes(&old))?;
            if name == "get" {
                let values = old
                    .iter()
                    .filter(|(k, _)| k == &key)
                    .map(|(_, v)| v.as_str())
                    .collect::<Vec<_>>();
                return Ok(if values.is_empty() {
                    Value::Null
                } else {
                    Value::from(values.join(", "))
                });
            }
            if name == "has" {
                return Ok(Value::Bool(old.iter().any(|(k, _)| k == &key)));
            }
            if guard == Guard::Immutable {
                return Err(JsError::type_error("Headers are immutable"));
            }
            let allowed = if name == "delete" && guard == Guard::NoCors {
                matches!(
                    key.as_str(),
                    "accept" | "accept-language" | "content-language" | "content-type"
                )
            } else {
                header_guard(guard, &key, &value)
            };
            if !allowed {
                return Ok(Value::Undefined);
            }
            let mut next = old.clone();
            if matches!(name, "delete" | "set") {
                next.retain(|(k, _)| k != &key);
            }
            if name != "delete" {
                if guard == Guard::NoCors {
                    let combined = next
                        .iter()
                        .find(|(k, _)| k == &key)
                        .map_or(value.clone(), |(_, old)| format!("{old}, {value}"));
                    if !header_guard(guard, &key, &combined) {
                        return Ok(Value::Undefined);
                    }
                }
                append(&mut next, &key, &value)?;
            }
            budget(vm, &shared, bytes(&next).saturating_sub(bytes(&old)))?;
            data.borrow_mut().list = next;
            Ok(Value::Undefined)
        })?;
    }
    method(vm, prototype.clone(), "getSetCookie", |vm, this, _| {
        let data = typed::<HeadersData>(vm, &this, "Headers")?;
        let values = data
            .borrow()
            .list
            .iter()
            .filter(|(n, _)| n == "set-cookie")
            .map(|(_, v)| Value::from(v.clone()))
            .collect::<Vec<_>>();
        Ok(vm.new_array(&values))
    })?;
    method(vm, prototype.clone(), "forEach", |vm, this, args| {
        let data = typed::<HeadersData>(vm, &this, "Headers")?;
        let callback = arg(args, 0);
        if !vm.is_callable(&callback) {
            return Err(JsError::type_error("Headers callback is not callable"));
        }
        let mut index = 0;
        loop {
            let mut entries = data.borrow().list.clone();
            vm.charge_work(entries.len() + bytes(&entries))?;
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let Some((name, value)) = entries.get(index).cloned() else {
                break;
            };
            vm.call(
                callback.clone(),
                arg(args, 1),
                &[Value::from(value), Value::from(name), this.clone()],
            )?;
            index += 1;
        }
        Ok(Value::Undefined)
    })?;
    for name in ["entries", "keys", "values"] {
        method(vm, prototype.clone(), name, |_, _, _| {
            Err(unsupported(
                "Headers iterators require the unimplemented iterator protocol",
            ))
        })?;
    }
    Ok(())
}
fn body_for(vm: &Vm, object: &Value, request: bool) -> Result<Value, JsError> {
    if request {
        typed::<RequestData>(vm, object, "Request")?;
    } else {
        typed::<ResponseData>(vm, object, "Response")?;
    }
    Ok(vm.host_slot(object, BODY_SLOT).unwrap_or(Value::Null))
}

fn extract_mime(headers: &HeaderList) -> String {
    let mut mime = None::<crate::mime::MimeType>;
    let mut charset = None;
    for (_, header) in headers.iter().filter(|(name, _)| name == "content-type") {
        let mut quoted = false;
        let mut escaped = false;
        let mut start = 0;
        for (index, ch) in header.char_indices().chain([(header.len(), ',')]) {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' if quoted => escaped = true,
                '"' => quoted = !quoted,
                ',' if !quoted || index == header.len() => {
                    let item = crate::mime::MimeType::parse(&header[start..index]);
                    start = index + 1;
                    let Some(mut item) = item.filter(|item| item.essence != "*/*") else {
                        continue;
                    };
                    if mime
                        .as_ref()
                        .is_none_or(|prev| item.essence != prev.essence)
                    {
                        charset = item.parameter("charset").map(str::to_owned);
                    } else if item.parameter("charset").is_none()
                        && let Some(charset) = &charset
                    {
                        item.parameters.push(("charset".into(), charset.clone()));
                    }
                    mime = Some(item);
                }
                _ => {}
            }
        }
    }
    mime.map_or_else(String::new, |mime| mime.serialize())
}

fn body_methods(
    vm: &mut Vm,
    state: &Shared,
    prototype: Value,
    request: bool,
) -> Result<(), JsError> {
    for name in ["text", "json", "arrayBuffer", "bytes", "blob", "formData"] {
        let shared = state.clone();
        method(vm, prototype.clone(), name, move |vm, this, _| {
            let body = match body_for(vm, &this, request) {
                Ok(value) => value,
                Err(error) => {
                    let cap = vm.new_promise_capability()?;
                    reject(vm, cap.reject, error)?;
                    return Ok(cap.promise);
                }
            };
            let mime = if matches!(name, "blob" | "formData") {
                let headers = if request {
                    typed::<RequestData>(vm, &this, "Request")?
                        .borrow()
                        .headers
                        .clone()
                } else {
                    typed::<ResponseData>(vm, &this, "Response")?
                        .borrow()
                        .headers
                        .clone()
                };
                extract_mime(&headers.borrow().list)
            } else {
                String::new()
            };
            let (streams, blobs, forms) = {
                let state = shared.borrow();
                (
                    state.streams.clone(),
                    state.blobs.clone(),
                    state.form_data.clone(),
                )
            };
            streams.collect_bytes(
                vm,
                body,
                Box::new(move |vm, bytes, resolve, reject| {
                    let value = match name {
                        "arrayBuffer" => vm.new_array_buffer(bytes)?,
                        "bytes" => vm.new_uint8_array(bytes)?,
                        "blob" => blobs.from_bytes(vm, bytes, &mime)?,
                        "formData" => forms
                            .ok_or_else(|| JsError::type_error("FormData is unavailable"))?
                            .from_body(vm, &mime, &bytes)?,
                        "text" | "json" => {
                            vm.charge_work(bytes.len())?;
                            vm.ensure_string_allocation(bytes.len())?;
                            let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
                            let text = JsString::from(String::from_utf8_lossy(bytes).as_ref());
                            if name == "json" {
                                return vm.enqueue_json_parse(resolve, reject, text);
                            }
                            Value::String(text)
                        }
                        _ => unreachable!(),
                    };
                    vm.enqueue_call(resolve, Value::Undefined, &[value])
                }),
            )
        })?;
    }
    let streams = state.borrow().streams.clone();
    getter(vm, prototype.clone(), "bodyUsed", move |vm, this, _| {
        Ok(streams.disturbed(vm, &body_for(vm, &this, request)?).into())
    })?;
    getter(vm, prototype, "body", move |vm, this, _| {
        body_for(vm, &this, request)
    })?;
    Ok(())
}

fn cloneable_body(vm: &Vm, state: &Shared, value: &Value) -> Result<Value, JsError> {
    let body = vm.host_slot(value, BODY_SLOT).unwrap_or(Value::Null);
    if state.borrow().streams.unusable(vm, &body) {
        return Err(JsError::type_error("Body has already been used or locked"));
    }
    Ok(body)
}

fn install_cloned_body(
    vm: &mut Vm,
    state: &Shared,
    body: Value,
    original: Value,
    clone: Value,
) -> Result<Value, JsError> {
    if !body.is_nullish() {
        let streams = state.borrow().streams.clone();
        let [left, right] = streams.tee(vm, &body)?;
        vm.set_host_slot(original, BODY_SLOT, left)?;
        vm.set_host_slot(clone.clone(), BODY_SLOT, right)?;
    }
    Ok(clone)
}

fn install_request(vm: &mut Vm, state: &Shared, prototype: Value) -> Result<(), JsError> {
    let shared = state.clone();
    constructor(vm, "Request", prototype.clone(), move |vm, _, args, new| {
        if !new {
            return Err(JsError::type_error("Request requires new"));
        }
        if args.is_empty() {
            return Err(JsError::type_error("Request requires input"));
        }
        let (data, signal, body) = request_init(vm, &shared, arg(args, 0), arg(args, 1))?;
        let streams = shared.borrow().streams.clone();
        match body {
            RequestBody::Stream { value, inherited } => {
                let stream = if inherited {
                    streams.request_proxy(vm, value)?
                } else {
                    value
                };
                request_object(vm, &shared, data, signal, stream)
            }
            RequestBody::Buffered(inherited) => {
                let body = memory_stream(vm, &streams, data.request.body.clone())?;
                let request = request_object(vm, &shared, data, signal, body)?;
                if let Some(body) = inherited {
                    streams.transfer_request(vm, &body);
                }
                Ok(request)
            }
        }
    })?;
    for name in [
        "url",
        "method",
        "mode",
        "credentials",
        "redirect",
        "duplex",
        "headers",
        "signal",
    ] {
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            let data = typed::<RequestData>(vm, &this, "Request")?;
            if matches!(name, "headers" | "signal") {
                let slot = if name == "headers" {
                    HEADERS_SLOT
                } else {
                    SIGNAL_SLOT
                };
                return vm
                    .host_slot(&this, slot)
                    .ok_or_else(|| JsError::type_error("Missing Request property"));
            }
            let data = data.borrow();
            Ok(Value::from(match name {
                "url" => data.request.url.as_str(),
                "method" => &data.request.method,
                "mode" => match data.request.mode {
                    RequestMode::Cors => "cors",
                    RequestMode::SameOrigin => "same-origin",
                    RequestMode::NoCors => "no-cors",
                },
                "credentials" => match data.request.credentials {
                    CredentialsMode::Omit => "omit",
                    CredentialsMode::SameOrigin => "same-origin",
                    CredentialsMode::Include => "include",
                },
                "duplex" => "half",
                _ => match data.request.redirect {
                    RedirectMode::Follow => "follow",
                    RedirectMode::Error => "error",
                    RedirectMode::Manual => "manual",
                },
            }))
        })?;
    }
    body_methods(vm, state, prototype.clone(), true)?;
    let shared = state.clone();
    method(vm, prototype, "clone", move |vm, this, _| {
        let original = typed::<RequestData>(vm, &this, "Request")?;
        let mut data = original.borrow().clone();
        let body = cloneable_body(vm, &shared, &this)?;
        let (guard, list) = {
            let headers = data.headers.borrow();
            (headers.guard, headers.list.clone())
        };
        data.headers = headers_data(vm, &shared, list, guard)?;
        let source = vm
            .host_slot(&this, SIGNAL_SLOT)
            .ok_or_else(|| JsError::type_error("Request signal slot is missing"))?;
        let abort = shared.borrow().abort.clone();
        let signal = abort.dependent(vm, &[source])?;
        let clone = request_object(vm, &shared, data, signal, Value::Null)?;
        install_cloned_body(vm, &shared, body, this, clone)
    })?;
    Ok(())
}
fn install_response(vm: &mut Vm, state: &Shared, prototype: Value) -> Result<(), JsError> {
    let shared = state.clone();
    let constructor = constructor(
        vm,
        "Response",
        prototype.clone(),
        move |vm, _, args, new| {
            if !new {
                return Err(JsError::type_error("Response requires new"));
            }
            let (data, body) = response_init(vm, &shared, arg(args, 0), arg(args, 1))?;
            response_object(vm, &shared, data, body)
        },
    )?;
    for name in [
        "url",
        "type",
        "status",
        "statusText",
        "ok",
        "redirected",
        "headers",
    ] {
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            let data = typed::<ResponseData>(vm, &this, "Response")?;
            if name == "headers" {
                return vm
                    .host_slot(&this, HEADERS_SLOT)
                    .ok_or_else(|| JsError::type_error("Response headers slot is missing"));
            }
            let data = data.borrow();
            Ok(match name {
                "url" => Value::from(data.response.url.clone()),
                "type" => Value::from(data.response.response_type.as_str()),
                "status" => Value::from(data.response.status as u32),
                "statusText" => Value::from(data.response.status_text.clone()),
                "ok" => Value::Bool((200..=299).contains(&data.response.status)),
                _ => Value::Bool(data.response.redirected),
            })
        })?;
    }
    body_methods(vm, state, prototype.clone(), false)?;
    let shared = state.clone();
    method(vm, prototype, "clone", move |vm, this, _| {
        let original = typed::<ResponseData>(vm, &this, "Response")?;
        let mut data = original.borrow().clone();
        let body = cloneable_body(vm, &shared, &this)?;
        let (guard, list) = {
            let headers = data.headers.borrow();
            (headers.guard, headers.list.clone())
        };
        data.headers = headers_data(vm, &shared, list, guard)?;
        let clone = response_object(vm, &shared, data, Value::Null)?;
        install_cloned_body(vm, &shared, body, this, clone)
    })?;
    let shared = state.clone();
    method(vm, constructor.clone(), "error", move |vm, _, _| {
        let response = WebResponse::opaque(ResponseType::Error);
        let headers = headers_data(vm, &shared, Vec::new(), Guard::Immutable)?;
        response_object(vm, &shared, ResponseData { response, headers }, Value::Null)
    })?;
    for name in ["redirect", "json"] {
        method(vm, constructor.clone(), name, |_, _, _| {
            Err(unsupported(
                "Static Response.redirect/json are not implemented",
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Vm, FetchHost) {
        let mut vm = Vm::new();
        let page = LoadedPage::from_html("https://example.test/base/", "<base href='/assets/'>");
        let abort = abort::AbortHost::install(&mut vm, None).unwrap();
        let streams = StreamHost::install(&mut vm, abort.clone()).unwrap();
        let blobs = blobs::install(&mut vm, streams.clone()).unwrap();
        let host = FetchHost::install_with_urls(
            &mut vm,
            page.base,
            Url::parse(&page.url).unwrap(),
            abort,
            streams,
            blobs,
        )
        .unwrap();
        (vm, host)
    }
    fn drain(vm: &mut Vm) {
        vm.with_task(|vm| {
            while vm.has_pending_jobs() {
                vm.drain_jobs(1)?;
            }
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn headers_are_branded_byte_strings_with_private_shared_identity() {
        let (mut vm, _) = setup();
        assert_eq!(vm.eval("var h=new Headers([['X-A',' a '],['x-a','b']]);h.get('X-A')==='a, b' && Object.keys(h).length===0").unwrap(),Value::Bool(true));
        assert_eq!(
            vm.eval("Headers.prototype.get.call({},'x')")
                .unwrap_err()
                .kind,
            ErrorKind::TypeError
        );
        assert_eq!(
            vm.eval("new Headers({'x':'\\u0100'})").unwrap_err().kind,
            ErrorKind::TypeError
        );
        assert_eq!(
            vm.eval(
                "var r=new Response('x');r.headers===r.headers && r.headers instanceof Headers"
            )
            .unwrap(),
            Value::Bool(true)
        );
    }
    #[test]
    fn response_body_reads_are_queued_single_use_and_clone_independent() {
        let (mut vm, _) = setup();
        vm.eval("var log=[];var r=new Response('{\"n\":7}');var c=r.clone();r.json().then(v=>log.push(v.n));c.text().then(v=>log.push(v));r.text().catch(e=>log.push(e instanceof TypeError));log.push('sync');").unwrap();
        assert_eq!(
            vm.eval("r.bodyUsed && c.bodyUsed && log.join(',')==='sync'")
                .unwrap(),
            Value::Bool(true)
        );
        drain(&mut vm);
        assert_eq!(
            vm.eval("log[1]===7 && log[2]==='{\"n\":7}' && log[3]===true")
                .unwrap(),
            Value::Bool(true)
        );
    }
    #[test]
    fn fetch_queues_requests_settles_promises_and_drops_pending_roots() {
        let (mut vm, host) = setup();
        vm.eval("var result;fetch('data',{method:'POST',body:'hello',headers:{'x-test':'yes'}}).then(r=>r.text()).then(v=>result=v);").unwrap();
        let mut requests = host.take_requests();
        assert_eq!(requests.len(), 1);
        let (id, request) = requests.remove(0);
        assert_eq!(request.url.as_str(), "https://example.test/assets/data");
        assert_eq!(request.body.as_ref().unwrap().as_slice(), b"hello");
        assert_eq!(host.pending_count(), 1);
        vm.collect_garbage();
        host.complete(
            &mut vm,
            id,
            Ok(WebResponse {
                url: request.url.into(),
                status: 200,
                status_text: "OK".into(),
                headers: vec![("content-type".into(), "text/plain".into())],
                body: Some(Arc::new(b"reply".to_vec())),
                response_type: ResponseType::Basic,
                redirected: false,
            }
            .into()),
        )
        .unwrap();
        assert_eq!(host.pending_count(), 0);
        drain(&mut vm);
        assert_eq!(vm.get_global("result").unwrap(), Value::from("reply"));
    }
    #[test]
    fn invalid_fetch_rejects_and_mutable_globals_do_not_replace_intrinsics() {
        let (mut vm, host) = setup();
        vm.eval("var n=0;fetch('/x',{method:'GET',body:'bad'}).catch(e=>n++);fetch('/x',{signal:{}}).catch(e=>n++);JSON=null;Promise=null;Object=null;var r=new Response('bad json');r.json().catch(e=>{if(e instanceof SyntaxError)n++;});").unwrap();
        assert_eq!(host.pending_count(), 0);
        drain(&mut vm);
        assert_eq!(vm.get_global("n").unwrap(), Value::from(3));
    }
    #[test]
    fn live_body_quota_counts_shared_clones_once() {
        let (mut vm, host) = setup();
        host.state.borrow_mut().byte_limit = 256 * 1024;
        let body = Arc::new(vec![b'x'; 64 * 1024]);
        for i in 0..5 {
            vm.eval(&format!(
                "var r{i};fetch('/{i}').then(r=>r{i}=r).catch(()=>{{}});"
            ))
            .unwrap();
            let (id, _) = host.take_requests().remove(0);
            let result = host.complete(
                &mut vm,
                id,
                Ok(WebResponse {
                    url: "https://example.test/x".into(),
                    status: 200,
                    status_text: String::new(),
                    headers: vec![],
                    body: Some(body.clone()),
                    response_type: ResponseType::Basic,
                    redirected: false,
                }
                .into()),
            );
            assert!(result.is_ok());
            drain(&mut vm);
        }
        assert!(
            vm.eval("r4.clone() instanceof Response")
                .unwrap()
                .is_truthy()
        );
    }
    #[test]
    fn small_live_body_quota_is_enforced_and_recovers_after_collection() {
        let (mut vm, host) = setup();
        host.state.borrow_mut().byte_limit = 256 * 1024;
        vm.eval("var a=new Response('x'.repeat(96*1024));var b=new Response('x'.repeat(96*1024));")
            .unwrap();
        assert_eq!(
            vm.eval("new Response('x'.repeat(96*1024))")
                .unwrap_err()
                .kind,
            ErrorKind::HeapLimit
        );
        vm.set_global("a", Value::Null).unwrap();
        vm.set_global("b", Value::Null).unwrap();
        vm.collect_garbage();
        assert!(vm.eval("new Response('ok')").unwrap().as_object().is_some());
    }
    #[test]
    fn pending_queue_predicate_does_not_confuse_inflight_with_outgoing() {
        let (mut vm, host) = setup();
        vm.eval("var rejected=0;for(var i=0;i<17;i++){fetch('/x').catch(()=>rejected++);}")
            .unwrap();
        assert_eq!(host.pending_count(), 16);
        assert!(host.has_queued_requests());
        let requests = host.take_requests();
        assert!(!host.has_queued_requests());
        assert_eq!(host.pending_count(), 16);
        for (id, _) in requests {
            host.complete(
                &mut vm,
                id,
                Err(FetchError::new(FetchErrorKind::Network, "offline")),
            )
            .unwrap();
        }
        drain(&mut vm);
        assert_eq!(vm.get_global("rejected").unwrap(), Value::from(17));
        assert_eq!(host.pending_count(), 0);
    }
    #[test]
    fn dynamic_base_and_request_header_guards_are_live() {
        let (mut vm, host) = setup();
        let base = Rc::new(RefCell::new(
            Url::parse("https://example.test/first/").unwrap(),
        ));
        let read = base.clone();
        host.set_base_resolver(Rc::new(move || read.borrow().clone()));
        vm.eval("fetch('one');var r=new Request('/x',{method:'POST',mode:'no-cors',headers:{'x-secret':'no','content-type':'text/plain','cookie':'secret'}});r.headers.delete('content-type');").unwrap();
        assert_eq!(
            host.take_requests()[0].1.url.as_str(),
            "https://example.test/first/one"
        );
        *base.borrow_mut() = Url::parse("https://example.test/second/").unwrap();
        vm.eval("fetch('two')").unwrap();
        assert_eq!(
            host.take_requests()[0].1.url.as_str(),
            "https://example.test/second/two"
        );
        assert_eq!(vm.eval("r.headers.get('x-secret')===null && r.headers.get('cookie')===null && r.headers.get('content-type')===null").unwrap(),Value::Bool(true));
    }
}
