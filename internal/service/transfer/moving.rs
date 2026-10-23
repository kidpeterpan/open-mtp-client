//! Moving files from the phone to the Mac: copy, check the copy, then remove
//! the original, and only the original of a copy that was checked.
//!
//! Android deletes a folder with everything in it, so these tests spend their
//! effort on what must *not* go: a file that did not copy, one that changed
//! after it was copied, a photo taken while the folder was being moved.

use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use super::Service;
use crate::dto::{
    Cancel, DownloadRequest, Error, Kept, Paste, Removal, TransferProgress, TransferSummary, Why,
};
use crate::service::device;
use crate::service::explorer;
use crate::service::testing::virtual_phone;

struct Move {
    phone: tempfile::TempDir,
    mac: tempfile::TempDir,
    transfer: Service,
    explorer: Arc<explorer::Service>,
    storage: u64,
    serial: String,
}

/// A phone with DCIM/Camera/{a.jpg, b.jpg}, DCIM/Other/c.jpg, an empty Music
/// and notes.txt, and an empty Mac folder.
async fn mover() -> Move {
    mover_with(|_| {}).await
}

async fn mover_with(configure: impl FnOnce(&mut device::VirtualPhone)) -> Move {
    let phone = tempfile::tempdir().unwrap();
    let root = phone.path();
    fs::create_dir_all(root.join("DCIM/Camera")).unwrap();
    fs::create_dir_all(root.join("DCIM/Other")).unwrap();
    fs::create_dir_all(root.join("Music")).unwrap();
    fs::write(root.join("DCIM/Camera/a.jpg"), b"aaaa").unwrap();
    fs::write(root.join("DCIM/Camera/b.jpg"), b"bb").unwrap();
    fs::write(root.join("DCIM/Other/c.jpg"), b"ccc").unwrap();
    fs::write(root.join("notes.txt"), b"hello").unwrap();

    let mut config = virtual_phone(root);
    configure(&mut config);
    let device = Arc::new(device::Service::new(device::Source::Virtual(config)));
    let serial = device.connect().await.unwrap().serial_number;
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    Move {
        phone,
        mac: tempfile::tempdir().unwrap(),
        transfer: Service::new(device, Arc::clone(&explorer)),
        explorer,
        storage,
        serial,
    }
}

impl Move {
    fn request(&self, source: &str, names: &[&str]) -> DownloadRequest {
        DownloadRequest {
            storage_id: self.storage,
            source: source.into(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
            destination: self.mac.path().to_string_lossy().into_owned(),
        }
    }

    async fn run(
        &self,
        mode: Paste,
        source: &str,
        names: &[&str],
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        self.transfer
            .download_as(mode, &self.request(source, names), &Cancel::new(), progress)
            .await
    }

    async fn moved(&self, source: &str, names: &[&str]) -> TransferSummary {
        self.run(Paste::Move, source, names, |_| {}).await.unwrap()
    }

    fn on_phone(&self, path: &str) -> bool {
        self.phone.path().join(path).exists()
    }

    fn on_mac(&self, path: &str) -> Vec<u8> {
        fs::read(self.mac.path().join(path)).unwrap()
    }

    /// Every file and folder left on the phone, relative to its root.
    fn phone_tree(&self) -> BTreeSet<String> {
        fn walk(dir: &std::path::Path, base: &std::path::Path, into: &mut BTreeSet<String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                into.insert(
                    path.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
                if path.is_dir() {
                    walk(&path, base, into);
                }
            }
        }
        let mut found = BTreeSet::new();
        walk(self.phone.path(), self.phone.path(), &mut found);
        found
    }

    /// Runs `once` the first time the move starts removing originals: the
    /// moment when everything is copied and checked, and nothing is removed yet.
    fn at_removal(
        &self,
        once: impl Fn() + Send + Sync + 'static,
    ) -> impl Fn(TransferProgress) + Send + Sync {
        let fired = AtomicBool::new(false);
        move |progress: TransferProgress| {
            if progress.removing && !fired.swap(true, Ordering::SeqCst) {
                once();
            }
        }
    }

    fn rescan(&self) {
        mtp_rs::rescan_virtual_device(&self.serial);
    }
}

fn names(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|n| (*n).to_owned()).collect()
}

fn removal(summary: &TransferSummary) -> &Removal {
    summary.removal.as_ref().expect("a move reports a removal")
}

