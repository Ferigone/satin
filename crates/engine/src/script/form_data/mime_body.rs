//! HTML form encoding and bounded Fetch form-data decoding.
use super::{EntryList, Field, blobs};
use crate::{mime::MimeType, network::FETCH_BODY_LIMIT};
use ring::rand::{SecureRandom, SystemRandom};
use rust_browser_javascript::{ErrorKind, JsError, Vm};
use std::fmt::Write;

const HEADER_LIMIT: usize = 64 * 1024;
const HEADER_COUNT: usize = 64;
const CRLF: &str = "\r\n";

#[derive(Clone, Copy)]
pub(in crate::script) enum Encoding {
    UrlEncoded,
    Multipart,
    Plain,
}

pub(super) fn too_large() -> JsError {
    JsError::new(ErrorKind::ExecutionLimit, "Encoded form body exceeds 4 MiB")
}

#[derive(Default)]
struct Output(Vec<u8>);

impl Output {
    fn push(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        if self.0.len().saturating_add(bytes.len()) > FETCH_BODY_LIMIT {
            return Err(too_large());
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), JsError> {
        self.push(text.as_bytes())
    }

    fn parameter(&mut self, text: &str) -> Result<(), JsError> {
        let mut start = 0;
        for (index, byte) in text.bytes().enumerate() {
            let escape = match byte {
                b'\r' => "%0D",
                b'\n' => "%0A",
                b'"' => "%22",
                _ => continue,
            };
            self.push(&text.as_bytes()[start..index])?;
            self.text(escape)?;
            start = index + 1;
        }
        self.push(&text.as_bytes()[start..])
    }
}

fn new_boundary() -> Result<String, JsError> {
    let mut entropy = [0u8; 16];
    SystemRandom::new().fill(&mut entropy).map_err(|_| {
        JsError::new(
            ErrorKind::InternalError,
            "Could not generate multipart boundary",
        )
    })?;
    let mut boundary = String::from("--------------------");
    for byte in entropy {
        write!(boundary, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(boundary)
}

fn file(
    vm: &Vm,
    value: &rust_browser_javascript::Value,
) -> Result<std::rc::Rc<blobs::Blob>, JsError> {
    blobs::get(vm, value)
        .filter(|blob| blob.file.is_some())
        .ok_or_else(|| JsError::new(ErrorKind::InternalError, "Missing FormData file"))
}

pub(super) fn encode(
    vm: &mut Vm,
    list: &EntryList,
    encoding: Encoding,
) -> Result<(Vec<u8>, String), JsError> {
    vm.charge_work(list.bytes.div_ceil(16) + list.entries.len())?;
    let mut out = Output::default();
    if matches!(encoding, Encoding::Multipart) {
        let boundary = new_boundary()?;
        let line = format!("--{boundary}");
        for entry in &list.entries {
            let name = blobs::line_endings(&entry.name, CRLF);
            out.text(&line)?;
            out.text("\r\nContent-Disposition: form-data; name=\"")?;
            out.parameter(&name)?;
            out.text("\"")?;
            match &entry.field {
                Field::Text(value) => {
                    out.text("\r\n\r\n")?;
                    out.text(&blobs::line_endings(value, CRLF))?;
                }
                Field::File(value) => {
                    let file = file(vm, value)?;
                    let info = file.file.as_ref().unwrap();
                    vm.charge_work(
                        (info.name.len() + file.mime.len()).div_ceil(16)
                            + file.bytes().len().div_ceil(32),
                    )?;
                    out.text("; filename=\"")?;
                    out.parameter(&info.name)?;
                    out.text("\"\r\nContent-Type: ")?;
                    out.text(if file.mime.is_empty() {
                        "application/octet-stream"
                    } else {
                        &file.mime
                    })?;
                    out.text("\r\n\r\n")?;
                    out.push(file.bytes())?;
                }
            }
            out.text(CRLF)?;
        }
        out.text(&line)?;
        out.text("--\r\n")?;
        return Ok((out.0, format!("multipart/form-data; boundary={boundary}")));
    }
    for (index, entry) in list.entries.iter().enumerate() {
        let name = blobs::line_endings(&entry.name, CRLF);
        let value = match &entry.field {
            Field::Text(text) => blobs::line_endings(text, CRLF),
            Field::File(value) => {
                let file = file(vm, value)?;
                let name = &file.file.as_ref().unwrap().name;
                vm.charge_work(name.len().div_ceil(16))?;
                blobs::line_endings(name, CRLF)
            }
        };
        if matches!(encoding, Encoding::UrlEncoded) {
            if index != 0 {
                out.text("&")?;
            }
            for part in url::form_urlencoded::byte_serialize(name.as_bytes()) {
                out.text(part)?;
            }
            out.text("=")?;
            for part in url::form_urlencoded::byte_serialize(value.as_bytes()) {
                out.text(part)?;
            }
        } else {
            out.text(&name)?;
            out.text("=")?;
            out.text(&value)?;
            out.text(CRLF)?;
        }
    }
    let mime = if matches!(encoding, Encoding::UrlEncoded) {
        "application/x-www-form-urlencoded"
    } else {
        "text/plain"
    };
    Ok((out.0, mime.into()))
}

fn invalid() -> JsError {
    JsError::type_error("Invalid multipart form body")
}

fn boundary_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 70
        && !token.ends_with(' ')
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&byte))
}

