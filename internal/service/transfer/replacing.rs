//! Downloads into a Mac folder that already has some of the names: the
//! question's answers. Replace and Replace older put the phone's copy in the
//! old one's place only once it is whole, and the old one goes to the Trash
//! (a recorder here, never the user's Trash); Skip leaves a taken item alone;
//! Keep both numbers a different file, as downloads always did.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::Service;
use crate::dto::{
    Cancel, Conflict, DownloadRequest, Error, FileOutcome, Original, Paste, TransferSummary,
};
use crate::service::device;
use crate::service::explorer;
use crate::service::testing::virtual_phone;

struct Taken {
    phone: tempfile::TempDir,
    mac: tempfile::TempDir,
    bin: tempfile::TempDir,
    trashed: Arc<Mutex<Vec<PathBuf>>>,
    transfer: Service,
    storage: u64,
}

/// A phone with notes.txt ("phone notes"), DCIM/a.jpg and DCIM/b.jpg; a Mac
/// folder that has its own notes.txt ("mac") and DCIM/a.jpg ("old").
async fn taken(trash: bool) -> Taken {
    let phone = tempfile::tempdir().unwrap();
    fs::create_dir_all(phone.path().join("DCIM")).unwrap();
    fs::write(phone.path().join("notes.txt"), b"phone notes").unwrap();
    fs::write(phone.path().join("DCIM/a.jpg"), b"phone a").unwrap();
    fs::write(phone.path().join("DCIM/b.jpg"), b"phone b").unwrap();
    let mac = tempfile::tempdir().unwrap();
    fs::create_dir_all(mac.path().join("DCIM")).unwrap();
    fs::write(mac.path().join("notes.txt"), b"mac").unwrap();
    fs::write(mac.path().join("DCIM/a.jpg"), b"old").unwrap();

    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(phone.path()),
    )));
    device.connect().await.unwrap();
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    let bin = tempfile::tempdir().unwrap();
    let trashed = Arc::new(Mutex::new(Vec::new()));
    let mut transfer = Service::new(device, explorer);
    if trash {
        let (into, log) = (bin.path().to_path_buf(), Arc::clone(&trashed));
        transfer = transfer.with_trash(move |path: &Path| {
            log.lock().unwrap().push(path.to_path_buf());
            fs::rename(path, into.join(path.file_name().unwrap()))
        });
    }
    Taken {
        phone,
        mac,
        bin,
        trashed,
        transfer,
        storage,
    }
}

impl Taken {
    fn request(&self, names: &[&str]) -> DownloadRequest {
        DownloadRequest {
            storage_id: self.storage,
            source: "/".into(),
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            destination: self.mac.path().to_string_lossy().into_owned(),
        }
    }

    async fn download(
        &self,
        mode: Paste,
        names: &[&str],
        conflict: Conflict,
    ) -> Result<TransferSummary, Error> {
        self.transfer
            .download_with(mode, &self.request(names), conflict, &Cancel::new(), |_| {})
            .await
    }

    fn on_mac(&self, path: &str) -> Vec<u8> {
        fs::read(self.mac.path().join(path)).unwrap()
    }

