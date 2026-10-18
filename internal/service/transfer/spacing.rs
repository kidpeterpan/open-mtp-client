//! Free Up Space on the virtual phone: which files are found to be copies,
//! which photos count as already imported, and what removing does.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use chrono::TimeZone;

use super::{ImportRecord, Service};
use crate::dto::{Cancel, Error, FreeUpRequest, ImportRequest, SpaceProgress, SpaceReport, Why};
use crate::service::testing::virtual_phone;
use crate::service::{device, explorer};

struct Space {
    phone: tempfile::TempDir,
    mac: tempfile::TempDir,
    _record: tempfile::TempDir,
    transfer: Service,
    device: Arc<device::Service>,
    storage: u64,
}

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

/// 300 KB that no window or end can mistake for another file's.
fn photo() -> Vec<u8> {
    (0..300 * 1024).map(|i| (i * 7 % 253) as u8).collect()
}

/// One photo in three places (the copy in Download the oldest, then the
/// camera's, then WhatsApp's); two strangers of its size, one differing in
/// its last byte and one only in the middle (past both ends); an empty file
/// twice; and files of sizes no other has.
async fn space_with(configure: impl FnOnce(&mut device::VirtualPhone)) -> Space {
    let phone = tempfile::tempdir().unwrap();
    let root = phone.path();
    let original = photo();
    put(root, "Download/IMG_1.jpg", &original, 10);
    put(root, "DCIM/Camera/IMG_1.jpg", &original, 12);
    put(root, "WhatsApp/Media/IMG-1.jpg", &original, 14);
    let mut last = original.clone();
    *last.last_mut().unwrap() ^= 1;
    put(root, "Pictures/last.jpg", &last, 11);
    let mut middle = original.clone();
    middle[150 * 1024] ^= 1;
    put(root, "Pictures/middle.jpg", &middle, 11);
    put(root, "a.txt", b"", 11);
    put(root, "b.txt", b"", 11);
    put(root, "notes.txt", b"hello", 11);
    put(root, "DCIM/Camera/IMG_2.jpg", b"only this", 13);

    let mut config = virtual_phone(root);
    configure(&mut config);
    let device = Arc::new(device::Service::new(device::Source::Virtual(config)));
    device.connect().await.unwrap();
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    let record = tempfile::tempdir().unwrap();
    Space {
        transfer: Service::new(Arc::clone(&device), explorer)
            .with_imports(ImportRecord::at(record.path().join("imported"))),
        phone,
        mac: tempfile::tempdir().unwrap(),
        _record: record,
        device,
        storage,
    }
}

async fn space() -> Space {
    space_with(|_| {}).await
}

impl Space {
    async fn report(&self) -> SpaceReport {
        self.transfer
            .space_report(self.storage, &Cancel::new(), |_| {})
            .await
            .unwrap()
    }

    async fn import(&self) {
        self.transfer
            .import(
                &ImportRequest {
                    storage_id: self.storage,
                    destination: self.mac.path().to_string_lossy().into_owned(),
                    folders: crate::dto::ImportFolders::None,
                    remove_from_phone: false,
                },
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();
    }

    async fn free(&self, paths: &[&str]) -> Result<crate::dto::Removal, Error> {
        self.transfer
            .free_up(
                &FreeUpRequest {
                    storage_id: self.storage,
                    paths: paths.iter().map(|p| (*p).to_owned()).collect(),
                },
                &Cancel::new(),
                |_| {},
            )
            .await
    }
}

fn paths(files: &[crate::dto::SpaceFile]) -> Vec<&str> {
    files.iter().map(|file| file.path.as_str()).collect()
}

#[tokio::test]
async fn only_files_the_same_byte_for_byte_are_copies_and_the_oldest_is_kept() {
    let s = space().await;
    let seen = Mutex::new(Vec::new());

    let report = s
        .transfer
        .space_report(s.storage, &Cancel::new(), |p| seen.lock().unwrap().push(p))
        .await
        .unwrap();

    assert_eq!(report.duplicates.len(), 1, "{:?}", report.duplicates);
    let group = &report.duplicates[0];
    assert_eq!(
        paths(&group.files),
        [
            "/Download/IMG_1.jpg",
            "/DCIM/Camera/IMG_1.jpg",
            "/WhatsApp/Media/IMG-1.jpg"
        ],
        "the oldest first, as the one to keep"
    );
    assert_eq!(group.size, 300 * 1024);
    assert_eq!(report.duplicate_bytes(), 2 * 300 * 1024);
    assert_eq!(report.files, 9);
    // The stranger differing at its end was told apart by its ends alone;
    // the one differing in the middle had to be read in full.
    let compared = seen
        .into_inner()
        .unwrap()
        .into_iter()
        .filter_map(|p| match p {
            SpaceProgress::Comparing { total, .. } => Some(total),
            SpaceProgress::Walking { .. } => None,
        })
        .max();
    assert_eq!(compared, Some(4 * 300 * 1024));
}

#[tokio::test]
async fn the_cameras_own_copy_is_kept_when_both_have_one_date() {
    let s = space().await;
    // "/Backup" comes before "/DCIM" by path: only the camera rule puts the
    // camera's copy first.
    fs::remove_file(s.phone.path().join("Download/IMG_1.jpg")).unwrap();
    fs::remove_file(s.phone.path().join("WhatsApp/Media/IMG-1.jpg")).unwrap();
    put(s.phone.path(), "Backup/IMG_1.jpg", &photo(), 12);

    let report = s.report().await;

    assert_eq!(
        paths(&report.duplicates[0].files),
        ["/DCIM/Camera/IMG_1.jpg", "/Backup/IMG_1.jpg"]
    );
}

#[tokio::test]
async fn a_phone_that_sends_whole_files_only_is_read_in_full_and_still_right() {
    let s = space_with(|phone| phone.partial_reads = false).await;

    let report = s.report().await;

    assert_eq!(report.duplicates.len(), 1);
    assert_eq!(report.duplicates[0].files.len(), 3);
}

#[tokio::test]
async fn photos_already_imported_are_offered_only_while_their_copy_is_where_it_went() {
    let s = space().await;
    assert!(s.report().await.imported.is_empty(), "nothing imported yet");
    s.import().await;
    // The user moved one copy away on the Mac: it isn't safe to offer.
    fs::rename(
        s.mac.path().join("IMG_2.jpg"),
        s.mac.path().join("elsewhere.jpg"),
    )
    .unwrap();

    let report = s.report().await;

    assert_eq!(paths(&report.imported), ["/DCIM/Camera/IMG_1.jpg"]);
    assert_eq!(report.imported[0].size, 300 * 1024);

    // A copy still there but changed (another file put in its place) isn't
    // the photo any more.
    put(s.mac.path(), "IMG_1.jpg", b"edited on the Mac", 12);
    assert!(s.report().await.imported.is_empty());
}

#[tokio::test]
async fn only_dcim_counts_whatever_a_hand_edited_record_says() {
    let s = space().await;
    // A line for a file outside DCIM, its copy in place at the same size
    // and date.
    let copy = s.mac.path().join("IMG_1.jpg");
    put(s.mac.path(), "IMG_1.jpg", &photo(), 10);
    let hex: String = s
        .device
        .connect()
        .await
        .unwrap()
        .serial_number
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = s._record.path().join("imported");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{hex}.tsv")),
        format!(
            "/Download/IMG_1.jpg\t{}\t2026-10-10 12:00:00\t{}\n",
            300 * 1024,
            copy.display()
        ),
    )
    .unwrap();