#[tokio::test]
async fn a_moved_file_is_on_the_mac_and_gone_from_the_phone() {
    let s = mover().await;

    let summary = s.moved("/", &["notes.txt"]).await;

    assert_eq!(s.on_mac("notes.txt"), b"hello");
    assert!(!s.on_phone("notes.txt"));
    assert_eq!(removal(&summary).removed, 1);
    assert!(removal(&summary).kept.is_empty());
    assert!(removal(&summary).left.is_empty());
    assert_eq!(removal(&summary).stopped, None);
    assert_eq!(summary.files, 1);
}

#[tokio::test]
async fn a_copy_removes_nothing_and_reports_no_removal() {
    let s = mover().await;

    let summary = s
        .run(Paste::Copy, "/", &["notes.txt"], |_| {})
        .await
        .unwrap();

    assert!(s.on_phone("notes.txt"));
    assert_eq!(summary.removal, None);
}

#[tokio::test]
async fn a_moved_folder_takes_its_files_and_then_its_folders() {
    let s = mover().await;

    let summary = s.moved("/", &["DCIM", "notes.txt"]).await;

    assert_eq!(s.on_mac("DCIM/Camera/a.jpg"), b"aaaa");
    assert_eq!(s.on_mac("DCIM/Camera/b.jpg"), b"bb");
    assert_eq!(s.on_mac("DCIM/Other/c.jpg"), b"ccc");
    assert_eq!(
        s.phone_tree(),
        names(&["Music"]),
        "only what wasn't selected is left"
    );
    assert_eq!(removal(&summary).removed, 4);
    assert!(removal(&summary).left.is_empty());
}

#[tokio::test]
async fn an_empty_folder_moves_too() {
    let s = mover().await;

    let summary = s.moved("/", &["Music"]).await;

    assert!(s.mac.path().join("Music").is_dir());
    assert!(!s.on_phone("Music"));
    assert_eq!(removal(&summary).removed, 0, "no files, one folder");
    assert!(removal(&summary).left.is_empty());
}

#[tokio::test]
async fn a_file_that_did_not_copy_stays_with_its_folders_and_the_rest_goes() {
    let s = mover().await;
    // The Mac refuses new files in Camera.
    let camera = s.mac.path().join("DCIM/Camera");
    fs::create_dir_all(&camera).unwrap();
    set_mode(&camera, 0o555);

    let summary = s.moved("/", &["DCIM"]).await;
    set_mode(&camera, 0o755);

    assert_eq!(summary.failures.len(), 2, "a.jpg and b.jpg");
    assert_eq!(
        s.phone_tree(),
        names(&[
            "DCIM",
            "DCIM/Camera",
            "DCIM/Camera/a.jpg",
            "DCIM/Camera/b.jpg",
            "Music",
            "notes.txt"
        ]),
        "Other and c.jpg moved; Camera and DCIM stay for what did not"
    );
    assert_eq!(s.on_mac("DCIM/Other/c.jpg"), b"ccc");
    assert_eq!(removal(&summary).removed, 1);
    assert_eq!(removal(&summary).left, ["DCIM"]);
    assert!(
        removal(&summary).kept.is_empty(),
        "failed copies are in `failures`, not listed twice"
    );
}

fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[tokio::test]
async fn a_photo_taken_while_the_folder_moves_is_never_removed_with_it() {
    let s = mover().await;
    let (camera, serial) = (s.phone.path().join("DCIM/Camera"), s.serial.clone());
    let progress = s.at_removal(move || {
        fs::write(camera.join("new.jpg"), b"just taken").unwrap();
        mtp_rs::rescan_virtual_device(&serial);
    });

    let summary = s.run(Paste::Move, "/", &["DCIM"], progress).await.unwrap();

    assert_eq!(
        fs::read(s.phone.path().join("DCIM/Camera/new.jpg")).unwrap(),
        b"just taken"
    );
    assert!(!s.on_phone("DCIM/Camera/a.jpg") && !s.on_phone("DCIM/Camera/b.jpg"));
    assert!(
        !s.on_phone("DCIM/Other"),
        "what was planned and is empty goes"
    );
    assert_eq!(removal(&summary).removed, 3);
    assert_eq!(
        removal(&summary).kept,
        [Kept {
            label: "DCIM/Camera".into(),
            why: Why::NotEmpty
        }]
    );
    assert_eq!(removal(&summary).left, ["DCIM"]);
}