    /// Dates a Mac file `seconds` from now (negative: in the past).
    fn date_mac(&self, path: &str, seconds: i64) {
        let when = if seconds >= 0 {
            SystemTime::now() + Duration::from_secs(seconds.unsigned_abs())
        } else {
            SystemTime::now() - Duration::from_secs(seconds.unsigned_abs())
        };
        fs::File::options()
            .write(true)
            .open(self.mac.path().join(path))
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    /// Nothing the app works in is left in the Mac folder.
    fn leaves_nothing_hidden(&self) {
        let hidden: Vec<String> = fs::read_dir(self.mac.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(hidden.is_empty(), "{hidden:?}");
    }
}

#[tokio::test]
async fn the_question_is_only_about_names_holding_something_different() {
    let t = taken(true).await;
    // The very file already there is no question.
    fs::write(t.mac.path().join("same.txt"), b"same").unwrap();
    fs::write(t.phone.path().join("same.txt"), b"same").unwrap();
    let phone_date = fs::metadata(t.phone.path().join("same.txt"))
        .unwrap()
        .modified()
        .unwrap();
    fs::File::options()
        .write(true)
        .open(t.mac.path().join("same.txt"))
        .unwrap()
        .set_modified(phone_date)
        .unwrap();
    fs::write(t.phone.path().join("new.txt"), b"new").unwrap();

    let taken = t
        .transfer
        .taken_on_mac(&t.request(&["notes.txt", "DCIM", "same.txt", "new.txt"]))
        .await
        .unwrap();

    assert_eq!(
        taken,
        ["notes.txt", "DCIM"],
        "a different file and a folder"
    );
}

#[tokio::test]
async fn replace_swaps_each_copy_in_once_whole_and_trashes_the_old_ones_together() {
    let t = taken(true).await;

    let summary = t
        .download(Paste::Copy, &["notes.txt", "DCIM"], Conflict::Replace)
        .await
        .unwrap();

    assert_eq!(t.on_mac("notes.txt"), b"phone notes");
    assert_eq!(
        t.on_mac("DCIM/a.jpg"),
        b"phone a",
        "inside the merged folder too"
    );
    assert_eq!(t.on_mac("DCIM/b.jpg"), b"phone b");
    assert_eq!((summary.replaced, summary.files), (2, 3));
    let trashed = t.trashed.lock().unwrap().clone();
    assert_eq!(trashed.len(), 1, "one Trash call for every old file");
    let bin: Vec<_> = fs::read_dir(t.bin.path()).unwrap().collect();
    assert_eq!(bin.len(), 1);
    let kept = bin[0].as_ref().unwrap().path();
    assert_eq!(fs::read(kept.join("notes.txt")).unwrap(), b"mac");
    assert_eq!(fs::read(kept.join("DCIM/a.jpg")).unwrap(), b"old");
    t.leaves_nothing_hidden();
    assert_eq!(summary.arrival.unwrap().there, 3);
}

#[tokio::test]
async fn replace_older_replaces_only_a_file_newer_on_the_phone() {
    let t = taken(true).await;
    t.date_mac("notes.txt", 3_600);
    t.date_mac("DCIM/a.jpg", -3_600);

    let summary = t
        .download(Paste::Copy, &["notes.txt", "DCIM"], Conflict::ReplaceOlder)
        .await
        .unwrap();

    assert_eq!(t.on_mac("notes.txt"), b"mac", "newer on the Mac: stays");
    assert_eq!(
        t.on_mac("DCIM/a.jpg"),
        b"phone a",
        "older on the Mac: replaced"
    );
    assert_eq!(t.on_mac("DCIM/b.jpg"), b"phone b");
    assert_eq!((summary.replaced, summary.skipped), (1, 1));
    t.leaves_nothing_hidden();
}

#[tokio::test]
async fn skip_leaves_a_taken_item_alone_and_keep_both_numbers_it() {
    let t = taken(true).await;
    let skipped = t
        .download(Paste::Copy, &["notes.txt", "DCIM"], Conflict::Skip)
        .await
        .unwrap();
    assert_eq!(t.on_mac("notes.txt"), b"mac");
    assert!(
        !t.mac.path().join("DCIM/b.jpg").exists(),
        "a taken folder is left alone, whole"
    );
    assert_eq!((skipped.skipped, skipped.files), (2, 0));
    assert!(t.trashed.lock().unwrap().is_empty());

    let both = t
        .download(Paste::Copy, &["notes.txt"], Conflict::KeepBoth)
        .await
        .unwrap();
    assert_eq!(t.on_mac("notes.txt"), b"mac");
    assert_eq!(t.on_mac("notes (1).txt"), b"phone notes");
    assert_eq!(both.renamed, 1);
}

#[tokio::test]
async fn nothing_of_one_kind_replaces_the_other() {
    let t = taken(true).await;
    // A folder on the Mac where the phone has the file notes.txt.
    fs::remove_file(t.mac.path().join("notes.txt")).unwrap();
    fs::create_dir(t.mac.path().join("notes.txt")).unwrap();
    // A file on the Mac where the phone has the folder DCIM.
    fs::remove_dir_all(t.mac.path().join("DCIM")).unwrap();
    fs::write(t.mac.path().join("DCIM"), b"a file").unwrap();

    let summary = t
        .download(Paste::Copy, &["notes.txt", "DCIM"], Conflict::Replace)
        .await
        .unwrap();

    assert!(t.mac.path().join("notes.txt").is_dir());
    assert_eq!(t.on_mac("DCIM"), b"a file");
    assert_eq!(summary.skipped, 2);
    assert!(t.trashed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn without_a_trash_nothing_is_replaced_or_copied() {
    let t = taken(false).await;

    let result = t
        .download(Paste::Copy, &["notes.txt", "DCIM"], Conflict::Replace)
        .await;

    assert_eq!(result.err(), Some(Error::Unsupported));
    assert_eq!(t.on_mac("notes.txt"), b"mac");
    assert!(
        !t.mac.path().join("DCIM/b.jpg").exists(),
        "nothing copied first"
    );
    t.leaves_nothing_hidden();
    // With nothing to replace, Replace needs no Trash.
    fs::remove_file(t.mac.path().join("notes.txt")).unwrap();
    t.download(Paste::Copy, &["notes.txt"], Conflict::Replace)
        .await
        .unwrap();
    assert_eq!(t.on_mac("notes.txt"), b"phone notes");
}

#[tokio::test]
async fn a_move_removes_an_original_only_once_its_copy_is_in_place() {
    let t = taken(true).await;
    // The old DCIM/a.jpg can't be moved aside: its folder is read-only.
    let dcim = t.mac.path().join("DCIM");
    let mut locked = fs::metadata(&dcim).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut locked, 0o555);
    fs::set_permissions(&dcim, locked).unwrap();

    let summary = t
        .download(Paste::Move, &["notes.txt", "DCIM"], Conflict::Replace)
        .await
        .unwrap();

    let mut open = fs::metadata(&dcim).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut open, 0o755);
    fs::set_permissions(&dcim, open).unwrap();
    assert_eq!(t.on_mac("notes.txt"), b"phone notes");
    assert!(
        !t.phone.path().join("notes.txt").exists(),
        "in place: the original goes"
    );
    assert_eq!(
        t.on_mac("DCIM/a.jpg"),
        b"old",
        "the swap failed: the old one stays"
    );
    assert!(
        t.phone.path().join("DCIM/a.jpg").exists(),
        "and so does the phone's original"
    );
    assert!(!summary.failures.is_empty());
    assert_eq!(summary.replaced, 1);
}

/// When the phone's `path` was last changed, as its disk says.
fn phone_date(t: &Taken, path: &str) -> SystemTime {
    fs::metadata(t.phone.path().join(path))
        .unwrap()
        .modified()
        .unwrap()
}

fn set_mac_date(t: &Taken, path: &str, when: SystemTime) {
    fs::File::options()
        .write(true)
        .open(t.mac.path().join(path))
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[tokio::test]
async fn replace_older_keeps_a_file_only_as_new_within_two_seconds() {
    let t = taken(true).await;
    // A different file, a second older on the Mac: as new, not replaced.
    set_mac_date(
        &t,
        "notes.txt",
        phone_date(&t, "notes.txt") - Duration::from_secs(1),
    );

    let summary = t
        .download(Paste::Copy, &["notes.txt"], Conflict::ReplaceOlder)
        .await
        .unwrap();

    assert_eq!(t.on_mac("notes.txt"), b"mac");
    assert_eq!((summary.replaced, summary.skipped), (0, 1));
}

#[tokio::test]
async fn the_very_file_already_there_is_neither_copied_again_nor_trashed() {
    let t = taken(true).await;
    fs::write(t.mac.path().join("notes.txt"), b"phone notes").unwrap();
    set_mac_date(&t, "notes.txt", phone_date(&t, "notes.txt"));

    let summary = t
        .download(Paste::Copy, &["notes.txt"], Conflict::Replace)
        .await
        .unwrap();

    assert_eq!((summary.replaced, summary.already_there), (0, 1));
    assert!(t.trashed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_copy_that_did_not_arrive_leaves_the_old_file_with_one_failure() {
    let t = taken(true).await;
    // The phone won't read this one.
    let unreadable = t.phone.path().join("notes.txt");
    let mut locked = fs::metadata(&unreadable).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut locked, 0o000);
    fs::set_permissions(&unreadable, locked).unwrap();

    let result = t
        .download(Paste::Copy, &["notes.txt", "DCIM"], Conflict::Replace)
        .await;

    let mut open = fs::metadata(&unreadable).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut open, 0o644);
    fs::set_permissions(&unreadable, open).unwrap();
    let summary = result.unwrap();
    assert_eq!(t.on_mac("notes.txt"), b"mac", "the old one stays");
    assert_eq!(summary.failures.len(), 1, "{:?}", summary.failures);
    assert_eq!(t.on_mac("DCIM/a.jpg"), b"phone a", "the rest go on");
}

#[tokio::test]
async fn a_swap_never_sets_the_old_file_aside_over_something_already_there() {
    let dir = tempfile::tempdir().unwrap();
    let (staged, old, aside) = (
        dir.path().join("staged"),
        dir.path().join("old"),
        dir.path().join("aside/old"),
    );
    fs::write(&staged, b"new").unwrap();
    fs::write(&old, b"old").unwrap();
    fs::create_dir_all(dir.path().join("aside")).unwrap();
    fs::write(&aside, b"set aside before").unwrap();

    let result = super::mirror::swap_in(&staged, &old, &aside, &old).await;

    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&aside).unwrap(), b"set aside before", "not lost");
    assert_eq!(fs::read(&old).unwrap(), b"old", "left where it was");
    assert_eq!(fs::read(&staged).unwrap(), b"new");
}

#[tokio::test]
async fn a_name_selected_twice_replaces_its_mac_file_once() {
    let t = taken(true).await;

    let summary = t
        .download(Paste::Copy, &["notes.txt", "notes.txt"], Conflict::Replace)
        .await
        .unwrap();

    assert_eq!(t.on_mac("notes.txt"), b"phone notes");
    assert_eq!(summary.replaced, 1);
    let bin: Vec<_> = fs::read_dir(t.bin.path()).unwrap().collect();
    let kept = bin[0].as_ref().unwrap().path();
    assert_eq!(
        fs::read(kept.join("notes.txt")).unwrap(),
        b"mac",
        "the original kept"
    );
    t.leaves_nothing_hidden();
}

/// The report's lines as (path, what happened), in order.
fn lines(summary: &TransferSummary) -> Vec<(&str, FileOutcome)> {
    summary
        .report
        .iter()
        .map(|file| (file.path.as_str(), file.outcome.clone()))
        .collect()
}

#[tokio::test]
async fn the_report_names_each_file_as_the_mac_has_it_and_what_became_of_it() {
    let t = taken(true).await;
    let copied = FileOutcome::Copied { checked: true };

    let kept_both = t
        .download(Paste::Copy, &["notes.txt"], Conflict::KeepBoth)
        .await
        .unwrap();
    assert_eq!(lines(&kept_both), [("notes (1).txt", copied.clone())]);
    assert_eq!(
        kept_both.report[0].from.as_deref(),
        Some("notes.txt"),
        "saved under another name"
    );

    let replaced = t
        .download(Paste::Copy, &["notes.txt", "DCIM"], Conflict::Replace)
        .await
        .unwrap();
    assert_eq!(
        lines(&replaced),
        [
            ("notes.txt", FileOutcome::Replaced),
            ("DCIM/b.jpg", copied),
            ("DCIM/a.jpg", FileOutcome::Replaced),
        ],
        "in the order copied"
    );
    assert_eq!(replaced.report_left_out, 0);
}

#[tokio::test]
async fn a_skipped_item_is_listed_as_skipped() {
    let t = taken(true).await;
    fs::write(t.phone.path().join("new.txt"), b"new").unwrap();

    let summary = t
        .download(Paste::Copy, &["notes.txt", "new.txt"], Conflict::Skip)
        .await
        .unwrap();

    assert_eq!(
        lines(&summary),
        [
            ("new.txt", FileOutcome::Copied { checked: true }),
            ("notes.txt", FileOutcome::Skipped),
        ]
    );
}

#[tokio::test]
async fn a_moves_report_says_each_original_was_removed() {
    let t = taken(true).await;

    let summary = t
        .download(Paste::Move, &["DCIM"], Conflict::KeepBoth)
        .await
        .unwrap();

    assert!(!summary.report.is_empty());
    assert!(
        summary
            .report
            .iter()
            .all(|file| file.original == Some(Original::Removed)),
        "{:?}",
        summary.report
    );
    let copy = t
        .download(Paste::Copy, &["notes.txt"], Conflict::KeepBoth)
        .await
        .unwrap();
    assert_eq!(
        copy.report[0].original, None,
        "a copy has no originals to tell of"
    );
}

#[tokio::test]
async fn an_original_whose_copy_could_not_be_checked_is_reported_kept_and_why() {
    use super::relay::Nowhere;
    use super::service::Outcome;
    use crate::dto::Why;
    let t = taken(true).await;
    let request = t.request(&["DCIM"]);
    let mut down = t
        .transfer
        .download_run_onward::<Nowhere>(true, &request, &Cancel::new(), &|_| {}, None)
        .await
        .unwrap();
    // As a phone that gave no size would leave it.
    let unchecked = down
        .outcomes
        .iter()
        .rposition(|outcome| *outcome == Outcome::Verified)
        .unwrap();
    down.outcomes[unchecked] = Outcome::Unchecked;
    let mut fates = vec![None; down.plan.steps.len()];

    let _ = t
        .transfer
        .remove_originals(
            &down.storage,
            &request,
            &down.plan,
            &down.outcomes,
            &Cancel::new(),
            &|_| {},
            &mut fates,
        )
        .await;

    assert_eq!(fates[unchecked], Some(Original::Kept(Why::Unchecked)));
    assert!(fates.contains(&Some(Original::Removed)), "the others went");
}
