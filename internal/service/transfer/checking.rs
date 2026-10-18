//! Checking copies byte for byte on the virtual phone: what is found the
//! same, different, missing or unreadable, and that nothing is ever written.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::Service;
use crate::dto::{
    Cancel, CheckRequest, Compared, Error, FileOutcome, FileReport, TransferProgress,
    TransferSummary,
};
use crate::service::testing::virtual_phone;
use crate::service::{device, explorer};

const MIB: usize = 1024 * 1024;

struct Check {
    phone: tempfile::TempDir,
    mac: tempfile::TempDir,
    transfer: Service,
    storage: u64,
}

fn put(root: &Path, path: &str, content: &[u8]) {
    let file = root.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, content).unwrap();
    // The same date on both sides: what a transfer would call already there.
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(1_000_000))
        .unwrap();
}

/// A big file whose bytes are not all alike, so a shifted window would show.
fn big(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Phone and Mac both: Backup/a.jpg, Backup/Sub/c.jpg, notes.txt and
/// video.mp4 (5 MB, three windows).
async fn check_with(limit: impl FnOnce(&mut device::VirtualPhone)) -> Check {
    let phone = tempfile::tempdir().unwrap();
    let mac = tempfile::tempdir().unwrap();
    for root in [phone.path(), mac.path()] {
        put(root, "Backup/a.jpg", b"aaaa");
        put(root, "Backup/Sub/c.jpg", b"ccc");
        put(root, "notes.txt", b"hello");
        put(root, "video.mp4", &big(5 * MIB));
    }
    let mut config = virtual_phone(phone.path());
    limit(&mut config);
    let device = Arc::new(device::Service::new(device::Source::Virtual(config)));
    device.connect().await.unwrap();
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    Check {
        phone,
        mac,
        transfer: Service::new(device, explorer),
        storage,
    }
}

async fn check() -> Check {
    check_with(|_| {}).await
}

impl Check {
    fn request(&self, names: &[&str]) -> CheckRequest {
        CheckRequest {
            storage_id: self.storage,
            phone_folder: "/".into(),
            mac_folder: self.mac.path().to_string_lossy().into_owned(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
        }
    }

    async fn run(&self, names: &[&str]) -> Result<TransferSummary, Error> {
        self.transfer
            .check_contents(&self.request(names), &Cancel::new(), |_| {})
            .await
    }

    /// Every file on the Mac with its bytes and date: what the check must
    /// leave exactly as it was.
    fn mac_state(&self) -> Vec<(String, Vec<u8>, SystemTime)> {
        let mut found = Vec::new();
        walk(self.mac.path(), self.mac.path(), &mut found);
        found.sort();
        found
    }
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<(String, Vec<u8>, SystemTime)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(root, &path, found);
        } else {
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let when = fs::metadata(&path).unwrap().modified().unwrap();
            found.push((name, fs::read(&path).unwrap_or_default(), when));
        }
    }
}

fn outcomes(summary: &TransferSummary) -> Vec<(&str, &FileOutcome)> {
    let mut lines: Vec<_> = summary
        .report
        .iter()
        .map(|line: &FileReport| (line.path.as_str(), &line.outcome))
        .collect();
    lines.sort_by_key(|(path, _)| *path);
    lines
}

#[tokio::test]
async fn copies_that_are_the_same_are_said_identical_and_nothing_is_written() {
    let c = check().await;
    let before = c.mac_state();

    let summary = c.run(&["Backup", "notes.txt", "video.mp4"]).await.unwrap();

    assert_eq!(
        outcomes(&summary),
        [
            ("Backup/Sub/c.jpg", &FileOutcome::Identical),
            ("Backup/a.jpg", &FileOutcome::Identical),
            ("notes.txt", &FileOutcome::Identical),
            ("video.mp4", &FileOutcome::Identical),
        ]
    );
    assert_eq!(
        summary.compared,
        Some(Compared {
            identical: 4,
            ..Compared::default()
        })
    );
    assert_eq!(summary.files, 4);
    assert_eq!(summary.bytes, (5 * MIB + 12) as u64, "every byte read once");
    assert_eq!(
        c.mac_state(),
        before,
        "not a byte, a date or a file changed"
    );
}

#[tokio::test]
async fn one_changed_byte_in_a_same_size_same_date_copy_is_found_where_it_is() {
    let c = check().await;
    let mut damaged = big(5 * MIB);
    let at = 3 * MIB + 7;
    damaged[at] ^= 0x01;
    put(c.mac.path(), "video.mp4", &damaged);
    let seen = Mutex::new(Vec::<TransferProgress>::new());

    let summary = c
        .transfer
        .check_contents(
            &c.request(&["video.mp4", "notes.txt"]),
            &Cancel::new(),
            |p| seen.lock().unwrap().push(p),
        )
        .await
        .unwrap();

    assert_eq!(
        outcomes(&summary),
        [
            ("notes.txt", &FileOutcome::Identical),
            ("video.mp4", &FileOutcome::Differs { at: at as u64 }),
        ]
    );
    assert_eq!(summary.compared.unwrap().differ, 1);
    assert_eq!(
        summary.bytes,
        (4 * MIB + 5) as u64,
        "reading stopped after the window that differed"
    );
    let last = seen.into_inner().unwrap().pop().unwrap();
    assert_eq!(
        (last.bytes_done, last.files_done),
        (last.bytes_total, 2),
        "what wasn't read left the totals: the bar ends full"
    );
}

