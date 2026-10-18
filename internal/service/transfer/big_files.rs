//! Files of 4 GB or more, which MTP's 32-bit size fields cannot hold, so a client that
//! ignores them fails or hangs. The phone reports such a
//! file's size in a second request, offsets past 4 GB need the 64-bit read,
//! and every total, bar and speed here has to be 64-bit too.
//!
//! The files are sparse: the phone's 4.7 GB file takes 4 KB of disk, and the
//! tests that copy it only move the last few megabytes (by resuming), so the
//! suite stays fast. Two tests move a whole file; they are `#[ignore]`d and run
//! with `make test-large`.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use super::Service;
use crate::dto::{Cancel, Conflict, DownloadRequest, TransferProgress, UploadRequest};
use crate::service::device;
use crate::service::explorer;
use crate::service::testing::virtual_phone;

const MIB: u64 = 1024 * 1024;
/// 4.7 GB: past the 4 GiB line, so 32 bits cannot say how big it is.
const BIG: u64 = 4 * 1024 * MIB + 700 * MIB;
const MARKER: &[u8; 16] = b"END-OF-THE-FILE!";
const PART: &str = ".big.mkv.open-mtp-client-part";

struct Big {
    phone: tempfile::TempDir,
    mac: tempfile::TempDir,
    transfer: Service,
    explorer: Arc<explorer::Service>,
    device: Arc<device::Service>,
    storage: u64,
    serial: String,
}

/// A sparse `size`-byte file with `MARKER` in its last 16 bytes.
fn sparse_with_marker(path: &Path, size: u64) {
    let file = fs::File::create(path).unwrap();
    file.set_len(size).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(size - 16)).unwrap();
    file.write_all(MARKER).unwrap();
}

fn tail_of(path: &Path) -> Vec<u8> {
    let mut file = fs::File::open(path).unwrap();
    file.seek(SeekFrom::End(-16)).unwrap();
    let mut tail = vec![0; 16];
    file.read_exact(&mut tail).unwrap();
    tail
}

fn dated(path: &Path) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(1_791_376_200))
        .unwrap();
}

/// A phone holding `big.mkv`, BIG bytes, and a Mac with room for it.
async fn big() -> Big {
    let phone = tempfile::tempdir().unwrap();
    sparse_with_marker(&phone.path().join("big.mkv"), BIG);
    dated(&phone.path().join("big.mkv"));
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(phone.path()),
    )));
    let serial = device.connect().await.unwrap().serial_number;
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    Big {
        phone,
        mac: tempfile::tempdir().unwrap(),
        transfer: Service::new(Arc::clone(&device), Arc::clone(&explorer)),
        explorer,
        device,
        storage,
        serial,
    }
}

impl Big {
    fn download_request(&self) -> DownloadRequest {
        DownloadRequest {
            storage_id: self.storage,
            source: "/".into(),
            names: vec!["big.mkv".into()],
            destination: self.mac.path().to_string_lossy().into_owned(),
        }
    }
}

#[tokio::test]
async fn a_file_over_4_gb_lists_with_its_real_size() {
    let s = big().await;

    let listed = s.explorer.list(s.storage, "/", false).await.unwrap();

    let file = listed.iter().find(|e| e.name == "big.mkv").unwrap();
    assert_eq!(file.size_bytes, BIG);
    assert!(file.size_bytes > u64::from(u32::MAX));
}

