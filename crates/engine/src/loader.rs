use crate::{
    cookies::{CookieContext, CookieStore},
    dom::{Document, NodeId, NodeKind, escape},
    html,
};
use std::{
    collections::HashMap,
    future::{Future, ready},
    io::{Cursor, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use url::Url;
mod connect_policy;
mod home_assets;
mod html_encoding;
mod resources;
mod svg_image;
pub(crate) mod transport;
mod worker_policy;
pub(crate) use connect_policy::ConnectPolicy;
pub use home_assets::built_in_site_icon;
pub(crate) use home_assets::is_home_image;
pub(crate) use resources::decode_oriented_bitmap;
pub use resources::{DocumentResource, ResourceBody, ResourceKind, ResourceRequest};
pub use svg_image::SvgImage;
use svg_image::svg_mime;
use transport::{BodyError, CancelSignal, Interrupted, RequestControl};
pub(crate) use worker_policy::DocumentPolicy;

pub const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(18);
pub(crate) const USER_AGENT: &str = concat!(
    "Mozilla/5.0 Satin/",
    env!("CARGO_PKG_VERSION"),
    " (independent Rust engine; development alpha)"
);
const DOCUMENT_LIMIT: usize = 8 * 1024 * 1024;
const ENCODING_WAIT: Duration = Duration::from_millis(100);
const RESOURCE_LIMIT: usize = 4 * 1024 * 1024;
const DECODED_IMAGES_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct LoadStats {
    pub bytes: usize,
    pub resources: usize,
    pub elapsed_ms: u128,
    pub skipped_resources: usize,
    pub script_count: usize,
}
#[derive(Clone, Debug)]
pub struct PageScript {
    pub node: NodeId,
    pub name: String,
    pub source: Option<String>,
    pub deferred: bool,
    pub unsupported: Option<String>,
}
#[derive(Clone, Debug)]
pub struct LoadedPage {
    pub url: String,
    pub base: Url,
    pub document: Arc<Document>,
    pub stylesheets: Arc<Vec<Arc<str>>>,
    pub linked_stylesheets: HashMap<NodeId, Arc<str>>,
    pub(crate) stylesheet_media: Arc<Vec<Option<Arc<str>>>>,
    pub scripts: Arc<Vec<PageScript>>,
    pub images: HashMap<String, Arc<image::RgbaImage>>,
    pub svg_images: HashMap<String, Arc<SvgImage>>,
    pub(crate) image_references: Arc<HashMap<NodeId, (String, String)>>,
    pub(crate) image_availability: Arc<HashMap<String, ImageAvailability>>,
    pub(crate) canvas_frames: crate::canvas::Frames,
    pub stats: LoadStats,
    pub cookies: CookieStore,
    pub(crate) fetch_loader: Option<Loader>,
    pub(crate) connect_policy: ConnectPolicy,
    pub(crate) meta_connect_policy: ConnectPolicy,
    pub navigation_context: CookieContext,
    pub(crate) document_policy: DocumentPolicy,
    pub form_controls: Arc<HashMap<NodeId, Arc<crate::script::ControlSnapshot>>>,
    pub fonts: Arc<crate::fonts::FontSet>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ImageAvailability {
    Clean,
    Tainted,
    Failed,
}

impl ImageAvailability {
    pub(crate) fn decoded(origin_clean: bool) -> Self {
        if origin_clean {
            Self::Clean
        } else {
            Self::Tainted
        }
    }
}

impl LoadedPage {
    pub(crate) fn empty(url: Url, cookies: CookieStore, navigation_context: CookieContext) -> Self {
        Self {
            url: url.to_string(),
            base: url,
            document: Arc::new(Document::default()),
            stylesheets: Arc::default(),
            linked_stylesheets: HashMap::new(),
            stylesheet_media: Arc::default(),
            scripts: Arc::default(),
            images: HashMap::new(),
            svg_images: HashMap::new(),
            image_references: Arc::default(),
            image_availability: Arc::default(),
            canvas_frames: Default::default(),
            stats: LoadStats {
                resources: 1,
                ..LoadStats::default()
            },
            cookies,
            fetch_loader: None,
            connect_policy: ConnectPolicy::default(),
            meta_connect_policy: ConnectPolicy::default(),
            navigation_context,
            document_policy: DocumentPolicy::default(),
            form_controls: Arc::default(),
            fonts: Arc::default(),
        }
    }
    pub fn from_svg(url: &str, source: &str) -> Result<Self, String> {
        let document =
            crate::svg::parse_document(source).map_err(|error| format!("SVG: {error}"))?;
        let mut page = Self::from_html(url, "");
        page.stylesheets = Arc::new(
            document
                .connected_nodes()
                .filter(|&id| {
                    document.namespace(id) == crate::dom::Namespace::Svg
                        && document.tag(id) == "style"
                })
                .map(|id| Arc::from(document.text(id)))
                .collect(),
        );
        page.stats.bytes = source.len();
        page.document = Arc::new(document);
        // SVG script elements and external XML resources remain inert in this
        // milestone; the normal inline event host still applies to the DOM.
        Ok(page)
    }
    pub fn from_html(url: &str, source: &str) -> Self {
        let document = Arc::new(html::parse_with_scripting(source, true));
        // This API returns an already-parsed document. Activate its final
        // meta policy; only DocumentSession executes scripts during parsing.
        let mut document_policy = DocumentPolicy::default();
        for node in document.html_elements("meta") {
            if let Some(policy) = DocumentPolicy::from_meta_element(&document, node) {
                document_policy.restrict(policy);
            }
        }
        let meta_connect_policy = ConnectPolicy::from_document(&document);
        let mut base = Url::parse(url).unwrap_or_else(|_| Url::parse("about:home").unwrap());
        if let Some(href) = document
            .html_elements("base")
            .next()
            .and_then(|n| document.attr(n, "href"))
            && let Ok(value) = base.join(href)
        {
            base = value;
        }
        let stats = LoadStats {
            bytes: source.len(),
            resources: 1,
            script_count: document.html_elements("script").count(),
            ..Default::default()
        };
        let scripts = document
            .html_elements("script")
            .filter_map(|node| {
                let kind = document
                    .attr(node, "type")
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                let unsupported = if kind == "module" {
                    Some("ES module loading/evaluation is not implemented".into())
                } else if !kind.is_empty() && !javascript_mime(&kind) {
                    return None; // Data blocks, e.g. application/ld+json, are not executable.
                } else if document.attr(node, "src").is_some()
                    && document.attr(node, "async").is_some()
                {
                    Some("Async script scheduling is not implemented".into())
                } else {
                    None
                };
                let external = document.attr(node, "src");
                Some(PageScript {
                    node,
                    name: external
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{url}#inline-{}", node.0)),
                    source: external.is_none().then(|| document.text(node)),
                    deferred: external.is_some() && document.attr(node, "defer").is_some(),
                    unsupported,
                })
            })
            .collect();
        let form_controls = Arc::new(
            crate::script::initial_control_snapshots(&document)
                .into_iter()
                .map(|(id, value)| (id, Arc::new(value)))
                .collect(),
        );
        let mut page = Self {
            url: url.into(),
            base,
            document,
            stylesheets: Arc::default(),
            linked_stylesheets: HashMap::new(),
            stylesheet_media: Arc::default(),
            scripts: Arc::new(scripts),
            images: HashMap::new(),
            svg_images: HashMap::new(),
            image_references: Arc::default(),
            image_availability: Arc::default(),
            canvas_frames: Default::default(),
            stats,
            cookies: CookieStore::new(),
            fetch_loader: None,
            connect_policy: ConnectPolicy::default(),
            meta_connect_policy,
            navigation_context: CookieContext::navigation(
                None,
                &Url::parse(url).unwrap_or_else(|_| Url::parse("about:home").unwrap()),
                "GET",
            ),
            document_policy,
            form_controls,
            fonts: Arc::new(crate::fonts::FontSet::default()),
        };
        page.refresh_stylesheets();
        page
    }
    pub fn resolve(&self, reference: &str) -> Option<String> {
        Url::parse(reference)
            .or_else(|_| self.base.join(reference))
            .ok()
            .map(Into::into)
    }
    pub fn image_url(&self, id: NodeId) -> Option<String> {
        let src = self.document.attr(id, "src")?;
        if let Some((observed, url)) = self.image_references.get(&id)
            && observed == src
        {
            return Some(url.clone());
        }
        self.resolve(src)
    }
    pub(crate) fn image_asset_key(&self, id: NodeId) -> Option<String> {
        let url = self.image_url(id)?;
        let credentials =
            crate::network::crossorigin_credentials(self.document.attr(id, "crossorigin"));
        Some(image_asset_key(&url, credentials))
    }
    pub(crate) fn refresh_stylesheets(&mut self) {
        let (sheets, media): (Vec<_>, Vec<_>) = self
            .document
            .connected_nodes()
            .filter_map(|id| {
                let source = match self.document.tag(id) {
                    "style"
                        if self.document.attr(id, "type").is_none_or(|ty| {
                            ty.trim().is_empty() || ty.trim().eq_ignore_ascii_case("text/css")
                        }) =>
                    {
                        Some(Arc::from(self.document.text(id)))
                    }
                    "link" if self.document.attr(id, "disabled").is_none() => {
                        self.linked_stylesheets.get(&id).cloned()
                    }
                    _ => None,
                }?;
                let media = self.document.attr(id, "media").unwrap_or("").trim();
                if media.len() <= 4096 && !media.contains(['{', '}', ';']) {
                    Some((source, (!media.is_empty()).then(|| Arc::from(media))))
                } else {
                    None
                }
            })
            .unzip();
        self.stylesheets = Arc::new(sheets);
        self.stylesheet_media = Arc::new(media);
    }
    pub fn title(&self) -> String {
        let title = self.document.title();
        if title.is_empty() {
            self.url.clone()
        } else {
            title
        }
    }
    pub fn error(url: &str, message: &str) -> Self {
        Self::from_html(
            url,
            &format!(
                "<!doctype html><meta charset='utf-8'><title>Nie udało się otworzyć strony</title><style>{BASE_STYLE}</style><main><p class='eyebrow'>RUST BROWSER</p><h1>Nie udało się otworzyć strony</h1><p>Sprawdź adres i połączenie z Internetem, a następnie spróbuj ponownie.</p><pre>{}</pre><p><a href='about:home'>Wróć na stronę startową</a></p></main>",
                escape(message)
            ),
        )
    }
}

pub(crate) fn image_asset_key(url: &str, cors: Option<crate::network::CredentialsMode>) -> String {
    use crate::network::CredentialsMode;
    // URL serializations never contain a literal NUL. An opaque display-only
    // fetch cannot satisfy a CORS-enabled image request for the same address.
    match cors {
        None => url.into(),
        Some(CredentialsMode::SameOrigin) => format!("\0anonymous:{url}"),
        Some(CredentialsMode::Include) => format!("\0credentials:{url}"),
        Some(CredentialsMode::Omit) => format!("\0omit:{url}"),
    }
}

#[derive(Clone, Debug)]
pub struct Loader {
    pub(crate) fetch_client: reqwest::Client,
    reactor: &'static tokio::runtime::Runtime,
    pub cookies: CookieStore,
}

/// Navigation metadata and decoded input, before any author script executes.
/// For standalone SVG or raster images the document is already available
/// and has no HTML tokenizer input.
pub struct OpenedPage {
    pub(crate) page: LoadedPage,
    pub(crate) input: DocumentInput,
    pub(crate) started: Instant,
    pub(crate) response_start: Instant,
}
pub(crate) enum DocumentInput {
    Html(String),
    Streaming,
    Prepared,
}
pub enum NavigationPart {
    Open(Box<OpenedPage>),
    Text(String, usize),
    Finish,
}

#[derive(Clone, Debug)]
pub struct NavigationRequest {
    pub url: Url,
    pub method: String,
    pub body: Option<Vec<u8>>,
    pub content_type: Option<String>,
    pub context: CookieContext,
    pub initiator: Option<Url>,
}
impl NavigationRequest {
    pub fn get(url: &str) -> Result<Self, String> {
        let url = Url::parse(url).map_err(|error| error.to_string())?;
        Ok(Self {
            context: CookieContext::navigation(None, &url, "GET"),
            url,
            method: "GET".into(),
            body: None,
            content_type: None,
            initiator: None,
        })
    }
    pub fn from_document(url: Url, initiator: Url) -> Self {
        Self {
            context: CookieContext::navigation(Some(&initiator), &url, "GET"),
            url,
            method: "GET".into(),
            body: None,
            content_type: None,
            initiator: Some(initiator),
        }
    }
    pub fn subresource(url: Url, client: &Url) -> Self {
        Self {
            context: CookieContext::subresource(client),
            url,
            method: "GET".into(),
            body: None,
            content_type: None,
            initiator: Some(client.clone()),
        }
    }
}
impl Loader {
    pub fn new() -> Result<Self, String> {
        Self::with_cookies(CookieStore::new())
    }
    pub fn with_cookies(cookies: CookieStore) -> Result<Self, String> {
        use rustls_platform_verifier::BuilderVerifierExt;
        // Keep Rustls transport while delegating platform trust, chain
        // building and distrust decisions to the operating system.
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_platform_verifier()
        .map_err(|e| e.to_string())?
        .with_no_client_auth();
        Ok(Self {
            fetch_client: Self::fetch_client_with_tls(tls)?,
            reactor: transport::runtime()?,
            cookies,
        })
    }

    pub(crate) fn fetch_client_with_tls(
        mut tls: rustls::ClientConfig,
    ) -> Result<reqwest::Client, String> {
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            // Only these explicitly pinned localhost names are treated as
            // trustworthy by Fetch's mixed-content policy.
            .resolve_to_addrs(
                "localhost",
                &[
                    std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                    std::net::SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 0)),
                ],
            )
            .resolve_to_addrs(
                "localhost.",
                &[
                    std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                    std::net::SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 0)),
                ],
            )
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())
    }

    pub fn transport_handle(&self) -> &tokio::runtime::Handle {
        self.reactor.handle()
    }

    pub fn open_navigation(
        &self,
        request: &NavigationRequest,
        cancel: &AtomicBool,
    ) -> Result<OpenedPage, String> {
        let started = Instant::now();
        check_cancel(cancel)?;
        if request.url.scheme() == "about" {
            return Ok(OpenedPage::internal(&request.url, self.cookies.clone()));
        }
        let main = self.fetch(request, DOCUMENT_LIMIT, NAVIGATION_TIMEOUT, cancel)?;
        self.opened_response(main, started)
    }

    fn opened_response(&self, main: Response, started: Instant) -> Result<OpenedPage, String> {
        let url = Url::parse(&main.url).map_err(|error| error.to_string())?;
        let size = main.bytes.len();
        let mime = crate::mime::MimeType::parse(&main.content_type);
        let mut page = LoadedPage::empty(url, self.cookies.clone(), main.context);
        page.document_policy = main.document_policy;
        page.fetch_loader = Some(self.clone());
        page.connect_policy = main.connect_policy;
        let input = if svg_mime(&main.content_type) {
            let source = decode_text(&main.bytes, "image/svg+xml; charset=utf-8");
            let svg = LoadedPage::from_svg(&page.url, &source)?;
            page.document = svg.document;
            page.stylesheets = svg.stylesheets;
            DocumentInput::Prepared
        } else if mime
            .as_ref()
            .is_some_and(|mime| mime.essence.starts_with("image/"))
        {
            let image = resources::decode_bitmap(&main.bytes)?;
            let title = escape(&page.url.chars().take(240).collect::<String>());
            page.images
                .insert("about:document-image".into(), Arc::new(image));
            DocumentInput::Html(format!(
                "<!doctype html><title>{title}</title><img src='about:document-image'>"
            ))
        } else {
            let text = decode_text(&main.bytes, &main.content_type);
            DocumentInput::Html(if mime.is_some_and(|mime| mime.essence == "text/html") {
                text
            } else {
                format!(
                    "<!doctype html><title>{}</title><pre>{}</pre>",
                    escape(&page.url.chars().take(240).collect::<String>()),
                    escape(&text)
                )
            })
        };
        page.stats.bytes = size;
        Ok(OpenedPage {
            page,
            input,
            started,
            response_start: main.response_start,
        })
    }

    /// Read HTML as it arrives. Encoding is selected once, then an incremental
    /// decoder keeps incomplete byte sequences across transport chunks.
    pub fn stream_navigation(
        &self,
        request: &NavigationRequest,
        cancel: &AtomicBool,
        mut send: impl FnMut(NavigationPart),
    ) -> Result<(), String> {
        let control = RequestControl::legacy(cancel, Instant::now() + NAVIGATION_TIMEOUT);
        self.reactor
            .block_on(self.stream_controlled(request, &control, |part| {
                send(part);
                ready(Ok(()))
            }))
    }

    pub async fn stream_navigation_cancellable<F, Fut>(
        &self,
        request: &NavigationRequest,
        cancel: &CancelSignal,
        deadline: Instant,
        send: F,
    ) -> Result<(), String>
    where
        F: FnMut(NavigationPart) -> Fut,
        Fut: Future<Output = Result<(), String>>,
    {
        let control = RequestControl::new(cancel, deadline);
        self.stream_controlled(request, &control, send).await
    }

    async fn stream_controlled<F, Fut>(
        &self,
        request: &NavigationRequest,
        control: &RequestControl<'_>,
        mut send: F,
    ) -> Result<(), String>
    where
        F: FnMut(NavigationPart) -> Fut,
        Fut: Future<Output = Result<(), String>>,
    {
        let started = Instant::now();
        control.check().map_err(interrupted_navigation)?;
        if !matches!(request.url.scheme(), "http" | "https") {
            let opened = if request.url.scheme() == "about" {
                OpenedPage::internal(&request.url, self.cookies.clone())
            } else {
                let main = self
                    .fetch_controlled(request, DOCUMENT_LIMIT, control)
                    .await?;
                self.prepare_response(main, started, control).await?
            };
            return control
                .wait(send(NavigationPart::Open(Box::new(opened))))
                .await
                .map_err(interrupted_navigation)?;
        }
        let (mut response, url, context, origin_clean, response_start) =
            self.fetch_navigation_http(request, control).await?;
        if response
            .content_length()
            .is_some_and(|len| len > DOCUMENT_LIMIT as u64)
        {
            return Err("Dokument przekracza limit rozmiaru wersji alfa".into());
        }
        let content_type = response_content_type(response.headers(), true);
        let document_policy = DocumentPolicy::from_headers(response.headers());
        let connect_policy = ConnectPolicy::from_headers(response.headers());
        let mime = crate::mime::MimeType::parse(&content_type);
        if mime.as_ref().is_none_or(|mime| mime.essence != "text/html") {
            let status = response.status().as_u16();
            let bytes = control
                .read_body(response, DOCUMENT_LIMIT)
                .await
                .map_err(navigation_body_error)?;
            let opened = self
                .prepare_response(
                    Response {
                        url: url.into(),
                        content_type,
                        bytes,
                        status,
                        context,
                        nosniff: false,
                        origin_clean,
                        response_start,
                        document_policy,
                        connect_policy,
                    },
                    started,
                    control,
                )
                .await?;
            return control
                .wait(send(NavigationPart::Open(Box::new(opened))))
                .await
                .map_err(interrupted_navigation)?;
        }

        let mut page = LoadedPage::empty(url, self.cookies.clone(), context);
        page.document_policy = document_policy;
        page.fetch_loader = Some(self.clone());
        page.connect_policy = connect_policy;
        let opened = OpenedPage {
            page,
            input: DocumentInput::Streaming,
            started,
            response_start,
        };
        control
            .wait(send(NavigationPart::Open(Box::new(opened))))
            .await
            .map_err(interrupted_navigation)??;
        let has_transport_encoding = declared_encoding(&content_type).is_some();
        let sniff = if has_transport_encoding {
            3
        } else {
            html_encoding::PREFIX_BYTES
        };
        let mut prefix = Vec::with_capacity(sniff);
        let mut rest = None;
        let mut sniff_until = None;
        while prefix.len() < sniff {
            let next = control.wait(response.chunk());
            let received = if let Some(until) = sniff_until {
                match tokio::time::timeout_at(until, next).await {
                    Ok(result) => result,
                    Err(_) => break,
                }
            } else {
                next.await
            };
            let Some(chunk) = received
                .map_err(interrupted_navigation)?
                .map_err(error_chain)?
            else {
                break;
            };
            sniff_until.get_or_insert_with(|| tokio::time::Instant::now() + ENCODING_WAIT);
            let count = (sniff - prefix.len()).min(chunk.len());
            prefix.extend_from_slice(&chunk[..count]);
            if count < chunk.len() {
                rest = Some(chunk.slice(count..));
            }
            if encoding_rs::Encoding::for_bom(&prefix).is_some()
                || (!has_transport_encoding && html_encoding::prescan(&prefix).is_some())
            {
                break;
            }
        }
        let (encoding, bom) = text_encoding(&prefix, &content_type);
        let mut decoder = encoding.new_decoder_without_bom_handling();
        let mut total = prefix.len();
        if !prefix.is_empty() {
            let text = decode_navigation_chunk(&mut decoder, &prefix[bom..], false)?;
            control
                .wait(send(NavigationPart::Text(text, prefix.len())))
                .await
                .map_err(interrupted_navigation)??;
        }
        loop {
            let chunk = if let Some(rest) = rest.take() {
                Some(rest)
            } else {
                control
                    .wait(response.chunk())
                    .await
                    .map_err(interrupted_navigation)?
                    .map_err(error_chain)?
            };
            let Some(chunk) = chunk else {
                break;
            };
            total = total.saturating_add(chunk.len());
            if total > DOCUMENT_LIMIT {
                return Err("Dokument przekracza limit rozmiaru wersji alfa".into());
            }
            for bytes in chunk.chunks(16 * 1024) {
                let text = decode_navigation_chunk(&mut decoder, bytes, false)?;
                control
                    .wait(send(NavigationPart::Text(text, bytes.len())))
                    .await
                    .map_err(interrupted_navigation)??;
            }
        }
        let tail = decode_navigation_chunk(&mut decoder, &[], true)?;
        if !tail.is_empty() {
            control
                .wait(send(NavigationPart::Text(tail, 0)))
                .await
                .map_err(interrupted_navigation)??;
        }
        control
            .wait(send(NavigationPart::Finish))
            .await
            .map_err(interrupted_navigation)?
    }

    async fn prepare_response(
        &self,
        main: Response,
        started: Instant,
        control: &RequestControl<'_>,
    ) -> Result<OpenedPage, String> {
        control.check().map_err(interrupted_navigation)?;
        let loader = self.clone();
        let opened = tokio::task::spawn_blocking(move || loader.opened_response(main, started))
            .await
            .map_err(|error| error.to_string())??;
        control.check().map_err(interrupted_navigation)?;
        Ok(opened)
    }
    pub fn load(&self, url: &str, cancel: &AtomicBool) -> Result<LoadedPage, String> {
        self.load_with_preview(url, cancel, |_| {})
    }
    pub fn load_with_preview(
        &self,
        url: &str,
        cancel: &AtomicBool,
        preview: impl FnMut(LoadedPage),
    ) -> Result<LoadedPage, String> {
        self.load_navigation_with_preview(&NavigationRequest::get(url)?, cancel, preview)
    }
    pub fn load_navigation_with_preview(
        &self,
        request: &NavigationRequest,
        cancel: &AtomicBool,
        mut preview: impl FnMut(LoadedPage),
    ) -> Result<LoadedPage, String> {
        let start = Instant::now();
        let parsed = request.url.clone();
        if parsed.scheme() == "about" {
            let mut p = builtin(parsed.path());
            p.url = parsed.to_string();
            p.base = parsed;
            p.cookies = self.cookies.clone();
            p.fetch_loader = Some(self.clone());
            p.navigation_context = request.context.clone();
            return Ok(p);
        }
        check_cancel(cancel)?;
        let main = self.fetch(request, DOCUMENT_LIMIT, NAVIGATION_TIMEOUT, cancel)?;
        let source = if svg_mime(&main.content_type) {
            decode_text(&main.bytes, "image/svg+xml; charset=utf-8")
        } else {
            decode_text(&main.bytes, &main.content_type)
        };
        let source = if main.content_type.starts_with("text/plain") {
            format!(
                "<title>{}</title><pre>{}</pre>",
                escape(&main.url),
                escape(&source)
            )
        } else {
            source
        };
        let mut page = if svg_mime(&main.content_type) {
            LoadedPage::from_svg(&main.url, &source)?
        } else {
            LoadedPage::from_html(&main.url, &source)
        };
        page.cookies = self.cookies.clone();
        page.fetch_loader = Some(self.clone());
        page.connect_policy = main.connect_policy;
        page.navigation_context = main.context;
        page.document_policy.restrict(main.document_policy);
        if page.document.truncated {
            return Err("Dokument przekracza limit złożoności parsera w tej wersji alfa.".into());
        }
        page.stats.bytes = main.bytes.len();
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut stylesheet_count = 0;
        for id in page.document.connected_nodes().collect::<Vec<_>>() {
            let node = &page.document.nodes[id.0];
            if let NodeKind::Element { tag, .. } = &node.kind
                && tag == "link"
                && page.document.attr(id, "rel").is_some_and(|s| {
                    s.split_ascii_whitespace()
                        .any(|v| v.eq_ignore_ascii_case("stylesheet"))
                })
            {
                if stylesheet_count >= 8 || Instant::now() > deadline {
                    page.stats.skipped_resources += 1;
                    continue;
                }
                if let Some(uri) = page
                    .document
                    .attr(id, "href")
                    .and_then(|h| resource_url(&page, h))
                {
                    stylesheet_count += 1;
                    match self
                        .fetch(
                            &NavigationRequest::subresource(uri, &Url::parse(&page.url).unwrap()),
                            RESOURCE_LIMIT,
                            Duration::from_secs(4),
                            cancel,
                        )
                        .and_then(Response::resource)
                    {
                        Ok(data) => {
                            page.stats.bytes += data.bytes.len();
                            page.stats.resources += 1;
                            page.linked_stylesheets.insert(
                                id,
                                Arc::from(decode_text(&data.bytes, &data.content_type)),
                            );
                        }
                        Err(_) => page.stats.skipped_resources += 1,
                    }
                }
            }
        }
        page.refresh_stylesheets();
        // Script bytes are fetched by the bounded loader; execution and the DOM
        // stay on the tab's thread. There is no foreign JavaScript runtime.
        let mut scripts = (*page.scripts).clone();
        let mut script_bytes = 0usize;
        for (index, script) in scripts.iter_mut().enumerate() {
            check_cancel(cancel)?;
            if script.unsupported.is_some() {
                continue;
            }
            if index >= 32 || script_bytes >= 4 * 1024 * 1024 {
                script.unsupported = Some("Script count/byte budget exceeded".into());
                page.stats.skipped_resources += 1;
                continue;
            }
            if script.source.is_none() {
                let uri = page
                    .document
                    .attr(script.node, "src")
                    .and_then(|src| resource_url(&page, src));
                match uri {
                    Some(uri) if Instant::now() <= deadline => match self
                        .fetch(
                            &NavigationRequest::subresource(uri, &Url::parse(&page.url).unwrap()),
                            1024 * 1024,
                            Duration::from_secs(4),
                            cancel,
                        )
                        .and_then(Response::script)
                    {
                        Ok(data) => {
                            page.stats.bytes += data.bytes.len();
                            page.stats.resources += 1;
                            script.name = data.url;
                            script.source = Some(decode_text(&data.bytes, &data.content_type));
                        }
                        Err(error) => {
                            script.unsupported = Some(error);
                            page.stats.skipped_resources += 1;
                        }
                    },
                    _ => {
                        script.unsupported = Some("Script URL/deadline is unsupported".into());
                        page.stats.skipped_resources += 1;
                    }
                }
            }
            script_bytes += script.source.as_ref().map_or(0, String::len);
            if script_bytes > 4 * 1024 * 1024 {
                script.source = None;
                script.unsupported = Some("Script byte budget exceeded".into());
                page.stats.skipped_resources += 1;
            }
        }
        page.scripts = Arc::new(scripts);
        page.stats.elapsed_ms = start.elapsed().as_millis();
        preview(page.clone());
        let image_urls = page
            .document
            .html_elements("img")
            .filter_map(|n| {
                page.document
                    .attr(n, "src")
                    .and_then(|s| resource_url(&page, s))
            })
            .collect::<Vec<_>>();
        let mut decoded = 0;
        for uri in image_urls {
            check_cancel(cancel)?;
            if page.images.contains_key(uri.as_str()) || page.svg_images.contains_key(uri.as_str())
            {
                continue;
            }
            if page.images.len() + page.svg_images.len() >= 32
                || Instant::now() > deadline
                || decoded >= DECODED_IMAGES_LIMIT
            {
                page.stats.skipped_resources += 1;
                continue;
            }
            match self
                .fetch(
                    &NavigationRequest::subresource(uri.clone(), &Url::parse(&page.url).unwrap()),
                    RESOURCE_LIMIT,
                    Duration::from_secs(3),
                    cancel,
                )
                .and_then(Response::resource)
                .and_then(|data| {
                    if svg_mime(&data.content_type) {
                        let source = decode_text(&data.bytes, "image/svg+xml; charset=utf-8");
                        let asset = SvgImage::from_source(&source)?;
                        if asset.retained_bytes() > DECODED_IMAGES_LIMIT - decoded {
                            return Err("SVG image exceeds retained resource budget".into());
                        }
                        return Ok((data.bytes.len(), None, Some(asset), data.origin_clean));
                    }
                    let mut reader = image::ImageReader::new(Cursor::new(&data.bytes))
                        .with_guessed_format()
                        .map_err(|e| e.to_string())?;
                    let mut limits = image::Limits::default();
                    limits.max_image_width = Some(8192);
                    limits.max_image_height = Some(8192);
                    limits.max_alloc = Some((DECODED_IMAGES_LIMIT - decoded) as u64);
                    reader.limits(limits);
                    let bitmap = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
                    Ok((data.bytes.len(), Some(bitmap), None, data.origin_clean))
                }) {
                Ok((bytes, image, svg, origin_clean)) => {
                    page.stats.bytes += bytes;
                    page.stats.resources += 1;
                    Arc::make_mut(&mut page.image_availability)
                        .insert(uri.to_string(), ImageAvailability::decoded(origin_clean));
                    if let Some(image) = image {
                        decoded += image.as_raw().len();
                        page.images.insert(uri.into(), Arc::new(image));
                    } else if let Some(svg) = svg {
                        decoded += svg.retained_bytes();
                        page.svg_images.insert(uri.into(), Arc::new(svg));
                    }
                }
                Err(_) => {
                    Arc::make_mut(&mut page.image_availability)
                        .insert(uri.to_string(), ImageAvailability::Failed);
                    page.stats.skipped_resources += 1;
                }
            }
        }
        page.stats.elapsed_ms = start.elapsed().as_millis();
        Ok(page)
    }

    async fn fetch_navigation_http(
        &self,
        original: &NavigationRequest,
        control: &RequestControl<'_>,
    ) -> Result<(reqwest::Response, Url, CookieContext, bool, Instant), String> {
        let mut request = original.clone();
        request.context.redirect_to(&request.url, &request.method);
        if request
            .body
            .as_ref()
            .is_some_and(|body| body.len() > RESOURCE_LIMIT)
        {
            return Err("Navigation body exceeds 4 MiB".into());
        }
        let mut origin = request
            .initiator
            .as_ref()
            .map(|url| url.origin().ascii_serialization());
        let mut origin_clean = true;
        for redirect in 0..=20 {
            control.check().map_err(interrupted_navigation)?;
            // A no-CORS fetch stays tainted if any hop crossed origins,
            // including a chain that returns to the initiating document.
            origin_clean &= original
                .initiator
                .as_ref()
                .is_some_and(|client| client.origin() == request.url.origin());
            if !request.context.top_level_navigation
                && request
                    .initiator
                    .as_ref()
                    .is_some_and(|client| client.scheme() == "https")
                && !crate::cookies::secure_origin(&request.url)
            {
                return Err("Mixed content subresource from HTTPS to an untrustworthy HTTP origin is blocked".into());
            }
            if !matches!(request.url.scheme(), "http" | "https") {
                return Err("HTTP navigation cannot redirect to a local/internal resource".into());
            }
            let method = crate::network::normalize_method(&request.method)
                .map_err(|error| error.to_string())?;
            let mut builder = self.fetch_client.request(
                reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|error| error.to_string())?,
                request.url.clone(),
            );
            let cookies = self.cookies.header(&request.url, &request.context);
            if !cookies.is_empty() {
                builder = builder.header(
                    reqwest::header::COOKIE,
                    reqwest::header::HeaderValue::from_bytes(&cookies)
                        .map_err(|error| error.to_string())?,
                );
            }
            if !matches!(method.as_str(), "GET" | "HEAD") {
                builder = builder.header("origin", origin.as_deref().unwrap_or("null"));
            }
            if let Some(content_type) = &request.content_type {
                builder = builder.header("content-type", content_type);
            }
            if let Some(body) = &request.body {
                builder = builder.body(body.clone());
            }
            let response = control
                .wait(builder.send())
                .await
                .map_err(interrupted_navigation)?
                .map_err(error_chain)?;
            let response_start = Instant::now();
            control.check().map_err(interrupted_navigation)?;
            for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
                self.cookies
                    .receive(&request.url, value.as_bytes(), &request.context);
            }
            if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308)
                && let Some(location) = response.headers().get(reqwest::header::LOCATION)
            {
                if redirect == 20 {
                    return Err("Navigation exceeded 20 redirects".into());
                }
                let location = location.to_str().map_err(|_| "Invalid redirect location")?;
                let mut target = request
                    .url
                    .join(location)
                    .map_err(|error| error.to_string())?;
                if target.fragment().is_none() {
                    target.set_fragment(request.url.fragment());
                }
                if request.url.origin() != target.origin()
                    && request
                        .initiator
                        .as_ref()
                        .is_none_or(|client| client.origin() != request.url.origin())
                {
                    origin = Some("null".into());
                }
                if (matches!(response.status().as_u16(), 301 | 302) && method == "POST")
                    || response.status().as_u16() == 303
                        && !matches!(method.as_str(), "GET" | "HEAD")
                {
                    request.method = "GET".into();
                    request.body = None;
                    request.content_type = None;
                }
                request.context.redirect_to(&target, &request.method);
                request.url = target;
                continue;
            }
            if request.context.top_level_navigation {
                // An attachment can contain attacker-controlled HTML served
                // from the same origin; it must never produce a document.
                if response
                    .headers()
                    .get_all(reqwest::header::CONTENT_DISPOSITION)
                    .iter()
                    .any(|value| {
                        value
                            .as_bytes()
                            .split(|&byte| byte == b';')
                            .next()
                            .is_some_and(|value| {
                                value.trim_ascii().eq_ignore_ascii_case(b"attachment")
                            })
                    })
                {
                    return Err("Ten adres zwraca plik do pobrania. Pobieranie plików jest w przygotowaniu.".into());
                }
                if matches!(response.status().as_u16(), 204 | 205) {
                    return Err("Serwer nie zwrócił dokumentu.".into());
                }
            }
            let mut url = response.url().clone();
            if request.context.top_level_navigation {
                // Fragments belong to the document address, not the HTTP request.
                // The redirect loop has already inherited or replaced the fragment.
                url.set_fragment(request.url.fragment());
            }
            return Ok((response, url, request.context, origin_clean, response_start));
        }
        unreachable!()
    }
    fn fetch(
        &self,
        request: &NavigationRequest,
        limit: usize,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<Response, String> {
        let control = RequestControl::legacy(cancel, Instant::now() + timeout);
        self.reactor
            .block_on(self.fetch_controlled(request, limit, &control))
    }

    async fn fetch_controlled(
        &self,
        request: &NavigationRequest,
        limit: usize,
        control: &RequestControl<'_>,
    ) -> Result<Response, String> {
        control.check().map_err(interrupted_navigation)?;
        let response = match request.url.scheme() {
            "data" => {
                let data = crate::data_url::process(&request.url, limit)
                    .map_err(|error| error.to_string())?;
                Response {
                    url: request.url.to_string(),
                    content_type: data.mime_type.serialize(),
                    bytes: data.body,
                    status: 200,
                    nosniff: false,
                    origin_clean: true,
                    context: request.context.clone(),
                    response_start: Instant::now(),
                    document_policy: DocumentPolicy::default(),
                    connect_policy: ConnectPolicy::default(),
                }
            }
            "http" | "https" => {
                let (response, final_url, context, origin_clean, response_start) =
                    self.fetch_navigation_http(request, control).await?;
                if response.content_length().is_some_and(|n| n > limit as u64) {
                    return Err("Dokument przekracza limit rozmiaru wersji alfa".into());
                }
                let content_type =
                    response_content_type(response.headers(), request.context.top_level_navigation);
                let connect_policy = if request.context.top_level_navigation {
                    ConnectPolicy::from_headers(response.headers())
                } else {
                    ConnectPolicy::default()
                };
                let document_policy = if request.context.top_level_navigation {
                    DocumentPolicy::from_headers(response.headers())
                } else {
                    DocumentPolicy::default()
                };
                let status = response.status().as_u16();
                let nosniff = response
                    .headers()
                    .get("x-content-type-options")
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(resources::is_nosniff);
                let bytes = control
                    .read_body(response, limit)
                    .await
                    .map_err(navigation_body_error)?;
                Response {
                    url: final_url.into(),
                    content_type,
                    status,
                    nosniff,
                    bytes,
                    context,
                    origin_clean,
                    response_start,
                    document_policy,
                    connect_policy,
                }
            }
            "file" => {
                if request
                    .initiator
                    .as_ref()
                    .is_some_and(|initiator| initiator.scheme() != "file")
                {
                    return Err("Strona internetowa nie może otwierać lokalnych plików.".into());
                }
                let path = request
                    .url
                    .to_file_path()
                    .map_err(|_| "Nieprawidłowy adres pliku")?;
                let kind = match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
                    "css" => "text/css; charset=utf-8",
                    "js" | "mjs" => "text/javascript; charset=utf-8",
                    "txt" => "text/plain; charset=utf-8",
                    "svg" => "image/svg+xml; charset=utf-8",
                    _ => "text/html",
                };
                let bytes = tokio::task::spawn_blocking(move || read_local_file(&path, limit))
                    .await
                    .map_err(|error| error.to_string())??;
                Response {
                    url: request.url.to_string(),
                    content_type: kind.into(),
                    status: 200,
                    nosniff: false,
                    bytes,
                    context: request.context.clone(),
                    origin_clean: false,
                    response_start: Instant::now(),
                    document_policy: DocumentPolicy::default(),
                    connect_policy: ConnectPolicy::default(),
                }
            }
            _ => {
                return Err(format!(
                    "Ten typ adresu nie jest jeszcze obsługiwany: {}",
                    request.url.scheme()
                ));
            }
        };
        control.check().map_err(interrupted_navigation)?;
        Ok(response)
    }
}
fn response_content_type(headers: &reqwest::header::HeaderMap, document: bool) -> String {
    let nosniff = headers
        .get_all("x-content-type-options")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(resources::is_nosniff);
    headers
        .get_all(reqwest::header::CONTENT_TYPE)
        .iter()
        .next_back()
        .map(|value| value.to_str().unwrap_or("application/octet-stream"))
        .unwrap_or(if document && !nosniff {
            "text/html"
        } else {
            "application/octet-stream"
        })
        .to_owned()
}
fn read_local_file(path: &std::path::Path, limit: usize) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() > limit as u64 {
        return Err("Plik przekracza limit rozmiaru wersji alfa".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > limit {
        return Err("Zasób przekracza limit rozmiaru wersji alfa".into());
    }
    Ok(bytes)
}

fn decode_navigation_chunk(
    decoder: &mut encoding_rs::Decoder,
    chunk: &[u8],
    last: bool,
) -> Result<String, String> {
    let capacity = decoder
        .max_utf8_buffer_length(chunk.len())
        .ok_or("Document decoding exceeded its buffer budget")?;
    let mut text = String::with_capacity(capacity);
    let (status, read, _) = decoder.decode_to_string(chunk, &mut text, last);
    if status != encoding_rs::CoderResult::InputEmpty || read != chunk.len() {
        return Err("Document decoding exceeded its buffer budget".into());
    }
    Ok(text)
}

fn interrupted_navigation(error: Interrupted) -> String {
    match error {
        Interrupted::Aborted => "Nawigacja anulowana".into(),
        Interrupted::Deadline => "Navigation deadline exceeded".into(),
    }
}

fn navigation_body_error(error: BodyError) -> String {
    match error {
        BodyError::Interrupted(error) => interrupted_navigation(error),
        BodyError::Http(error) => error_chain(error),
        BodyError::Limit => "Zasób przekracza limit rozmiaru wersji alfa".into(),
    }
}
struct Response {
    url: String,
    content_type: String,
    bytes: Vec<u8>,
    status: u16,
    nosniff: bool,
    context: CookieContext,
    origin_clean: bool,
    response_start: Instant,
    document_policy: DocumentPolicy,
    connect_policy: ConnectPolicy,
}
impl Response {
    fn resource(self) -> Result<Self, String> {
        if !(200..300).contains(&self.status) {
            Err(format!(
                "HTTP {} loading resource {}",
                self.status, self.url
            ))
        } else {
            Ok(self)
        }
    }
    fn script(self) -> Result<Self, String> {
        let response = self.resource()?;
        let essence = response
            .content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        // Fetch's script-like MIME blocking and X-Content-Type-Options rules.
        if ["audio/", "image/", "video/"]
            .iter()
            .any(|prefix| essence.starts_with(prefix))
            || essence == "text/csv"
            || (response.nosniff && !javascript_mime(&essence))
        {
            Err(format!(
                "Script MIME type blocked: {}",
                response.content_type
            ))
        } else {
            Ok(response)
        }
    }
}
fn javascript_mime(kind: &str) -> bool {
    matches!(
        kind,
        "application/ecmascript"
            | "application/javascript"
            | "application/x-ecmascript"
            | "application/x-javascript"
            | "text/ecmascript"
            | "text/javascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
            | "text/x-javascript"
    )
}
fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Nawigacja anulowana".into())
    } else {
        Ok(())
    }
}
fn error_chain(error: reqwest::Error) -> String {
    use std::error::Error;
    let mut text = error.to_string();
    let mut cause = error.source();
    let mut count = 0;
    while let Some(e) = cause {
        if count > 6 {
            break;
        }
        text.push_str(": ");
        text.push_str(&e.to_string());
        cause = e.source();
        count += 1;
    }
    text
}
fn resource_url(page: &LoadedPage, value: &str) -> Option<Url> {
    let uri = Url::parse(&page.resolve(value)?).ok()?;
    if uri.scheme() == "file" && Url::parse(&page.url).ok()?.scheme() != "file" {
        return None;
    }
    if matches!(uri.scheme(), "http" | "https" | "file" | "data") {
        Some(uri)
    } else {
        None
    }
}