#[tokio::test]
async fn a_file_the_phone_will_not_describe_keeps_its_folder_and_itself() {
    // The phone lists a handle for it but refuses to say what it is, so the
    // plan (which reads descriptions) never sees it and it is never copied. A
    // folder holding it must not be removed: Android would take it along.
    let s = mover().await;
    fs::write(
        s.phone.path().join("DCIM/Other/secret.jpg"),
        b"never copied",
    )
    .unwrap();
    s.rescan();
    let secret = s
        .explorer
        .list(s.storage, "/DCIM/Other", true)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.name == "secret.jpg")
        .unwrap();
    mtp_rs::force_object_info_error(&s.serial, mtp_rs::mtp::ObjectHandle(secret.id), None);

    let summary = s.moved("/", &["DCIM"]).await;

    assert_eq!(
        fs::read(s.phone.path().join("DCIM/Other/secret.jpg")).unwrap(),
        b"never copied",
        "the file the plan never saw is still there"
    );
    assert!(
        !s.on_phone("DCIM/Other/c.jpg"),
        "what was copied and checked went"
    );
    assert!(
        removal(&summary).kept.contains(&Kept {
            label: "DCIM/Other".into(),
            why: Why::NotEmpty
        }),
        "{:?}",
        removal(&summary).kept
    );
    assert_eq!(removal(&summary).left, ["DCIM"]);
    assert!(!s.mac.path().join("DCIM/Other/secret.jpg").exists());
}

#[tokio::test]
async fn a_file_that_changed_after_it_was_copied_stays() {
    let s = mover().await;
    let (notes, serial) = (s.phone.path().join("notes.txt"), s.serial.clone());
    let progress = s.at_removal(move || {
        fs::write(&notes, b"edited on the phone since").unwrap();
        mtp_rs::rescan_virtual_device(&serial);
    });

    let summary = s
        .run(Paste::Move, "/", &["notes.txt"], progress)
        .await
        .unwrap();

    assert_eq!(
        fs::read(s.phone.path().join("notes.txt")).unwrap(),
        b"edited on the phone since"
    );
    assert_eq!(
        s.on_mac("notes.txt"),
        b"hello",
        "the copy is what was copied"
    );
    assert_eq!(removal(&summary).removed, 0);
    assert_eq!(
        removal(&summary).kept,
        [Kept {
            label: "notes.txt".into(),
            why: Why::Changed
        }]
    );
    assert_eq!(removal(&summary).left, ["notes.txt"]);
}

