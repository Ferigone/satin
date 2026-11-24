//! Hand off the original HTTP response before any document body collector runs.
use crate::network::CancelSignal;
use percent_encoding::percent_decode_str;
use reqwest::header::{CONTENT_DISPOSITION, HeaderMap};
use tokio::{fs::File, io::AsyncWriteExt};
use url::Url;

pub(super) const NEEDS_DESTINATION: &str = "This attachment needs a desktop download destination.";
pub const DOWNLOAD_BUFFER_BYTES: usize = 64 * 1024;

pub struct DownloadResponse {
    pub suggested_name: String,
    response: reqwest::Response,
    pub headers_at: std::time::Instant,
}

impl DownloadResponse {
    pub(super) fn new(
        response: reqwest::Response,
        url: &Url,
        headers_at: std::time::Instant,
        name: Option<&str>,
    ) -> Self {
        let url_name = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .unwrap_or("");
        let url_name = percent_decode_str(url_name).decode_utf8_lossy();
        let name = name.filter(|s| !s.is_empty()).unwrap_or(&url_name);
        Self {
            suggested_name: safe_name(name),
            response,
            headers_at,
        }
    }

    pub fn http_metadata(&self) -> (&Url, u16, super::HttpContentMetadata) {
        (
            self.response.url(),
            self.response.status().as_u16(),
            super::HttpContentMetadata::headers(self.response.headers()),
        )
    }

    /// Bytes are decoded by the HTTP transport. Each write reuses the file's
    /// bounded Tokio blocking buffer and completes before the next socket read.
    pub async fn write_to(
        mut self,
        file: &mut File,
        cancel: &CancelSignal,
        mut progress: impl FnMut(u64),
    ) -> Result<u64, String> {
        file.set_max_buf_size(DOWNLOAD_BUFFER_BYTES);
        let mut total = 0u64;
        loop {
            let chunk = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err("Download cancelled".into()),
                result = self.response.chunk() => result.map_err(|e| e.to_string())?,
            };
            let Some(chunk) = chunk else { break };
            for block in chunk.chunks(DOWNLOAD_BUFFER_BYTES) {
                if cancel.is_cancelled() {
                    return Err("Download cancelled".into());
                }
                file.write_all(block).await.map_err(|e| e.to_string())?;
                total = total
                    .checked_add(block.len() as u64)
                    .ok_or("File offset overflow")?;
            }
            progress(total);
        }
        file.flush().await.map_err(|e| e.to_string())?;
        Ok(total)
    }
}

// Only a basename is advisory. Never use a header value as a filesystem path.
fn safe_name(input: &str) -> String {
    let base = input.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name = String::new();
    for ch in base.chars() {
        if name.len() + ch.len_utf8() > 180 {
            break;
        }
        name.push(
            if ch.is_control()
                || "<>:\"|?*".contains(ch)
                || matches!(ch, '\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')
            {
                '_'
            } else {
                ch
            },
        );
    }
    let name = name.trim_matches([' ', '.']);
    if name.is_empty() {
        return "download".into();
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|s| {
            matches!(
                s,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    }) {
        format!("_{name}")
    } else {
        name.into()
    }
}

pub(super) fn attachment(headers: &HeaderMap) -> Option<Option<String>> {
    for value in headers.get_all(CONTENT_DISPOSITION) {
        let raw = value.as_bytes();
        let first = raw.split(|&b| b == b';').next().unwrap_or_default();
        if !first.trim_ascii().eq_ignore_ascii_case(b"attachment") {
            continue;
        }
        let text = String::from_utf8_lossy(raw);
        let mut quoted = false;
        let mut escaped = false;
        let params = text.split(|ch| {
            if escaped {
                escaped = false;
                return false;
            }
            if ch == '\\' && quoted {
                escaped = true;
            }
            if ch == '"' {
                quoted = !quoted;
            }
            ch == ';' && !quoted
        });
        let mut name = None;
        for field in params.skip(1) {
            let Some((k, v)) = field.split_once('=') else {
                continue;
            };
            let v = v.trim().trim_matches('"');
            if k.trim().eq_ignore_ascii_case("filename*")
                && let Some((charset, rest)) = v.split_once('\'')
                && charset.eq_ignore_ascii_case("utf-8")
                && let Some((_, encoded)) = rest.split_once('\'')
                && let Ok(decoded) = percent_decode_str(encoded).decode_utf8()
            {
                return Some(Some(decoded.into_owned()));
            }
            if name.is_none() && k.trim().eq_ignore_ascii_case("filename") {
                name = Some(v.to_owned());
            }
        }
        return Some(name);
    }
    None
}
