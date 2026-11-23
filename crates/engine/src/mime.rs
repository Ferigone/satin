//! MIME parsing/serialization shared by embedded resources and script policy.
//! Recovery follows MIME Sniffing rather than splitting quoted parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MimeType {
    pub essence: String,
    pub parameters: Vec<(String, String)>,
}
const LIMIT: usize = 64 * 1024;
fn whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | ' ')
}
fn token(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}
impl MimeType {
    pub fn parse(input: &str) -> Option<Self> {
        if input.len() > LIMIT {
            return None;
        }
        let input = input.trim_matches(whitespace);
        let (kind, tail) = input.split_once('/')?;
        if kind.is_empty() || !kind.bytes().all(token) {
            return None;
        }
        let split = tail.find(';').unwrap_or(tail.len());
        let subtype = tail[..split].trim_end_matches(whitespace);
        if subtype.is_empty() || !subtype.bytes().all(token) {
            return None;
        }
        let mut result = Self {
            essence: format!(
                "{}/{}",
                kind.to_ascii_lowercase(),
                subtype.to_ascii_lowercase()
            ),
            parameters: vec![],
        };
        let mut seen = std::collections::HashSet::new();
        let bytes = tail.as_bytes();
        let mut pos = split;
        while pos < bytes.len() {
            pos += 1;
            while pos < bytes.len() && matches!(bytes[pos], b'\t' | b'\n' | b'\r' | b' ') {
                pos += 1;
            }
            let start = pos;
            while pos < bytes.len() && !matches!(bytes[pos], b';' | b'=') {
                pos += 1;
            }
            let name = tail[start..pos].to_ascii_lowercase();
            if pos == bytes.len() {
                break;
            }
            if bytes[pos] == b';' {
                continue;
            }
            pos += 1;
            if pos == bytes.len() {
                break;
            }
            let value = if bytes[pos] == b'"' {
                pos += 1;
                let mut value = Vec::new();
                while pos < bytes.len() {
                    let b = bytes[pos];
                    pos += 1;
                    if b == b'"' {
                        break;
                    }
                    if b == b'\\' && pos < bytes.len() {
                        value.push(bytes[pos]);
                        pos += 1;
                    } else {
                        value.push(b);
                    }
                }
                while pos < bytes.len() && bytes[pos] != b';' {
                    pos += 1;
                }
                String::from_utf8(value).ok()?
            } else {
                let start = pos;
                while pos < bytes.len() && bytes[pos] != b';' {
                    pos += 1;
                }
                let value = tail[start..pos].trim_end_matches(whitespace);
                if value.is_empty() {
                    continue;
                }
                value.to_owned()
            };
            if !name.is_empty()
                && name.bytes().all(token)
                && value.chars().all(|c| {
                    c == '\t' || (' '..='~').contains(&c) || ('\u{80}'..='\u{ff}').contains(&c)
                })
                && seen.insert(name.clone())
            {
                result.parameters.push((name, value));
            }
        }
        Some(result)
    }
    pub fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    pub fn serialize(&self) -> String {
        let mut out = self.essence.clone();
        for (name, value) in &self.parameters {
            out.push(';');
            out.push_str(name);
            out.push('=');
            if !value.is_empty() && value.bytes().all(token) {
                out.push_str(value);
            } else {
                out.push('"');
                for c in value.chars() {
                    if matches!(c, '"' | '\\') {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push('"');
            }
        }
        out
    }
    pub fn javascript(&self) -> bool {
        matches!(
            self.essence.as_str(),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_parameters_recover_and_duplicates_keep_first_valid_value() {
        let m=MimeType::parse(" Text/HTML ;Charset=\"utf-8\"junk; charset=windows-1252; x=\"a;b\\\"c\"; empty=\"\";bad =ignored;zero=").unwrap();
        assert_eq!(m.essence, "text/html");
        assert_eq!(m.parameter("charset"), Some("utf-8"));
        assert_eq!(m.parameter("x"), Some("a;b\"c"));
        assert_eq!(m.parameter("empty"), Some(""));
        assert!(m.parameter("bad ").is_none());
        assert!(m.parameter("zero").is_none());
        assert_eq!(MimeType::parse(&m.serialize()), Some(m));
        assert!(MimeType::parse("text /plain").is_none());
        assert!(MimeType::parse("text/").is_none());
        assert!(MimeType::parse("text/plain;broken").is_some());
    }
}
