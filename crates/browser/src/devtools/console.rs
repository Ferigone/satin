//! Bounded Console history for one tab's document.
//!
//! Keep one model per live document, replace it on a new document navigation,
//! and drop it on tab close. A same-document history change keeps the model.
//! Route observations by tab and document before recording them here.

use std::collections::VecDeque;

const MAX_ROWS: usize = 256;
const MAX_MESSAGE_BYTES: usize = 2048;
const MAX_SOURCE_BYTES: usize = 256;
const MAX_PHASE_BYTES: usize = 128;
const TRUNCATED: &str = "…";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ConsoleSeverity {
    Debug,
    Log,
    Info,
    Warn,
    Error,
}

impl ConsoleSeverity {
    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// An explicit `console.error` has error severity but is not a browser report.
/// Caught exceptions are not inferred from console calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConsoleOrigin {
    ConsoleApi,
    BrowserReported,
}

/// Already formatted plain text; no script values or formatting callbacks.
pub(crate) struct ConsoleInput<'a> {
    pub(crate) severity: ConsoleSeverity,
    pub(crate) origin: ConsoleOrigin,
    pub(crate) message: &'a str,
    pub(crate) source: Option<&'a str>,
    pub(crate) phase: Option<&'a str>,
}

/// Borrow from the model when painting; all text is bounded and remains inert.
#[derive(Debug)]
pub(crate) struct ConsoleRow {
    pub(crate) sequence: u64,
    pub(crate) severity: ConsoleSeverity,
    pub(crate) origin: ConsoleOrigin,
    pub(crate) message: Box<str>,
    pub(crate) source: Option<Box<str>>,
    pub(crate) phase: Option<Box<str>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConsoleFilter(u8);

impl ConsoleFilter {
    pub(crate) const NONE: Self = Self(0);
    pub(crate) const ALL: Self = Self(
        ConsoleSeverity::Debug.bit()
            | ConsoleSeverity::Log.bit()
            | ConsoleSeverity::Info.bit()
            | ConsoleSeverity::Warn.bit()
            | ConsoleSeverity::Error.bit(),
    );

    pub(crate) fn includes(self, severity: ConsoleSeverity) -> bool {
        self.0 & severity.bit() != 0
    }

    pub(crate) fn set(&mut self, severity: ConsoleSeverity, visible: bool) {
        if visible {
            self.0 |= severity.bit();
        } else {
            self.0 &= !severity.bit();
        }
    }
}

impl Default for ConsoleFilter {
    fn default() -> Self {
        Self::ALL
    }
}

#[derive(Default)]
pub(crate) struct Console {
    rows: VecDeque<ConsoleRow>,
    last_sequence: u64,
    discarded: u64,
}

impl Console {
    /// Accept an entry from an ordered stream or rolling snapshot. Sequence
    /// numbers are 1-based, document-local, and assigned consecutively before
    /// source eviction. Gaps count as unseen source losses; do not separately
    /// add the producer's cumulative eviction count. Replay or stale records
    /// return `false`, but identical messages at different sequences are kept.
    pub(crate) fn record(&mut self, sequence: u64, input: ConsoleInput<'_>) -> bool {
        if sequence <= self.last_sequence {
            return false;
        }
        let unseen = sequence - self.last_sequence - 1;
        self.last_sequence = sequence;
        self.discarded = self.discarded.saturating_add(unseen);
        if self.rows.len() == MAX_ROWS {
            self.rows.pop_front();
            self.discarded = self.discarded.saturating_add(1);
        }
        self.rows.push_back(ConsoleRow {
            sequence,
            severity: input.severity,
            origin: input.origin,
            message: bounded(input.message, MAX_MESSAGE_BYTES),
            source: input.source.map(|text| bounded(text, MAX_SOURCE_BYTES)),
            phase: input.phase.map(|text| bounded(text, MAX_PHASE_BYTES)),
        });
        true
    }

    pub(crate) fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// Read oldest to newest (or reverse for painting from the bottom).
    /// Filtering is a view operation and never affects retention or losses.
    pub(crate) fn rows(
        &self,
        filter: ConsoleFilter,
    ) -> impl DoubleEndedIterator<Item = &ConsoleRow> + '_ {
        self.rows
            .iter()
            .filter(move |row| filter.includes(row.severity))
    }

    /// Records lost to source gaps or local eviction since navigation/clear.
    /// Intentional clear, filtered rows, and clipped text are not losses.
    pub(crate) fn discarded(&self) -> u64 {
        self.discarded
    }

    /// Clear the displayed history and loss count without replaying old rows.
    /// The caller should ingest the latest snapshot first so the clear also
    /// covers records produced but not yet displayed. No engine clear needed.
    pub(crate) fn clear(&mut self) {
        self.rows.clear();
        self.discarded = 0;
    }
}