#[tokio::test]
async fn a_copy_of_another_size_is_told_without_reading_the_phone() {
    let c = check().await;
    put(c.mac.path(), "notes.txt", b"hello, world");

    let summary = c.run(&["notes.txt"]).await.unwrap();

    assert_eq!(
        outcomes(&summary),
        [("notes.txt", &FileOutcome::SizesDiffer { phone: 5, mac: 12 })]
    );
    assert_eq!(summary.compared.unwrap().differ, 1, "another size differs");
    assert_eq!(summary.bytes, 0);
}

#[tokio::test]
async fn what_is_on_one_side_only_is_said_missing() {
    let c = check().await;
    fs::remove_file(c.mac.path().join("Backup/Sub/c.jpg")).unwrap();
    fs::remove_file(c.mac.path().join("notes.txt")).unwrap();
    fs::create_dir(c.mac.path().join("notes.txt")).unwrap();

    let summary = c.run(&["Backup", "notes.txt", "gone.jpg"]).await.unwrap();

    assert_eq!(
        outcomes(&summary),
        [
            ("Backup/Sub/c.jpg", &FileOutcome::NotOnMac),
            ("Backup/a.jpg", &FileOutcome::Identical),
            ("gone.jpg", &FileOutcome::NotOnPhone),
            ("notes.txt", &FileOutcome::NotOnMac),
        ],
        "a folder where the file should be is no copy of it"
    );
    assert_eq!(
        summary.compared,
        Some(Compared {
            identical: 1,
            missing: 3,
            ..Compared::default()
        })
    );
}

#[tokio::test]
async fn a_mac_file_that_cannot_be_read_is_that_files_problem_and_the_rest_go_on() {
    let c = check().await;
    let locked = c.mac.path().join("notes.txt");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    let summary = c.run(&["notes.txt", "Backup"]).await;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    let summary = summary.unwrap();

    let lines = outcomes(&summary);
    assert!(
        matches!(lines[2], ("notes.txt", FileOutcome::Failed(_))),
        "{lines:?}"
    );
    assert_eq!(summary.compared.unwrap().unreadable, 1);
    assert_eq!(summary.compared.unwrap().identical, 2);
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].label, "notes.txt");
}

#[tokio::test]
async fn cancel_stops_between_windows() {
    let c = check().await;
    let cancel = Cancel::new();
    let trigger = cancel.clone();
    let seen = Mutex::new(Vec::<TransferProgress>::new());

    let result = c
        .transfer
        .check_contents(&c.request(&["video.mp4", "notes.txt"]), &cancel, |p| {
            if p.file_bytes_done > 0 {
                trigger.cancel();
            }
            seen.lock().unwrap().push(p);
        })
        .await;

    assert_eq!(result, Err(Error::Cancelled));
    let seen = seen.into_inner().unwrap();
    let last = seen.last().unwrap();
    assert_eq!(
        last.bytes_done,
        (2 * MIB) as u64,
        "one window, then it stopped"
    );
    assert_eq!(seen[0].bytes_total, (5 * MIB + 5) as u64);
    assert_eq!(seen[0].files_total, 2);
}

#[tokio::test]
async fn a_phone_that_sends_whole_files_only_is_read_to_the_end_but_still_found_different() {
    let c = check_with(|phone| phone.partial_reads = false).await;
    let mut damaged = big(5 * MIB);
    damaged[10] ^= 0xff;
    put(c.mac.path(), "video.mp4", &damaged);

    let summary = c.run(&["video.mp4"]).await.unwrap();

    assert_eq!(
        outcomes(&summary),
        [("video.mp4", &FileOutcome::Differs { at: 10 })]
    );
    assert_eq!(
        summary.bytes,
        (5 * MIB) as u64,
        "breaking off its one long read could wedge it"
    );
}

#[tokio::test]
async fn a_mac_folder_that_is_not_there_is_refused_before_the_phone_is_asked() {
    let c = check().await;
    let mut request = c.request(&["notes.txt"]);
    request.mac_folder = c.mac.path().join("nowhere").to_string_lossy().into_owned();

    let result = c
        .transfer
        .check_contents(&request, &Cancel::new(), |_| {})
        .await;

    assert!(matches!(result, Err(Error::PathNotFound(_))), "{result:?}");
    let _ = &c.phone;
}

#[tokio::test]
async fn where_the_mac_file_ends_early_or_goes_on_is_where_they_differ() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    fs::write(&path, b"abc").unwrap();
    let mut buffer = Vec::new();

    let mut file = tokio::fs::File::open(&path).await.unwrap();
    assert_eq!(
        super::check::first_difference(&mut file, b"abcd", &mut buffer)
            .await
            .unwrap(),
        Some(3)
    );
    let mut file = tokio::fs::File::open(&path).await.unwrap();
    assert_eq!(
        super::check::first_difference(&mut file, b"abc", &mut buffer)
            .await
            .unwrap(),
        None
    );
    let mut file = tokio::fs::File::open(&path).await.unwrap();
    assert_eq!(
        super::check::first_difference(&mut file, b"aXc", &mut buffer)
            .await
            .unwrap(),
        Some(1)
    );
}

#[tokio::test]
async fn a_name_from_the_phone_that_could_lead_out_of_the_folder_is_never_joined() {
    let mac = tempfile::tempdir().unwrap();
    let mut object = mtp_rs::mtp::ObjectInfo::default();
    object.filename = "x".into();
    object.size = 1;

    let refused = super::check::pair(mac.path(), "Backup/../../etc".into(), &object)
        .await
        .map_err(|line| *line);

    assert!(
        matches!(
            refused,
            Err(FileReport {
                outcome: FileOutcome::Failed(Error::UnsafeName(_)),
                ..
            })
        ),
        "{:?}",
        refused.err()
    );
}
