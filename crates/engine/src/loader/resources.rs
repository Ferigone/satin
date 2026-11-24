//! Typed parser resource requests. Transport and decoding run off the tab
//! thread; script input reaches the VM only after applying resource policy.
use super::*;
use crate::{
    fonts::FontFace,
    mime::MimeType,
    network::{CredentialsMode, RequestMode, WebRequest},
};
use base64::{Engine, engine::general_purpose::STANDARD};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Stylesheet,
    ClassicScript,
    ModuleScript,
    Image,
    Media,
    Font,
    LocalFont(Arc<str>),
}

#[derive(Debug)]
pub struct ResourceRequest {
    pub(crate) url: Url,
    pub(crate) client: Arc<Url>,
    pub(crate) kind: ResourceKind,
    pub(crate) cors: Option<CredentialsMode>,
    pub(crate) integrity: Option<Arc<str>>,
    pub(crate) deadline: Instant,
}

#[derive(Debug)]
pub struct DocumentResource {
    pub url: Url,
    pub transferred: usize,
    pub body: ResourceBody,
    pub origin_clean: bool,
}

#[derive(Debug)]
pub enum ResourceBody {
    Text(String),
    Image(Arc<image::RgbaImage>),
    Media(Arc<[u8]>),
    Svg(Arc<SvgImage>),
    Font(FontFace),
}

/// Final HTTP subresource response seen by the buffered loader, before local
/// status, integrity or decoder checks. This is not resource acceptance.
#[derive(Debug)]
pub struct HttpResourceObservation {
    pub kind: ResourceKind,
    pub method: &'static str,
    /// Final HTTP(S) URL, without userinfo, query or fragment.
    pub url: String,
    pub status: u16,
    /// Body bytes delivered by the HTTP client (possibly decompressed), not
    /// wire bytes. `None` means the HTTP client did not deliver a body.
    pub body_bytes: Option<usize>,
    pub content: HttpContentMetadata,
    /// When the loader received headers, if the chosen fetch path exposes it.
    pub headers_at: Option<Instant>,
    /// When the buffered HTTP result reached this loader boundary.
    pub observed_at: Instant,
}

impl HttpResourceObservation {
    fn capture(
        request: &ResourceRequest,
        final_url: &str,
        status: u16,
        body_bytes: Option<usize>,
        headers_at: Option<Instant>,
    ) -> Option<Self> {
        use url::Position::{AfterPath, BeforeHost};
        if !matches!(request.url.scheme(), "http" | "https") {
            return None;
        }
        let observed_at = Instant::now();
        let url = Url::parse(final_url).ok()?;
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        Some(Self {
            kind: request.kind.clone(),
            method: "GET",
            url: format!("{}://{}", url.scheme(), &url[BeforeHost..AfterPath]),
            status,
            body_bytes,
            content: Default::default(),
            headers_at,
            observed_at,
        })
    }
}

impl Loader {
    pub fn load_document_resource(
        &self,
        request: &ResourceRequest,
        cancel: &AtomicBool,
    ) -> Result<DocumentResource, String> {
        let control = RequestControl::legacy(cancel, request.transport_deadline());
        self.reactor
            .block_on(self.load_resource_controlled(request, &control, None))
    }

    pub async fn load_document_resource_cancellable(
        &self,
        request: &ResourceRequest,
        cancel: &CancelSignal,
    ) -> Result<DocumentResource, String> {
        let control = RequestControl::new(cancel, request.transport_deadline());
        self.load_resource_controlled(request, &control, None).await
    }

    /// Opt in to one observation for this call, with no shared observer or
    /// queue. Intermediate redirect hops and local loads are not recorded.
    /// Failures before a buffered HTTP result produce no observation; a later
    /// resource failure can still have one (e.g. HTTP 404 or bad MIME).
    pub async fn load_document_resource_observed_cancellable(
        &self,
        request: &ResourceRequest,
        cancel: &CancelSignal,
    ) -> (
        Result<DocumentResource, String>,
        Option<HttpResourceObservation>,
    ) {
        let control = RequestControl::new(cancel, request.transport_deadline());
        let mut observation = None;
        let result = self
            .load_resource_controlled(request, &control, Some(&mut observation))
            .await;
        (result, observation)
    }

