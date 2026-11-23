//! XML document response eligibility and bounded byte decoding.
use crate::{
    mime::MimeType,
    network::{ResponseType, WebResponse},
};
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};
use std::{borrow::Cow, sync::Arc};
use url::Url;

const SOURCE_LIMIT: usize = 256 * 1024;
const NODES_LIMIT: u32 = 4096;
const DEPTH_LIMIT: usize = 64;
const ATTR_LIMIT: usize = 128;

pub(in crate::script::fetch::xhr) struct Source {
    raw: Arc<Vec<u8>>,
    content_type: String,
    charset: Option<&'static Encoding>,
}

impl Source {
    pub(in crate::script::fetch::xhr) fn new(
        response: &WebResponse,
        client: &Url,
        raw: Arc<Vec<u8>>,
    ) -> Option<Self> {
        if raw.is_empty()
            || raw.len() > SOURCE_LIMIT
            || response.response_type != ResponseType::Basic
            || !Url::parse(&response.url).is_ok_and(|url| {
                matches!(url.scheme(), "http" | "https") && url.origin() == client.origin()
            })
        {
            return None;
        }
        let mime = response_mime(response);
        if !is_xml(&mime) {
            return None;
        }
        let charset = charset(&mime);
        Some(Self {
            raw,
            content_type: mime.essence,
            charset,
        })
    }

    pub(in crate::script::fetch::xhr) fn size(&self) -> usize {
        self.raw.capacity() + self.content_type.capacity() + 128
    }

    pub(super) fn bytes_len(&self) -> usize {
        self.raw.len()
    }

    pub(super) fn content_type(&self) -> &str {
        &self.content_type
    }

    pub(super) fn decode(&self) -> Option<(Cow<'_, str>, &'static Encoding)> {
        let raw = self.raw.as_slice();
        // RFC 7303 §3.2: BOM, then external charset, then XML's
        // in-band encoding detection (with UTF-8 as its default).
        let (encoding, skip) = match Encoding::for_bom(raw) {
            Some(bom) => bom,
            None => (self.charset.or_else(|| in_band_encoding(raw))?, 0),
        };
        let xml = encoding.decode_without_bom_handling_and_without_replacement(&raw[skip..])?;
        (xml.len() <= SOURCE_LIMIT).then_some((xml, encoding))
    }
}

pub(in crate::script::fetch::xhr) fn fallback_text_encoding(
    response: &WebResponse,
    raw: &[u8],
) -> &'static Encoding {
    let mime = response_mime(response);
    // XHR with the default response type sniffs XML with no known MIME
    // charset. Its text decoder is lossy and handles a BOM independently.
    charset(&mime)
        .or_else(|| {
            if is_xml(&mime) {
                in_band_encoding(raw)
            } else {
                None
            }
        })
        .unwrap_or(UTF_8)
}

fn is_xml(mime: &MimeType) -> bool {
    matches!(mime.essence.as_str(), "text/xml" | "application/xml")
        || mime.essence.ends_with("+xml")
}

fn charset(mime: &MimeType) -> Option<&'static Encoding> {
    mime.parameter("charset")
        .and_then(|label| Encoding::for_label(label.as_bytes()))
}

fn in_band_encoding(raw: &[u8]) -> Option<&'static Encoding> {
    if raw.starts_with(b"\0<\0?") {
        Some(UTF_16BE)
    } else if raw.starts_with(b"<\0?\0") {
        Some(UTF_16LE)
    } else if let Some(label) = declaration_encoding(raw) {
        Encoding::for_label(label.as_bytes())
    } else {
        Some(UTF_8)
    }
}