#[tokio::test]
async fn a_download_resumes_past_the_4_gb_line_and_ends_with_every_byte_in_place() {
    let s = big().await;
    // An earlier attempt got all but the last 3 MB (the Mac keeps it sparse too).
    let part = s.mac.path().join(PART);
    fs::File::create(&part)
        .unwrap()
        .set_len(BIG - 3 * MIB)
        .unwrap();
    dated(&part);
    let reports = Mutex::new(Vec::<TransferProgress>::new());

    let summary = s
        .transfer
        .download(&s.download_request(), &Cancel::new(), |p| {
            reports.lock().unwrap().push(p)
        })
        .await
        .unwrap();

    let done = s.mac.path().join("big.mkv");
    assert_eq!(fs::metadata(&done).unwrap().len(), BIG);
    assert_eq!(
        tail_of(&done),
        MARKER,
        "the bytes past 4 GB are the phone's"
    );
    assert!(!s.mac.path().join(PART).exists());
    assert_eq!(summary.resumed, 1);
    assert_eq!(summary.bytes, 3 * MIB, "only the last 3 MB came over USB");
    let reports = reports.into_inner().unwrap();
    assert_eq!(
        reports[0].file_bytes_total, BIG,
        "the whole size, in 64 bits"
    );
    assert_eq!(
        reports[0].bytes_total,
        3 * MIB,
        "but only 3 MB are left to do"
    );
    assert_eq!(
        reports[0].file_bytes_done,
        BIG - 3 * MIB,
        "the bar starts where it left off"
    );
    let last = reports.last().unwrap();
    assert_eq!(last.file_bytes_done, BIG);
    assert_eq!(last.bytes_done, 3 * MIB);
}

#[tokio::test]
async fn cancelling_a_file_over_4_gb_keeps_a_part_to_continue_from() {
    let s = big().await;
    let part = s.mac.path().join(PART);
    fs::File::create(&part)
        .unwrap()
        .set_len(BIG - 6 * MIB)
        .unwrap();
    dated(&part);
    let cancel = Cancel::new();
    let trigger = cancel.clone();

    let result = s
        .transfer
        .download(&s.download_request(), &cancel, |p| {
            // After the first window of the resumed part has arrived.
            if p.file_bytes_done > BIG - 6 * MIB && p.file_bytes_done < BIG {
                trigger.cancel();
            }
        })
        .await;

    assert_eq!(result, Err(crate::dto::Error::Cancelled));
    let kept = fs::metadata(&part).unwrap().len();
    assert!(kept > BIG - 6 * MIB && kept < BIG, "{kept}");
    assert!(
        kept > u64::from(u32::MAX),
        "the kept length is past 4 GB too"
    );
    assert!(!s.mac.path().join("big.mkv").exists());
}

#[tokio::test]
async fn a_part_the_size_of_the_whole_file_is_not_resumed_even_past_4_gb() {
    let s = big().await;
    let part = s.mac.path().join(PART);
    fs::File::create(&part).unwrap().set_len(BIG).unwrap();
    dated(&part);
    // Pretend to be a complete earlier copy under another name so that nothing
    // but the part could be matched: a full-length part is not "shorter than
    // the file", so it must not be appended to (that would grow past BIG).
    let resumed = super::service::resumable_part(
        &s.mac.path().join("big.mkv"),
        BIG,
        UNIX_EPOCH + Duration::from_secs(1_791_376_200),
    )
    .await;

    assert_eq!(resumed, None);
}