    async fn load_resource_controlled(
        &self,
        request: &ResourceRequest,
        control: &RequestControl<'_>,
        observation: Option<&mut Option<HttpResourceObservation>>,
    ) -> Result<DocumentResource, String> {
        use ResourceKind::*;
        control.check().map_err(interrupted_resource)?;
        if let LocalFont(name) = &request.kind {
            let name = name.clone();
            let face = tokio::task::spawn_blocking(move || FontFace::local(&name))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?
                .ok_or("Local font not found")?;
            control.check().map_err(interrupted_resource)?;
            return Ok(DocumentResource {
                url: request.url.clone(),
                transferred: 0,
                body: ResourceBody::Font(face),
                origin_clean: true,
            });
        }
        let limit = match request.kind {
            ClassicScript | ModuleScript => 1024 * 1024,
            Media => 16 * 1024 * 1024,
            _ => RESOURCE_LIMIT,
        };
        let cors = request.cors.or(match request.kind {
            ModuleScript | Font => Some(CredentialsMode::SameOrigin),
            _ => None,
        });
        let integrity = request
            .integrity
            .as_deref()
            .map(Integrity::parse)
            .transpose()?
            .flatten();
        let (final_url, content_type, bytes, nosniff, origin_clean) = if cors.is_some()
            || (integrity.is_some() && matches!(request.url.scheme(), "http" | "https"))
        {
            let mut web = WebRequest::get(request.url.clone(), (*request.client).clone());
            if let Some(credentials) = cors {
                web.credentials = credentials;
            } else {
                // A no-CORS opaque response is ineligible for SRI, including
                // an initially same-origin request that redirects across origins.
                web.mode = RequestMode::SameOrigin;
                web.credentials = CredentialsMode::Include;
            }
            let data = self
                .fetch_with_policy(&web, control, limit, false)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(observation) = observation {
                *observation = HttpResourceObservation::capture(
                    request,
                    &data.url,
                    data.status,
                    data.body.as_ref().map(|body| body.len()),
                    None,
                );
                if let Some(record) = observation {
                    record.content = HttpContentMetadata::from_exposed_headers(&data.headers);
                }
            }
            if !(200..300).contains(&data.status) {
                return Err(format!("HTTP {} loading resource", data.status));
            }
            let mut content_type = String::new();
            let mut nosniff = false;
            for (name, value) in data.headers {
                match name.as_str() {
                    "content-type" if content_type.is_empty() => content_type = value,
                    "x-content-type-options" => nosniff |= is_nosniff(&value),
                    _ => {}
                }
            }
            (
                data.url,
                content_type,
                data.body.unwrap_or_default(),
                nosniff,
                true,
            )
        } else {
            if request.url.scheme() == "file" && request.client.scheme() != "file" {
                return Err("A non-local page cannot read a local file".into());
            }
            let data = self
                .fetch_controlled(
                    &NavigationRequest::subresource(request.url.clone(), &request.client),
                    limit,
                    control,
                )
                .await?;
            if let Some(observation) = observation {
                *observation = HttpResourceObservation::capture(
                    request,
                    &data.url,
                    data.status,
                    Some(data.bytes.len()),
                    Some(data.response_start),
                );
                if let Some(record) = observation {
                    record.content = data.http_content.clone();
                }
            }
            let data = data.resource()?;
            (
                data.url,
                data.content_type,
                Arc::new(data.bytes),
                data.nosniff,
                data.origin_clean,
            )
        };
        if integrity.is_some_and(|integrity| !integrity.matches(&bytes)) {
            return Err("Subresource integrity digest mismatch".into());
        }
        control.check().map_err(interrupted_resource)?;
        let transferred = bytes.len();
        let kind = request.kind.clone();
        let body = if matches!(kind, Image | Font) {
            // A decoder can be CPU-intensive. Keep the bounded resource slot
            // until its work returns so canceling tabs cannot pile up decoders.
            tokio::task::spawn_blocking(move || {
                decode_resource(kind, &content_type, &bytes, nosniff)
            })
            .await
            .map_err(|error| error.to_string())??
        } else {
            decode_resource(kind, &content_type, &bytes, nosniff)?
        };
        control.check().map_err(interrupted_resource)?;
        let mut url = Url::parse(&final_url).map_err(|error| error.to_string())?;
        // Fetch fragments are not sent over the wire, but separate fragment
        // module URLs still have distinct module-map identities.
        if url.fragment().is_none() {
            url.set_fragment(request.url.fragment());
        }
        Ok(DocumentResource {
            url,
            transferred,
            body,
            origin_clean,
        })
    }
}

impl ResourceRequest {
    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn kind(&self) -> &ResourceKind {
        &self.kind
    }

    fn transport_deadline(&self) -> Instant {
        self.deadline.min(Instant::now() + Duration::from_secs(6))
    }
}

fn interrupted_resource(error: Interrupted) -> String {
    match error {
        Interrupted::Aborted => interrupted_navigation(error),
        Interrupted::Deadline => "Document resource deadline exceeded".into(),
    }
}

