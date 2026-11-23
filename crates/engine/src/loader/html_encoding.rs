//! Bounded HTML byte prescan before the streaming decoder commits to an
//! encoding. Attribute values and comments are scanned in their own states.
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252, X_USER_DEFINED};

pub(super) const PREFIX_BYTES: usize = 1024;
fn space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

pub(super) fn prescan(bytes: &[u8]) -> Option<&'static Encoding> {
    let bytes = &bytes[..bytes.len().min(PREFIX_BYTES)];
    if bytes.starts_with(b"<\0?\0x\0") {
        return Some(UTF_16LE);
    }
    if bytes.starts_with(b"\0<\0?\0x") {
        return Some(UTF_16BE);
    }
    let mut rest = bytes;
    while let Some(open) = rest.iter().position(|&byte| byte == b'<') {
        rest = &rest[open..];
        if rest.starts_with(b"<!--") {
            let Some(end) = rest.windows(3).position(|end| end == b"-->") else {
                break;
            };
            rest = &rest[end + 3..];
        } else if rest.len() >= 6
            && rest[..5].eq_ignore_ascii_case(b"<meta")
            && (space(rest[5]) || rest[5] == b'/')
        {
            let mut scan = &rest[5..];
            let Some(attributes) = read_tag(&mut scan) else {
                break;
            };
            let encoding = if let Some(label) = attributes.charset {
                Encoding::for_label(label)
            } else if attributes
                .pragma
                .is_some_and(|value| value.eq_ignore_ascii_case(b"content-type"))
            {
                attributes.content.and_then(content_encoding)
            } else {
                None
            };
            if let Some(encoding) = encoding {
                return Some(if encoding == X_USER_DEFINED {
                    WINDOWS_1252
                } else {
                    utf16_as_utf8(encoding)
                });
            }
            rest = scan;
        } else {
            let tag = &rest[1..];
            let tag = tag.strip_prefix(b"/").unwrap_or(tag);
            if tag.first().is_some_and(u8::is_ascii_alphabetic) {
                let Some(attributes) = tag.iter().position(|&byte| space(byte) || byte == b'>')
                else {
                    break;
                };
                rest = &tag[attributes..];
                if read_tag(&mut rest).is_none() {
                    break;
                }
            } else if rest
                .get(1)
                .is_some_and(|byte| matches!(byte, b'!' | b'/' | b'?'))
            {
                let Some(end) = rest.iter().position(|&byte| byte == b'>') else {
                    break;
                };
                rest = &rest[end + 1..];
            } else {
                rest = &rest[1..];
            }
        }
    }
    xml_encoding(bytes)
}

#[derive(Default)]
struct Attributes<'a> {
    charset: Option<&'a [u8]>,
    pragma: Option<&'a [u8]>,
    content: Option<&'a [u8]>,
}

fn read_tag<'a>(input: &mut &'a [u8]) -> Option<Attributes<'a>> {
    let mut attributes = Attributes::default();
    loop {
        *input = trim_start(input, |byte| space(byte) || byte == b'/');
        if input[0..].first() == Some(&b'>') {
            *input = &input[1..];
            return Some(attributes);
        }
        let end = input.iter().enumerate().position(|(index, &byte)| {
            space(byte) || matches!(byte, b'/' | b'>') || (byte == b'=' && index > 0)
        })?;
        let name = &input[..end];
        *input = trim_start(&input[end..], space);
        let mut value = &[][..];
        if let Some(rest) = input.strip_prefix(b"=") {
            let rest = trim_start(rest, space);
            let &first = rest.first()?;
            if matches!(first, b'"' | b'\'') {
                let value_end = rest[1..].iter().position(|&byte| byte == first)? + 1;
                value = &rest[1..value_end];
                *input = &rest[value_end + 1..];
            } else {
                let value_end = rest.iter().position(|&byte| space(byte) || byte == b'>')?;
                value = &rest[..value_end];
                *input = &rest[value_end..];
            }
        }
        if name.eq_ignore_ascii_case(b"charset") {
            attributes.charset.get_or_insert(value);
        } else if name.eq_ignore_ascii_case(b"http-equiv") {
            attributes.pragma.get_or_insert(value);
        } else if name.eq_ignore_ascii_case(b"content") {
            attributes.content.get_or_insert(value);
        }
    }
}

fn content_encoding(mut bytes: &[u8]) -> Option<&'static Encoding> {
    while let Some(index) = bytes
        .windows(7)
        .position(|part| part.eq_ignore_ascii_case(b"charset"))
    {
        bytes = trim_start(&bytes[index + 7..], space);
        if let Some(value) = bytes.strip_prefix(b"=") {
            let value = trim_start(value, space);
            if let Some(&quote @ (b'\'' | b'"')) = value.first() {
                let end = value[1..].iter().position(|&byte| byte == quote)? + 1;
                return Encoding::for_label(&value[1..end]);
            }
            let end = value
                .iter()
                .position(|&byte| space(byte) || byte == b';')
                .unwrap_or(value.len());
            return Encoding::for_label(&value[..end]);
        }
    }
    None
}

fn xml_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    let bytes = bytes.strip_prefix(b"<?xml")?;
    let end = bytes.iter().position(|&byte| byte == b'>')?;
    let index = bytes[..end]
        .windows(8)
        .position(|part| part == b"encoding")?;
    let rest = trim_start(&bytes[index + 8..end], |byte| byte <= b' ');
    let rest = trim_start(rest.strip_prefix(b"=")?, |byte| byte <= b' ');
    let &quote @ (b'\'' | b'"') = rest.first()? else {
        return None;
    };
    let end = rest[1..].iter().position(|&byte| byte == quote)? + 1;
    let label = &rest[1..end];
    if label.iter().any(|&byte| byte <= b' ') {
        return None;
    }
    Encoding::for_label(label).map(utf16_as_utf8)
}

fn utf16_as_utf8(encoding: &'static Encoding) -> &'static Encoding {
    if encoding == UTF_16LE || encoding == UTF_16BE {
        UTF_8
    } else {
        encoding
    }
}

fn trim_start(bytes: &[u8], skip: impl Fn(u8) -> bool) -> &[u8] {
    let start = bytes
        .iter()
        .position(|&byte| !skip(byte))
        .unwrap_or(bytes.len());
    &bytes[start..]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognizes_complete_meta_and_ignores_encoding_like_text() {
        assert_eq!(
            prescan(
                b"<!-- <meta charset=gbk> --><div title='<meta charset=gbk>'><meta charset=UtF-8>"
            ),
            Some(UTF_8)
        );
        assert_eq!(prescan(b"charset=gbk<meta content='text/html;charset=gbk'><meta http-equiv=CONTENT-TYPE content=\"text/html;charset = 'shift_jis'\">"), Some(encoding_rs::SHIFT_JIS));
        assert_eq!(
            prescan(b"<meta charset=unknown charset=utf-8><meta charset=x-user-defined>"),
            Some(WINDOWS_1252)
        );
        assert_eq!(prescan(b"<meta charset='utf-8'"), None);
        assert_eq!(prescan(b"<?xml encoding='utf-16'?>"), Some(UTF_8));
    }
}