#[tokio::test]
async fn a_file_over_4_gb_already_on_the_mac_is_recognised_as_there() {
    let s = big().await;
    let copy = s.mac.path().join("big.mkv");
    fs::File::create(&copy).unwrap().set_len(BIG).unwrap();
    dated(&copy);

    let summary = s
        .transfer
        .download(&s.download_request(), &Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(
        (summary.files, summary.already_there, summary.bytes),
        (0, 1, 0)
    );
    assert_eq!(
        fs::read_dir(s.mac.path()).unwrap().count(),
        1,
        "no big (1).mkv"
    );
}

#[tokio::test]
async fn a_mac_file_over_4_gb_is_recognised_as_already_on_the_phone() {
    let s = big().await;
    let mac_copy = s.mac.path().join("big.mkv");
    fs::File::create(&mac_copy).unwrap().set_len(BIG).unwrap();
    dated(&mac_copy);
    let request = UploadRequest {
        sources: vec![mac_copy.to_string_lossy().into_owned()],
        storage_id: s.storage,
        destination: "/".into(),
    };

    let taken = s.transfer.existing(&request).await.unwrap();
    let summary = s
        .transfer
        .upload(&request, Conflict::Skip, &Cancel::new(), |_| {})
        .await
        .unwrap();

    assert!(
        taken.is_empty(),
        "the same file is not a conflict: {taken:?}"
    );
    assert_eq!((summary.files, summary.already_there), (0, 1));
}

#[tokio::test]
async fn a_file_over_4_gb_of_a_different_size_on_the_mac_is_a_conflict() {
    let s = big().await;
    let mac_copy = s.mac.path().join("big.mkv");
    // One byte over the 4 GiB line short of the phone's file.
    fs::File::create(&mac_copy)
        .unwrap()
        .set_len(BIG - 1)
        .unwrap();
    dated(&mac_copy);
    let request = UploadRequest {
        sources: vec![mac_copy.to_string_lossy().into_owned()],
        storage_id: s.storage,
        destination: "/".into(),
    };

    let taken = s.transfer.existing(&request).await.unwrap();

    assert_eq!(taken, ["big.mkv"]);
}

/// Runs `make test-large`: moves a whole 4.7 GB file each way.
#[tokio::test]
#[ignore = "moves 4.7 GB; run with `make test-large`"]
async fn a_file_over_4_gb_downloads_whole() {
    let s = big().await;
    let started = std::time::Instant::now();

    let summary = s
        .transfer
        .download(&s.download_request(), &Cancel::new(), |_| {})
        .await
        .unwrap();

    let done = s.mac.path().join("big.mkv");
    assert_eq!(fs::metadata(&done).unwrap().len(), BIG);
    assert_eq!(tail_of(&done), MARKER);
    assert_eq!(summary.bytes, BIG);
    eprintln!("4.7 GB downloaded in {:?}", started.elapsed());
}

#[tokio::test]
#[ignore = "moves 4.7 GB; run with `make test-large`"]
async fn a_file_over_4_gb_uploads_whole() {
    let s = big().await;
    let from_mac = s.mac.path().join("movie.mkv");
    sparse_with_marker(&from_mac, BIG);
    let request = UploadRequest {
        sources: vec![from_mac.to_string_lossy().into_owned()],
        storage_id: s.storage,
        destination: "/".into(),
    };
    let started = std::time::Instant::now();

    let summary = s
        .transfer
        .upload(&request, Conflict::Skip, &Cancel::new(), |_| {})
        .await
        .unwrap();

    let sent = s.phone.path().join("movie.mkv");
    assert_eq!(fs::metadata(&sent).unwrap().len(), BIG);
    assert_eq!(tail_of(&sent), MARKER);
    assert_eq!(summary.bytes, BIG);
    eprintln!("4.7 GB uploaded in {:?}", started.elapsed());
    let _ = (&s.device, &s.serial);
}

#[tokio::test]
#[ignore = "moves 4.7 GB; run with `make test-large`"]
async fn a_file_over_4_gb_is_confirmed_by_its_real_size_and_its_original_goes() {
    // The phone reports a 4 GB or larger file's size in a second request, so
    // the read-back can compare it. (A phone that gives no size at all keeps
    // the original: see `judge`.) The Trash is only ever the recorder below.
    let s = big().await;
    let from_mac = s.mac.path().join("movie.mkv");
    sparse_with_marker(&from_mac, BIG);
    let trashed = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&trashed);
    let service =
        Service::new(Arc::clone(&s.device), Arc::clone(&s.explorer)).with_trash(move |path| {
            log.lock().unwrap().push(path.to_path_buf());
            Ok(())
        });
    let request = UploadRequest {
        sources: vec![from_mac.to_string_lossy().into_owned()],
        storage_id: s.storage,
        destination: "/".into(),
    };
    let started = std::time::Instant::now();

    let summary = service
        .upload_as(
            crate::dto::Paste::Move,
            &request,
            Conflict::Skip,
            &Cancel::new(),
            |_| {},
        )
        .await
        .unwrap();

    let sent = s.phone.path().join("movie.mkv");
    assert_eq!(fs::metadata(&sent).unwrap().len(), BIG);
    assert_eq!(tail_of(&sent), MARKER);
    let removal = summary.removal.unwrap();
    eprintln!(
        "4.7 GB moved in {:?}: removed {}, kept {:?}, trash calls {}",
        started.elapsed(),
        removal.removed,
        removal.kept,
        trashed.lock().unwrap().len()
    );
    assert_eq!(removal.removed, 1);
    assert!(removal.kept.is_empty() && removal.left.is_empty());
    assert_eq!(*trashed.lock().unwrap(), [from_mac]);
}
