//! Offline model for profile bookmarks and session-history entries from the
//! caller's current tab. No storage, network, search provider, or editor input.
#![allow(dead_code, reason = "The app owns omnibox input and navigation")]

use super::chrome::SuggestionRow;
use url::{Position, Url};

const MAX_TARGET_BYTES: usize = 8_192;
const MAX_TITLE_CHARS: usize = 120;
const MAX_EMPTY_ROWS: usize = 4;
const MAX_MATCHED_ROWS: usize = 6;

/// The sources are ordered for rank ties, not presented as history coverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::app) enum Source {
    Bookmark,
    TabHistory,
}

pub(in crate::app) struct Suggestion {
    pub source: Source,
    target: Url,
    title: String,
}

impl Suggestion {
    pub(in crate::app) fn display(&self) -> SuggestionRow<'_> {
        let address = visible_address(&self.target);
        if self.title.is_empty() {
            SuggestionRow {
                label: address,
                caption: None,
            }
        } else {
            SuggestionRow {
                label: &self.title,
                caption: Some(address),
            }
        }
    }

    /// Only use for explicit activation; unlike the display address, this
    /// retains the original page's query and fragment.
    pub(in crate::app) fn target(&self) -> &str {
        self.target.as_str()
    }
}

#[derive(Clone, Copy)]
pub(in crate::app) enum Direction {
    Previous,
    Next,
}

#[derive(Default)]
pub(in crate::app) struct Suggestions {
    rows: Vec<Suggestion>,
    selected: Option<usize>,
}

impl Suggestions {
    /// Call when opening or after a committed edit, never during preedit. Pass
    /// `(title, url)` bookmarks from the current profile/context and only the
    /// current tab's available history URLs, in caller-preferred order.
    /// Refreshing deliberately leaves Enter on the raw address until the
    /// user moves the highlight. No hint is written into the editor.
    pub(in crate::app) fn refresh<'a, U: AsRef<str>>(
        &mut self,
        query: &str,
        bookmarks: impl IntoIterator<Item = (&'a str, &'a str)>,
        tab_history: impl IntoIterator<Item = U>,
    ) {
        self.dismiss();
        let query = query.trim();
        if query.len() > MAX_TARGET_BYTES || query.chars().any(unsafe_char) {
            return;
        }
        let query = query.to_lowercase();
        let bookmarks = bookmarks
            .into_iter()
            .filter_map(|(title, url)| candidate(&query, Source::Bookmark, title, url));
        let mut candidates: Vec<_> = bookmarks.collect();
        let limit = if query.is_empty() {
            // Do not surface recent session URLs merely because the user
            // focused or cleared the address field.
            MAX_EMPTY_ROWS
        } else {
            candidates.extend(
                tab_history
                    .into_iter()
                    .filter_map(|url| candidate(&query, Source::TabHistory, "", url.as_ref())),
            );
            MAX_MATCHED_ROWS
        };

        // Stable within equal rank and source; the first canonical target wins.
        candidates.sort_by_key(|(rank, row)| (*rank, row.source));
        for (_, row) in candidates {
            if !self.rows.iter().any(|shown| shown.target == row.target) {
                self.rows.push(row);
                if self.rows.len() == limit {
                    break;
                }
            }
        }
    }

    pub(in crate::app) fn rows(&self) -> &[Suggestion] {
        &self.rows
    }

    pub(in crate::app) fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Resolve Enter only after the caller has handled any active composition.
    /// With no keyboard selection, let Enter submit the current address.
    pub(in crate::app) fn selected_target(&self) -> Option<&str> {
        self.rows.get(self.selected?).map(Suggestion::target)
    }

    /// An unselected list chooses first/last according to direction, and
    /// subsequent steps wrap. Returns false when the input was not handled.
    pub(in crate::app) fn step(&mut self, direction: Direction) -> bool {
        let count = self.rows.len();
        if count == 0 {
            return false;
        }
        self.selected = Some(match (self.selected, direction) {
            (None, Direction::Next) => 0,
            (None | Some(0), Direction::Previous) => count - 1,
            (Some(index), Direction::Previous) => index - 1,
            (Some(index), Direction::Next) => (index + 1) % count,
        });
        true
    }

    /// Escape dismisses an open list without editing. False allows the
    /// existing Escape behavior on the next press; also use on focus/tab or
    /// context changes and before dispatching a navigation.
    pub(in crate::app) fn dismiss(&mut self) -> bool {
        let was_open = !self.rows.is_empty();
        self.rows.clear();
        self.selected = None;
        was_open
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Exact,
    Prefix,
    Contains,
}

