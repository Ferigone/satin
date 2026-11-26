//! Desktop download row model. Only the native controller can perform the
//! effects requested by a row, and an intent contains no path.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Action {
    Cancel,
    #[cfg(test)]
    Open,
    #[cfg(test)]
    ShowInFolder,
}

#[cfg(test)]
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
    None,
    #[cfg(test)]
    RevealOnly,
    #[cfg(test)]
    OpenAndReveal,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Failure {
    #[cfg(test)]
    Connection,
    Save,
    #[cfg(test)]
    Blocked,
    #[cfg(test)]
    Cleanup,
}

impl Failure {
    fn message(self) -> &'static str {
        match self {
            #[cfg(test)]
            Self::Connection => "Połączenie przerwane.",
            Self::Save => "Nie udało się zapisać pliku.",
            #[cfg(test)]
            Self::Blocked => "Plik został zablokowany przez ochronę pobierania.",
            #[cfg(test)]
            Self::Cleanup => "Nie udało się usunąć pliku częściowego.",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Event {
    BytesWritten(u64),
    // Only after the authorized destination has been published and policy passed.
    Saved(u64, SavedFileActions),
    // Only after the worker has stopped and cleaned up its partial data.
    Canceled,
    Failed(Failure),
    // The original file can no longer be identified at the recorded location.
    #[cfg(test)]
    FileUnavailable,
}

enum State {
    Transferring {
        written: u64,
        total: Option<u64>,
    },
    Canceling,
    Ready(u64, SavedFileActions),
    #[cfg(test)]
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
    pub(super) saved_bytes: Option<u64>,
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
            (State::Transferring { .. } | State::Canceling, Event::Saved(bytes, actions)) => {
                self.state = State::Ready(bytes, actions);
            }
            (State::Transferring { .. } | State::Canceling, Event::Canceled) => {
                self.state = State::Canceled;
            }
            (State::Transferring { .. } | State::Canceling, Event::Failed(reason)) => {
                self.state = State::Failed(reason);
            }
            #[cfg(test)]
            (State::Ready(..), Event::FileUnavailable) => self.state = State::Unavailable,
            _ => {}
        }
    }

    pub(super) fn id(&self) -> u64 {
        self.id
    }

    pub(super) fn is_active(&self) -> bool {
        matches!(self.state, State::Transferring { .. } | State::Canceling)
    }

    fn actions(&self) -> &'static [Action] {
        match self.state {
            State::Transferring { .. } => &[Action::Cancel],
            State::Ready(_, SavedFileActions::None) => &[],
            #[cfg(test)]
            State::Ready(_, SavedFileActions::OpenAndReveal) => {
                &[Action::Open, Action::ShowInFolder]
            }
            #[cfg(test)]
            State::Ready(_, SavedFileActions::RevealOnly) => &[Action::ShowInFolder],
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
                    "Kończenie…"
                } else {
                    "Pobieranie…"
                },
                None,
                Some(Progress {
                    written,
                    total,
                    percent: total
                        .map(|total| (u128::from(written) * 100 / u128::from(total)) as u8),
                }),
            ),
            State::Canceling => ("Anulowanie…", None, None),
            State::Ready(..) => ("Gotowe", None, None),
            #[cfg(test)]
            State::Unavailable => ("Plik niedostępny", None, None),
            State::Canceled => ("Anulowano", None, None),
            State::Failed(reason) => ("Błąd", Some(reason.message()), None),
        };
        RowView {
            name: &self.name,
            status,
            detail,
            progress,
            saved_bytes: match self.state {
                State::Ready(bytes, _) => Some(bytes),
                _ => None,
            },
            actions: self.actions(),
        }
    }
}
