//! Fetch transport and origin/header policy. This runs on the transport reactor,
//! never inside a JavaScript native callback or the UI event loop.
mod streaming;
mod upload;

pub use crate::loader::transport::CancelSignal;
use crate::{
    Loader,
    loader::{
        ConnectPolicy,
        transport::{BodyError, Interrupted, RequestControl},
    },
};
use reqwest::{
    Method, StatusCode,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use std::{
    collections::BTreeSet,
    fmt,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
pub use streaming::{FETCH_CHUNK_BYTES, FetchBody, FetchTransfer, StreamingResponse};
pub use upload::{FetchJob, UploadBody, UploadDemand};
pub(crate) use upload::{UploadSender, upload_pipe};
use url::Url;

/// Maximum size of an outgoing request body.
pub const FETCH_BODY_LIMIT: usize = 4 * 1024 * 1024;
/// Maximum decoded response size for Fetch and XMLHttpRequest.
pub const FETCH_RESPONSE_BODY_LIMIT: usize = 64 * 1024 * 1024;
pub const FETCH_HEADER_LIMIT: usize = 64 * 1024;
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(18);
pub type HeaderList = Vec<(String, String)>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RequestMode {
    #[default]
    Cors,
    SameOrigin,
    NoCors,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CredentialsMode {
    Omit,
    #[default]
    SameOrigin,
    Include,
}
pub(crate) fn crossorigin_credentials(value: Option<&str>) -> Option<CredentialsMode> {
    value.map(|value| {
        if value.eq_ignore_ascii_case("use-credentials") {
            CredentialsMode::Include
        } else {
            CredentialsMode::SameOrigin
        }
    })
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RedirectMode {
    #[default]
    Follow,
    Error,
    Manual,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RequestCache {
    #[default]
    Default,
    NoStore,
    Reload,
    NoCache,
    ForceCache,
    OnlyIfCached,
}

impl RequestCache {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::NoStore => "no-store",
            Self::Reload => "reload",
            Self::NoCache => "no-cache",
            Self::ForceCache => "force-cache",
            Self::OnlyIfCached => "only-if-cached",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseType {
    Basic,
    Cors,
    Opaque,
    OpaqueRedirect,
    Default,
    Error,
}
impl ResponseType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Cors => "cors",
            Self::Opaque => "opaque",
            Self::OpaqueRedirect => "opaqueredirect",
            Self::Default => "default",
            Self::Error => "error",
        }
    }
}
#[derive(Clone, Debug)]
pub struct WebRequest {
    pub url: Url,
    pub client_url: Url,
    pub method: String,
    pub headers: HeaderList,
    pub body: Option<Arc<Vec<u8>>>,
    pub mode: RequestMode,
    pub credentials: CredentialsMode,
    pub redirect: RedirectMode,
    pub cache: RequestCache,
    pub signal: Option<CancelSignal>,
    // A document API can carry the active connect-src snapshot through the
    // generic transport, including checks on redirect destinations.
    pub(crate) connect_policy: Option<ConnectPolicy>,
}
impl WebRequest {
    pub fn get(url: Url, client_url: Url) -> Self {
        Self {
            url,
            client_url,
            method: "GET".into(),
            headers: vec![],
            body: None,
            mode: RequestMode::Cors,
            credentials: CredentialsMode::SameOrigin,
            redirect: RedirectMode::Follow,
            cache: RequestCache::Default,
            signal: None,
            connect_policy: None,
        }
    }
    pub fn validate(&mut self) -> Result<(), FetchError> {
        validate_url(&self.url)?;
        self.validate_metadata()
    }

    pub(crate) fn validate_metadata(&mut self) -> Result<(), FetchError> {
        self.method = normalize_method(&self.method)?;
        if self.cache == RequestCache::OnlyIfCached && self.mode != RequestMode::SameOrigin {
            return Err(FetchError::invalid("only-if-cached requires same-origin mode"));
        }
        if matches!(self.method.as_str(), "GET" | "HEAD") && self.body.is_some() {
            return Err(FetchError::invalid("GET/HEAD cannot have a request body"));
        }
        if self
            .body
            .as_ref()
            .is_some_and(|body| body.len() > FETCH_BODY_LIMIT)
        {
            return Err(FetchError::limit("Fetch request body exceeds 4 MiB"));
        }
        if self.mode == RequestMode::NoCors && !simple_method(&self.method) {
            return Err(FetchError::invalid(
                "no-cors method must be GET, HEAD or POST",
            ));
        }
        let mut headers = vec![];
        for (name, value) in &self.headers {
            let (name, value) = normalize_header(name, value)?;
            if forbidden_request_header(&name, &value) {
                continue;
            }
            if self.mode == RequestMode::NoCors
                && (!matches!(
                    name.as_str(),
                    "accept" | "accept-language" | "content-language" | "content-type"
                ) || !safelisted_header(&name, &value))
            {
                continue;
            }
            append_header(&mut headers, &name, &value)?;
        }
        self.headers = headers;
        Ok(())
    }

    pub(crate) fn validate_streaming_upload(&self) -> Result<(), FetchError> {
        if matches!(self.method.as_str(), "GET" | "HEAD") {
            return Err(FetchError::invalid("GET/HEAD cannot have a request body"));
        }
        if self.mode == RequestMode::NoCors {
            return Err(FetchError::invalid(
                "A streaming body requires cors or same-origin mode",
            ));
        }
        if self.body.is_some() {
            return Err(FetchError::invalid("Conflicting Fetch upload bodies"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct WebResponse {
    pub url: String,
    pub status: u16,
    pub status_text: String,
    pub headers: HeaderList,
    pub body: Option<Arc<Vec<u8>>>,
    pub response_type: ResponseType,
    pub redirected: bool,
}
impl WebResponse {
    pub fn opaque(kind: ResponseType) -> Self {
        Self {
            url: String::new(),
            status: 0,
            status_text: String::new(),
            headers: vec![],
            body: None,
            response_type: kind,
            redirected: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchErrorKind {
    Invalid,
    Network,
    Cors,
    Aborted,
    Limit,
    Unsupported,
}
#[derive(Clone, Debug)]
pub struct FetchError {
    pub kind: FetchErrorKind,
    pub message: String,
}
impl FetchError {
    pub fn new(kind: FetchErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(FetchErrorKind::Invalid, message)
    }
    pub fn limit(message: impl Into<String>) -> Self {
        Self::new(FetchErrorKind::Limit, message)
    }
    fn cors(message: impl Into<String>) -> Self {
        Self::new(FetchErrorKind::Cors, message)
    }
}
impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for FetchError {}

fn validate_url(url: &Url) -> Result<(), FetchError> {
    if url.scheme() == "data" {
        return Ok(());
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::new(
            FetchErrorKind::Unsupported,
            "Fetch currently supports HTTP(S) and data URLs",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::invalid(
            "Fetch URL must not contain credentials",
        ));
    }
    // Fetch's bad-port list. Ordinary navigation is handled separately.
    if url.port_or_known_default().is_some_and(|port| {
        matches!(
            port,
            0 | 1
                | 7
                | 9
                | 11
                | 13
                | 15
                | 17
                | 19
                | 20
                | 21
                | 22
                | 23
                | 25
                | 37
                | 42
                | 43
                | 53
                | 69
                | 77
                | 79
                | 87
                | 95
                | 101
                | 102
                | 103
                | 104
                | 109
                | 110
                | 111
                | 113
                | 115
                | 117
                | 119
                | 123
                | 135
                | 137
                | 139
                | 143
                | 161
                | 179
                | 389
                | 427
                | 465
                | 512
                | 513
                | 514
                | 515
                | 526
                | 530
                | 531
                | 532
                | 540
                | 548
                | 554
                | 556
                | 563
                | 587
                | 601
                | 636
                | 989
                | 990
                | 993
                | 995
                | 1719
                | 1720
                | 1723
                | 2049
                | 3659
                | 4045
                | 4190
                | 5060
                | 5061
                | 6000
                | 6566
                | 6665..=6669 | 6679 | 6697 | 10080
        )
    }) {
        return Err(FetchError::invalid("Fetch URL uses a blocked network port"));
    }
    Ok(())
}
fn trustworthy(url: &Url) -> bool {
    url.scheme() == "https"
        || match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(name)) => matches!(name, "localhost" | "localhost."),
            _ => false,
        }
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}
pub fn normalize_method(value: &str) -> Result<String, FetchError> {
    if !token(value)
        || ["CONNECT", "TRACE", "TRACK"]
            .iter()
            .any(|method| value.eq_ignore_ascii_case(method))
    {
        return Err(FetchError::invalid("Invalid or forbidden request method"));
    }
    if ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"]
        .iter()
        .any(|method| value.eq_ignore_ascii_case(method))
    {
        Ok(value.to_ascii_uppercase())
    } else {
        Ok(value.into())
    }
}
pub fn normalize_header(name: &str, value: &str) -> Result<(String, String), FetchError> {
    if !token(name) {
        return Err(FetchError::invalid("Invalid HTTP header name"));
    }
    let value = value.trim_matches(['\t', '\n', '\r', ' ']);
    if value
        .chars()
        .any(|c| c as u32 > 255 || matches!(c, '\0' | '\r' | '\n'))
    {
        return Err(FetchError::invalid("Invalid HTTP header value"));
    }
    Ok((name.to_ascii_lowercase(), value.into()))
}
pub fn append_header(headers: &mut HeaderList, name: &str, value: &str) -> Result<(), FetchError> {
    let (name, value) = normalize_header(name, value)?;
    let bytes = headers
        .iter()
        .map(|(name, value)| name.len() + value.len())
        .sum::<usize>();
    if headers.len() >= 128
        || bytes.saturating_add(name.len() + value.len() + 2) > FETCH_HEADER_LIMIT
    {
        return Err(FetchError::limit("HTTP header list exceeds alpha budget"));
    }
    if let Some((_, old)) = headers.iter_mut().find(|(key, _)| key == &name) {
        old.push_str(", ");
        old.push_str(&value);
    } else {
        headers.push((name, value));
    }
    Ok(())
}
pub fn forbidden_request_header(name: &str, value: &str) -> bool {
    matches!(
        name,
        "accept-charset"
            | "accept-encoding"
            | "access-control-request-headers"
            | "access-control-request-method"
            | "connection"
            | "content-length"
            | "cookie"
            | "cookie2"
            | "date"
            | "dnt"
            | "expect"
            | "host"
            | "keep-alive"
            | "origin"
            | "referer"
            | "set-cookie"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "via"
    ) || name.starts_with("proxy-")
        || name.starts_with("sec-")
        || (matches!(
            name,
            "x-http-method" | "x-http-method-override" | "x-method-override"
        ) && value.split(',').any(|method| {
            ["CONNECT", "TRACE", "TRACK"]
                .iter()
                .any(|forbidden| method.trim().eq_ignore_ascii_case(forbidden))
        }))
}
fn simple_method(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "POST")
}
fn unsafe_byte(byte: u8) -> bool {
    (byte < 32 && byte != 9) || b"\"():<>?@[\\]{}\x7f".contains(&byte)
}
pub fn safelisted_header(name: &str, value: &str) -> bool {
    let bytes = value.chars().map(|c| c as u32).collect::<Vec<_>>();
    if bytes.len() > 128 || bytes.iter().any(|&byte| byte > 255) {
        return false;
    }
    match name {
        "accept" => !bytes.iter().any(|&byte| unsafe_byte(byte as u8)),
        "accept-language" | "content-language" => bytes.iter().all(|&byte| {
            (byte as u8).is_ascii_alphanumeric() || b" *,-.;=".contains(&(byte as u8))
        }),
        "content-type" => {
            !bytes.iter().any(|&byte| unsafe_byte(byte as u8))
                && matches!(
                    value
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_lowercase()
                        .as_str(),
                    "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"
                )
        }
        "range" => value
            .strip_prefix("bytes=")
            .and_then(|range| range.split_once('-'))
            .is_some_and(|(first, last)| {
                !first.is_empty()
                    && first.bytes().all(|b| b.is_ascii_digit())
                    && last.bytes().all(|b| b.is_ascii_digit())
                    && (last.is_empty()
                        || first
                            .parse::<u64>()
                            .ok()
                            .zip(last.parse::<u64>().ok())
                            .is_some_and(|(first, last)| first <= last))
            }),
        _ => false,
    }
}
fn unsafe_names(headers: &HeaderList) -> Vec<String> {
    let large = headers
        .iter()
        .filter(|(name, value)| safelisted_header(name, value))
        .map(|(_, value)| value.chars().count())
        .sum::<usize>()
        > 1024;
    headers
        .iter()
        .filter(|(name, value)| large || !safelisted_header(name, value))
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn header_map(headers: &HeaderList) -> Result<HeaderMap, FetchError> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let bytes = value.chars().map(|c| c as u8).collect::<Vec<_>>();
        map.append(
            HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| FetchError::invalid("Invalid header name"))?,
            HeaderValue::from_bytes(&bytes).map_err(|_| {
                FetchError::invalid("Header value cannot be sent by HTTP transport")
            })?,
        );
    }
    Ok(map)
}
fn response_headers(headers: &HeaderMap) -> Result<HeaderList, FetchError> {
    let mut result = vec![];
    for (name, value) in headers {
        let value = value
            .as_bytes()
            .iter()
            .map(|&byte| char::from(byte))
            .collect::<String>();
        append_header(&mut result, name.as_str(), &value)?;
    }
    Ok(result)
}
fn get<'a>(headers: &'a HeaderList, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}
fn cors_check(
    headers: &HeaderList,
    origin: &str,
    credentials: CredentialsMode,
) -> Result<(), FetchError> {
    let allowed = get(headers, "access-control-allow-origin")
        .ok_or_else(|| FetchError::cors("CORS response has no Access-Control-Allow-Origin"))?;
    if allowed == "*" && credentials != CredentialsMode::Include {
        return Ok(());
    }
    if allowed != origin {
        return Err(FetchError::cors("CORS response origin does not match"));
    }
    if credentials == CredentialsMode::Include
        && get(headers, "access-control-allow-credentials") != Some("true")
    {
        return Err(FetchError::cors("CORS response does not allow credentials"));
    }
    Ok(())
}
fn allowed_tokens(headers: &HeaderList, key: &str) -> Result<Vec<String>, FetchError> {
    let Some(value) = get(headers, key) else {
        return Ok(vec![]);
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if token(value) {
                Ok(value.to_owned())
            } else {
                Err(FetchError::cors("Invalid CORS permission token"))
            }
        })
        .collect()
}
fn interrupted_fetch(error: Interrupted) -> FetchError {
    match error {
        Interrupted::Aborted => FetchError::new(FetchErrorKind::Aborted, "Fetch was aborted"),
        Interrupted::Deadline => {
            FetchError::new(FetchErrorKind::Network, "Fetch deadline exceeded")
        }
    }
}
fn net_error(error: reqwest::Error) -> FetchError {
    FetchError::new(
        FetchErrorKind::Network,
        if error.is_timeout() {
            "Fetch network timeout".into()
        } else {
            format!("Fetch network error: {error}")
        },
    )
}

struct AuthorizedResponse {
    metadata: WebResponse,
    transfer: Option<reqwest::Response>,
}

impl From<WebResponse> for AuthorizedResponse {
    fn from(metadata: WebResponse) -> Self {
        Self {
            metadata,
            transfer: None,
        }
    }
}

impl Loader {
    pub fn web_fetch(
        &self,
        request: &WebRequest,
        cancel: &AtomicBool,
    ) -> Result<WebResponse, FetchError> {
        let control = RequestControl::legacy(cancel, Instant::now() + FETCH_TIMEOUT)
            .with_request(request.signal.as_ref());
        self.transport_handle().block_on(self.fetch_with_policy(
            request,
            &control,
            FETCH_RESPONSE_BODY_LIMIT,
            true,
        ))
    }

    pub async fn web_fetch_cancellable(
        &self,
        request: &WebRequest,
        cancel: &CancelSignal,
        deadline: Instant,
    ) -> Result<WebResponse, FetchError> {
        let control = RequestControl::new(cancel, deadline).with_request(request.signal.as_ref());
        self.fetch_with_policy(request, &control, FETCH_RESPONSE_BODY_LIMIT, true)
            .await
    }

    /// Resource consumers need response metadata even when an author script
    /// could not read that header through Fetch. CORS and cookies still go
    /// through the same transport and checks.
    pub(crate) async fn fetch_with_policy(
        &self,
        request: &WebRequest,
        control: &RequestControl<'_>,
        body_limit: usize,
        expose_only: bool,
    ) -> Result<WebResponse, FetchError> {
        let AuthorizedResponse {
            mut metadata,
            transfer,
        } = self
            .open_fetch(request, control, body_limit, expose_only, None, None)
            .await?;
        if let Some(response) = transfer {
            let bytes =
                control
                    .read_body(response, body_limit)
                    .await
                    .map_err(|error| match error {
                        BodyError::Interrupted(error) => interrupted_fetch(error),
                        BodyError::Http(error) => net_error(error),
                        BodyError::Limit => FetchError::limit(format!(
                            "Fetch response body exceeds {body_limit} bytes"
                        )),
                    })?;
            metadata.body = Some(Arc::new(bytes));
        }
        Ok(metadata)
    }

    async fn open_fetch(
        &self,
        request: &WebRequest,
        control: &RequestControl<'_>,
        body_limit: usize,
        expose_only: bool,
        mut upload: Option<UploadBody>,
        connect_policy: Option<&ConnectPolicy>,
    ) -> Result<AuthorizedResponse, FetchError> {
        let mut request = request.clone();
        request.validate()?;
        let use_cors_preflight = upload.is_some();
        let mut source_is_stream = use_cors_preflight;
        if source_is_stream {
            request.validate_streaming_upload()?;
        }
        control.check().map_err(interrupted_fetch)?;
        let check_connect = |url: &Url, redirected| {
            if connect_policy
                .into_iter()
                .chain(request.connect_policy.as_ref())
                .all(|policy| policy.allows(&request.client_url, url, redirected))
            {
                Ok(())
            } else {
                Err(FetchError::new(
                    FetchErrorKind::Network,
                    "Content Security Policy blocked this Fetch connection",
                ))
            }
        };
        check_connect(&request.url, false)?;
        if request.url.scheme() == "data" {
            let data = crate::data_url::process(&request.url, body_limit).map_err(|error| {
                FetchError::new(
                    if error.kind == crate::data_url::DataUrlErrorKind::Limit {
                        FetchErrorKind::Limit
                    } else {
                        FetchErrorKind::Network
                    },
                    error.to_string(),
                )
            })?;
            control.check().map_err(interrupted_fetch)?;
            let mut url = request.url.clone();
            if expose_only {
                url.set_fragment(None);
            }
            return Ok(WebResponse {
                url: url.into(),
                status: 200,
                status_text: "OK".into(),
                headers: vec![("content-type".into(), data.mime_type.serialize())],
                body: if request.method == "HEAD" {
                    None
                } else {
                    Some(Arc::new(data.body))
                },
                response_type: ResponseType::Basic,
                redirected: false,
            }
            .into());
        }
        // The transport has no HTTP response cache. A cache-only request
        // therefore fails without sending a network request.
        if request.cache == RequestCache::OnlyIfCached {
            return Err(FetchError::new(FetchErrorKind::Network, "No cached HTTP response"));
        }
        let source_origin = request.client_url.origin();
        let mut cookie_context = crate::cookies::CookieContext::subresource(&request.client_url);
        cookie_context.method = request.method.clone();
        cookie_context.redirect_to(&request.url, &request.method);
        let mut origin = source_origin.ascii_serialization();
        let mut cors_tainted = false;
        let mut opaque_tainted = false;
        for redirects in 0..=20 {
            control.check().map_err(interrupted_fetch)?;
            validate_url(&request.url)?;
            if redirects != 0 {
                check_connect(&request.url, true)?;
            }
            if request.client_url.scheme() == "https" && !trustworthy(&request.url) {
                return Err(FetchError::new(
                    FetchErrorKind::Network,
                    "Mixed content Fetch from HTTPS to an untrustworthy HTTP origin is blocked",
                ));
            }
            let same = source_origin == request.url.origin();
            if !same {
                match request.mode {
                    RequestMode::SameOrigin => {
                        return Err(FetchError::cors(
                            "same-origin fetch cannot access another origin",
                        ));
                    }
                    RequestMode::Cors => cors_tainted = true,
                    RequestMode::NoCors => opaque_tainted = true,
                }
            }
            let unsafe_headers = unsafe_names(&request.headers);
            if cors_tainted
                && (use_cors_preflight
                    || !simple_method(&request.method)
                    || !unsafe_headers.is_empty())
            {
                let mut builder = self
                    .fetch_client
                    .request(Method::OPTIONS, request.url.clone())
                    .header("origin", &origin)
                    .header("accept", "*/*")
                    .header("access-control-request-method", &request.method);
                if !unsafe_headers.is_empty() {
                    builder =
                        builder.header("access-control-request-headers", unsafe_headers.join(","));
                }
                let preflight = control
                    .wait(builder.send())
                    .await
                    .map_err(interrupted_fetch)?
                    .map_err(net_error)?;
                let headers = response_headers(preflight.headers())?;
                if !preflight.status().is_success() {
                    return Err(FetchError::cors(
                        "CORS preflight did not return an OK status",
                    ));
                }
                cors_check(&headers, &origin, request.credentials)?;
                let methods = allowed_tokens(&headers, "access-control-allow-methods")?;
                let names = allowed_tokens(&headers, "access-control-allow-headers")?;
                let wildcard = request.credentials != CredentialsMode::Include;
                if !simple_method(&request.method)
                    && !methods.contains(&request.method)
                    && !(wildcard && methods.iter().any(|name| name == "*"))
                {
                    return Err(FetchError::cors(
                        "CORS preflight did not allow request method",
                    ));
                }
                for name in &unsafe_headers {
                    if !names
                        .iter()
                        .any(|allowed| allowed.eq_ignore_ascii_case(name))
                        && !(wildcard
                            && name != "authorization"
                            && names.iter().any(|name| name == "*"))
                    {
                        return Err(FetchError::cors(format!(
                            "CORS preflight did not allow header {name}"
                        )));
                    }
                }
            }
            let method = Method::from_bytes(request.method.as_bytes())
                .map_err(|_| FetchError::invalid("Invalid method"))?;
            let mut headers = header_map(&request.headers)?;
            // These are transport headers and are outside the CORS check of
            // headers supplied by the author.
            match request.cache {
                RequestCache::NoStore | RequestCache::Reload => {
                    headers.entry(reqwest::header::PRAGMA).or_insert(HeaderValue::from_static("no-cache"));
                    headers.entry(reqwest::header::CACHE_CONTROL).or_insert(HeaderValue::from_static("no-cache"));
                }
                RequestCache::NoCache => {
                    headers.entry(reqwest::header::CACHE_CONTROL).or_insert(HeaderValue::from_static("max-age=0"));
                }
                _ => {}
            }
            let mut builder = self
                .fetch_client
                .request(method, request.url.clone())
                .headers(headers);
            // Same-origin credentials stop after CORS/opaque tainting, even if
            // a later redirect returns to the original origin.
            let credentials = request.credentials == CredentialsMode::Include
                || (request.credentials == CredentialsMode::SameOrigin
                    && !cors_tainted
                    && !opaque_tainted);
            if credentials {
                let cookies = self.cookies.header(&request.url, &cookie_context);
                if !cookies.is_empty() {
                    builder = builder.header(
                        "cookie",
                        HeaderValue::from_bytes(&cookies)
                            .map_err(|_| FetchError::invalid("Invalid cookie header"))?,
                    );
                }
            }
            if cors_tainted || !matches!(request.method.as_str(), "GET" | "HEAD") {
                builder = builder.header("origin", &origin);
            }
            let upload_completion = if let Some(body) = upload.take() {
                let completion = body.completion();
                // A requested H2 version is rejected by Hyper before writing
                // the request on a connection that negotiated HTTP/1.x.
                builder = builder
                    .version(reqwest::Version::HTTP_2)
                    .body(reqwest::Body::wrap(body));
                Some(completion)
            } else if let Some(body) = &request.body {
                builder = builder.body((**body).clone());
                None
            } else {
                None
            };
            let response = control
                .wait(builder.send())
                .await
                .map_err(interrupted_fetch)?
                .map_err(net_error)?;
            if let Some(completion) = upload_completion {
                // Fetch's half-duplex mode exposes a response only after the
                // request body has reached transport EOF.
                control
                    .wait(upload::wait_for_eof(completion))
                    .await
                    .map_err(interrupted_fetch)??;
            }
            control.check().map_err(interrupted_fetch)?;
            if credentials {
                for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
                    self.cookies
                        .receive(&request.url, value.as_bytes(), &cookie_context);
                }
            }
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED
                && source_is_stream
                && credentials
                && !cors_tainted
            {
                return Err(FetchError::new(
                    FetchErrorKind::Network,
                    "A streamed request body cannot be replayed for HTTP authentication",
                ));
            }
            let headers = response_headers(response.headers())?;
            let status_text = if matches!(
                response.version(),
                reqwest::Version::HTTP_2 | reqwest::Version::HTTP_3
            ) {
                String::new()
            } else if let Some(reason) = response.extensions().get::<hyper::ext::ReasonPhrase>() {
                reason
                    .as_bytes()
                    .iter()
                    .map(|&byte| char::from(byte))
                    .collect()
            } else {
                status.canonical_reason().unwrap_or("").into()
            };
            if cors_tainted {
                cors_check(&headers, &origin, request.credentials)?;
            }
            if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
                && let Some(location) = get(&headers, "location")
            {
                match request.redirect {
                    RedirectMode::Error => {
                        return Err(FetchError::new(
                            FetchErrorKind::Network,
                            "Fetch redirect mode is error",
                        ));
                    }
                    RedirectMode::Manual => {
                        return Ok(WebResponse::opaque(ResponseType::OpaqueRedirect).into());
                    }
                    RedirectMode::Follow => {}
                }
                if redirects == 20 {
                    return Err(FetchError::new(
                        FetchErrorKind::Network,
                        "Fetch exceeded 20 redirects",
                    ));
                }
                let mut next = request
                    .url
                    .join(location)
                    .map_err(|_| FetchError::invalid("Invalid redirect URL"))?;
                if next.fragment().is_none() {
                    next.set_fragment(request.url.fragment());
                }
                // HTTP redirect fetch only follows HTTP(S), even though a direct
                // data fetch is a local scheme fetch with a readable response.
                if !matches!(next.scheme(), "http" | "https") {
                    return Err(FetchError::new(
                        FetchErrorKind::Network,
                        "HTTP redirect target must use HTTP(S)",
                    ));
                }
                validate_url(&next)?;
                if source_is_stream && status.as_u16() != 303 {
                    return Err(FetchError::new(
                        FetchErrorKind::Network,
                        "A streamed request body cannot be replayed after a redirect",
                    ));
                }
                if request.url.origin() != next.origin() {
                    request.headers.retain(|(name, _)| name != "authorization");
                    if source_origin != request.url.origin() {
                        origin = "null".into();
                    }
                }
                if (matches!(status.as_u16(), 301 | 302) && request.method == "POST")
                    || (status.as_u16() == 303
                        && !matches!(request.method.as_str(), "GET" | "HEAD"))
                {
                    request.method = "GET".into();
                    request.body = None;
                    source_is_stream = false;
                    request.headers.retain(|(name, _)| {
                        !matches!(
                            name.as_str(),
                            "content-encoding"
                                | "content-language"
                                | "content-location"
                                | "content-type"
                        )
                    });
                }
                cookie_context.redirect_to(&next, &request.method);
                request.url = next;
                continue;
            }
            if opaque_tainted {
                return Ok(WebResponse::opaque(ResponseType::Opaque).into());
            }
            let has_body =
                request.method != "HEAD" && !matches!(status.as_u16(), 101 | 103 | 204 | 205 | 304);
            let mut exposed = if cors_tainted {
                allowed_tokens(&headers, "access-control-expose-headers")
                    .unwrap_or_default()
                    .into_iter()
                    .map(|name| name.to_ascii_lowercase())
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };
            let all = !expose_only
                || !cors_tainted
                || (request.credentials != CredentialsMode::Include
                    && exposed.iter().any(|name| name == "*"));
            exposed.extend(
                [
                    "cache-control",
                    "content-language",
                    "content-length",
                    "content-type",
                    "expires",
                    "last-modified",
                    "pragma",
                ]
                .map(str::to_owned),
            );
            let headers = headers
                .into_iter()
                .filter(|(name, _)| {
                    !matches!(name.as_str(), "set-cookie" | "set-cookie2")
                        && (all || exposed.contains(name))
                })
                .collect();
            let mut url = request.url.clone();
            if expose_only {
                url.set_fragment(None);
            }
            control.check().map_err(interrupted_fetch)?;
            return Ok(AuthorizedResponse {
                metadata: WebResponse {
                    url: url.into(),
                    status: status.as_u16(),
                    status_text,
                    headers,
                    body: None,
                    response_type: if cors_tainted {
                        ResponseType::Cors
                    } else {
                        ResponseType::Basic
                    },
                    redirected: redirects > 0,
                },
                transfer: has_body.then_some(response),
            });
        }
        unreachable!("redirect loop always returns")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headers_are_bytes_guarded_and_cors_safelist_is_value_sensitive() {
        assert_eq!(
            normalize_header("X-Test", " \tvalue\r\n").unwrap(),
            ("x-test".into(), "value".into())
        );
        assert!(normalize_header("a b", "x").is_err());
        assert!(normalize_header("x", "one\ntwo").is_err());
        assert!(forbidden_request_header("cookie", "secret"));
        assert!(forbidden_request_header("sec-fetch-mode", "cors"));
        assert!(forbidden_request_header("x-method-override", "POST, TRACE"));
        assert!(!forbidden_request_header("authorization", "Bearer test"));
        assert!(safelisted_header(
            "content-type",
            "text/plain;charset=UTF-8"
        ));
        assert!(!safelisted_header(
            "content-type",
            "text/plain, application/json"
        ));
        assert!(!safelisted_header("accept", "text/html\""));
        assert!(safelisted_header("range", "bytes=0-99"));
        assert!(!safelisted_header("range", "bytes=-99"));
        assert!(!safelisted_header("range", "bytes=99-0"));
    }
    #[test]
    fn credentials_change_wildcard_permission_and_origins_need_exact_match() {
        let headers = vec![("access-control-allow-origin".into(), "*".into())];
        assert!(
            cors_check(
                &headers,
                "https://example.test",
                CredentialsMode::SameOrigin
            )
            .is_ok()
        );
        assert!(cors_check(&headers, "https://example.test", CredentialsMode::Include).is_err());
        let headers = vec![
            (
                "access-control-allow-origin".into(),
                "https://example.test".into(),
            ),
            ("access-control-allow-credentials".into(), "true".into()),
        ];
        assert!(cors_check(&headers, "https://example.test", CredentialsMode::Include).is_ok());
        assert!(cors_check(&headers, "https://other.test", CredentialsMode::Include).is_err());
    }
}