fn decode_resource(
    kind: ResourceKind,
    content_type: &str,
    bytes: &[u8],
    nosniff: bool,
) -> Result<ResourceBody, String> {
    use ResourceKind::*;
    let mime = MimeType::parse(content_type);
    let essence = mime.as_ref().map_or("", |m| m.essence.as_str());
    let body = match kind {
        ModuleScript => {
            if !mime.as_ref().is_some_and(MimeType::javascript) {
                return Err(format!(
                    "Module must have a JavaScript MIME type (received {essence:?})"
                ));
            }
            // Module source is always decoded as UTF-8 independently of
            // the declared HTTP charset or the document's encoding.
            ResourceBody::Text(
                encoding_rs::UTF_8
                    .decode_with_bom_removal(bytes)
                    .0
                    .into_owned(),
            )
        }
        ClassicScript => {
            if ["audio/", "image/", "video/"]
                .iter()
                .any(|prefix| essence.starts_with(prefix))
                || essence == "text/csv"
                || (nosniff && !mime.as_ref().is_some_and(MimeType::javascript))
            {
                return Err(format!(
                    "Classic script blocked by MIME policy: {essence:?}"
                ));
            }
            ResourceBody::Text(decode_text(bytes, content_type))
        }
        Stylesheet => {
            if essence != "text/css" {
                return Err(format!(
                    "Stylesheet must have text/css MIME type (received {essence:?})"
                ));
            }
            ResourceBody::Text(decode_text(bytes, content_type))
        }
        Image if svg_mime(content_type) => {
            let text = decode_text(bytes, "image/svg+xml; charset=utf-8");
            ResourceBody::Svg(Arc::new(SvgImage::from_source(&text)?))
        }
        Image => ResourceBody::Image(Arc::new(decode_bitmap(bytes)?)),
        Media => ResourceBody::Media(Arc::from(bytes)),
        Font => ResourceBody::Font(FontFace::decode(bytes).map_err(|error| error.to_string())?),
        LocalFont(_) => unreachable!("local font handled above"),
    };
    Ok(body)
}

pub(super) fn is_nosniff(value: &str) -> bool {
    value
        .split(',')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("nosniff"))
}

struct Integrity<'a> {
    algorithm: &'static ring::digest::Algorithm,
    values: Vec<&'a str>,
}
impl<'a> Integrity<'a> {
    fn parse(source: &'a str) -> Result<Option<Self>, String> {
        if source.len() > 16 * 1024 {
            return Err("Subresource integrity metadata exceeds 16 KiB".into());
        }
        let mut rank = 0;
        let mut chosen = None;
        for token in source.split_ascii_whitespace() {
            let mut parts = token.split('?').next().unwrap_or("").split('-');
            let (priority, algorithm) =
                match parts.next().unwrap_or("").to_ascii_lowercase().as_str() {
                    "sha256" => (1, &ring::digest::SHA256),
                    "sha384" => (2, &ring::digest::SHA384),
                    "sha512" => (3, &ring::digest::SHA512),
                    _ => continue,
                };
            if priority < rank {
                continue;
            }
            if priority > rank {
                rank = priority;
                chosen = Some(Self {
                    algorithm,
                    values: Vec::new(),
                });
            }
            chosen
                .as_mut()
                .unwrap()
                .values
                .push(parts.next().unwrap_or(""));
        }
        Ok(chosen)
    }

    fn matches(&self, bytes: &[u8]) -> bool {
        let actual = STANDARD.encode(ring::digest::digest(self.algorithm, bytes).as_ref());
        self.values.contains(&actual.as_str())
    }
}

type BitmapReader<'a> = image::ImageReader<Cursor<&'a [u8]>>;

fn bitmap_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(DECODED_IMAGES_LIMIT as u64);
    limits
}

fn bitmap_reader<'a>(bytes: &'a [u8], limits: &image::Limits) -> Result<BitmapReader<'a>, String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    reader.limits(limits.clone());
    Ok(reader)
}

fn checked_rgba(image: image::DynamicImage) -> Result<image::RgbaImage, String> {
    if u64::from(image.width()) * u64::from(image.height()) * 4 > DECODED_IMAGES_LIMIT as u64 {
        return Err("Decoded image exceeds its byte budget".into());
    }
    Ok(image.into_rgba8())
}

pub(super) fn decode_bitmap(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let reader = bitmap_reader(bytes, &bitmap_limits())?;
    checked_rgba(reader.decode().map_err(|error| error.to_string())?)
}

pub(crate) fn decode_oriented_bitmap(
    bytes: &[u8],
    orient: bool,
) -> Result<image::RgbaImage, String> {
    use image::ImageDecoder as _;

    if !orient {
        return decode_bitmap(bytes);
    }
    let mut limits = bitmap_limits();
    let reader = bitmap_reader(bytes, &limits)?;
    let mut decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    limits
        .reserve(decoder.total_bytes())
        .map_err(|error| error.to_string())?;
    decoder
        .set_limits(limits)
        .map_err(|error| error.to_string())?;
    let orientation = decoder.orientation().map_err(|error| error.to_string())?;
    let mut image =
        image::DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
    image.apply_orientation(orientation);
    checked_rgba(image)
}

#[cfg(test)]
mod tests;
