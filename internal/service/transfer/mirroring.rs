//! Mirroring a phone folder and a Mac folder on the virtual phone: what is
//! copied, replaced and removed, and what is never touched.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::Service;
use crate::dto::{Cancel, Error, MirrorDirection, MirrorPreview, MirrorRequest, TransferSummary};
use crate::service::device;
use crate::service::explorer;
use crate::service::testing::virtual_phone;

struct Mirror {
    phone: tempfile::TempDir,
    mac: tempfile::TempDir,
    /// Where the Trash recorder puts what it is given: the test's own folder.
    bin: tempfile::TempDir,
    trashed: Arc<Mutex<Vec<PathBuf>>>,
    transfer: Service,
    explorer: Arc<explorer::Service>,
    device: Arc<device::Service>,
    storage: u64,
    serial: String,
}

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

fn put(root: &Path, path: &str, content: &[u8], when: u64) {
    let file = root.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, content).unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(at(when))
        .unwrap();
}

/// Phone: Backup/a.jpg, Backup/b.jpg, Backup/Sub/c.jpg and a hidden
/// Backup/.thumbnails/t.jpg; an empty Mac folder.
async fn mirror() -> Mirror {
    let phone = tempfile::tempdir().unwrap();
    put(phone.path(), "Backup/a.jpg", b"aaaa", 1_000_000);
    put(phone.path(), "Backup/b.jpg", b"bb", 1_000_000);
    put(phone.path(), "Backup/Sub/c.jpg", b"ccc", 1_000_000);
    put(phone.path(), "Backup/.thumbnails/t.jpg", b"t", 1_000_000);
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(phone.path()),
    )));
    let serial = device.connect().await.unwrap().serial_number;
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    let bin = tempfile::tempdir().unwrap();
    let trashed = Arc::new(Mutex::new(Vec::new()));
    let (log, into) = (Arc::clone(&trashed), bin.path().to_path_buf());
    // Never the user's Trash: the item goes into a folder of the test's.
    let transfer =
        Service::new(Arc::clone(&device), Arc::clone(&explorer)).with_trash(move |path: &Path| {
            let mut log = log.lock().unwrap();
            log.push(path.to_path_buf());
            let call = into.join(log.len().to_string());
            fs::create_dir_all(&call)?;
            fs::rename(path, call.join(path.file_name().unwrap()))
        });
    Mirror {
        phone,
        mac: tempfile::tempdir().unwrap(),
        bin,
        trashed,
        transfer,
        explorer,
        device,
        storage,
        serial,
    }
}

impl Mirror {
    fn request(&self, direction: MirrorDirection, remove_extras: bool) -> MirrorRequest {
        MirrorRequest {
            storage_id: self.storage,
            phone_folder: "/Backup".into(),
            mac_folder: self.mac.path().to_string_lossy().into_owned(),
            direction,
            remove_extras,
        }
    }

    async fn preview(&self, direction: MirrorDirection, remove_extras: bool) -> MirrorPreview {
        self.transfer
            .mirror_preview(
                &self.request(direction, remove_extras),
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap()
    }

    async fn run(
        &self,
        direction: MirrorDirection,
        remove_extras: bool,
    ) -> Result<TransferSummary, Error> {
        self.transfer
            .mirror(
                &self.request(direction, remove_extras),
                &Cancel::new(),
                |_| {},
            )
            .await
    }

    fn rescan(&self) {
        mtp_rs::rescan_virtual_device(&self.serial);
    }

    /// Every file under `root`, as "folder/name", hidden ones too.
    fn files(root: &Path) -> BTreeSet<String> {
        fn walk(dir: &Path, base: &Path, out: &mut BTreeSet<String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, base, out);
                } else {
                    out.insert(path.strip_prefix(base).unwrap().to_string_lossy().into());
                }
            }
        }
        let mut out = BTreeSet::new();
        walk(root, root, &mut out);
        out
    }

    fn on_mac(&self) -> BTreeSet<String> {
        Self::files(self.mac.path())
    }

    fn on_phone(&self) -> BTreeSet<String> {
        Self::files(&self.phone.path().join("Backup"))
    }

    fn in_bin(&self) -> BTreeSet<String> {
        Self::files(self.bin.path())
    }
}

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

