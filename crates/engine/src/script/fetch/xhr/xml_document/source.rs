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
        // XHR's response MIME type defaults to text/xml if extraction fails.
        let mime = response
            .headers
            .iter()
            .find(|(name, _)| name == "content-type")
            .and_then(|(_, value)| MimeType::parse(value))
            .unwrap_or_else(|| MimeType::parse("text/xml").expect("XML MIME type"));
        if !(matches!(mime.essence.as_str(), "text/xml" | "application/xml")
            || mime.essence.ends_with("+xml"))
        {
            return None;
        }
        let charset = mime
            .parameter("charset")
            .and_then(|label| Encoding::for_label(label.as_bytes()));
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
        let (encoding, skip) = if let Some(bom) = Encoding::for_bom(raw) {
            bom
        } else if let Some(encoding) = self.charset {
            (encoding, 0)
        } else if raw.starts_with(b"\0<\0?") {
            (UTF_16BE, 0)
        } else if raw.starts_with(b"<\0?\0") {
            (UTF_16LE, 0)
        } else if let Some(label) = declaration_encoding(raw) {
            (Encoding::for_label(label.as_bytes())?, 0)
        } else {
            (UTF_8, 0)
        };
        let xml = encoding.decode_without_bom_handling_and_without_replacement(&raw[skip..])?;
        (xml.len() <= SOURCE_LIMIT).then_some((xml, encoding))
    }
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