struct Delimiter {
    start: usize,
    after: usize,
    end: bool,
}

fn delimiter_at(body: &[u8], marker: &[u8], start: usize) -> Option<Delimiter> {
    let rest = body.get(start..)?.strip_prefix(marker)?;
    let (end, tail) = rest
        .strip_prefix(b"--")
        .map_or((false, rest), |tail| (true, tail));
    let padding = tail
        .iter()
        .take_while(|&&b| matches!(b, b' ' | b'\t'))
        .count();
    let tail = &tail[padding..];
    let after = body.len() - tail.len();
    if tail.starts_with(b"\r\n") || end && tail.is_empty() {
        Some(Delimiter {
            start,
            after: after + 2 * usize::from(!tail.is_empty()),
            end,
        })
    } else {
        None
    }
}

/// Boundary-like data inside a file has no meaning unless it spans a complete
/// delimiter line. Starting at a newline makes the scan linear in body bytes.
fn next_delimiter(body: &[u8], marker: &[u8], from: usize) -> Option<Delimiter> {
    let mut offset = from;
    while let Some(newline) = body[offset..].iter().position(|&byte| byte == b'\n') {
        offset += newline + 1;
        if offset > 1
            && body[offset - 2] == b'\r'
            && let Some(delimiter) = delimiter_at(body, marker, offset)
        {
            return Some(delimiter);
        }
    }
    None
}

struct Disposition {
    name: String,
    filename: Option<String>,
}

fn disposition(header: &[u8]) -> Result<Disposition, JsError> {
    let split = header
        .iter()
        .position(|&byte| byte == b';')
        .ok_or_else(invalid)?;
    if !header[..split]
        .trim_ascii()
        .eq_ignore_ascii_case(b"form-data")
    {
        return Err(invalid());
    }
    let mut tail = &header[split + 1..];
    let mut name = None;
    let mut filename = None;
    while !tail.trim_ascii().is_empty() {
        let index = tail
            .iter()
            .position(|&byte| matches!(byte, b';' | b'='))
            .ok_or_else(invalid)?;
        if tail[index] != b'=' {
            return Err(invalid());
        }
        let key = tail[..index].trim_ascii();
        let input = tail[index + 1..].trim_ascii_start();
        let value;
        if let Some(quoted) = input.strip_prefix(b"\"") {
            let mut bytes = Vec::new();
            let mut position = 0;
            loop {
                let byte = *quoted.get(position).ok_or_else(invalid)?;
                position += 1;
                if byte == b'"' {
                    break;
                }
                if byte == b'\\' {
                    bytes.push(*quoted.get(position).ok_or_else(invalid)?);
                    position += 1;
                } else {
                    bytes.push(byte);
                }
            }
            value = String::from_utf8_lossy(&bytes).into_owned();
            tail = quoted[position..].trim_ascii_start();
        } else {
            let end = input.iter().position(|&b| b == b';').unwrap_or(input.len());
            value = String::from_utf8_lossy(input[..end].trim_ascii()).into_owned();
            tail = &input[end..];
        }
        if key.eq_ignore_ascii_case(b"name") && name.is_none() {
            name = Some(value);
        } else if key.eq_ignore_ascii_case(b"filename") && filename.is_none() {
            filename = Some(value);
        }
        if tail.is_empty() {
            break;
        }
        tail = tail.strip_prefix(b";").ok_or_else(invalid)?;
    }
    Ok(Disposition {
        name: name.ok_or_else(invalid)?,
        filename,
    })
}