const TO_MAC: MirrorDirection = MirrorDirection::ToMac;
const TO_PHONE: MirrorDirection = MirrorDirection::ToPhone;

#[tokio::test]
async fn a_first_mirror_copies_everything_but_hidden_things_and_a_second_does_nothing() {
    let m = mirror().await;

    let preview = m.preview(TO_MAC, false).await;
    let summary = m.run(TO_MAC, false).await.unwrap();

    assert_eq!((preview.new_files, preview.new_bytes), (3, 4 + 2 + 3));
    assert_eq!(preview.samples.new, ["Sub/c.jpg", "a.jpg", "b.jpg"]);
    assert_eq!(summary.files, 3);
    assert_eq!(m.on_mac(), set(&["Sub/c.jpg", "a.jpg", "b.jpg"]));
    assert_eq!(
        fs::metadata(m.mac.path().join("a.jpg"))
            .unwrap()
            .modified()
            .unwrap(),
        at(1_000_000)
    );
    assert_eq!(m.preview(TO_MAC, true).await, MirrorPreview::default());
    let again = m.run(TO_MAC, true).await.unwrap();
    assert_eq!(again.files, 0);
    assert!(m.trashed.lock().unwrap().is_empty(), "nothing to put away");
}

#[tokio::test]
async fn a_changed_file_replaces_the_macs_and_the_old_one_goes_to_the_trash() {
    let m = mirror().await;
    m.run(TO_MAC, false).await.unwrap();
    put(m.phone.path(), "Backup/b.jpg", b"bbbbbb", 2_000_000);
    m.rescan();

    let preview = m.preview(TO_MAC, false).await;
    let summary = m.run(TO_MAC, false).await.unwrap();

    assert_eq!((preview.changed_files, preview.changed_bytes), (1, 6));
    assert_eq!(preview.samples.changed, ["b.jpg"]);
    assert_eq!(summary.files, 1);
    assert_eq!(summary.mirrored.as_ref().unwrap().replaced, 1);
    assert_eq!(summary.mirrored.as_ref().unwrap().copied, 0, "nothing new");
    assert_eq!(fs::read(m.mac.path().join("b.jpg")).unwrap(), b"bbbbbb");
    assert_eq!(
        m.on_mac(),
        set(&["Sub/c.jpg", "a.jpg", "b.jpg"]),
        "no staging left"
    );
    assert_eq!(m.trashed.lock().unwrap().len(), 1, "one call to the Trash");
    let old: Vec<String> = m.in_bin().into_iter().collect();
    assert_eq!(old.len(), 1);
    assert!(old[0].ends_with("/b.jpg"), "{old:?}");
}

#[tokio::test]
async fn what_is_only_on_the_mac_stays_unless_asked_and_then_goes_to_the_trash_in_one_call() {
    let m = mirror().await;
    m.run(TO_MAC, false).await.unwrap();
    put(m.mac.path(), "mine.txt", b"m", 5);
    put(m.mac.path(), "Old/x.jpg", b"x", 5);
    put(m.mac.path(), ".DS_Store", b"d", 5);
    put(m.phone.path(), "Backup/a.jpg", b"AAAAA", 3_000_000);
    m.rescan();

    let kept = m.preview(TO_MAC, false).await;
    let summary = m.run(TO_MAC, false).await.unwrap();
    assert_eq!(kept.extras, 2);
    assert_eq!(kept.samples.extras, ["Old", "mine.txt"]);
    assert_eq!(summary.mirrored.as_ref().unwrap().kept, 2);
    assert!(m.on_mac().contains("mine.txt"));

    put(m.phone.path(), "Backup/b.jpg", b"BBBBBBB", 4_000_000);
    m.rescan();
    let summary = m.run(TO_MAC, true).await.unwrap();

    let mirrored = summary.mirrored.unwrap();
    assert_eq!((mirrored.replaced, mirrored.removed), (1, 2));
    assert_eq!(
        m.on_mac(),
        set(&[".DS_Store", "Sub/c.jpg", "a.jpg", "b.jpg"]),
        "hidden things stay"
    );
    assert_eq!(m.trashed.lock().unwrap().len(), 2, "one call per mirror");
    assert!(m.in_bin().iter().any(|p| p.ends_with("/mine.txt")));
    assert!(m.in_bin().iter().any(|p| p.ends_with("/Old/x.jpg")));
}

