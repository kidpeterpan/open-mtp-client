//! Importing new photos and videos from a phone's DCIM folder, on the virtual
//! phone: what counts as new, where it goes, and what is never imported twice.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use chrono::TimeZone;

use super::{ImportRecord, Service};
use crate::dto::{
    Cancel, Error, FileOutcome, ImportPreview, ImportRequest, Original, TransferSummary,
};
use crate::service::device;
use crate::service::explorer;
use crate::service::testing::virtual_phone;

struct Import {
    phone: tempfile::TempDir,
    mac: tempfile::TempDir,
    /// Where the record of imports is kept: the test's own folder.
    record: tempfile::TempDir,
    transfer: Service,
    explorer: Arc<explorer::Service>,
    device: Arc<device::Service>,
    storage: u64,
    serial: String,
}

/// Noon on a day, the Mac's local time (what a phone writes, zoneless).
fn noon(day: u32) -> SystemTime {
    chrono::Local
        .with_ymd_and_hms(2026, 10, day, 12, 0, 0)
        .unwrap()
        .into()
}

fn put(root: &Path, path: &str, content: &[u8], day: u32) {
    let at = root.join(path);
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    fs::write(&at, content).unwrap();
    fs::File::options()
        .write(true)
        .open(&at)
        .unwrap()
        .set_modified(noon(day))
        .unwrap();
}

/// DCIM/Camera/a.jpg (17th) and b.mp4 (16th); DCIM/Screenshots/a.jpg (17th,
/// another photo of the same name); things never imported: a note in DCIM,
/// hidden thumbnails and trash, and music outside DCIM.
async fn importer() -> Import {
    let phone = tempfile::tempdir().unwrap();
    let root = phone.path();
    put(root, "DCIM/Camera/a.jpg", b"aaaa", 17);
    put(root, "DCIM/Camera/b.mp4", b"bbbbbb", 16);
    put(root, "DCIM/Screenshots/a.jpg", b"ss", 17);
    put(root, "DCIM/Camera/notes.txt", b"n", 17);
    put(root, "DCIM/.thumbnails/t.jpg", b"t", 17);
    put(root, "DCIM/.trashed-1-x.jpg", b"x", 17);
    put(root, "Music/song.mp3", b"m", 17);
    on_phone(phone).await
}

async fn on_phone(phone: tempfile::TempDir) -> Import {
    on_phone_with(phone, |_| {}).await
}

async fn on_phone_with(
    phone: tempfile::TempDir,
    configure: impl FnOnce(&mut device::VirtualPhone),
) -> Import {
    let mut config = virtual_phone(phone.path());
    configure(&mut config);
    let device = Arc::new(device::Service::new(device::Source::Virtual(config)));
    let serial = device.connect().await.unwrap().serial_number;
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    let record = tempfile::tempdir().unwrap();
    Import {
        transfer: Service::new(Arc::clone(&device), Arc::clone(&explorer))
            .with_imports(ImportRecord::at(record.path().join("imported"))),
        phone,
        mac: tempfile::tempdir().unwrap(),
        record,
        explorer,
        device,
        storage,
        serial,
    }
}

impl Import {
    fn request(&self, by_date: bool) -> ImportRequest {
        self.request_as(if by_date {
            crate::dto::ImportFolders::Day
        } else {
            crate::dto::ImportFolders::None
        })
    }

    fn request_as(&self, folders: crate::dto::ImportFolders) -> ImportRequest {
        ImportRequest {
            storage_id: self.storage,
            destination: self.mac.path().to_string_lossy().into_owned(),
            folders,
            remove_from_phone: false,
        }
    }

    async fn preview(&self, by_date: bool) -> ImportPreview {
        self.transfer
            .import_preview(&self.request(by_date), &Cancel::new(), |_| {})
            .await
            .unwrap()
    }

    async fn import(&self, by_date: bool) -> TransferSummary {
        self.transfer
            .import(&self.request(by_date), &Cancel::new(), |_| {})
            .await
            .unwrap()
    }

