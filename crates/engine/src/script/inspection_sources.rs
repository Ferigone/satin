//! Retain a bounded preview of the source actually handed to the main-realm VM.
use std::sync::Arc;

const ENTRY_LIMIT: usize = 80;
const FILE_BYTES: usize = 96 * 1024;
const TOTAL_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug)]
pub struct InspectedSource {
    pub node: Option<crate::dom::NodeId>,
    pub label: String,
    pub text: Arc<str>,
    pub original_bytes: usize,
}

#[derive(Default)]
pub(super) struct SourceArchive {
    entries: Vec<InspectedSource>,
    retained: usize,
    omitted: usize,
}

impl SourceArchive {
    pub(super) fn capture(
        &mut self,
        node: Option<crate::dom::NodeId>,
        identifier: Option<&str>,
        fallback: impl FnOnce() -> String,
        text: &str,
    ) {
        if self.entries.len() >= ENTRY_LIMIT || self.retained >= TOTAL_BYTES {
            self.omitted = self.omitted.saturating_add(1);
            return;
        }
        let end =
            text.floor_char_boundary(text.len().min(FILE_BYTES).min(TOTAL_BYTES - self.retained));
        let label = identifier
            .and_then(|raw| url::Url::parse(raw).ok())
            .and_then(|url| {
                if matches!(url.scheme(), "http" | "https") {
                    Some(format!(
                        "{}{}",
                        url.origin().ascii_serialization(),
                        url.path().chars().take(220).collect::<String>()
                    ))
                } else {
                    None
                }
            })
            .unwrap_or_else(fallback);
        self.retained += end;
        self.entries.push(InspectedSource {
            node,
            label,
            text: Arc::from(&text[..end]),
            original_bytes: text.len(),
        });
    }

    pub(super) fn view(&self) -> (&[InspectedSource], usize) {
        (&self.entries, self.omitted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_capture_preserves_utf8_and_strips_url_secrets() {
        let mut store = SourceArchive::default();
        let text = format!("{}🦀", "a".repeat(FILE_BYTES - 1));
        store.capture(
            None,
            Some("https://user:pass@example.com/app.js?k=secret#fragment"),
            String::new,
            &text,
        );
        assert_eq!(store.entries[0].label, "https://example.com/app.js");
        assert_eq!(store.entries[0].text.len(), FILE_BYTES - 1);
        assert_eq!(store.entries[0].original_bytes, text.len());
        for _ in 0..ENTRY_LIMIT {
            store.capture(None, None, String::new, &text);
        }
        assert!(store.retained <= TOTAL_BYTES && store.omitted > 0);
    }
}