#[tokio::test]
async fn without_a_trash_nothing_on_the_mac_is_replaced_or_removed_nor_anything_copied() {
    let m = mirror().await;
    let plain = Service::new(Arc::clone(&m.device), Arc::clone(&m.explorer));
    plain
        .mirror(&m.request(TO_MAC, false), &Cancel::new(), |_| {})
        .await
        .unwrap();
    put(m.phone.path(), "Backup/b.jpg", b"bbbbbb", 2_000_000);
    put(m.phone.path(), "Backup/new.jpg", b"n", 2_000_000);
    m.rescan();

    let refused = plain
        .mirror(&m.request(TO_MAC, false), &Cancel::new(), |_| {})
        .await;

    assert_eq!(refused, Err(Error::Unsupported));
    assert_eq!(fs::read(m.mac.path().join("b.jpg")).unwrap(), b"bb");
    assert!(
        !m.on_mac().contains("new.jpg"),
        "refused before the first copy"
    );
}

#[tokio::test]
async fn a_file_where_the_mac_has_a_folder_is_left_alone_and_reported() {
    let m = mirror().await;
    put(m.mac.path(), "a.jpg/inside.txt", b"i", 5);

    let preview = m.preview(TO_MAC, true).await;
    let summary = m.run(TO_MAC, true).await.unwrap();

    assert_eq!(
        (preview.conflicts, preview.samples.conflicts.clone()),
        (1, vec!["a.jpg".to_owned()])
    );
    assert_eq!(summary.mirrored.unwrap().conflicts, 1);
    assert!(m.on_mac().contains("a.jpg/inside.txt"));
    assert!(m.trashed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_changed_file_that_does_not_arrive_leaves_the_old_one_in_place() {
    let m = mirror().await;
    m.run(TO_MAC, false).await.unwrap();
    put(m.phone.path(), "Backup/b.jpg", b"bbbbbb", 2_000_000);
    m.rescan();
    // Something the mirror can't make its staging folder through.
    fs::write(m.mac.path().join(".open-mtp-client-mirror"), b"in the way").unwrap();

    assert_eq!(m.preview(TO_MAC, false).await.changed_files, 1);
    let result = m.run(TO_MAC, false).await;

    // The staging folder could not be made: that is all that happened.
    assert!(result.is_err(), "{result:?}");
    assert_eq!(
        fs::read(m.mac.path().join("b.jpg")).unwrap(),
        b"bb",
        "the old one stays"
    );
    assert!(m.trashed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_mirror_into_a_folder_that_is_not_there_or_from_one_that_is_not_says_so() {
    let m = mirror().await;
    let gone_mac = MirrorRequest {
        mac_folder: m.mac.path().join("gone").to_string_lossy().into_owned(),
        ..m.request(TO_MAC, false)
    };
    let gone_phone = MirrorRequest {
        phone_folder: "/Nowhere".into(),
        ..m.request(TO_MAC, false)
    };

    let mac = m
        .transfer
        .mirror_preview(&gone_mac, &Cancel::new(), |_| {})
        .await;
    let phone = m
        .transfer
        .mirror_preview(&gone_phone, &Cancel::new(), |_| {})
        .await;

    assert!(matches!(mac, Err(Error::PathNotFound(_))), "{mac:?}");
    assert_eq!(phone, Err(Error::PathNotFound("/Nowhere".into())));
}

#[tokio::test]
async fn a_cancelled_mirror_stops_before_touching_anything() {
    let m = mirror().await;
    let cancel = Cancel::new();
    cancel.cancel();

    let result = m
        .transfer
        .mirror(&m.request(TO_MAC, true), &cancel, |_| {})
        .await;

    assert_eq!(result, Err(Error::Cancelled));
    assert!(m.on_mac().is_empty());
}

#[tokio::test]
async fn the_mirror_right_after_its_preview_reads_nothing_again() {
    let m = mirror().await;
    m.preview(TO_MAC, false).await;
    let after_preview = m.explorer.full_reads();

    m.run(TO_MAC, false).await.unwrap();

    assert_eq!(m.explorer.full_reads(), after_preview);
}

#[tokio::test]
async fn a_mirror_without_a_fresh_preview_reads_the_phone_and_sees_a_file_changed_in_place() {
    let m = mirror().await;
    m.run(TO_MAC, false).await.unwrap();
    // The same number of items: only a real reading shows the change.
    put(m.phone.path(), "Backup/b.jpg", b"bbbbbb", 2_000_000);
    m.rescan();

    let summary = m.run(TO_MAC, false).await.unwrap();

    assert_eq!(summary.mirrored.unwrap().replaced, 1);
    assert_eq!(fs::read(m.mac.path().join("b.jpg")).unwrap(), b"bbbbbb");
}

#[tokio::test]
async fn five_thousand_files_are_previewed_and_mirrored_reading_the_phone_once() {
    let m = mirror().await;
    for folder in 0..50 {
        for n in 0..100 {
            let at = m
                .phone
                .path()
                .join(format!("Backup/F{folder:02}/IMG_{n:03}.jpg"));
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, b"x").unwrap();
        }
    }
    m.rescan();

    let started = std::time::Instant::now();
    let preview = m.preview(TO_MAC, false).await;
    let reads = m.explorer.full_reads();
    let mirrored_at = std::time::Instant::now();
    let summary = m.run(TO_MAC, false).await.unwrap();
    let again_at = std::time::Instant::now();
    let again = m.preview(TO_MAC, false).await;
    eprintln!(
        "5,000 files: preview {:?}, mirror {:?}, preview again {:?}",
        mirrored_at - started,
        again_at - mirrored_at,
        again_at.elapsed()
    );

    assert_eq!(preview.new_files, 5_003);
    assert_eq!(summary.files, 5_003);
    assert_eq!(again.new_files, 0);
    assert_eq!(
        m.explorer.full_reads() - reads,
        1 + 1 + 50,
        "only the preview after the mirror reads: Backup, Sub and the 50 folders"
    );
}

#[tokio::test]
async fn a_link_on_the_mac_where_a_folder_should_go_takes_nothing_out_of_the_folder() {
    let m = mirror().await;
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), m.mac.path().join("Sub")).unwrap();

    let summary = m.run(TO_MAC, false).await.unwrap();

    assert!(
        Mirror::files(elsewhere.path()).is_empty(),
        "nothing written through the link"
    );
    assert_eq!(summary.files, 2, "a.jpg and b.jpg");
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].label, "Sub");
    assert_eq!(summary.mirrored.unwrap().conflicts, 1);
}

