//! Offline fixture and state tests; no browser process, network, or download files.

#[path = "../src/app/downloads_ui/row.rs"]
mod downloads_ui;

use downloads_ui::{Action, Event, Failure, Intent, Progress, Row, SavedFileActions};

fn row(id: u64, name: &str, total: Option<u64>) -> Row {
    Row::new(id, name.into(), total)
}

#[test]
fn offline_fixture_presents_the_four_primary_states() {
    let mut ready = row(1, "notes.txt", Some(18));
    let mut downloading = row(2, "schedule.csv", Some(200));
    let mut canceled = row(3, "agenda.txt", None);
    let mut failed = row(4, "reading-list.txt", None);

    ready.apply(1, Event::Saved(18, SavedFileActions::OpenAndReveal));
    downloading.apply(2, Event::BytesWritten(50));
    assert!(canceled.activate(Action::Cancel).is_some());
    canceled.apply(3, Event::Canceled);
    failed.apply(4, Event::Failed(Failure::Connection));

    let mut lines = Vec::new();
    for item in [ready, downloading, canceled, failed] {
        let view = item.view();
        let mut line = format!("{} | {}", view.name, view.status);
        if let Some(progress) = view.progress {
            line.push_str(&format!(
                " {} / {} B ({}%)",
                progress.written,
                progress.total.unwrap(),
                progress.percent.unwrap()
            ));
        }
        if let Some(detail) = view.detail {
            line.push_str(&format!(" {detail}"));
        }
        for action in view.actions {
            line.push_str(&format!(" | {}", action.label()));
        }
        lines.push(line);
    }
    let snapshot = lines.join("\n");
    println!("Offline downloads fixture (no network or download files)\n{snapshot}");
    assert_eq!(
        snapshot,
        "notes.txt | Gotowe | Open | Show in folder\n\
         schedule.csv | Pobieranie… 50 / 200 B (25%) | Cancel\n\
         agenda.txt | Anulowano\n\
         reading-list.txt | Błąd Połączenie przerwane."
    );
}

#[test]
fn bytes_are_monotonic_and_only_save_acknowledgment_makes_the_row_ready() {
    let mut item = row(1, "notes.txt", Some(u64::MAX));
    item.apply(1, Event::BytesWritten(u64::MAX - 1));
    item.apply(1, Event::BytesWritten(1));
    assert_eq!(
        item.view().progress,
        Some(Progress {
            written: u64::MAX - 1,
            total: Some(u64::MAX),
            percent: Some(99),
        })
    );

    item.apply(1, Event::BytesWritten(u64::MAX));
    assert_eq!(item.view().progress.unwrap().percent, Some(100));
    assert_eq!(item.view().status, "Kończenie…");
    assert_eq!(item.view().actions, &[Action::Cancel]);
    assert_eq!(item.activate(Action::Open), None);
    assert_eq!(item.activate(Action::ShowInFolder), None);

    item.apply(1, Event::Saved(u64::MAX, SavedFileActions::RevealOnly));
    assert_eq!(item.view().status, "Gotowe");
    assert_eq!(item.view().progress, None);
    assert_eq!(item.view().saved_bytes, Some(u64::MAX));
}

#[test]
fn unknown_zero_and_inconsistent_totals_are_indeterminate() {
    for (total, written) in [(None, 20), (Some(0), 0), (Some(15), 20)] {
        let mut item = row(1, "notes.txt", total);
        item.apply(1, Event::BytesWritten(written));
        assert_eq!(item.view().status, "Pobieranie…");
        assert_eq!(
            item.view().progress,
            Some(Progress {
                written,
                total: None,
                percent: None,
            })
        );
    }
}