#[tokio::test]
async fn a_file_the_phone_re_keyed_meanwhile_stays_and_a_retry_removes_it() {
    let s = mover().await;
    let serial = s.serial.clone();
    let progress = s.at_removal(move || {
        mtp_rs::rekey_virtual_object(&serial, std::path::Path::new("notes.txt")).unwrap();
    });

    let first = s
        .run(Paste::Move, "/", &["notes.txt"], progress)
        .await
        .unwrap();

    assert!(s.on_phone("notes.txt"), "an old handle removes nothing");
    assert_eq!(removal(&first).removed, 0);
    assert_eq!(
        removal(&first).kept,
        [Kept {
            label: "notes.txt".into(),
            why: Why::Changed
        }],
        "an old handle is a file that is not what was copied, not a phone saying no"
    );
    assert_eq!(removal(&first).left, ["notes.txt"]);

    // The retry plans again with fresh handles, finds the copy already there
    // (same size and date), and removes the original without copying twice.
    let again = s.moved("/", &["notes.txt"]).await;

    assert!(!s.on_phone("notes.txt"));
    assert_eq!(again.already_there, 1);
    assert_eq!(removal(&again).removed, 1);
    let on_mac: Vec<_> = fs::read_dir(s.mac.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(on_mac, ["notes.txt"], "no `notes (1).txt`");
}

#[tokio::test]
async fn a_move_after_a_copy_removes_the_originals_without_copying_again() {
    let s = mover().await;
    s.run(Paste::Copy, "/DCIM/Camera", &["a.jpg", "b.jpg"], |_| {})
        .await
        .unwrap();

    let summary = s.moved("/DCIM/Camera", &["a.jpg", "b.jpg"]).await;

    assert_eq!((summary.files, summary.already_there), (0, 2));
    assert_eq!(removal(&summary).removed, 2);
    assert!(!s.on_phone("DCIM/Camera/a.jpg") && !s.on_phone("DCIM/Camera/b.jpg"));
    assert_eq!(s.on_mac("a.jpg"), b"aaaa");
    assert!(!s.mac.path().join("a (1).jpg").exists());
}

#[tokio::test]
async fn a_different_file_with_the_same_name_on_the_mac_does_not_make_the_original_go_early() {
    let s = mover().await;
    fs::write(s.mac.path().join("notes.txt"), b"something else entirely").unwrap();

    let summary = s.moved("/", &["notes.txt"]).await;

    // Kept under a number, as any copy would be, and verified as itself.
    assert_eq!(s.on_mac("notes.txt"), b"something else entirely");
    assert_eq!(s.on_mac("notes (1).txt"), b"hello");
    assert!(!s.on_phone("notes.txt"));
    assert_eq!(removal(&summary).removed, 1);
}

#[tokio::test]
async fn cancelling_while_originals_are_removed_leaves_the_rest_where_they_are() {
    let s = mover().await;
    let cancel = Cancel::new();
    let removed = AtomicUsize::new(0);

    let summary = s
        .transfer
        .download_as(
            Paste::Move,
            &s.request("/", &["DCIM"]),
            &cancel,
            |progress| {
                if progress.removing && progress.files_done == 1 {
                    removed.fetch_add(1, Ordering::SeqCst);
                    cancel.cancel();
                }
            },
        )
        .await
        .unwrap();

    assert_eq!(removal(&summary).removed, 1);
    assert_eq!(removal(&summary).stopped, Some(Error::Cancelled));
    assert_eq!(removal(&summary).left, ["DCIM"]);
    // Every copy is on the Mac, and every original that is left is whole.
    assert_eq!(s.on_mac("DCIM/Camera/a.jpg"), b"aaaa");
    assert_eq!(s.on_mac("DCIM/Camera/b.jpg"), b"bb");
    assert_eq!(s.on_mac("DCIM/Other/c.jpg"), b"ccc");
    let left = s
        .phone_tree()
        .iter()
        .filter(|p| p.ends_with(".jpg"))
        .count();
    assert_eq!(left, 2, "one of three removed, then it stopped");
}

#[tokio::test]
async fn a_phone_that_stops_answering_while_removing_ends_the_removal_not_the_move() {
    let s = mover().await;
    let serial = s.serial.clone();
    let progress = s.at_removal(move || {
        mtp_rs::force_operation_wedge(&serial);
    });

    let summary = s
        .run(Paste::Move, "/", &["notes.txt"], progress)
        .await
        .unwrap();

    assert!(s.on_phone("notes.txt"), "nothing was removed");
    assert_eq!(
        s.on_mac("notes.txt"),
        b"hello",
        "the copy is safe on the Mac"
    );
    assert_eq!(removal(&summary).removed, 0);
    assert_eq!(removal(&summary).stopped, Some(Error::Disconnected));
    assert_eq!(removal(&summary).left, ["notes.txt"]);
}

#[tokio::test]
async fn a_phone_that_refuses_to_remove_keeps_every_original_with_its_reason() {
    let s = mover_with(|phone| phone.read_only = true).await;

    let summary = s.moved("/DCIM/Camera", &["a.jpg", "b.jpg"]).await;

    assert!(s.on_phone("DCIM/Camera/a.jpg") && s.on_phone("DCIM/Camera/b.jpg"));
    assert_eq!(s.on_mac("a.jpg"), b"aaaa");
    assert_eq!(removal(&summary).removed, 0);
    assert_eq!(removal(&summary).kept.len(), 2);
    assert!(
        removal(&summary)
            .kept
            .iter()
            .all(|kept| matches!(kept.why, Why::Refused(_))),
        "{:?}",
        removal(&summary).kept
    );
    assert_eq!(removal(&summary).left, ["a.jpg", "b.jpg"]);
}

#[tokio::test]
async fn progress_says_it_is_removing_and_counts_the_originals() {
    let s = mover().await;
    let seen = std::sync::Mutex::new(Vec::new());

    s.run(Paste::Move, "/DCIM/Camera", &["a.jpg", "b.jpg"], |p| {
        if p.removing {
            seen.lock().unwrap().push((p.files_done, p.files_total));
        }
    })
    .await
    .unwrap();

    assert_eq!(*seen.lock().unwrap(), [(0, 2), (1, 2), (2, 2)]);
}

#[tokio::test]
async fn nothing_is_removed_when_nothing_could_be_copied() {
    let s = mover().await;
    // The Mac refuses every new file in the destination.
    set_mode(s.mac.path(), 0o555);

    let result = s
        .run(Paste::Move, "/DCIM/Camera", &["a.jpg", "b.jpg"], |_| {})
        .await;
    set_mode(s.mac.path(), 0o755);

    let summary = result.unwrap();
    assert_eq!(summary.failures.len(), 2);
    assert_eq!(summary.files, 0);
    assert_eq!(removal(&summary).removed, 0);
    assert_eq!(removal(&summary).left, ["a.jpg", "b.jpg"]);
    assert!(s.on_phone("DCIM/Camera/a.jpg") && s.on_phone("DCIM/Camera/b.jpg"));
}

#[tokio::test]
async fn one_shared_failure_of_a_one_file_move_is_the_error_and_removes_nothing() {
    let s = mover().await;
    set_mode(s.mac.path(), 0o555);

    let result = s.run(Paste::Move, "/", &["notes.txt"], |_| {}).await;
    set_mode(s.mac.path(), 0o755);

    assert!(
        matches!(result, Err(Error::FolderAccessDenied(_))),
        "{result:?}"
    );
    assert!(s.on_phone("notes.txt"));
}

#[tokio::test]
async fn the_panes_memory_of_the_folder_is_kept_true_without_reading_it_again() {
    let s = mover().await;
    s.explorer
        .list(s.storage, "/DCIM/Camera", true)
        .await
        .unwrap();
    let reads = s.explorer.full_reads();

    s.moved("/DCIM/Camera", &["a.jpg"]).await;
    let shown: Vec<_> = s
        .explorer
        .list(s.storage, "/DCIM/Camera", true)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();

    assert_eq!(shown, ["b.jpg"]);
    assert_eq!(
        s.explorer.full_reads(),
        reads,
        "served from what was remembered"
    );
}

#[tokio::test]
async fn a_removed_folder_is_gone_from_the_explorer_too() {
    let s = mover().await;
    s.explorer
        .list(s.storage, "/DCIM/Camera", true)
        .await
        .unwrap();

    s.moved("/", &["DCIM"]).await;

    assert!(matches!(
        s.explorer.list(s.storage, "/DCIM/Camera", true).await,
        Err(Error::PathNotFound(_))
    ));
    let root: Vec<_> = s
        .explorer
        .list(s.storage, "/", true)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(
        root,
        ["Music", "notes.txt"],
        "DCIM was the only thing moved"
    );
}

#[tokio::test]
async fn moving_a_few_files_out_of_a_big_folder_does_not_read_it_again() {
    let s = mover().await;
    fs::create_dir_all(s.phone.path().join("DCIM/Big")).unwrap();
    for i in 0..5_000 {
        fs::write(
            s.phone.path().join(format!("DCIM/Big/IMG_{i:05}.jpg")),
            b"x",
        )
        .unwrap();
    }
    s.rescan();
    assert_eq!(
        s.explorer
            .list(s.storage, "/DCIM/Big", true)
            .await
            .unwrap()
            .len(),
        5_000
    );
    let reads = s.explorer.full_reads();
    let heard = s.transfer.device.activity().times_heard();

    let summary = s
        .moved(
            "/DCIM/Big",
            &["IMG_00001.jpg", "IMG_00002.jpg", "IMG_00003.jpg"],
        )
        .await;
    let asks = s.transfer.device.activity().times_heard() - heard;
    let after = s.explorer.list(s.storage, "/DCIM/Big", true).await.unwrap();

    assert_eq!(removal(&summary).removed, 3);
    assert_eq!(after.len(), 4_997);
    assert_eq!(
        s.explorer.full_reads(),
        reads,
        "the 5,000 files were not described again"
    );
    // The phone answered about twenty-five times, to find the three files and
    // check them, never once per file of the 5,000. A move that costs a round
    // trip per file would be 5,000 or more. (Counted, not timed: a stopwatch
    // here only measured how slow the machine was.)
    assert!(asks < 500, "{asks} answers from the phone for three files");
}

#[tokio::test]
async fn the_dates_of_the_copies_are_the_phones() {
    let s = mover().await;
    let when = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    fs::File::options()
        .write(true)
        .open(s.phone.path().join("notes.txt"))
        .unwrap()
        .set_modified(when)
        .unwrap();
    s.rescan();

    s.moved("/", &["notes.txt"]).await;

    let copy = fs::metadata(s.mac.path().join("notes.txt")).unwrap();
    assert_eq!(copy.modified().unwrap(), when);
}