    /// Every file in the Mac folder, as "folder/name".
    fn on_mac(&self) -> BTreeSet<String> {
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
        walk(self.mac.path(), self.mac.path(), &mut out);
        out
    }

    fn mac(&self, path: &str) -> Vec<u8> {
        fs::read(self.mac.path().join(path)).unwrap()
    }
}

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

#[tokio::test]
async fn the_preview_counts_the_photos_and_videos_in_dcim_and_nothing_else() {
    let i = importer().await;
    let seen = std::sync::Mutex::new(Vec::new());

    let preview = i
        .transfer
        .import_preview(&i.request(false), &Cancel::new(), |n| {
            seen.lock().unwrap().push(n)
        })
        .await
        .unwrap();

    assert_eq!(
        preview,
        ImportPreview {
            new_files: 3,
            new_bytes: 4 + 6 + 2,
            already: 0,
        }
    );
    assert_eq!(
        seen.lock().unwrap().last(),
        Some(&3),
        "told as the walk went"
    );
    assert!(i.on_mac().is_empty(), "a preview copies nothing");
}

#[tokio::test]
async fn an_import_puts_every_new_photo_in_the_folder_keeping_both_of_a_taken_name() {
    let i = importer().await;

    let summary = i.import(false).await;

    assert_eq!(summary.files, 3);
    assert_eq!(summary.already_there, 0);
    assert_eq!(summary.renamed, 1);
    assert_eq!(i.on_mac(), set(&["a (1).jpg", "a.jpg", "b.mp4"]));
    assert_eq!(i.mac("a.jpg"), b"aaaa", "Camera comes before Screenshots");
    assert_eq!(i.mac("a (1).jpg"), b"ss");
    let modified = fs::metadata(i.mac.path().join("b.mp4"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(modified, noon(16), "the phone's date is kept");
}

#[tokio::test]
async fn by_date_each_photo_goes_in_a_folder_for_its_day() {
    let i = importer().await;

    let summary = i.import(true).await;

    assert_eq!(summary.files, 3);
    assert_eq!(
        i.on_mac(),
        set(&[
            "2026-10-16/b.mp4",
            "2026-10-17/a (1).jpg",
            "2026-10-17/a.jpg"
        ])
    );
}

#[tokio::test]
async fn importing_again_brings_only_what_is_new() {
    let i = importer().await;
    i.import(true).await;
    assert_eq!(i.preview(true).await.new_files, 0);

    put(i.phone.path(), "DCIM/Camera/c.jpg", b"ccc", 18);
    mtp_rs::rescan_virtual_device(&i.serial);
    let preview = i.preview(true).await;
    let summary = i.import(true).await;

    assert_eq!((preview.new_files, preview.already), (1, 3));
    assert_eq!((summary.files, summary.already_there), (1, 3));
    assert!(i.on_mac().contains("2026-10-18/c.jpg"));
}

#[tokio::test]
async fn a_photo_moved_or_deleted_on_the_mac_is_not_brought_back_nor_after_changing_the_layout() {
    let i = importer().await;
    i.import(false).await;
    fs::remove_file(i.mac.path().join("a.jpg")).unwrap();

    // Dated now: nothing is where these would go, but they were imported.
    let preview = i.preview(true).await;
    let summary = i.import(true).await;

    assert_eq!((preview.new_files, preview.already), (0, 3));
    assert_eq!(summary.files, 0);
    assert_eq!(i.on_mac(), set(&["a (1).jpg", "b.mp4"]));
}

#[tokio::test]
async fn without_a_record_the_destination_alone_decides() {
    let i = importer().await;
    let plain = Service::new(Arc::clone(&i.device), Arc::clone(&i.explorer));
    plain
        .import(&i.request(false), &Cancel::new(), |_| {})
        .await
        .unwrap();

    let again = plain
        .import_preview(&i.request(false), &Cancel::new(), |_| {})
        .await
        .unwrap();
    let elsewhere = plain
        .import_preview(&i.request(true), &Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(
        (again.new_files, again.already),
        (0, 3),
        "same size and date there"
    );
    assert_eq!(elsewhere.new_files, 3, "which is why the record is kept");
}

#[tokio::test]
async fn a_folder_a_month_or_a_year_and_month_and_switching_copies_nothing_twice() {
    let month = importer().await;
    month
        .transfer
        .import(
            &month.request_as(crate::dto::ImportFolders::Month),
            &Cancel::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(
        month.on_mac().contains("2026-10/b.mp4"),
        "{:?}",
        month.on_mac()
    );

    let years = importer().await;
    let summary = years
        .transfer
        .import(
            &years.request_as(crate::dto::ImportFolders::YearMonth),
            &Cancel::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(
        years.on_mac().contains("2026/10/b.mp4"),
        "{:?}",
        years.on_mac()
    );
    assert_eq!(summary.files, 3);

    // The same phone imported again by month: the record says it's all here.
    let again = years
        .transfer
        .import_preview(
            &years.request_as(crate::dto::ImportFolders::Month),
            &Cancel::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!((again.new_files, again.already), (0, 3));
}

#[tokio::test]
async fn the_record_is_the_phones_own_and_lists_what_was_checked() {
    let i = importer().await;

    i.import(false).await;

    let hex: String = i.serial.bytes().map(|b| format!("{b:02x}")).collect();
    let text = fs::read_to_string(i.record.path().join(format!("imported/{hex}.tsv"))).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|l| l.starts_with("/DCIM/")), "{lines:?}");
    let copy = i.mac.path().join("b.mp4");
    assert!(
        lines.contains(
            &format!(
                "/DCIM/Camera/b.mp4\t6\t2026-10-16 12:00:00\t{}",
                copy.display()
            )
            .as_str()
        ),
        "each with where its copy went: {lines:?}"
    );
}

#[tokio::test]
async fn a_record_written_before_targets_were_kept_still_keeps_photos_from_coming_back() {
    let dir = tempfile::tempdir().unwrap();
    let record = ImportRecord::at(dir.path().to_path_buf());
    let file = dir.path().join(format!(
        "{}.tsv",
        "AB".bytes().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    fs::write(
        &file,
        "/DCIM/a.jpg\t4\t2026-10-16 12:00:00\n/DCIM/b.jpg\t2\t2026-10-16 12:00:00\t/Users/me/b.jpg\n/DCIM/d.jpg\t2\t2026-10-16 12:00:00\trelative/d.jpg\nnot a line\n",
    )
    .unwrap();

    let keys = record.load("AB").await;
    assert!(keys.contains("/DCIM/a.jpg\t4\t2026-10-16 12:00:00"));
    assert!(keys.contains("/DCIM/b.jpg\t2\t2026-10-16 12:00:00"));
    assert_eq!(keys.len(), 3, "a line that isn't one is dropped");
    let targets = record.targets("AB").await;
    assert_eq!(targets.len(), 1, "only where the line says, as a full path");
    assert_eq!(
        targets["/DCIM/b.jpg\t2\t2026-10-16 12:00:00"],
        std::path::PathBuf::from("/Users/me/b.jpg")
    );

    // Adding keeps the old lines as they were; a path that can't sit on one
    // line keeps its key only.
    record
        .add(
            "AB",
            [(
                "/DCIM/c.jpg\t1\t2026-10-16 12:00:00".to_owned(),
                std::path::PathBuf::from("/Users/me/odd\nname.jpg"),
            )],
        )
        .await
        .unwrap();
    // A phone name with a tab or a line break could forge fields or lines:
    // never written.
    record
        .add(
            "AB",
            [
                (
                    "/DCIM/x.jpg\t1\t2026-10-16 12:00:00\t/Users/me/forged\n/DCIM/b.jpg\t2\t2026-10-16 12:00:00".to_owned(),
                    std::path::PathBuf::from("/Users/me/x.jpg"),
                ),
                (
                    "/DCIM/y\tz.jpg\t1\t2026-10-16 12:00:00".to_owned(),
                    std::path::PathBuf::from("/Users/me/y.jpg"),
                ),
            ],
        )
        .await
        .unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(!text.contains("forged") && !text.contains("y\tz"), "{text}");
    assert_eq!(
        record.targets("AB").await["/DCIM/b.jpg\t2\t2026-10-16 12:00:00"],
        std::path::PathBuf::from("/Users/me/b.jpg"),
        "another photo's line is as it was"
    );
    assert!(
        text.contains("/DCIM/a.jpg\t4\t2026-10-16 12:00:00\n"),
        "{text}"
    );
    assert!(text.contains("/DCIM/b.jpg\t2\t2026-10-16 12:00:00\t/Users/me/b.jpg\n"));
    assert!(
        text.contains("/DCIM/c.jpg\t1\t2026-10-16 12:00:00\n"),
        "{text}"
    );
    assert!(!text.contains("odd"));
}

#[tokio::test]
async fn a_record_that_cannot_be_written_does_not_fail_the_import() {
    let i = importer().await;
    // A file where the record's folder would be.
    let blocked = i.record.path().join("blocked");
    fs::write(&blocked, b"x").unwrap();
    let transfer = Service::new(Arc::clone(&i.device), Arc::clone(&i.explorer))
        .with_imports(ImportRecord::at(blocked));

    let summary = transfer
        .import(&i.request(false), &Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(summary.files, 3);
}

#[tokio::test]
async fn a_storage_without_dcim_says_so() {
    let phone = tempfile::tempdir().unwrap();
    put(phone.path(), "Music/song.mp3", b"m", 17);
    let i = on_phone(phone).await;

    let preview = i
        .transfer
        .import_preview(&i.request(false), &Cancel::new(), |_| {})
        .await;

    assert_eq!(preview, Err(Error::PathNotFound("/DCIM".into())));
}

#[tokio::test]
async fn an_import_into_a_folder_that_is_not_there_says_so_before_asking_the_phone() {
    let i = importer().await;
    let request = ImportRequest {
        destination: i.mac.path().join("gone").to_string_lossy().into_owned(),
        ..i.request(false)
    };

    let result = i.transfer.import(&request, &Cancel::new(), |_| {}).await;

    assert!(matches!(result, Err(Error::PathNotFound(_))), "{result:?}");
}

#[tokio::test]
async fn a_cancelled_preview_or_import_stops() {
    let i = importer().await;
    let cancel = Cancel::new();
    cancel.cancel();

    let preview = i
        .transfer
        .import_preview(&i.request(false), &cancel, |_| {})
        .await;
    let import = i.transfer.import(&i.request(false), &cancel, |_| {}).await;

    assert_eq!(preview, Err(Error::Cancelled));
    assert_eq!(import, Err(Error::Cancelled));
    assert!(i.on_mac().is_empty());
}

#[tokio::test]
async fn five_thousand_photos_are_looked_at_once_and_then_only_counted() {
    let phone = tempfile::tempdir().unwrap();
    for folder in 0..50 {
        for n in 0..100 {
            let at = phone
                .path()
                .join(format!("DCIM/F{folder:02}/IMG_{n:03}.jpg"));
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, b"x").unwrap();
        }
    }
    let i = on_phone(phone).await;

    let started = std::time::Instant::now();
    let first = i.preview(true).await;
    let first_reads = i.explorer.full_reads();
    let again_started = std::time::Instant::now();
    let summary = i.import(true).await;
    eprintln!(
        "5,000 photos: preview {:?}, import {:?}",
        again_started - started,
        again_started.elapsed()
    );

    assert_eq!(first.new_files, 5_000);
    assert_eq!(summary.files, 5_000);
    assert_eq!(
        i.explorer.full_reads(),
        first_reads,
        "the import reads no folder in full again after the preview"
    );
}

#[tokio::test]
async fn every_kind_of_photo_and_video_a_phone_takes_is_imported() {
    let phone = tempfile::tempdir().unwrap();
    let kinds = [
        "jpg", "JPEG", "heic", "heif", "dng", "png", "webp", "gif", "avif", "mp4", "MOV", "3gp",
        "mkv", "webm",
    ];
    for (n, extension) in kinds.iter().enumerate() {
        put(
            phone.path(),
            &format!("DCIM/Camera/f{n}.{extension}"),
            b"x",
            17,
        );
    }
    put(phone.path(), "DCIM/Camera/.nomedia", b"", 17);
    put(phone.path(), "DCIM/Camera/clip.srt", b"x", 17);
    let i = on_phone(phone).await;

    assert_eq!(i.preview(false).await.new_files, kinds.len());
}

#[tokio::test]
async fn photos_re_keyed_while_the_preview_walks_are_counted_once() {
    let phone = tempfile::tempdir().unwrap();
    for folder in ["A", "B"] {
        for n in 0..600 {
            let at = phone.path().join(format!("DCIM/{folder}/IMG_{n:03}.jpg"));
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, b"x").unwrap();
        }
    }
    let i = on_phone(phone).await;
    let rekeyed = std::sync::atomic::AtomicBool::new(false);

    let preview = i
        .transfer
        .import_preview(&i.request(false), &Cancel::new(), |_| {
            // What Android's media scanner does part way through.
            if !rekeyed.swap(true, std::sync::atomic::Ordering::Relaxed) {
                for folder in ["DCIM", "DCIM/A", "DCIM/B"] {
                    mtp_rs::rekey_virtual_object(&i.serial, Path::new(folder));
                }
            }
        })
        .await
        .unwrap();

    assert!(rekeyed.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(preview.new_files, 1_200);
}

#[tokio::test]
async fn a_phone_without_a_serial_number_gets_no_record() {
    let phone = tempfile::tempdir().unwrap();
    put(phone.path(), "DCIM/Camera/a.jpg", b"a", 17);
    let i = on_phone_with(phone, |config| config.serial = String::new()).await;

    let summary = i.import(false).await;

    assert_eq!(summary.files, 1);
    assert!(
        !i.record.path().join("imported").exists(),
        "one phone without a serial can't be told from the next"
    );
}

// Freeing the phone after an import.

impl Import {
    async fn import_freeing(&self) -> TransferSummary {
        self.transfer
            .import(
                &ImportRequest {
                    remove_from_phone: true,
                    ..self.request(false)
                },
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap()
    }

    fn on_phone(&self, path: &str) -> bool {
        self.phone.path().join(path).exists()
    }
}

#[tokio::test]
async fn a_freeing_import_removes_each_checked_original_and_never_a_folder() {
    let i = importer().await;

    let summary = i.import_freeing().await;

    let removal = summary.removal.expect("asked to free the phone");
    assert_eq!(removal.removed, 3);
    assert_eq!(removal.freed, 4 + 6 + 2);
    assert!(removal.kept.is_empty() && removal.stopped.is_none());
    assert_eq!(i.on_mac(), set(&["a.jpg", "a (1).jpg", "b.mp4"]));
    for gone in [
        "DCIM/Camera/a.jpg",
        "DCIM/Camera/b.mp4",
        "DCIM/Screenshots/a.jpg",
    ] {
        assert!(!i.on_phone(gone), "{gone}");
    }
    for stays in [
        "DCIM/Camera",
        "DCIM/Screenshots",
        "DCIM/Camera/notes.txt",
        "DCIM/.thumbnails/t.jpg",
        "Music/song.mp3",
    ] {
        assert!(i.on_phone(stays), "{stays}");
    }
    // The pane's memory of the folder is kept true: showing it again reads
    // nothing in full.
    let reads = i.explorer.full_reads();
    let camera = i
        .explorer
        .list(i.storage, "/DCIM/Camera", true)
        .await
        .unwrap();
    assert_eq!(i.explorer.full_reads(), reads, "remembered, amended");
    let names: Vec<&str> = camera.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["notes.txt"]);
}

#[tokio::test]
async fn without_asking_an_import_removes_nothing() {
    let i = importer().await;

    let summary = i.import(false).await;

    assert!(summary.removal.is_none());
    assert!(i.on_phone("DCIM/Camera/a.jpg") && i.on_phone("DCIM/Camera/b.mp4"));
}

#[tokio::test]
async fn a_photo_already_on_the_mac_as_it_is_counts_as_checked_and_goes() {
    let i = importer().await;
    put(i.mac.path(), "b.mp4", b"bbbbbb", 16);

    let summary = i.import_freeing().await;

    assert_eq!(summary.already_there, 1);
    assert_eq!(summary.removal.unwrap().removed, 3);
    assert!(!i.on_phone("DCIM/Camera/b.mp4"));
    assert_eq!(i.mac("b.mp4"), b"bbbbbb");
}

#[tokio::test]
async fn a_cancel_while_removing_leaves_the_rest_on_the_phone() {
    let i = importer().await;
    let cancel = Cancel::new();
    let trigger = cancel.clone();

    let summary = i
        .transfer
        .import(
            &ImportRequest {
                remove_from_phone: true,
                ..i.request(false)
            },
            &cancel,
            |p| {
                if p.removing {
                    trigger.cancel();
                }
            },
        )
        .await
        .unwrap();

    let removal = summary.removal.unwrap();
    assert_eq!(removal.stopped, Some(Error::Cancelled));
    assert_eq!(removal.removed, 0);
    assert_eq!(i.on_mac().len(), 3, "the copies are safe");
    assert!(i.on_phone("DCIM/Camera/a.jpg"));
}

#[tokio::test]
async fn a_cancelled_import_removes_nothing() {
    let i = importer().await;
    let cancel = Cancel::new();
    cancel.cancel();

    let result = i
        .transfer
        .import(
            &ImportRequest {
                remove_from_phone: true,
                ..i.request(false)
            },
            &cancel,
            |_| {},
        )
        .await;

    assert_eq!(result.err(), Some(Error::Cancelled));
    assert!(i.on_phone("DCIM/Camera/a.jpg"));
}

#[tokio::test]
async fn a_photo_that_did_not_copy_stays_on_the_phone() {
    use std::os::unix::fs::PermissionsExt;
    let i = importer().await;
    // b.mp4 (the 16th) goes into a day folder the Mac won't let it into.
    let locked = i.mac.path().join("2026-10-16");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

    let summary = i
        .transfer
        .import(
            &ImportRequest {
                remove_from_phone: true,
                ..i.request(true)
            },
            &Cancel::new(),
            |_| {},
        )
        .await
        .unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(summary.failures.len(), 1, "{:?}", summary.failures);
    assert!(
        i.on_phone("DCIM/Camera/b.mp4"),
        "not copied, so not removed"
    );
    assert_eq!(summary.removal.unwrap().removed, 2);
    assert!(!i.on_phone("DCIM/Camera/a.jpg"));
}

#[tokio::test]
async fn a_freeing_import_forces_its_copies_to_the_disk_first_and_a_plain_one_does_not() {
    let syncs = || super::service::SYNCS.with(std::cell::Cell::get);
    let plain = importer().await;
    let before = syncs();
    let _ = plain.import(false).await;
    assert_eq!(syncs(), before, "the originals stay: no need");

    let freeing = importer().await;
    let before = syncs();
    let _ = freeing.import_freeing().await;
    assert_eq!(
        syncs(),
        before + 3,
        "each copy is on the disk before its original goes"
    );
}

#[tokio::test]
async fn an_imports_report_lists_each_photo_and_a_freeing_ones_says_what_was_removed() {
    let i = importer().await;

    let summary = i.import(false).await;

    assert_eq!(summary.report.len(), summary.files);
    assert!(summary.report.iter().all(|file| file.outcome
        == FileOutcome::Copied { checked: true }
        && file.original.is_none()));
    // Another phone's worth, imported freeing the phone.
    let again = importer().await.import_freeing().await;
    assert!(!again.report.is_empty());
    assert!(
        again
            .report
            .iter()
            .all(|file| file.original == Some(Original::Removed)),
        "{:?}",
        again.report
    );
}
