//! Copying and moving inside a phone that cannot do it by itself, by way of the
//! Mac. The virtual phone can do both natively, so these call the fallback
//! directly; what they pin is what ends up where, that nothing is overwritten
//! or lost, and that the scratch folder never outlives the paste.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use super::Service;
use crate::dto::{
    Cancel, Direction, Error, Kept, Paste, PasteRequest, TransferProgress, TransferSummary, Why,
};
use crate::service::device;
use crate::service::explorer;
use crate::service::testing::virtual_phone;

struct Via {
    phone: tempfile::TempDir,
    /// Where scratch folders are made, so a test can see they are gone.
    base: tempfile::TempDir,
    transfer: Service,
    explorer: Arc<explorer::Service>,
    storage: u64,
    serial: String,
}

/// A phone with DCIM/Camera/{a.jpg, b.jpg}, DCIM/Other/c.jpg, empty Music and
/// Download folders, and notes.txt.
async fn via() -> Via {
    via_with(|_| {}).await
}

async fn via_with(configure: impl FnOnce(&mut device::VirtualPhone)) -> Via {
    let phone = tempfile::tempdir().unwrap();
    let root = phone.path();
    fs::create_dir_all(root.join("DCIM/Camera")).unwrap();
    fs::create_dir_all(root.join("DCIM/Other")).unwrap();
    fs::create_dir_all(root.join("Music")).unwrap();
    fs::create_dir_all(root.join("Download")).unwrap();
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
    Via {
        phone,
        base: tempfile::tempdir().unwrap(),
        transfer: Service::new(device, Arc::clone(&explorer)),
        explorer,
        storage,
        serial,
    }
}