#[test]
fn cancel_waits_for_worker_acknowledgment_and_terminal_rows_ignore_late_events() {
    let mut item = row(8, "notes.txt", Some(100));
    assert_eq!(
        item.activate(Action::Cancel),
        Some(Intent {
            id: 8,
            action: Action::Cancel,
        })
    );
    item.apply(1, Event::Canceled);
    item.apply(8, Event::BytesWritten(90));
    assert_eq!(item.view().status, "Anulowanie…");
    assert_eq!(item.view().progress, None);
    assert_eq!(item.activate(Action::Cancel), None);

    item.apply(8, Event::Canceled);
    item.apply(8, Event::Saved(100, SavedFileActions::OpenAndReveal));
    item.apply(8, Event::Failed(Failure::Save));
    item.apply(8, Event::BytesWritten(100));
    assert_eq!(item.view().status, "Anulowano");
    assert_eq!(item.view().actions, &[]);
    assert_eq!(item.activate(Action::Open), None);
    assert_eq!(item.activate(Action::ShowInFolder), None);
}

#[test]
fn worker_may_report_a_save_that_won_the_cancel_race() {
    let mut item = row(9, "notes.txt", None);
    assert!(item.activate(Action::Cancel).is_some());
    item.apply(9, Event::Saved(25, SavedFileActions::OpenAndReveal));
    item.apply(9, Event::Canceled);
    assert_eq!(item.view().status, "Gotowe");
    assert_eq!(item.activate(Action::Cancel), None);
    assert_eq!(
        item.activate(Action::Open),
        Some(Intent {
            id: 9,
            action: Action::Open,
        })
    );
}

#[test]
fn reveal_only_policy_and_unavailable_files_disable_stale_actions() {
    let mut item = row(9, "notes.txt", None);
    item.apply(1, Event::Saved(25, SavedFileActions::OpenAndReveal));
    assert_eq!(item.view().status, "Pobieranie…");

    item.apply(9, Event::Saved(25, SavedFileActions::RevealOnly));
    assert_eq!(item.view().actions, &[Action::ShowInFolder]);
    assert_eq!(item.activate(Action::Open), None);
    assert_eq!(
        item.activate(Action::ShowInFolder),
        Some(Intent {
            id: 9,
            action: Action::ShowInFolder,
        })
    );
    item.apply(9, Event::FileUnavailable);
    item.apply(9, Event::Saved(25, SavedFileActions::OpenAndReveal));
    assert_eq!(item.view().status, "Plik niedostępny");
    assert_eq!(item.view().actions, &[]);
    assert_eq!(item.activate(Action::Open), None);
    assert_eq!(item.activate(Action::ShowInFolder), None);
}

#[test]
fn failure_reports_safe_copy_and_never_offers_file_actions() {
    for (failure, message) in [
        (Failure::Save, "Nie udało się zapisać pliku."),
        (
            Failure::Blocked,
            "Plik został zablokowany przez ochronę pobierania.",
        ),
        (Failure::Cleanup, "Nie udało się usunąć pliku częściowego."),
    ] {
        let mut item = row(1, "notes.txt", None);
        assert!(item.activate(Action::Cancel).is_some());
        item.apply(1, Event::Failed(failure));
        item.apply(1, Event::Canceled);
        item.apply(1, Event::Saved(25, SavedFileActions::OpenAndReveal));
        assert_eq!(item.view().status, "Błąd");
        assert_eq!(item.view().detail, Some(message));
        assert_eq!(item.view().actions, &[]);
        assert_eq!(item.activate(Action::Open), None);
        assert_eq!(item.activate(Action::ShowInFolder), None);
    }
}

#[test]
fn saving_without_a_verified_file_adapter_exposes_no_file_actions() {
    let mut item = row(19, "notes.txt", None);
    assert_eq!(item.id(), 19);
    assert!(item.is_active());
    item.apply(19, Event::Saved(2048, SavedFileActions::None));
    assert!(!item.is_active());
    assert_eq!(item.view().saved_bytes, Some(2048));
    assert_eq!(item.view().actions, &[]);
    assert_eq!(item.activate(Action::Open), None);
    assert_eq!(item.activate(Action::ShowInFolder), None);
}