fn bounded(text: &str, max_bytes: usize) -> Box<str> {
    if text.len() <= max_bytes {
        return text.into();
    }
    let end = text.floor_char_boundary(max_bytes - TRUNCATED.len());
    let mut clipped = String::with_capacity(end + TRUNCATED.len());
    clipped.push_str(&text[..end]);
    clipped.push_str(TRUNCATED);
    clipped.into_boxed_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(message: &str) -> ConsoleInput<'_> {
        ConsoleInput {
            severity: ConsoleSeverity::Log,
            origin: ConsoleOrigin::ConsoleApi,
            message,
            source: None,
            phase: None,
        }
    }

    #[test]
    fn identical_messages_are_independent_but_snapshots_do_not_replay_them() {
        let mut console = Console::default();
        assert!(!console.record(0, log("invalid sequence")));
        assert!(console.record(1, log("repeat")));
        assert!(console.record(2, log("repeat")));
        assert!(!console.record(1, log("replayed")));
        assert!(!console.record(2, log("replayed")));

        let mut rows = console.rows(ConsoleFilter::ALL);
        let first = rows.next().unwrap();
        let second = rows.next().unwrap();
        assert_eq!(first.message, second.message);
        assert_eq!((first.sequence, second.sequence), (1, 2));
        assert!(rows.next().is_none());
        assert_eq!(console.discarded(), 0);
    }

    #[test]
    fn filter_does_not_confuse_console_error_with_a_browser_report() {
        let mut console = Console::default();
        for (index, severity) in [
            ConsoleSeverity::Debug,
            ConsoleSeverity::Log,
            ConsoleSeverity::Info,
            ConsoleSeverity::Warn,
            ConsoleSeverity::Error,
        ]
        .into_iter()
        .enumerate()
        {
            console.record(
                index as u64 + 1,
                ConsoleInput {
                    severity,
                    ..log("console call")
                },
            );
        }
        console.record(
            6,
            ConsoleInput {
                severity: ConsoleSeverity::Error,
                origin: ConsoleOrigin::BrowserReported,
                message: "unhandled",
                source: Some("app.js"),
                phase: Some("promise rejection"),
            },
        );

        let mut filter = ConsoleFilter::NONE;
        assert_eq!(console.rows(filter).count(), 0);
        filter.set(ConsoleSeverity::Warn, true);
        filter.set(ConsoleSeverity::Error, true);
        let visible: Vec<_> = console.rows(filter).collect();
        assert_eq!(visible.len(), 3);
        assert_eq!(visible[0].severity, ConsoleSeverity::Warn);
        assert_eq!(visible[1].severity, ConsoleSeverity::Error);
        assert_eq!(visible[1].origin, ConsoleOrigin::ConsoleApi);
        assert_eq!(visible[1].source, None);
        assert_eq!(visible[1].phase, None);
        assert_eq!(visible[2].severity, ConsoleSeverity::Error);
        assert_eq!(visible[2].origin, ConsoleOrigin::BrowserReported);
        assert_eq!(visible[2].source.as_deref(), Some("app.js"));
        assert_eq!(visible[2].phase.as_deref(), Some("promise rejection"));

        filter.set(ConsoleSeverity::Error, false);
        assert_eq!(console.rows(filter).next().unwrap().sequence, 4);
        assert_eq!(console.rows(filter).count(), 1);
        assert_eq!(console.rows(ConsoleFilter::default()).count(), 6);
        assert_eq!(console.discarded(), 0);
    }

    #[test]
    fn every_text_field_is_capped_on_a_utf8_boundary() {
        let mut console = Console::default();
        let long = "🦀".repeat(MAX_MESSAGE_BYTES);
        console.record(
            1,
            ConsoleInput {
                source: Some(&long),
                phase: Some(&long),
                ..log(&long)
            },
        );
        let row = console.rows(ConsoleFilter::ALL).next().unwrap();
        for (text, limit) in [
            (row.message.as_ref(), MAX_MESSAGE_BYTES),
            (row.source.as_deref().unwrap(), MAX_SOURCE_BYTES),
            (row.phase.as_deref().unwrap(), MAX_PHASE_BYTES),
        ] {
            let prefix = text.strip_suffix(TRUNCATED).unwrap();
            assert!(text.len() <= limit);
            assert!(text.len() + '🦀'.len_utf8() > limit);
            assert_eq!(prefix, &long[..prefix.len()]);
        }
        assert_eq!(console.discarded(), 0);

        let exact = "x".repeat(MAX_MESSAGE_BYTES);
        console.record(2, log(&exact));
        let row = console.rows(ConsoleFilter::ALL).next_back().unwrap();
        assert_eq!(row.message.as_ref(), exact);
    }

    #[test]
    fn keeps_the_newest_rows_and_counts_source_and_browser_losses_once() {
        let mut console = Console::default();
        assert!(console.record(4, log("three unavailable at first read")));
        assert!(console.record(7, log("two more unavailable")));
        for sequence in 8..=MAX_ROWS as u64 + 9 {
            assert!(console.record(sequence, log("latest")));
        }
        assert!(!console.record(4, log("old snapshot")));
        assert_eq!(console.last_sequence(), MAX_ROWS as u64 + 9);
        assert_eq!(console.rows(ConsoleFilter::NONE).count(), 0);
        assert_eq!(console.rows(ConsoleFilter::ALL).count(), MAX_ROWS);
        let mut rows = console.rows(ConsoleFilter::ALL);
        assert_eq!(rows.next().unwrap().sequence, 10);
        assert_eq!(rows.next_back().unwrap().sequence, MAX_ROWS as u64 + 9);
        assert_eq!(console.discarded(), 9); // five unseen and four evicted
    }

    #[test]
    fn clear_resets_losses_and_rows_but_only_navigation_resets_the_cursor() {
        let mut console = Console::default();
        console.record(5, log("before clear"));
        assert_eq!(console.discarded(), 4);
        console.clear();
        assert_eq!(console.rows(ConsoleFilter::ALL).count(), 0);
        assert_eq!(console.discarded(), 0);
        assert_eq!(console.last_sequence(), 5);
        assert!(!console.record(5, log("before clear")));
        assert!(console.record(7, log("after clear, six was lost")));
        assert_eq!(console.discarded(), 1);

        let mut new_document = Console::default();
        assert!(new_document.record(1, log("new document")));
        assert_eq!(new_document.discarded(), 0);
        assert_eq!(new_document.rows(ConsoleFilter::ALL).count(), 1);
        assert_eq!(console.last_sequence(), 7);
    }
}