impl Via {
    fn request(&self, mode: Paste, from: &str, names: &[&str], to: &str) -> PasteRequest {
        PasteRequest {
            mode,
            from_storage: self.storage,
            from: from.into(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
            to_storage: self.storage,
            to: to.into(),
        }
    }

    async fn paste_with(
        &self,
        mode: Paste,
        from: &str,
        names: &[&str],
        to: &str,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        self.transfer
            .through_mac_in(
                self.base.path(),
                &self.request(mode, from, names, to),
                false,
                cancel,
                &progress,
            )
            .await
    }

    async fn paste(
        &self,
        mode: Paste,
        from: &str,
        names: &[&str],
        to: &str,
    ) -> Result<TransferSummary, Error> {
        self.paste_with(mode, from, names, to, &Cancel::new(), |_| {})
            .await
    }

    /// The retry of a move that stopped part way.
    async fn retry(&self, from: &str, names: &[&str], to: &str) -> Result<TransferSummary, Error> {
        self.transfer
            .through_mac_in(
                self.base.path(),
                &self.request(Paste::Move, from, names, to),
                true,
                &Cancel::new(),
                &|_| {},
            )
            .await
    }

    fn read(&self, path: &str) -> Vec<u8> {
        fs::read(self.phone.path().join(path)).unwrap()
    }

    fn exists(&self, path: &str) -> bool {
        self.phone.path().join(path).exists()
    }

    fn names(&self, folder: &str) -> BTreeSet<String> {
        fs::read_dir(self.phone.path().join(folder))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// The scratch folder is made inside `base` and must be gone by the time
    /// the paste has returned, however it ended.
    fn scratch_is_gone(&self) -> bool {
        fs::read_dir(self.base.path()).unwrap().next().is_none()
    }

    fn rescan(&self) {
        mtp_rs::rescan_virtual_device(&self.serial);
    }
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|n| (*n).to_owned()).collect()
}

fn set_date(path: &Path, secs: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

#[tokio::test]
async fn a_file_is_copied_to_another_folder_and_the_original_stays() {
    let v = via().await;

    let summary = v
        .paste(Paste::Copy, "/", &["notes.txt"], "/Download")
        .await
        .unwrap();

    assert_eq!(v.read("Download/notes.txt"), b"hello");
    assert_eq!(v.read("notes.txt"), b"hello");
    assert_eq!((summary.files, summary.bytes), (1, 5));
    assert!(summary.through_mac);
    assert_eq!(summary.direction, Direction::Upload);
    assert_eq!(summary.destination, "/Download");
    assert_eq!(summary.first_item.as_deref(), Some("notes.txt"));
    assert_eq!(summary.removal, None);
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn a_file_copied_into_its_own_folder_makes_a_numbered_copy_even_though_it_is_identical() {
    // Same size and date as the original: an upload that skips "the very file
    // the phone already has" would do nothing here.
    let v = via().await;

    let summary = v
        .paste(Paste::Copy, "/", &["notes.txt"], "/")
        .await
        .unwrap();

    assert_eq!(v.read("notes.txt"), b"hello");
    assert_eq!(v.read("notes (1).txt"), b"hello");
    assert_eq!(summary.files, 1);
    assert_eq!(summary.already_there, 0);
    assert_eq!(summary.renamed, 1);
    assert_eq!(summary.first_item.as_deref(), Some("notes (1).txt"));
}

#[tokio::test]
async fn a_name_that_is_taken_is_never_overwritten() {
    let v = via().await;
    fs::write(v.phone.path().join("Download/notes.txt"), b"something else").unwrap();
    v.rescan();

    let summary = v
        .paste(Paste::Copy, "/", &["notes.txt"], "/Download")
        .await
        .unwrap();

    assert_eq!(v.read("Download/notes.txt"), b"something else", "untouched");
    assert_eq!(v.read("Download/notes (1).txt"), b"hello");
    assert_eq!(summary.renamed, 1);
}

#[tokio::test]
async fn a_folder_is_copied_with_everything_in_it() {
    let v = via().await;

    let summary = v
        .paste(Paste::Copy, "/", &["DCIM", "notes.txt"], "/Music")
        .await
        .unwrap();

    assert_eq!(v.read("Music/DCIM/Camera/a.jpg"), b"aaaa");
    assert_eq!(v.read("Music/DCIM/Camera/b.jpg"), b"bb");
    assert_eq!(v.read("Music/DCIM/Other/c.jpg"), b"ccc");
    assert_eq!(v.read("Music/notes.txt"), b"hello");
    assert_eq!(summary.files, 4);
    assert_eq!(v.read("DCIM/Camera/a.jpg"), b"aaaa", "the originals stay");
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn a_moved_file_is_in_the_new_folder_and_gone_from_the_old_one() {
    let v = via().await;

    let summary = v
        .paste(Paste::Move, "/", &["notes.txt"], "/Download")
        .await
        .unwrap();

    assert_eq!(v.read("Download/notes.txt"), b"hello");
    assert!(!v.exists("notes.txt"));
    let removal = summary.removal.unwrap();
    assert_eq!(removal.removed, 1);
    assert!(removal.kept.is_empty() && removal.left.is_empty());
    assert_eq!(removal.stopped, None);
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn a_moved_folder_takes_its_files_and_folders_with_it() {
    let v = via().await;

    let summary = v
        .paste(Paste::Move, "/", &["DCIM"], "/Music")
        .await
        .unwrap();

    assert_eq!(v.read("Music/DCIM/Camera/a.jpg"), b"aaaa");
    assert_eq!(v.read("Music/DCIM/Other/c.jpg"), b"ccc");
    assert!(!v.exists("DCIM"), "nothing of it is left behind");
    assert_eq!(summary.removal.unwrap().removed, 3);
    assert_eq!(v.names(""), set(&["Music", "Download", "notes.txt"]));
}

#[tokio::test]
async fn a_move_into_the_folder_it_is_already_in_does_nothing() {
    let v = via().await;

    let summary = v
        .paste(Paste::Move, "/", &["notes.txt"], "/")
        .await
        .unwrap();

    assert_eq!(summary.files, 0);
    assert_eq!(v.read("notes.txt"), b"hello");
    assert_eq!(
        v.names(""),
        set(&["DCIM", "Music", "Download", "notes.txt"])
    );
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn a_folder_cannot_go_into_itself_either_way() {
    let v = via().await;
    for mode in [Paste::Copy, Paste::Move] {
        for to in ["/DCIM", "/DCIM/Camera"] {
            let result = v.paste(mode, "/", &["DCIM"], to).await;

            assert_eq!(
                result,
                Err(Error::IntoItself("DCIM".into())),
                "{mode:?} into {to}"
            );
        }
    }
    assert_eq!(
        v.names("DCIM/Camera"),
        set(&["a.jpg", "b.jpg"]),
        "nothing changed"
    );
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn a_read_only_target_refuses_before_anything_is_copied() {
    let v = via_with(|phone| phone.read_only = true).await;

    let result = v.paste(Paste::Move, "/", &["notes.txt"], "/Download").await;

    assert_eq!(result, Err(Error::ReadOnly));
    assert_eq!(v.read("notes.txt"), b"hello");
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn an_item_that_is_not_there_is_an_error_and_nothing_is_done() {
    let v = via().await;

    let result = v
        .paste(Paste::Copy, "/", &["notes.txt", "nope.txt"], "/Music")
        .await;

    assert!(matches!(result, Err(Error::PathNotFound(_))), "{result:?}");
    assert!(v.names("Music").is_empty());
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn pasting_nothing_does_nothing() {
    let v = via().await;

    let summary = v.paste(Paste::Copy, "/", &[], "/Music").await.unwrap();

    assert_eq!(summary.files, 0);
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn a_move_whose_copy_is_already_there_removes_only_the_original() {
    // An earlier attempt got the file across and could not remove the original.
    let v = via().await;
    for path in ["notes.txt", "Download/notes.txt"] {
        if path.starts_with("Download") {
            fs::write(v.phone.path().join(path), b"hello").unwrap();
        }
        set_date(&v.phone.path().join(path), 1_700_000_000);
    }
    v.rescan();

    let summary = v
        .paste(Paste::Move, "/", &["notes.txt"], "/Download")
        .await
        .unwrap();

    assert_eq!(summary.already_there, 1);
    assert_eq!(summary.files, 0);
    assert!(!v.exists("notes.txt"), "the original is removed");
    assert_eq!(
        v.names("Download"),
        set(&["notes.txt"]),
        "no `notes (1).txt`"
    );
    assert_eq!(summary.removal.unwrap().removed, 1);
}

#[tokio::test]
async fn an_item_the_phone_would_not_take_stays_and_the_rest_moves() {
    // The virtual phone refuses a name with a backslash.
    let v = via().await;
    fs::write(v.phone.path().join("Download/ok.txt"), b"fine").unwrap();
    fs::write(v.phone.path().join("Download/odd\\name.txt"), b"odd").unwrap();
    v.rescan();

    let summary = v
        .paste(
            Paste::Move,
            "/Download",
            &["ok.txt", "odd\\name.txt"],
            "/Music",
        )
        .await
        .unwrap();

    assert_eq!(v.read("Music/ok.txt"), b"fine");
    assert!(!v.exists("Download/ok.txt"), "what arrived is moved");
    assert_eq!(
        v.read("Download/odd\\name.txt"),
        b"odd",
        "what did not arrive stays"
    );
    assert!(!v.exists("Music/odd\\name.txt"));
    assert_eq!(summary.failures.len(), 1);
    let removal = summary.removal.unwrap();
    assert_eq!(removal.removed, 1);
    assert_eq!(removal.left, ["odd\\name.txt"]);
    assert!(
        removal.kept.is_empty(),
        "the failure says why it stayed; no second note: {:?}",
        removal.kept
    );
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn a_copy_that_could_not_be_read_back_leaves_the_original() {
    // The phone will not describe what was just sent, so the copy cannot be
    // confirmed and a move must not remove the original on its say-so.
    let v = via().await;
    let newest = [
        "/",
        "/DCIM",
        "/DCIM/Camera",
        "/DCIM/Other",
        "/Music",
        "/Download",
    ];
    let mut highest = 0;
    for folder in newest {
        for entry in v.explorer.list(v.storage, folder, true).await.unwrap() {
            highest = highest.max(entry.id);
        }
    }
    for next in highest + 1..=highest + 8 {
        mtp_rs::force_object_info_error(&v.serial, mtp_rs::mtp::ObjectHandle(next), None);
    }

    let summary = v
        .paste(Paste::Move, "/", &["notes.txt"], "/Download")
        .await
        .unwrap();

    assert_eq!(v.read("Download/notes.txt"), b"hello", "the copy is there");
    assert_eq!(v.read("notes.txt"), b"hello", "and so is the original");
    let removal = summary.removal.unwrap();
    assert_eq!(removal.removed, 0);
    assert_eq!(
        removal.kept,
        [Kept {
            label: "notes.txt".into(),
            why: Why::Unchecked
        }]
    );
    assert_eq!(removal.left, ["notes.txt"]);
}

#[tokio::test]
async fn progress_says_it_is_going_through_the_mac_and_reads_and_sends_each_file_in_turn() {
    let v = via().await;
    let seen = Mutex::new(Vec::new());

    v.paste_with(
        Paste::Move,
        "/DCIM/Camera",
        &["a.jpg", "b.jpg"],
        "/Download",
        &Cancel::new(),
        |p| seen.lock().unwrap().push(p),
    )
    .await
    .unwrap();

    let seen = seen.into_inner().unwrap();
    assert!(seen.iter().all(|p| p.through_mac && p.moving));
    let copying: Vec<_> = seen.iter().filter(|p| !p.removing).collect();
    // One picture of the whole selection, however the two halves interleave.
    assert!(
        copying
            .iter()
            .all(|p| p.files_total == 2 && p.bytes_total == 6)
    );
    assert!(
        copying
            .windows(2)
            .all(|pair| pair[0].bytes_done <= pair[1].bytes_done),
        "the bar never goes back"
    );
    assert_eq!(copying.last().unwrap().bytes_done, 6, "and ends full");
    assert_eq!(copying.last().unwrap().files_done, 2);
    // Reading and sending alternate a file at a time: the first file is sent
    // before the second is read.
    let sending_a = copying
        .iter()
        .position(|p| p.direction == Direction::Upload && p.current_file == "a.jpg")
        .expect("a.jpg is sent");
    let reading_b = copying
        .iter()
        .position(|p| p.direction == Direction::Download && p.current_file == "b.jpg")
        .expect("b.jpg is read");
    assert!(
        sending_a < reading_b,
        "a.jpg goes back before b.jpg is read"
    );
    let removed: Vec<_> = seen.iter().filter(|p| p.removing).collect();
    assert!(!removed.is_empty(), "then the originals go");
    assert!(
        seen.iter().position(|p| p.removing).unwrap()
            > seen.iter().rposition(|p| !p.removing).unwrap(),
        "only after the last file is copied"
    );
}

#[tokio::test]
async fn the_mac_holds_one_file_at_a_time_not_the_selection() {
    // Three files of 5 MiB: copying the selection through the Mac used to leave
    // all 15 MiB in the scratch folder at once.
    const FILE: usize = 5 * 1024 * 1024;
    let v = via().await;
    for name in ["x.bin", "y.bin", "z.bin"] {
        fs::write(v.phone.path().join("Music").join(name), vec![9u8; FILE]).unwrap();
    }
    v.rescan();
    let most = std::sync::atomic::AtomicU64::new(0);
    let base = v.base.path().to_path_buf();

    let summary = v
        .paste_with(
            Paste::Copy,
            "/Music",
            &["x.bin", "y.bin", "z.bin"],
            "/Download",
            &Cancel::new(),
            |_| {
                let held = size_of(&base);
                most.fetch_max(held, std::sync::atomic::Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    assert_eq!(summary.files, 3);
    assert_eq!(v.read("Download/z.bin").len(), FILE);
    let most = most.into_inner();
    assert!(most > 0, "the scratch folder did hold something");
    assert!(
        most <= FILE as u64,
        "never more than the biggest file: {most} bytes"
    );
    assert!(v.scratch_is_gone());
}

/// Bytes in every file under `dir`.
fn size_of(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| match entry.metadata() {
                    Ok(meta) if meta.is_dir() => size_of(&entry.path()),
                    Ok(meta) => meta.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn files_on_their_way_back_to_the_phone_are_not_forced_to_the_mac_s_disk() {
    // They are deleted from the Mac as soon as they are sent: the copy that
    // counts is the phone's, which a move reads back before it trusts it.
    for mode in [Paste::Copy, Paste::Move] {
        let v = via().await;
        let before = super::service::SYNCS.with(std::cell::Cell::get);

        v.paste(mode, "/DCIM/Camera", &["a.jpg", "b.jpg"], "/Download")
            .await
            .unwrap();

        assert_eq!(
            super::service::SYNCS.with(std::cell::Cell::get),
            before,
            "{mode:?}"
        );
    }
}

#[tokio::test]
async fn a_folder_is_made_on_the_phone_before_what_goes_in_it_and_empty_ones_too() {
    let v = via().await;
    fs::create_dir_all(v.phone.path().join("Music/Album/Disc 2")).unwrap();
    fs::create_dir_all(v.phone.path().join("Music/Album/Empty")).unwrap();
    fs::write(v.phone.path().join("Music/Album/Disc 2/t.mp3"), b"tt").unwrap();
    fs::write(v.phone.path().join("Music/Album/a.mp3"), b"a").unwrap();
    v.rescan();

    let summary = v
        .paste(Paste::Copy, "/Music", &["Album"], "/Download")
        .await
        .unwrap();

    assert_eq!(v.read("Download/Album/Disc 2/t.mp3"), b"tt");
    assert_eq!(v.read("Download/Album/a.mp3"), b"a");
    assert!(v.exists("Download/Album/Empty"));
    assert_eq!(summary.files, 2);
    assert_eq!(summary.first_item.as_deref(), Some("Album"));
}

#[tokio::test]
async fn files_go_into_the_folder_this_copy_made_not_one_that_has_the_same_name() {
    // "Album" is taken in the target, so the copy becomes "Album (1)", and its
    // files must go there and not into the Album that was already there.
    let v = via().await;
    fs::create_dir_all(v.phone.path().join("Music/Album")).unwrap();
    fs::write(v.phone.path().join("Music/Album/a.mp3"), b"new").unwrap();
    fs::create_dir_all(v.phone.path().join("Download/Album")).unwrap();
    fs::write(v.phone.path().join("Download/Album/keep.mp3"), b"mine").unwrap();
    v.rescan();

    let summary = v
        .paste(Paste::Copy, "/Music", &["Album"], "/Download")
        .await
        .unwrap();

    assert_eq!(v.names("Download/Album"), set(&["keep.mp3"]), "left alone");
    assert_eq!(v.read("Download/Album (1)/a.mp3"), b"new");
    assert_eq!(summary.renamed, 1);
    assert_eq!(summary.first_item.as_deref(), Some("Album (1)"));
}

#[tokio::test]
async fn several_items_are_named_one_by_one_without_clashing_with_each_other() {
    let v = via().await;
    fs::write(v.phone.path().join("Music/a.txt"), b"1").unwrap();
    fs::write(v.phone.path().join("Music/b.txt"), b"2").unwrap();
    fs::write(v.phone.path().join("Download/a.txt"), b"taken").unwrap();
    v.rescan();

    v.paste(Paste::Copy, "/Music", &["a.txt", "b.txt"], "/Download")
        .await
        .unwrap();

    assert_eq!(v.read("Download/a.txt"), b"taken");
    assert_eq!(v.read("Download/a (1).txt"), b"1");
    assert_eq!(v.read("Download/b.txt"), b"2");
}

/// A phone with Music/Album/{a.mp3, b.mp3, c.mp3} copied once into Download,
/// then with b and c taken out of that copy again: the state a move that
/// stopped after its first file leaves (nothing was removed from Music yet).
async fn after_a_move_that_stopped_part_way() -> Via {
    let v = via().await;
    for (name, content) in [("a.mp3", "aa"), ("b.mp3", "bb"), ("c.mp3", "cc")] {
        fs::create_dir_all(v.phone.path().join("Music/Album")).unwrap();
        fs::write(v.phone.path().join("Music/Album").join(name), content).unwrap();
    }
    v.rescan();
    v.paste(Paste::Copy, "/Music", &["Album"], "/Download")
        .await
        .unwrap();
    fs::remove_file(v.phone.path().join("Download/Album/b.mp3")).unwrap();
    fs::remove_file(v.phone.path().join("Download/Album/c.mp3")).unwrap();
    v.rescan();
    v
}

#[tokio::test]
async fn a_retried_move_merges_into_the_folder_the_first_try_made() {
    let v = after_a_move_that_stopped_part_way().await;

    let summary = v.retry("/Music", &["Album"], "/Download").await.unwrap();

    assert_eq!(v.names("Download"), set(&["Album"]), "no Album (1)");
    assert_eq!(
        v.names("Download/Album"),
        set(&["a.mp3", "b.mp3", "c.mp3"]),
        "what was missing is there now"
    );
    assert_eq!(v.read("Download/Album/c.mp3"), b"cc");
    assert_eq!(summary.merged, 1);
    assert_eq!(summary.already_there, 1, "a.mp3 was not sent again");
    assert_eq!(summary.files, 2);
    assert_eq!(summary.renamed, 0);
    assert_eq!(summary.first_item.as_deref(), Some("Album"));
    assert!(!v.exists("Music/Album"), "and the originals went");
    let removal = summary.removal.unwrap();
    assert!(removal.left.is_empty() && removal.kept.is_empty());
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn a_retry_never_replaces_a_different_file_and_does_not_add_it_twice() {
    let v = after_a_move_that_stopped_part_way().await;
    // Something else of the same name is where b.mp3 should go.
    fs::write(
        v.phone.path().join("Download/Album/b.mp3"),
        b"somebody else's",
    )
    .unwrap();
    v.rescan();

    let summary = v.retry("/Music", &["Album"], "/Download").await.unwrap();

    assert_eq!(
        v.read("Download/Album/b.mp3"),
        b"somebody else's",
        "untouched"
    );
    assert_eq!(v.read("Download/Album/b (1).mp3"), b"bb", "beside it");
    assert_eq!(summary.files, 2);
    assert_eq!(summary.merged, 1);

    // The same retry again (the originals are back, as they were) finds the
    // numbered copy for what it is, and sends nothing.
    fs::create_dir_all(v.phone.path().join("Music/Album")).unwrap();
    // The date the first copy gave its copies (they were made together), not
    // whatever the clock says now.
    let then = fs::metadata(v.phone.path().join("Download/Album/a.mp3"))
        .unwrap()
        .modified()
        .unwrap();
    for (name, content) in [("a.mp3", "aa"), ("b.mp3", "bb"), ("c.mp3", "cc")] {
        let original = v.phone.path().join("Music/Album").join(name);
        fs::write(&original, content).unwrap();
        fs::File::options()
            .write(true)
            .open(&original)
            .unwrap()
            .set_modified(then)
            .unwrap();
    }
    v.rescan();

    let again = v.retry("/Music", &["Album"], "/Download").await.unwrap();

    assert_eq!(again.files, 0, "{again:?}");
    assert_eq!(again.already_there, 3);
    assert_eq!(
        v.names("Download/Album"),
        set(&["a.mp3", "b.mp3", "b (1).mp3", "c.mp3"]),
        "no b (2).mp3"
    );
}

#[tokio::test]
async fn a_retry_merges_nested_folders_and_numbers_a_folder_where_a_file_is() {
    let v = via().await;
    fs::create_dir_all(v.phone.path().join("Music/Album/Disc 2")).unwrap();
    fs::create_dir_all(v.phone.path().join("Music/Album/Extras")).unwrap();
    fs::write(v.phone.path().join("Music/Album/Disc 2/t.mp3"), b"tt").unwrap();
    fs::write(v.phone.path().join("Music/Album/Disc 2/u.mp3"), b"uu").unwrap();
    fs::write(v.phone.path().join("Music/Album/Extras/x.txt"), b"x").unwrap();
    v.rescan();
    v.paste(Paste::Copy, "/Music", &["Album"], "/Download")
        .await
        .unwrap();
    fs::remove_file(v.phone.path().join("Download/Album/Disc 2/u.mp3")).unwrap();
    fs::remove_dir_all(v.phone.path().join("Download/Album/Extras")).unwrap();
    fs::write(v.phone.path().join("Download/Album/Extras"), b"a file").unwrap();
    v.rescan();

    let summary = v.retry("/Music", &["Album"], "/Download").await.unwrap();

    assert_eq!(v.names("Download"), set(&["Album"]));
    assert_eq!(
        v.names("Download/Album"),
        set(&["Disc 2", "Extras", "Extras (1)"]),
        "the file called Extras stays, the folder goes beside it"
    );
    assert_eq!(v.read("Download/Album/Extras"), b"a file");
    assert_eq!(v.read("Download/Album/Extras (1)/x.txt"), b"x");
    assert_eq!(
        v.read("Download/Album/Disc 2/u.mp3"),
        b"uu",
        "merged inside"
    );
    assert_eq!(summary.merged, 1, "only what was selected is counted");
    assert_eq!(summary.already_there, 1, "t.mp3 was there");
}

#[tokio::test]
async fn a_copy_never_merges_even_when_it_is_told_it_is_a_retry() {
    let v = after_a_move_that_stopped_part_way().await;

    let summary = v
        .transfer
        .through_mac_in(
            v.base.path(),
            &v.request(Paste::Copy, "/Music", &["Album"], "/Download"),
            true,
            &Cancel::new(),
            &|_| {},
        )
        .await
        .unwrap();

    assert_eq!(v.names("Download"), set(&["Album", "Album (1)"]));
    assert_eq!(summary.merged, 0);
    assert_eq!(summary.renamed, 1);
}

#[tokio::test]
async fn a_move_that_is_not_a_retry_still_numbers_a_folder_that_is_taken() {
    let v = after_a_move_that_stopped_part_way().await;

    let summary = v
        .paste(Paste::Move, "/Music", &["Album"], "/Download")
        .await
        .unwrap();

    assert_eq!(v.names("Download"), set(&["Album", "Album (1)"]));
    assert_eq!(summary.merged, 0);
}

#[tokio::test]
async fn retrying_the_move_of_a_five_thousand_file_folder_sends_nothing_again() {
    let v = via().await;
    fs::create_dir_all(v.phone.path().join("Music/Big")).unwrap();
    for i in 0..5_000 {
        fs::write(
            v.phone.path().join(format!("Music/Big/{i:05}.jpg")),
            format!("photo {i}"),
        )
        .unwrap();
    }
    v.rescan();
    v.paste(Paste::Copy, "/Music", &["Big"], "/Download")
        .await
        .unwrap();
    let started = std::time::Instant::now();

    let summary = v.retry("/Music", &["Big"], "/Download").await.unwrap();

    eprintln!("retry of 5,000 files, all there: {:?}", started.elapsed());
    assert_eq!(summary.files, 0, "nothing is read or sent again");
    assert_eq!(summary.already_there, 5_000);
    assert_eq!(summary.merged, 1);
    assert_eq!(v.names("Download").len(), 1);
    assert!(!v.exists("Music/Big"), "the originals went");
    // One listing of the folder, found by name after that: not a search for
    // each of 5,000 files. A search per file would take far longer than this,
    // and the limit leaves room for a slow machine (a CI runner takes ~3x).
    assert!(started.elapsed() < std::time::Duration::from_secs(60));
}

#[tokio::test]
async fn five_thousand_selected_files_are_named_in_linear_time() {
    // Select all in a folder: 5,000 sources, each named among the others.
    let v = via().await;
    for i in 0..5_000 {
        fs::write(v.phone.path().join(format!("Music/{i:05}.jpg")), b"x").unwrap();
    }
    v.rescan();
    let names: Vec<String> = (0..5_000).map(|i| format!("{i:05}.jpg")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let started = std::time::Instant::now();

    let summary = v
        .paste(Paste::Copy, "/Music", &names, "/Download")
        .await
        .unwrap();

    eprintln!(
        "5,000 selected files through the Mac in {:?}",
        started.elapsed()
    );
    assert_eq!(summary.files, 5_000);
    assert_eq!(v.names("Download").len(), 5_000);
    // A guard against naming each file by a search among the rest, not a
    // benchmark: about 12 s alone in a debug build, twice that with the
    // whole suite running beside it.
    assert!(started.elapsed() < std::time::Duration::from_secs(60));
}

#[tokio::test]
async fn a_five_thousand_file_folder_goes_through_the_mac_without_filling_it() {
    let v = via().await;
    fs::create_dir_all(v.phone.path().join("Music/Big")).unwrap();
    for i in 0..5_000 {
        fs::write(
            v.phone.path().join(format!("Music/Big/{i:05}.jpg")),
            format!("photo {i}"),
        )
        .unwrap();
    }
    v.rescan();
    let most = std::sync::atomic::AtomicU64::new(0);
    let base = v.base.path().to_path_buf();
    let reports = std::sync::atomic::AtomicUsize::new(0);
    let started = std::time::Instant::now();

    let summary = v
        .paste_with(
            Paste::Copy,
            "/Music",
            &["Big"],
            "/Download",
            &Cancel::new(),
            |_| {
                // (Measured now and then: walking the scratch folder at every
                // report would dominate what is being timed.)
                let n = reports.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n.is_multiple_of(997) {
                    most.fetch_max(size_of(&base), std::sync::atomic::Ordering::Relaxed);
                }
            },
        )
        .await
        .unwrap();

    eprintln!("5,000 files through the Mac in {:?}", started.elapsed());
    // A few seconds alone (11 s in a debug build); the whole suite running
    // beside it once pushed it past 15 s, so the bound is loose: it only
    // catches something gone quadratic, which would take minutes. Forcing
    // each scratch file to the disk (once 25 s) is caught by the count in
    // `files_on_their_way_back_to_the_phone_are_not_forced_to_the_mac_s_disk`.
    assert!(started.elapsed() < std::time::Duration::from_secs(45));
    assert_eq!(summary.files, 5_000);
    assert_eq!(v.names("Download/Big").len(), 5_000);
    assert!(
        most.into_inner() < 100,
        "a few bytes of one small file at most"
    );
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn a_copy_does_not_claim_to_be_a_move() {
    let v = via().await;
    let seen = Mutex::new(Vec::new());

    v.paste_with(
        Paste::Copy,
        "/",
        &["notes.txt"],
        "/Music",
        &Cancel::new(),
        |p| {
            seen.lock()
                .unwrap()
                .push((p.through_mac, p.moving, p.removing))
        },
    )
    .await
    .unwrap();

    let seen = seen.into_inner().unwrap();
    assert!(
        seen.iter()
            .all(|(through, moving, removing)| *through && !moving && !removing)
    );
}

#[tokio::test]
async fn cancelling_while_sending_leaves_the_originals_and_no_scratch_folder() {
    let v = via().await;
    let cancel = Cancel::new();

    let result = v
        .paste_with(
            Paste::Move,
            "/",
            &["notes.txt"],
            "/Download",
            &cancel,
            |p| {
                if p.direction == Direction::Upload {
                    cancel.cancel();
                }
            },
        )
        .await;

    assert_eq!(result, Err(Error::Cancelled));
    assert_eq!(v.read("notes.txt"), b"hello");
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn cancelling_while_copying_down_ends_it_cleanly() {
    let v = via().await;
    let cancel = Cancel::new();
    cancel.cancel();

    let result = v
        .paste_with(Paste::Copy, "/", &["DCIM"], "/Music", &cancel, |_| {})
        .await;

    assert_eq!(result, Err(Error::Cancelled));
    assert!(v.names("Music").is_empty());
    assert!(v.scratch_is_gone());
}

#[tokio::test]
async fn the_scratch_folder_is_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt;
    let v = via().await;
    let modes = Mutex::new(Vec::new());
    let base = v.base.path().to_owned();

    v.paste_with(
        Paste::Copy,
        "/",
        &["notes.txt"],
        "/Music",
        &Cancel::new(),
        |_| {
            for entry in fs::read_dir(&base).unwrap().flatten() {
                let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
                modes.lock().unwrap().push(mode);
            }
        },
    )
    .await
    .unwrap();

    let modes = modes.into_inner().unwrap();
    assert!(!modes.is_empty(), "the scratch folder existed while it ran");
    assert!(modes.iter().all(|mode| mode & 0o077 == 0), "{modes:?}");
}

#[tokio::test]
async fn pasting_out_of_a_big_folder_does_not_read_it_again() {
    let v = via().await;
    fs::create_dir_all(v.phone.path().join("DCIM/Big")).unwrap();
    for i in 0..5_000 {
        fs::write(
            v.phone.path().join(format!("DCIM/Big/IMG_{i:05}.jpg")),
            b"x",
        )
        .unwrap();
    }
    v.rescan();
    assert_eq!(
        v.explorer
            .list(v.storage, "/DCIM/Big", true)
            .await
            .unwrap()
            .len(),
        5_000
    );
    let reads = v.explorer.full_reads();

    let started = std::time::Instant::now();
    let summary = v
        .paste(
            Paste::Move,
            "/DCIM/Big",
            &["IMG_00001.jpg", "IMG_00002.jpg", "IMG_00003.jpg"],
            "/Music",
        )
        .await
        .unwrap();
    let took = started.elapsed();

    assert_eq!(summary.removal.unwrap().removed, 3);
    assert_eq!(v.names("Music").len(), 3);
    assert!(
        v.explorer.full_reads() - reads <= 2,
        "only the small target was read, never the 5,000 files: {} reads",
        v.explorer.full_reads() - reads
    );
    // The virtual phone answers "how many items" by reading the directory (about
    // 0.3 s at this size in a release build); a round trip per file would take
    // minutes.
    assert!(took < Duration::from_secs(30), "{took:?}");
}