    assert!(s.report().await.imported.is_empty());
}

#[tokio::test]
async fn removing_takes_only_what_is_still_as_it_was_seen_and_says_what_it_freed() {
    let s = space().await;
    let _ = s.report().await;
    // Changed on the phone since it was looked at.
    put(s.phone.path(), "WhatsApp/Media/IMG-1.jpg", b"edited", 14);

    let removal = s
        .free(&[
            "/Download/IMG_1.jpg",
            "/WhatsApp/Media/IMG-1.jpg",
            "/DCIM",
            "/nowhere.jpg",
        ])
        .await
        .unwrap();

    assert_eq!(removal.removed, 1);
    assert_eq!(removal.freed, 300 * 1024);
    assert!(!s.phone.path().join("Download/IMG_1.jpg").exists());
    assert!(s.phone.path().join("WhatsApp/Media/IMG-1.jpg").exists());
    assert!(s.phone.path().join("DCIM/Camera/IMG_1.jpg").exists());
    let kept: Vec<(&str, &Why)> = removal
        .kept
        .iter()
        .map(|kept| (kept.label.as_str(), &kept.why))
        .collect();
    assert_eq!(
        kept,
        [
            ("/WhatsApp/Media/IMG-1.jpg", &Why::Changed),
            ("/DCIM", &Why::Changed),
            ("/nowhere.jpg", &Why::Changed),
        ],
        "a folder is never removed, and nothing the look didn't see"
    );

    // Removed once is not removed again, nor asked about: the look forgets it.
    let again = s.free(&["/Download/IMG_1.jpg"]).await.unwrap();
    assert_eq!(again.removed, 0);
    assert_eq!(again.kept[0].why, Why::Changed);
}

#[tokio::test]
async fn nothing_is_removed_without_a_look_on_this_connection() {
    let s = space().await;

    let removal = s.free(&["/Download/IMG_1.jpg"]).await.unwrap();

    assert_eq!(removal.removed, 0);
    assert!(s.phone.path().join("Download/IMG_1.jpg").exists());

    // A look before the phone was opened again is about handles it may
    // have changed.
    let _ = s.report().await;
    s.device.disconnect().await.unwrap();
    s.device.connect().await.unwrap();
    let removal = s.free(&["/Download/IMG_1.jpg"]).await.unwrap();
    assert_eq!(removal.removed, 0);
    assert!(s.phone.path().join("Download/IMG_1.jpg").exists());
}

#[tokio::test]
async fn cancel_stops_the_look_between_windows_and_removing_between_files() {
    let s = space().await;
    let cancel = Cancel::new();
    let trigger = cancel.clone();

    let result = s
        .transfer
        .space_report(s.storage, &cancel, |p| {
            if matches!(p, SpaceProgress::Comparing { done, .. } if done > 0) {
                trigger.cancel();
            }
        })
        .await;
    assert_eq!(result, Err(Error::Cancelled));

    let _ = s.report().await;
    let stopped = Cancel::new();
    stopped.cancel();
    let removal = s
        .transfer
        .free_up(
            &FreeUpRequest {
                storage_id: s.storage,
                paths: vec!["/Download/IMG_1.jpg".into()],
            },
            &stopped,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        (removal.removed, removal.stopped),
        (0, Some(Error::Cancelled))
    );
    assert!(s.phone.path().join("Download/IMG_1.jpg").exists());
}

#[tokio::test]
async fn a_lost_phone_ends_the_look() {
    let s = space().await;
    s.device.disconnect().await.unwrap();

    let result = s
        .transfer
        .space_report(s.storage, &Cancel::new(), |_| {})
        .await;

    assert!(result.is_err(), "{result:?}");
}