fn response_mime(response: &WebResponse) -> MimeType {
    // The transport can combine repeated Content-Type fields. Follow
    // Fetch's MIME extraction: last valid type, splitting only unquoted
    // commas and remembering a charset across the same MIME essence.
    let mut selected: Option<MimeType> = None;
    let mut inherited_charset = None;
    for (_, value) in response
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        let (mut quoted, mut escaped) = (false, false);
        for entry in value.split(|c| match c {
            _ if escaped => {
                escaped = false;
                false
            }
            '\\' if quoted => {
                escaped = true;
                false
            }
            '"' => {
                quoted = !quoted;
                false
            }
            ',' => !quoted,
            _ => false,
        }) {
            let Some(mut mime) = MimeType::parse(entry).filter(|mime| mime.essence != "*/*") else {
                continue;
            };
            if selected
                .as_ref()
                .is_none_or(|last| last.essence != mime.essence)
            {
                inherited_charset = mime.parameter("charset").map(str::to_owned);
            } else if mime.parameter("charset").is_none()
                && let Some(charset) = &inherited_charset
            {
                mime.parameters.push(("charset".into(), charset.clone()));
            }
            selected = Some(mime);
        }
    }
    // XHR's response MIME type defaults to text/xml if extraction fails.
    selected.unwrap_or_else(|| MimeType::parse("text/xml").expect("XML MIME type"))
}

pub(super) fn parse(xml: &str) -> Option<roxmltree::Document<'_>> {
    preflight(xml)?;
    roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: NODES_LIMIT,
        },
    )
    .ok()
}

// Count depth and lexical attributes before the parser does any per-element
// namespace/duplicate checking; also exclude DTD tokenization altogether.
// All XML syntax and namespace validation remain the real parser's job.
fn preflight(mut xml: &str) -> Option<()> {
    let mut depth = 0usize;
    while let Some(start) = xml.find('<') {
        xml = &xml[start + 1..];
        if let Some((body, end)) = xml
            .strip_prefix("!--")
            .map(|s| (s, "-->"))
            .or_else(|| xml.strip_prefix("![CDATA[").map(|s| (s, "]]>")))
            .or_else(|| xml.strip_prefix('?').map(|s| (s, "?>")))
        {
            xml = &body[body.find(end)? + end.len()..];
            continue;
        }
        if xml.starts_with('!') {
            return None;
        }
        if xml.starts_with('/') {
            depth = depth.checked_sub(1)?;
            xml = &xml[xml.find('>')? + 1..];
            continue;
        }
        let mut quote = None;
        let mut attrs = 0;
        let mut end = None;
        for (i, byte) in xml.bytes().enumerate() {
            if let Some(active) = quote {
                if byte == active {
                    quote = None;
                }
                continue;
            }
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'=' => {
                    attrs += 1;
                    if attrs > ATTR_LIMIT {
                        return None;
                    }
                }
                b'>' => {
                    end = Some(i);
                    break;
                }
                b'<' => return None,
                _ => {}
            }
        }
        let end = end?;
        if depth == DEPTH_LIMIT {
            return None;
        }
        if !xml[..end].ends_with('/') {
            depth += 1;
        }
        xml = &xml[end + 1..];
    }
    (depth == 0).then_some(())
}

fn declaration_encoding(raw: &[u8]) -> Option<&str> {
    let raw = raw.strip_prefix(b"<?xml")?;
    if !raw
        .first()
        .copied()
        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
    {
        return None;
    }
    let end = raw.windows(2).position(|window| window == b"?>")?;
    let decl = std::str::from_utf8(&raw[..end]).ok()?;
    let mut decl = decl.trim_start_matches(xml_space);
    while let Some((name, value, rest)) = attribute(decl) {
        if name == "encoding" {
            return Some(value);
        }
        decl = rest.trim_start_matches(xml_space);
    }
    None
}

pub(super) fn xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Inspect the spelling of attributes in an already parsed start tag (or
/// sniff the ASCII XML declaration before decoding). The XML parser, not
/// this scanner, validates grammar and normalizes attribute values.
pub(super) fn attribute(input: &str) -> Option<(&str, &str, &str)> {
    let end = input.find(|c| xml_space(c) || c == '=')?;
    let name = &input[..end];
    let quoted = input[end..]
        .trim_start_matches(xml_space)
        .strip_prefix('=')?
        .trim_start_matches(xml_space);
    let quote = quoted.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let rest = &quoted[1..];
    let end = rest.find(quote)?;
    Some((name, &rest[..end], &rest[end + 1..]))
}
