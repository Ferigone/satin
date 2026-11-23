//! Desktop download row model. Only the offline integration test loads this module
//! until a real download service can supply the events and consume the intents.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Action {
    Cancel,
    Open,
    ShowInFolder,
}

impl Action {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Cancel => "Cancel",
            Self::Open => "Open",
            Self::ShowInFolder => "Show in folder",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Intent {
    pub(super) id: u64,
    pub(super) action: Action,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum SavedFileActions {
    RevealOnly,
    OpenAndReveal,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Failure {
    Connection,
    Save,
    Blocked,
    Cleanup,
}

impl Failure {
    fn message(self) -> &'static str {
        match self {
            Self::Connection => "Connection interrupted.",
            Self::Save => "Could not save the file.",
            Self::Blocked => "The file was blocked by download protection.",
            Self::Cleanup => "Could not remove the partial file.",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Event {
    BytesWritten(u64),
    // Only after the authorized destination has been published and policy passed.
    Saved(SavedFileActions),
    // Only after the worker has stopped and cleaned up its partial data.
    Canceled,
    Failed(Failure),
    // The original file can no longer be identified at the recorded location.
    FileUnavailable,
}

enum State {
    Transferring { written: u64, total: Option<u64> },
    Canceling,
    Ready(SavedFileActions),
    Unavailable,
    Canceled,
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Progress {
    pub(super) written: u64,
    pub(super) total: Option<u64>,
    pub(super) percent: Option<u8>,
}

pub(super) struct RowView<'a> {
    pub(super) name: &'a str,
    pub(super) status: &'static str,
    pub(super) detail: Option<&'static str>,
    pub(super) progress: Option<Progress>,
    pub(super) actions: &'static [Action],
}

pub(super) struct Row {
    id: u64,
    name: String,
    state: State,
}

impl Row {
    /// `name` is the already-checked display basename, never a path or an action target.
    /// `total` is a trusted estimate in the same byte domain as `BytesWritten`.
    pub(super) fn new(id: u64, name: String, total: Option<u64>) -> Self {
        Self {
            id,
            name,
            state: State::Transferring {
                written: 0,
                total: total.filter(|&total| total != 0),
            },
        }
    }

    pub(super) fn apply(&mut self, id: u64, event: Event) {
        if id != self.id {
            return;
        }
        match (&mut self.state, event) {
            (State::Transferring { written, total }, Event::BytesWritten(count)) => {
                *written = (*written).max(count);
                *total = total.filter(|&total| total >= *written);
            }
            // One worker-owned terminal outcome wins, even after a cancel request.
            (State::Transferring { .. } | State::Canceling, Event::Saved(actions)) => {
                self.state = State::Ready(actions);
            }
            (State::Transferring { .. } | State::Canceling, Event::Canceled) => {
                self.state = State::Canceled;
            }
            (State::Transferring { .. } | State::Canceling, Event::Failed(reason)) => {
                self.state = State::Failed(reason);
            }
            (State::Ready(_), Event::FileUnavailable) => self.state = State::Unavailable,
            _ => {}
        }
    }

    fn actions(&self) -> &'static [Action] {
        match self.state {
            State::Transferring { .. } => &[Action::Cancel],
            State::Ready(SavedFileActions::OpenAndReveal) => &[Action::Open, Action::ShowInFolder],
            State::Ready(SavedFileActions::RevealOnly) => &[Action::ShowInFolder],
            _ => &[],
        }
    }

    /// Called only by native input. An intent carries no path and performs no I/O.
    pub(super) fn activate(&mut self, action: Action) -> Option<Intent> {
        if !self.actions().contains(&action) {
            return None;
        }
        if action == Action::Cancel {
            self.state = State::Canceling;
        }
        Some(Intent {
            id: self.id,
            action,
        })
    }

    pub(super) fn view(&self) -> RowView<'_> {
        let (status, detail, progress) = match self.state {
            State::Transferring { written, total } => (
                if total == Some(written) {
                    "Finishing…"
                } else {
                    "Downloading…"
                },
                None,
                Some(Progress {
                    written,
                    total,
                    percent: total
                        .map(|total| (u128::from(written) * 100 / u128::from(total)) as u8),
                }),
            ),
            State::Canceling => ("Canceling…", None, None),
            State::Ready(_) => ("Ready", None, None),
            State::Unavailable => ("File unavailable", None, None),
            State::Canceled => ("Canceled", None, None),
            State::Failed(reason) => ("Failed", Some(reason.message()), None),
        };
        RowView {
            name: &self.name,
            status,
            detail,
            progress,
            actions: self.actions(),
        }
    }
}