fn candidate(query: &str, source: Source, title: &str, raw: &str) -> Option<(Rank, Suggestion)> {
    let target = safe_target(raw)?;
    let title = safe_title(title);
    let rank = if query.is_empty() {
        Rank::Exact
    } else {
        let address = visible_address(&target).to_lowercase();
        let (_, schemeless) = address.split_once("://")?;
        let title = title.to_lowercase();
        [title.as_str(), &address, schemeless]
            .into_iter()
            .filter_map(|value| {
                if value == query || value.strip_suffix('/') == Some(query) {
                    Some(Rank::Exact)
                } else if value.starts_with(query) {
                    Some(Rank::Prefix)
                } else if value.contains(query) {
                    Some(Rank::Contains)
                } else {
                    None
                }
            })
            .min()?
    };
    Some((
        rank,
        Suggestion {
            source,
            target,
            title,
        },
    ))
}

fn visible_address(url: &Url) -> &str {
    &url[..Position::AfterPath]
}

fn safe_target(input: &str) -> Option<Url> {
    if input.len() > MAX_TARGET_BYTES
        || input.contains('\\')
        || input.chars().any(|c| c.is_whitespace() || unsafe_char(c))
        || !escaped_bytes_are_safe(input)
    {
        return None;
    }
    // Reject credentials rather than silently navigating to a different URL.
    // Check the raw authority as parsers can drop empty user information.
    let (scheme, rest) = input.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || authority.is_empty()
        || authority.contains('@')
    {
        return None;
    }
    let url = Url::parse(input).ok()?;
    (url.has_host()
        && url.username().is_empty()
        && url.password().is_none()
        && url.as_str().len() <= MAX_TARGET_BYTES)
        .then_some(url)
}

fn escaped_bytes_are_safe(input: &str) -> bool {
    if !input.contains('%') {
        return true;
    }
    // Check the percent-decoded view, but keep the serialized URL unchanged.
    // Invalid UTF-8 can be a legitimate escaped path byte; inspect all valid
    // UTF-8 subsequences for controls, even when surrounded by invalid bytes.
    let mut decoded = Vec::with_capacity(input.len());
    let mut bytes = input.bytes();
    let hex = |byte: u8| (byte as char).to_digit(16).map(|digit| digit as u8);
    while let Some(byte) = bytes.next() {
        let byte = if byte == b'%' {
            let Some((hi, lo)) = bytes.next().and_then(hex).zip(bytes.next().and_then(hex)) else {
                return false;
            };
            hi * 16 + lo
        } else {
            byte
        };
        decoded.push(byte);
    }
    decoded
        .utf8_chunks()
        .all(|chunk| !chunk.valid().chars().any(unsafe_char))
}

fn unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