fn headers(bytes: &[u8]) -> Result<(Disposition, Option<String>), JsError> {
    let mut disposition_header = None;
    let mut content_type = None;
    for (index, line) in bytes.split(|&b| b == b'\n').enumerate() {
        if index >= HEADER_COUNT {
            return Err(too_large());
        }
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let (name, value) =
            line.split_at(line.iter().position(|&b| b == b':').ok_or_else(invalid)?);
        let value = value[1..].trim_ascii();
        if name.eq_ignore_ascii_case(b"content-disposition") && disposition_header.is_none() {
            disposition_header = Some(disposition(value)?);
        } else if name.eq_ignore_ascii_case(b"content-type") && content_type.is_none() {
            content_type = Some(String::from_utf8_lossy(value).into_owned());
        }
    }
    Ok((disposition_header.ok_or_else(invalid)?, content_type))
}

fn parse_multipart(
    vm: &mut Vm,
    factory: &blobs::BlobFactory,
    boundary: &str,
    bytes: &[u8],
) -> Result<EntryList, JsError> {
    if !boundary_token(boundary) {
        return Err(invalid());
    }
    let marker = format!("--{boundary}");
    let marker = marker.as_bytes();
    let mut delimiter = delimiter_at(bytes, marker, 0)
        .or_else(|| next_delimiter(bytes, marker, 0))
        .ok_or_else(invalid)?;
    let mut entries = EntryList::default();
    while !delimiter.end {
        let start = delimiter.after;
        let header_end = bytes[start..]
            .get(..HEADER_LIMIT.min(bytes.len() - start))
            .unwrap()
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|end| start + end)
            .ok_or_else(invalid)?;
        let (part, mime) = headers(&bytes[start..header_end])?;
        let data_start = header_end + 4;
        delimiter = next_delimiter(bytes, marker, data_start).ok_or_else(invalid)?;
        let data = bytes
            .get(data_start..delimiter.start - 2)
            .ok_or_else(invalid)?;
        if let Some(filename) = part.filename {
            let object = factory.file_from_bytes(
                vm,
                data.to_vec(),
                mime.as_deref().unwrap_or("text/plain"),
                filename,
            )?;
            entries.file(&part.name, object)?;
        } else {
            entries.text(&part.name, String::from_utf8_lossy(data).into_owned())?;
        }
    }
    Ok(entries)
}

pub(super) fn parse(
    vm: &mut Vm,
    factory: &blobs::BlobFactory,
    mime: &str,
    bytes: &[u8],
) -> Result<EntryList, JsError> {
    if bytes.len() > FETCH_BODY_LIMIT {
        return Err(too_large());
    }
    vm.charge_work(mime.len().div_ceil(16) + bytes.len().div_ceil(8))?;
    let mime = MimeType::parse(mime)
        .ok_or_else(|| JsError::type_error("Body has no form-data MIME type"))?;
    match mime.essence.as_str() {
        "application/x-www-form-urlencoded" => {
            let mut entries = EntryList::default();
            for (name, value) in url::form_urlencoded::parse(bytes) {
                entries.text(&name, value.into_owned())?;
            }
            Ok(entries)
        }
        "multipart/form-data" => parse_multipart(
            vm,
            factory,
            mime.parameter("boundary").ok_or_else(invalid)?,
            bytes,
        ),
        _ => Err(JsError::type_error("Body has no form-data MIME type")),
    }
}