#[tokio::test]
async fn a_file_on_the_mac_where_a_folder_should_go_is_left_and_named() {
    let m = mirror().await;
    // The Mac has a file where the phone has the folder Sub: a conflict.
    put(m.mac.path(), "Sub", b"mine", 5);

    let summary = m.run(TO_MAC, true).await.unwrap();

    assert_eq!(fs::read(m.mac.path().join("Sub")).unwrap(), b"mine");
    assert_eq!(summary.mirrored.unwrap().conflicts, 1);
}

#[tokio::test]
async fn a_reading_from_before_a_reconnect_is_not_used() {
    let m = mirror().await;
    m.preview(TO_MAC, false).await;
    let after_preview = m.explorer.full_reads();
    m.device.disconnect().await.unwrap();
    m.device.connect().await.unwrap();

    m.run(TO_MAC, false).await.unwrap();

    assert!(
        m.explorer.full_reads() > after_preview,
        "read again on the new connection"
    );
}

/// The Mac folder for a mirror onto the phone: a.jpg as the phone has it,
/// b.jpg changed, a new e.jpg, and a new folder New with d.jpg and a hidden
/// file.
fn mac_side(m: &Mirror) {
    put(m.mac.path(), "a.jpg", b"aaaa", 1_000_000);
    put(m.mac.path(), "b.jpg", b"bbbbbb", 2_000_000);
    put(m.mac.path(), "e.jpg", b"e", 2_000_000);
    put(m.mac.path(), "New/d.jpg", b"dd", 2_000_000);
    put(m.mac.path(), "New/.secret", b"s", 2_000_000);
}

