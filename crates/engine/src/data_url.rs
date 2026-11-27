//! Bounded WHATWG data-URL processor. Percent decoding and forgiving base64
//! stream directly into the output; encoded padding/whitespace is not copied.
use crate::mime::MimeType;
use std::fmt;
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataUrlErrorKind {
    Invalid,
    Limit,
}
#[derive(Clone, Debug)]
pub struct DataUrlError {
    pub kind: DataUrlErrorKind,
    pub message: &'static str,
}
impl fmt::Display for DataUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for DataUrlError {}
fn invalid(message: &'static str) -> DataUrlError {
    DataUrlError {
        kind: DataUrlErrorKind::Invalid,
        message,
    }
}
fn limit() -> DataUrlError {
    DataUrlError {
        kind: DataUrlErrorKind::Limit,
        message: "Data URL exceeds resource byte budget",
    }
}
#[derive(Clone, Debug)]
pub struct DataUrl {
    pub mime_type: MimeType,
    pub body: Vec<u8>,
}

struct Percent<'a> {
    source: &'a [u8],
    offset: usize,
}
pub(crate) fn percent_decode(source: &[u8]) -> impl Iterator<Item = u8> + '_ {
    Percent { source, offset: 0 }
}
fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
impl Iterator for Percent<'_> {
    type Item = u8;
    fn next(&mut self) -> Option<u8> {
        let b = *self.source.get(self.offset)?;
        self.offset += 1;
        if b == b'%'
            && let (Some(&a), Some(&b)) = (
                self.source.get(self.offset),
                self.source.get(self.offset + 1),
            )
            && let (Some(a), Some(b)) = (hex(a), hex(b))
        {
            self.offset += 2;
            return Some((a << 4) | b);
        }
        Some(b)
    }
}
fn push(out: &mut Vec<u8>, value: u8, max: usize) -> Result<(), DataUrlError> {
    if out.len() >= max {
        return Err(limit());
    }
    out.push(value);
    Ok(())
}
pub(crate) fn forgiving_base64(input: impl IntoIterator<Item = u8>, max: usize) -> Result<Vec<u8>, DataUrlError> {
    let mut out = Vec::new();
    let mut bits = 0u32;
    let mut used = 0u8;
    let mut count = 0usize;
    let mut padding = 0usize;
    for b in input {
        if matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ') {
            continue;
        }
        if b == b'=' {
            padding += 1;
            if padding > 2 {
                return Err(invalid("Invalid base64 padding"));
            }
            continue;
        }
        if padding > 0 {
            return Err(invalid("Base64 data follows padding"));
        }
        let n = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(invalid("Invalid base64 character")),
        };
        count += 1;
        bits = (bits << 6) | n as u32;
        used += 6;
        if used >= 8 {
            used -= 8;
            push(&mut out, (bits >> used) as u8, max)?;
            bits &= (1u32 << used) - 1;
        }
    }
    if count % 4 == 1 || (padding > 0 && !(count + padding).is_multiple_of(4)) {
        return Err(invalid("Invalid base64 length"));
    }
    Ok(out)
}
pub fn process(url: &Url, max_bytes: usize) -> Result<DataUrl, DataUrlError> {
    if url.scheme() != "data" {
        return Err(invalid("Expected a data URL"));
    }
    // A percent-encoded base64 body needs at most four source bytes per output
    // byte; remaining source space permits bounded ASCII whitespace/metadata.
    if url.as_str().len() > max_bytes.saturating_mul(4).saturating_add(64 * 1024) {
        return Err(limit());
    }
    let serial = url.as_str().split('#').next().unwrap_or(url.as_str());
    let input = &serial[5..];
    let (mime, encoded) = input
        .split_once(',')
        .ok_or_else(|| invalid("Data URL has no comma"))?;
    if mime.len() > 64 * 1024 {
        return Err(limit());
    }
    let mut mime = mime.trim_matches(|c: char| c.is_ascii_whitespace());
    let mut is_base64 = false;
    if mime.len() >= 6
        && mime
            .get(mime.len() - 6..)
            .is_some_and(|s| s.eq_ignore_ascii_case("base64"))
    {
        let prefix = mime[..mime.len() - 6].trim_end_matches(' ');
        if let Some(prefix) = prefix.strip_suffix(';') {
            is_base64 = true;
            mime = prefix;
        }
    }
    let input = Percent {
        source: encoded.as_bytes(),
        offset: 0,
    };
    let body = if is_base64 {
        forgiving_base64(input, max_bytes)?
    } else {
        let mut body = Vec::new();
        for b in input {
            push(&mut body, b, max_bytes)?;
        }
        body
    };
    let mime = if mime.starts_with(';') {
        format!("text/plain{mime}")
    } else {
        mime.to_owned()
    };
    let mime_type = MimeType::parse(&mime)
        .unwrap_or_else(|| MimeType::parse("text/plain;charset=US-ASCII").unwrap());
    Ok(DataUrl { mime_type, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decode(source: &str) -> DataUrl {
        process(&Url::parse(source).unwrap(), 1024).unwrap()
    }
    #[test]
    fn percent_bytes_query_and_fragment_are_not_form_decoding() {
        let d = decode("data:,a+b%00%FF%zz?tail#ignored");
        assert_eq!(d.body, b"a+b\0\xff%zz?tail");
        assert_eq!(d.mime_type.serialize(), "text/plain;charset=US-ASCII");
        assert_eq!(decode("data:;charset=utf-8,%C5%BC").body, "ż".as_bytes());
    }
    #[test]
    fn forgiving_base64_and_mime_marker_follow_the_standard() {
        for value in ["YQ==", "YQ", "Y%20Q%3D%3D"] {
            assert_eq!(
                decode(&format!("data:text/plain; BASE64,{value}")).body,
                b"a"
            );
        }
        assert_eq!(
            decode("data:TEXT/PLAIN;CHARSET=\"utf-8\";BASE64,YWI")
                .mime_type
                .serialize(),
            "text/plain;charset=utf-8"
        );
        assert_eq!(decode("data:;base64,/x==").body, [255]);
        for value in ["=", "Y", "YQ=", "YQ===", "YQ==a", "Y-Q_"] {
            assert!(
                process(&Url::parse(&format!("data:;base64,{value}")).unwrap(), 1024).is_err(),
                "{value}"
            );
        }
    }
    #[test]
    fn decoded_and_source_limits_are_explicit() {
        let url = Url::parse("data:;base64,YWJjZA==").unwrap();
        assert_eq!(process(&url, 3).unwrap_err().kind, DataUrlErrorKind::Limit);
        assert_eq!(
            process(&Url::parse("data:,abcd").unwrap(), 3)
                .unwrap_err()
                .kind,
            DataUrlErrorKind::Limit
        );
        assert!(
            process(&Url::parse("data:;base64,").unwrap(), 0)
                .unwrap()
                .body
                .is_empty()
        );
        assert!(process(&Url::parse("data:nocomma").unwrap(), 1024).is_err());
        assert_eq!(
            decode("data:bad mime,a").mime_type.serialize(),
            "text/plain;charset=US-ASCII"
        );
        assert_eq!(
            decode("data:éééa,a").mime_type.serialize(),
            "text/plain;charset=US-ASCII"
        );
    }
}