fn safe_title(input: &str) -> String {
    let mut title = String::new();
    let mut chars = 0;
    let mut space = false;
    for c in input.chars().take(MAX_TARGET_BYTES) {
        if c.is_whitespace() {
            space = chars > 0;
        } else if !unsafe_char(c) {
            let needed = 1 + usize::from(space);
            if chars + needed > MAX_TITLE_CHARS {
                break;
            }
            if space {
                title.push(' ');
            }
            title.push(c);
            chars += needed;
            space = false;
        }
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_safe_http_targets_without_repairing_input() {
        for bad in [
            "javascript:alert(1)",
            "file:///private/report.txt",
            "about:blank",
            "https:///example.test",
            "https://example.test:65536",
            "https://user:secret@example.test/",
            "https://@example.test/",
            "https://example.test\\@elsewhere.test/",
            " https://example.test/",
            "https://exam\nple.test/",
            "https://example.test/%00",
            "https://example.test/%FF%0D%0a",
            "https://example.test/%7F",
            "https://example.test/%C2%85",
            "https://example.test/%E2%80%AE",
            "https://example.test/%zz",
            "https://example.test/?bad=%",
            "https://example.test/#\u{2066}hidden",
        ] {
            assert!(safe_target(bad).is_none(), "{bad:?}");
        }
        assert!(
            safe_target(&format!(
                "https://example.test/{}",
                "x".repeat(MAX_TARGET_BYTES)
            ))
            .is_none()
        );
        assert_eq!(
            safe_target("HTTPS://EXAMPLE.test:443/a%20b/caf%C3%A9?q=ok#p")
                .unwrap()
                .as_str(),
            "https://example.test/a%20b/caf%C3%A9?q=ok#p"
        );
        assert!(safe_target("http://localhost:8080/?contact=a@b.test").is_some());
    }

    #[test]
    fn escaping_and_title_sanitization_do_not_echo_controls() {
        for unsafe_escapes in ["%xz", "%0", "%FF%0A", "%7f", "%c2%85", "%e2%80%ae"] {
            assert!(!escaped_bytes_are_safe(unsafe_escapes));
        }
        assert!(escaped_bytes_are_safe("a%20b/caf%C3%A9/%FF"));
        assert_eq!(safe_title(" \tCzy\u{202e}taj \r\n 🦊\u{7f} "), "Czytaj 🦊");
        assert_eq!(
            safe_title(&"🦊".repeat(MAX_TITLE_CHARS + 1))
                .chars()
                .count(),
            MAX_TITLE_CHARS
        );
    }

    #[test]
    fn ranks_exact_then_prefix_then_contains_with_stable_source_ties() {
        let mut model = Suggestions::default();
        let bookmarks = [
            ("The rust notes", "https://notes.test/"),
            ("Rust tools", "https://tools.test/"),
            ("Rust docs", "https://docs.test/"),
            ("Rust", "https://other.test/"),
            ("Rust duplicate", "HTTPS://DOCS.TEST:443"),
        ];
        let history = [
            "https://docs.test/",
            "https://rust.test/",
            "https://session.test/contains-rust",
            "https://rust.test/",
        ];
        model.refresh(" RUST ", bookmarks, history);
        let rows: Vec<_> = model
            .rows()
            .iter()
            .map(|row| (row.target(), row.source))
            .collect();
        assert_eq!(
            rows,
            [
                ("https://other.test/", Source::Bookmark),
                ("https://tools.test/", Source::Bookmark),
                ("https://docs.test/", Source::Bookmark),
                ("https://rust.test/", Source::TabHistory),
                ("https://notes.test/", Source::Bookmark),
                ("https://session.test/contains-rust", Source::TabHistory),
            ],
        );
        assert_eq!(model.rows()[2].display().label, "Rust docs");
    }

    #[test]
    fn equivalent_targets_are_deduplicated_across_sources() {
        let mut model = Suggestions::default();
        let bookmarks = [
            ("Saved docs", "HTTPS://DOCS.TEST:443"),
            ("Repeated", "https://docs.test/"),
        ];
        let history = [
            Url::parse("https://docs.test/").unwrap(),
            Url::parse("https://docs.test/other").unwrap(),
        ];
        model.refresh("docs.test", bookmarks, history);
        assert_eq!(model.rows().len(), 2);
        assert_eq!(model.rows()[0].source, Source::Bookmark);
        assert_eq!(model.rows()[0].display().label, "Saved docs");
        assert_eq!(model.rows()[1].source, Source::TabHistory);
        assert_eq!(model.rows()[1].target(), "https://docs.test/other");
    }

    #[test]
    fn display_and_matching_do_not_reveal_url_query_or_fragment() {
        let mut model = Suggestions::default();
        let bookmarked = "https://example.test/path?q=private-key#private-fragment";
        let history = ["https://session.test/path?q=private-key#private-fragment"];
        let bookmarks = [("  A\t\u{202e} title\u{7f}  ", bookmarked)];
        model.refresh("private", bookmarks, history);
        assert!(model.rows().is_empty());
        model.refresh("path", bookmarks, history);
        assert_eq!(model.rows()[0].display().label, "A title");
        assert_eq!(
            model.rows()[0].display().caption,
            Some("https://example.test/path")
        );
        assert_eq!(model.rows()[1].display().label, "https://session.test/path");
        assert_eq!(model.rows()[1].display().caption, None);
        assert_eq!(model.selected_target(), None);
        assert!(model.step(Direction::Next));
        assert_eq!(model.selected_target(), Some(bookmarked));
    }

    #[test]
    fn blank_query_never_reads_history_and_no_sources_means_no_rows() {
        let mut model = Suggestions::default();
        let bookmarks = [
            ("A", "https://a.test"),
            ("B", "https://b.test"),
            ("C", "https://c.test"),
            ("D", "https://d.test"),
            ("E", "https://e.test"),
        ];
        let history = std::iter::from_fn(|| -> Option<&str> {
            panic!("blank query must not consume history");
        });
        model.refresh(" ", bookmarks, history);
        assert_eq!(model.rows().len(), MAX_EMPTY_ROWS);
        assert!(
            model
                .rows()
                .iter()
                .all(|row| row.source == Source::Bookmark)
        );
        model.refresh("unmatched", [], [] as [&str; 0]);
        assert!(model.rows().is_empty());
    }

    #[test]
    fn keyboard_selection_is_explicit_wraps_and_never_changes_the_edit() {
        use crate::app::address_bar::{AddressBar, Movement};

        let mut model = Suggestions::default();
        let mut address = AddressBar::default();
        address.set_text("rust".into());
        address.move_caret(Movement::Left, true);
        let history = ["https://rust.test/", "https://rusting.test/"];
        model.refresh(address.text(), [], history);
        assert_eq!(model.selected_target(), None);
        assert!(model.step(Direction::Previous));
        assert_eq!(model.selected(), Some(1));
        assert!(model.step(Direction::Next));
        assert_eq!(model.selected(), Some(0));
        assert_eq!(model.selected_target(), Some(history[0]));
        assert!(model.step(Direction::Previous));
        assert_eq!(model.selected(), Some(1));
        assert!(model.dismiss());
        assert!(!model.dismiss());
        assert!(!model.step(Direction::Next));
        assert_eq!(address.text(), "rust");
        assert_eq!(address.selected_text(), Some("t"));

        model.refresh(address.text(), [], history);
        assert!(model.step(Direction::Next));
        model.refresh(address.text(), [], history);
        assert_eq!(model.selected_target(), None);
    }
}