pub fn normalize_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok("about:home".into());
    }
    if std::path::Path::new(input).is_absolute()
        && let Ok(url) = Url::from_file_path(input)
    {
        return Ok(url.into());
    }
    let value = if input.starts_with("about:")
        || input.starts_with("file:")
        || input
            .get(..5)
            .is_some_and(|p| p.eq_ignore_ascii_case("data:"))
        || input.contains("://")
    {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let uri = Url::parse(&value).map_err(|e| format!("Nieprawidłowy adres: {e}"))?;
    if !matches!(uri.scheme(), "http" | "https" | "file" | "about" | "data") {
        return Err("Obsługiwane adresy: http, https, file, data i about".into());
    }
    Ok(uri.into())
}
pub fn decode_text(bytes: &[u8], content_type: &str) -> String {
    let (encoding, bom) = text_encoding(bytes, content_type);
    encoding
        .decode_without_bom_handling(&bytes[bom..])
        .0
        .into_owned()
}

fn declared_encoding(content_type: &str) -> Option<&'static encoding_rs::Encoding> {
    crate::mime::MimeType::parse(content_type)?
        .parameter("charset")
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
}

fn text_encoding(bytes: &[u8], content_type: &str) -> (&'static encoding_rs::Encoding, usize) {
    if let Some((encoding, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        return (encoding, bom);
    }
    if let Some(encoding) = declared_encoding(content_type) {
        return (encoding, 0);
    }
    let is_html =
        crate::mime::MimeType::parse(content_type).is_some_and(|kind| kind.essence == "text/html");
    let meta_encoding = is_html.then(|| html_encoding::prescan(bytes)).flatten();
    (meta_encoding.unwrap_or(encoding_rs::WINDOWS_1252), 0)
}

const BASE_STYLE: &str = "body{margin:0;background:#f8fafc;color:#172936;font-family:sans-serif}main{max-width:820px;margin:48px auto;padding:28px}h1{font-size:42px;line-height:1.15;margin:16px 0 24px}p{font-size:18px;line-height:1.6}h2{font-size:23px;margin-top:32px}.eyebrow{font-size:13px;font-weight:bold;color:#16836a}a{color:#126f5e}.card{display:block;padding:18px;margin:12px 0;background:white;border:1px solid #dde5e8;text-decoration:none}.muted{color:#62727f;font-size:15px}pre{font-size:14px;padding:18px;background:#edf1f5}li{margin:10px 0}code{color:#13725d}";
fn builtin_source(name: &str) -> (String, bool) {
    let (title, body) = match name {
        "media" => ("Video playback", include_str!("../data/pages/media.html")),
        "streaming" => ("Segmented playback", include_str!("../data/pages/streaming.html")),
        "workers" => ("Worker and WebAssembly", include_str!("../data/pages/workers.html")),
        "time" => ("Zegar i układ", include_str!("../data/pages/time.html")),
        "canvas" => ("Płótno 2D", include_str!("../data/pages/canvas.html")),
        "numbers" => (
            "Liczby całkowite",
            include_str!("../data/pages/numbers.html"),
        ),
        "messages" => (
            "Wiadomości między portami",
            include_str!("../data/pages/messages.html"),
        ),
        "files" => ("Podgląd pliku", include_str!("../data/pages/files.html")),
        "bytes" => (
            "Pliki i strumienie",
            include_str!("../data/pages/bytes.html"),
        ),
        "routes" => ("Próba nawigacji", include_str!("../data/pages/routes.html")),
        "async" => (
            "Zadanie asynchroniczne",
            include_str!("../data/pages/async.html"),
        ),
        "modules" => (
            "Moduły i żywy dokument",
            include_str!("../data/pages/modules.html"),
        ),
        "graphics" => (
            "Grafika i układ",
            include_str!("../data/pages/graphics.html"),
        ),
        "forms" => ("Formularze", include_str!("../data/pages/forms.html")),
        "platform" => (
            "Platforma WWW",
            r#"<p class='eyebrow'>RUST BROWSER · ALFA.4</p><h1>HTML, dane i interakcje.</h1><p>Wstawiaj karty z szablonu, sprawdzaj Promise i JSON albo wykonaj zapytanie HTTPS.</p><style>.controls{display:flex;gap:12px;flex-wrap:wrap;margin:20px 0}button{display:inline-block;padding:12px 18px;background:#16836a;color:white;border:1px solid #16836a;font-size:16px}#cards{display:grid;grid-template-columns:repeat(2,1fr);gap:16px}article{padding:20px;background:#eaf6f0;border:1px solid #a7cfbc}article h2{margin-top:0}#state{padding:16px;background:#eff5fa}</style><div class='controls'><button id='add'>Dodaj kartę</button><button id='clear'>Wyczyść</button><button id='network'>Sprawdź HTTPS</button></div><p id='state'>Oczekiwanie na Promise i JSON.</p><div id='cards'></div><template id='template'><article><h2>Karta</h2><p>Elementy wstawione z zawartości template.</p></article></template><h2>Co testujesz</h2><p>Karty korzystają z Grid i operacji DOM. Dane przechodzą przez Promise oraz JSON. Przycisk HTTPS używa Fetch w trybie no-cors: połączenie można ukończyć, ale treść odpowiedzi pozostaje niedostępna zgodnie z regułami stron.</p><p>Pełna zgodność WWW jest w rozwoju. <a href='about:version'>Zobacz możliwości i braki tej wersji.</a></p><p><a href='about:home'>Wróć na start</a></p><script>var count=0;var cards=document.getElementById('cards');var state=document.getElementById('state');function addCard(){count++;var fragment=document.getElementById('template').content.cloneNode(true);fragment.querySelector('h2').textContent='Karta '+count;cards.appendChild(fragment);document.title='Platforma: '+count;}document.getElementById('add').addEventListener('click',addCard);document.getElementById('clear').addEventListener('click',function(){cards.replaceChildren();count=0;document.title='Platforma: 0';});document.getElementById('network').addEventListener('click',function(){state.textContent='Trwa zapytanie HTTPS...';fetch('https://example.com/',{mode:'no-cors'}).then(function(response){state.textContent='Połączenie HTTPS ukończone. Typ odpowiedzi: '+response.type;},function(error){state.textContent='Zapytanie nie powiodło się: '+String(error);});});new Response('{"message":"Promise i JSON działają.","cards":2}',{headers:{'Content-Type':'application/json'}}).json().then(function(data){state.textContent=data.message;for(var i=0;i<data.cards;i++)addCard();});</script>"#,
        ),
        "javascript" => (
            "Własny JavaScript",
            r#"<p class='eyebrow'>RUST BROWSER · JAVASCRIPT W RUST</p><h1>Strona reaguje.</h1><p>Te przyciski uruchamiają nasz własny interpreter JavaScript i zmieniają DOM. Nie korzystają z V8 ani WebView.</p><style>.controls{display:flex;gap:12px;flex-wrap:wrap;margin:24px 0}button{display:inline-block;padding:14px 20px;border:1px solid #16836a;background:#16836a;color:white;font-size:16px}#counter{font-size:42px;color:#146954;padding:16px 0}.panel{border:1px solid #cee4db;padding:20px;background:#f0f8f4}</style><div class='panel'><p id='counter'>Licznik: 0</p><div class='controls'><button type='button' id='increment'>Dodaj 1</button><button type='button' id='reset'>Wyzeruj</button><button type='button' id='timer'>Sprawdź timer</button></div><p id='status'>Gotowe do testowania.</p><p id='lifecycle' class='muted'>Oczekiwanie na dokument.</p></div><h2>Sprawdź samodzielnie</h2><p>Klikaj licznik, zerowanie i timer. Zmieniają się tekst strony, styl i tytuł karty. Otwórz tę stronę w drugiej karcie — każdy dokument ma oddzielny stan JavaScript.</p><p>To podzbiór języka i API DOM. Współczesne aplikacje wymagające Promise, modułów, Fetch, formularzy lub innych brakujących funkcji nadal mogą nie działać.</p><p><a href='about:home'>Wróć na start</a> · <a href='about:version'>Zakres tej wersji</a></p><script>var count = 0; var counter = document.getElementById('counter'); var status = document.getElementById('status'); function updateCounter() { counter.textContent = 'Licznik: ' + count; document.title = 'Licznik: ' + count; } document.getElementById('increment').addEventListener('click', function() { count++; updateCounter(); counter.style.color = '#146954'; }); document.getElementById('reset').addEventListener('click', function() { count = 0; updateCounter(); status.textContent = 'Licznik wyzerowany.'; }); document.getElementById('timer').addEventListener('click', function() { status.textContent = 'Timer oczekuje...'; setTimeout(function() { status.textContent = 'Timer zadziałał.'; }, 250); }); document.addEventListener('DOMContentLoaded', function() { document.getElementById('lifecycle').textContent = 'DOMContentLoaded — obsługa zdarzenia działa.'; });</script>"#,
        ),
        "demo" => (
            "Próba silnika",
            r#"<p class='eyebrow'>WŁASNY SILNIK · PRÓBA LOKALNA</p><h1>Od tekstu do pikseli.</h1><p>Ta strona przechodzi przez parser HTML, kaskadę CSS, układ i renderer napisane w Rust.</p><h2>Tekst i style</h2><p>Zwykły tekst, <strong>pogrubienie</strong>, <em>kursywa</em>, <code>kod o stałej szerokości</code> i <a href='#kotwica'>link do kotwicy</a>. Polskie litery: zażółć gęślą jaźń. Encje: &copy; &euro; &rarr;.</p><blockquote style='border:2px solid #16836a;padding:16px;background:#eaf6f0'>Układ jest przeliczany po zmianie rozmiaru okna.</blockquote><h2>Lista i obraz</h2><ol><li>Nawigacja przez wpisanie adresu lub kliknięcie linku.</li><li>Przewijanie kółkiem, Page Down i klawiszami strzałek.</li><li>Wstecz, dalej i odświeżanie oraz wiele kart.</li></ol><img src='about:sample-image' width='400' alt='Próbny obraz renderera'><h2 id='kotwica'>Jesteś przy kotwicy.</h2><pre>fn main() { println!("Rust Browser"); }</pre><p><a href='about:home'>Wróć na start</a> · <a href='about:version'>Informacje o wersji</a></p>"#,
        ),
        "version" => (
            "About Satin",
            include_str!("../data/pages/version.html"),
        ),
        _ => ("New Tab", include_str!("../data/pages/home.html")),
    };
    let body = body.replace("{{VERSION}}", env!("CARGO_PKG_VERSION")).replace("RUST BROWSER", "SATIN");
    use base64::{Engine, engine::general_purpose::STANDARD};
    let body = match name {
        "media" => body.replace("{{MEDIA_SAMPLE}}", &STANDARD.encode(include_bytes!("../data/pages/media-demo.mp4"))),
        "streaming" => body
            .replace("{{STREAM_VIDEO}}", &STANDARD.encode(include_bytes!("../data/pages/mse-video.mp4")))
            .replace("{{STREAM_AUDIO}}", &STANDARD.encode(include_bytes!("../data/pages/mse-audio.mp4"))),
        _ => body,
    };
    let home = title == "New Tab";
    let lang = if matches!(name, "media" | "streaming" | "version" | "workers") || home {
        "en"
    } else {
        "pl"
    };
    let source = format!(
        "<!doctype html><html lang='{lang}'><head><meta charset='utf-8'><title>{title}</title><style>{BASE_STYLE}</style></head><body><main>{}</main></body></html>",
        body
    );
    (source, home)
}

fn builtin_images(page: &mut LoadedPage, name: &str, home: bool) {
    if home {
        home_assets::add_to(page);
    } else if name == "demo" {
        let mut bitmap = image::RgbaImage::new(400, 160);
        for (x, y, p) in bitmap.enumerate_pixels_mut() {
            let grid = (x / 24 + y / 24) % 2;
            *p = image::Rgba([
                20 + (x * 50 / 400) as u8,
                112 + (y * 65 / 160) as u8,
                98 + (grid * 28) as u8,
                255,
            ]);
        }
        page.images
            .insert("about:sample-image".into(), Arc::new(bitmap));
    }
}

impl OpenedPage {
    pub fn internal(url: &Url, cookies: CookieStore) -> Self {
        let started = Instant::now();
        let (source, home) = builtin_source(url.path());
        let mut page = LoadedPage::empty(
            url.clone(),
            cookies,
            CookieContext::navigation(None, url, "GET"),
        );
        page.stats.bytes = source.len();
        builtin_images(&mut page, url.path(), home);
        Self {
            page,
            input: DocumentInput::Html(source),
            started,
            response_start: started,
        }
    }
}

pub fn builtin(name: &str) -> LoadedPage {
    let (source, home) = builtin_source(name);
    let mut page = LoadedPage::from_html(&format!("about:{name}"), &source);
    builtin_images(&mut page, name, home);
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csp_follows_the_final_http_response_in_every_navigation_path() {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                for line in reader.lines() {
                    if line.unwrap().is_empty() {
                        break;
                    }
                }
                let path = request.split_ascii_whitespace().nth(1).unwrap();
                let (status, headers, body) = match path {
                    "/redirect" => (
                        "302 Found",
                        "Location: /allowed\r\nContent-Security-Policy: default-src 'none'\r\n",
                        "",
                    ),
                    "/allowed" => (
                        "200 OK",
                        concat!(
                            "Content-Type: text/html\r\n",
                            "Content-Security-Policy: worker-src blob:; script-src 'wasm-unsafe-eval'\r\n",
                            "Content-Security-Policy-Report-Only: default-src 'none'\r\n"
                        ),
                        "<title>Allowed</title>",
                    ),
                    "/denied" => (
                        "200 OK",
                        "Content-Type: text/plain\r\nContent-Security-Policy: default-src 'self'\r\n",
                        "Denied",
                    ),
                    "/blocked-script" => (
                        "200 OK",
                        "Content-Type: text/html\r\nContent-Security-Policy: script-src 'unsafe-inline'\r\n",
                        r#"<!doctype html><head><script>
                            function probe() {
                                try { new WebAssembly.Module(new Uint8Array([0,97,115,109,1,0,0,0])); }
                                catch (error) { return error instanceof WebAssembly.CompileError; }
                                return false;
                            }
                            var before = probe();
                        </script>
                        <meta http-equiv="Content-Security-Policy" content="script-src 'unsafe-inline' 'wasm-unsafe-eval'">
                        <script>var after = probe();</script></head><body></body>"#,
                    ),
                    _ => panic!("unexpected fixture request {path}"),
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let loader = Loader::new().unwrap();
        let cancel = AtomicBool::new(false);
        let redirect = NavigationRequest::get(&format!("{origin}/redirect")).unwrap();
        let opened = loader.open_navigation(&redirect, &cancel).unwrap();
        assert_eq!(opened.page.url, format!("{origin}/allowed"));
        assert_eq!(opened.page.document_policy, DocumentPolicy::default());
        let denied = loader.load(&format!("{origin}/denied"), &cancel).unwrap();
        assert!(!denied.document_policy.allows_blob());
        assert!(!denied.document_policy.allows_wasm_compilation());
        for (path, expected) in [("allowed", true), ("denied", false)] {
            let request = NavigationRequest::get(&format!("{origin}/{path}")).unwrap();
            let mut policy = None;
            loader
                .stream_navigation(&request, &cancel, |part| {
                    if let NavigationPart::Open(opened) = part {
                        assert!(
                            policy
                                .replace((
                                    opened.page.document_policy.allows_blob(),
                                    opened.page.document_policy.allows_wasm_compilation(),
                                ))
                                .is_none()
                        );
                    }
                })
                .unwrap();
            assert_eq!(policy, Some((expected, expected)));
        }

        let request = NavigationRequest::get(&format!("{origin}/blocked-script")).unwrap();
        let opened = loader.open_navigation(&request, &cancel).unwrap();
        let (mut page, mut session) = crate::script::DocumentSession::start(opened).unwrap();
        let mut runtime = None;
        for _ in 0..64 {
            if session.done() {
                break;
            }
            session
                .advance(&mut page, &mut runtime, crate::layout::Viewport::default())
                .unwrap();
        }
        assert!(session.done());
        let mut runtime = runtime.unwrap();
        assert_eq!(
            runtime.eval("before && after").unwrap(),
            rust_browser_javascript::Value::Bool(true)
        );
        assert!(runtime.diagnostics().is_empty());
        fixture.join().unwrap();
    }

    #[test]
    fn csp_allows_pages_without_policy() {
        let loader = Loader::new().unwrap();
        let cancel = AtomicBool::new(false);
        for url in ["about:home", "data:text/html,<title>Fixture</title>"] {
            let request = NavigationRequest::get(url).unwrap();
            let opened = loader.open_navigation(&request, &cancel).unwrap();
            assert_eq!(opened.page.document_policy, DocumentPolicy::default());
        }
        assert_eq!(
            LoadedPage::from_html("https://example.test/", "").document_policy,
            DocumentPolicy::default()
        );
    }

    #[test]
    fn csp_is_active_in_an_already_parsed_html_snapshot() {
        let page = LoadedPage::from_html(
            "https://example.test/",
            r#"<!doctype html><head>
                <meta http-equiv="Content-Security-Policy" content="worker-src blob:; script-src 'none'">
                </head><body>
                <meta http-equiv="Content-Security-Policy" content="worker-src 'none'">
                </body>"#,
        );
        assert!(page.document_policy.allows_blob());
        assert!(!page.document_policy.allows_wasm_compilation());
    }

    #[test]
    fn encoding_bom_and_header() {
        assert_eq!(
            decode_text(
                &[0xef, 0xbb, 0xbf, 0xc5, 0xbc],
                "text/html;charset=windows-1252"
            ),
            "ż"
        );
        assert_eq!(
            decode_text(&[0x80], "text/plain; charset=windows-1252"),
            "€"
        );
        assert!(
            decode_text("<meta charset='utf-8'>zażółć".as_bytes(), "text/html").ends_with("zażółć")
        );
    }
    #[test]
    fn urls_and_resource_boundary() {
        assert_eq!(
            normalize_url("example.com").unwrap(),
            "https://example.com/"
        );
        let p = LoadedPage::from_html(
            "https://example.com/a/b",
            "<base href='/root/'><a href='child'>ok</a>",
        );
        assert_eq!(
            p.resolve("child").unwrap(),
            "https://example.com/root/child"
        );
        assert_eq!(
            p.resolve("#anchor").unwrap(),
            "https://example.com/root/#anchor"
        );
        assert!(resource_url(&p, "file:///C:/secret.txt").is_none());
    }
}