#[tokio::test]
async fn onto_the_phone_new_files_and_folders_go_over_and_a_changed_file_is_sent_again() {
    let m = mirror().await;
    mac_side(&m);

    let preview = m.preview(TO_PHONE, false).await;
    let summary = m.run(TO_PHONE, false).await.unwrap();

    assert_eq!(
        (preview.new_files, preview.changed_files, preview.extras),
        (2, 1, 1)
    );
    assert_eq!(preview.samples.new, ["New/d.jpg", "e.jpg"]);
    assert_eq!(preview.samples.extras, ["Sub"]);
    let mirrored = summary.mirrored.clone().unwrap();
    assert_eq!(
        (mirrored.copied, mirrored.replaced, mirrored.kept),
        (2, 1, 1)
    );
    assert_eq!(summary.files, 3);
    assert_eq!(
        m.on_phone(),
        set(&[
            ".thumbnails/t.jpg",
            "New/d.jpg",
            "Sub/c.jpg",
            "a.jpg",
            "b.jpg",
            "e.jpg"
        ]),
        "the hidden file stays on the Mac"
    );
    assert_eq!(
        fs::read(m.phone.path().join("Backup/b.jpg")).unwrap(),
        b"bbbbbb"
    );

    let again = m.preview(TO_PHONE, false).await;
    assert_eq!((again.new_files, again.changed_files), (0, 0), "{again:?}");
}

#[tokio::test]
async fn onto_the_phone_extras_are_deleted_only_when_asked() {
    let m = mirror().await;
    mac_side(&m);

    let summary = m.run(TO_PHONE, true).await.unwrap();

    assert_eq!(summary.mirrored.unwrap().removed, 1);
    assert!(!m.phone.path().join("Backup/Sub").exists());
    assert!(
        m.phone.path().join("Backup/.thumbnails/t.jpg").exists(),
        "hidden stays"
    );
}

#[tokio::test]
async fn onto_the_phone_a_folder_is_found_whatever_its_case() {
    let m = mirror().await;
    put(m.mac.path(), "SUB/x.jpg", b"x", 5);

    let summary = m.run(TO_PHONE, false).await.unwrap();

    assert_eq!(summary.files, 1);
    assert!(m.phone.path().join("Backup/Sub/x.jpg").exists());
}

#[tokio::test]
async fn after_a_mirror_onto_the_phone_its_pane_shows_what_is_there_now() {
    let m = mirror().await;
    let before = m.explorer.list(m.storage, "/Backup", false).await.unwrap();
    mac_side(&m);

    m.run(TO_PHONE, true).await.unwrap();
    let after = m.explorer.list(m.storage, "/Backup", false).await.unwrap();

    let names = |entries: &[crate::dto::Entry]| {
        entries
            .iter()
            .map(|e| e.name.clone())
            .collect::<BTreeSet<_>>()
    };
    assert!(names(&before).contains("Sub"));
    assert_eq!(names(&after), set(&["New", "a.jpg", "b.jpg", "e.jpg"]));
}

#[tokio::test]
async fn onto_the_phone_a_changed_file_that_cannot_be_sent_is_not_counted_as_replaced() {
    use std::os::unix::fs::PermissionsExt;
    let m = mirror().await;
    mac_side(&m);
    let unreadable = m.mac.path().join("b.jpg");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

    let summary = m.run(TO_PHONE, false).await;
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();

    let summary = summary.unwrap();
    assert_eq!(summary.mirrored.unwrap().replaced, 0);
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].label, "b.jpg");
}

#[tokio::test]
async fn a_link_on_the_mac_where_a_changed_file_is_is_not_written_through() {
    let m = mirror().await;
    let elsewhere = tempfile::tempdir().unwrap();
    put(elsewhere.path(), "b.jpg", b"theirs", 5);
    std::os::unix::fs::symlink(elsewhere.path().join("b.jpg"), m.mac.path().join("b.jpg")).unwrap();

    assert_eq!(m.preview(TO_MAC, false).await.changed_files, 1);
    let summary = m.run(TO_MAC, false).await.unwrap();

    assert_eq!(fs::read(elsewhere.path().join("b.jpg")).unwrap(), b"theirs");
    assert!(
        fs::symlink_metadata(m.mac.path().join("b.jpg"))
            .unwrap()
            .is_symlink()
    );
    let mirrored = summary.mirrored.unwrap();
    assert_eq!(
        (mirrored.copied, mirrored.replaced),
        (2, 0),
        "a.jpg and Sub/c.jpg"
    );
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].label, "b.jpg");
    assert!(m.trashed.lock().unwrap().is_empty());
}
