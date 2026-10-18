//! Fetching phone files to look at them, on the virtual phone.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::Service;
use crate::dto::{Cancel, Error, Fetched, PreviewRequest, TransferProgress};
use crate::service::device;
use crate::service::explorer;
use crate::service::testing::virtual_phone;

struct Look {
    phone: tempfile::TempDir,
    cache: tempfile::TempDir,
    transfer: Service,
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

/// DCIM/a.jpg, DCIM/b.mp4 and a folder DCIM/Inner.
async fn look() -> Look {
    let phone = tempfile::tempdir().unwrap();
    put(phone.path(), "DCIM/a.jpg", b"aaaa", 1_000_000);
    put(phone.path(), "DCIM/b.mp4", b"bbbbbb", 1_000_000);
    fs::create_dir_all(phone.path().join("DCIM/Inner")).unwrap();
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(phone.path()),
    )));
    let serial = device.connect().await.unwrap().serial_number;
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    let cache = tempfile::tempdir().unwrap();
    Look {
        transfer: Service::new(device, explorer).with_preview_cache(cache.path().join("preview")),
        phone,
        cache,
        storage,
        serial,
    }
}

impl Look {
    fn request(&self, names: &[&str]) -> PreviewRequest {
        PreviewRequest {
            storage_id: self.storage,
            folder: "/DCIM".into(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
        }
    }

    /// Fetches `names`, and how many bytes came over.
    async fn fetch(&self, names: &[&str]) -> (Fetched, u64) {
        let moved = Mutex::new(0);
        let fetched = self
            .transfer
            .fetch_for_preview(
                &self.request(names),
                &Cancel::new(),
                |p: TransferProgress| {
                    let mut moved = moved.lock().unwrap();
                    *moved = (*moved).max(p.bytes_done);
                },
            )
            .await
            .unwrap();
        let moved = *moved.lock().unwrap();
        (fetched, moved)
    }
}

#[tokio::test]
async fn a_look_brings_the_files_over_under_their_names_with_the_phones_date_kept_private() {
    let l = look().await;

    let (fetched, moved) = l.fetch(&["b.mp4", "a.jpg"]).await;

    assert_eq!(fetched.paths.len(), 2);
    assert!(
        fetched.paths[0].ends_with("/b.mp4") && fetched.paths[1].ends_with("/a.jpg"),
        "in the order asked"
    );
    assert_eq!(fs::read(&fetched.paths[1]).unwrap(), b"aaaa");
    assert_eq!(
        fs::metadata(&fetched.paths[1]).unwrap().modified().unwrap(),
        at(1_000_000)
    );
    assert_eq!(moved, 10);
    let root = l.cache.path().join("preview");
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let hex: String = l.serial.bytes().map(|b| format!("{b:02x}")).collect();
    let folder = Path::new(&fetched.paths[0]).parent().unwrap();
    assert_eq!(
        folder.file_name().unwrap().len(),
        16,
        "a short name for the phone folder"
    );
    assert!(
        fetched.paths[0].contains(&format!("/{hex}-{}/", l.storage)),
        "the phone's own place"
    );
}

#[tokio::test]
async fn a_second_look_copies_nothing_and_a_changed_file_takes_its_name_again() {
    let l = look().await;
    l.fetch(&["a.jpg"]).await;

    let (again, moved) = l.fetch(&["a.jpg"]).await;
    assert_eq!(moved, 0, "the copy there is the phone's file");

    put(l.phone.path(), "DCIM/a.jpg", b"AAAAAAA", 2_000_000);
    mtp_rs::rescan_virtual_device(&l.serial);
    let (changed, moved) = l.fetch(&["a.jpg"]).await;

    assert_eq!(
        changed.paths, again.paths,
        "the same name, not a numbered one"
    );
    assert_eq!(moved, 7);
    assert_eq!(fs::read(&changed.paths[0]).unwrap(), b"AAAAAAA");
}

#[tokio::test]
async fn folders_files_too_big_and_more_than_twenty_are_not_fetched() {
    let l = look().await;
    let big = l.phone.path().join("DCIM/huge.mov");
    fs::File::create(&big)
        .unwrap()
        .set_len(5 * 1024 * 1024 * 1024)
        .unwrap();
    for n in 0..22 {
        put(l.phone.path(), &format!("DCIM/x{n:02}.jpg"), b"x", 5);
    }
    mtp_rs::rescan_virtual_device(&l.serial);

    let (some, moved) = l.fetch(&["Inner", "huge.mov", "a.jpg"]).await;
    assert_eq!(some.folders, ["Inner"]);
    assert_eq!(some.too_big, ["huge.mov"]);
    assert_eq!(some.paths.len(), 1);
    assert_eq!(moved, 4, "nothing of the big one was read");

    let many: Vec<String> = (0..22).map(|n| format!("x{n:02}.jpg")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let (twenty, _) = l.fetch(&many).await;
    assert_eq!((twenty.paths.len(), twenty.left_out), (20, 2));
}

#[tokio::test]
async fn the_cache_lets_go_of_what_was_looked_at_longest_ago() {
    let l = look().await;
    let root = l.cache.path().join("preview");
    let old = root.join("aa-1/old");
    let recent = root.join("aa-1/recent");
    for (folder, used) in [(&old, 10), (&recent, 20)] {
        fs::create_dir_all(folder).unwrap();
        // Sparse: a big file that costs no disk.
        fs::File::create(folder.join("big.bin"))
            .unwrap()
            .set_len(1_500_000_000)
            .unwrap();
        put(folder, ".used", b"", used);
    }

    l.fetch(&["a.jpg"]).await;

    assert!(!old.exists(), "3 GB is over the cap: the oldest goes");
    assert!(recent.exists(), "and then it is under");
}

#[tokio::test]
async fn without_a_cache_or_when_cancelled_nothing_is_fetched() {
    let l = look().await;
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(l.phone.path()),
    )));
    device.connect().await.unwrap();
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    let plain = Service::new(device, explorer);
    let cancel = Cancel::new();
    cancel.cancel();

    let nowhere = plain
        .fetch_for_preview(&l.request(&["a.jpg"]), &Cancel::new(), |_| {})
        .await;
    let cancelled = l
        .transfer
        .fetch_for_preview(&l.request(&["a.jpg"]), &cancel, |_| {})
        .await;

    assert_eq!(nowhere, Err(Error::Unsupported));
    assert_eq!(cancelled, Err(Error::Cancelled));
}

#[tokio::test]
async fn a_file_that_is_not_there_says_so() {
    let l = look().await;

    let result = l
        .transfer
        .fetch_for_preview(&l.request(&["gone.jpg"]), &Cancel::new(), |_| {})
        .await;

    assert!(result.is_err(), "{result:?}");
}

#[tokio::test]
async fn the_folder_being_looked_in_is_kept_however_long_ago_it_was_used() {
    let l = look().await;
    let (first, _) = l.fetch(&["a.jpg"]).await;
    let place = Path::new(&first.paths[0]).parent().unwrap().to_path_buf();
    put(&place, ".used", b"", 1);
    fs::File::create(place.join("big.bin"))
        .unwrap()
        .set_len(3_000_000_000)
        .unwrap();

    let (again, _) = l.fetch(&["b.mp4"]).await;

    assert!(
        place.join("a.jpg").exists(),
        "over the cap, but it is this folder"
    );
    assert_eq!(again.paths.len(), 1);
}

#[tokio::test]
async fn a_deep_phone_folder_still_makes_a_short_name() {
    let l = look().await;
    let deep = format!(
        "/DCIM/{}",
        "Holiday in the mountains with the family ".repeat(4).trim()
    );
    put(l.phone.path(), &format!("{}/a.jpg", &deep[1..]), b"a", 5);
    mtp_rs::rescan_virtual_device(&l.serial);

    let fetched = l
        .transfer
        .fetch_for_preview(
            &PreviewRequest {
                storage_id: l.storage,
                folder: deep,
                names: vec!["a.jpg".into()],
            },
            &Cancel::new(),
            |_| {},
        )
        .await
        .unwrap();

    assert_eq!(fetched.paths.len(), 1);
}

#[tokio::test]
async fn looking_in_a_folder_of_five_thousand_reads_none_of_the_others() {
    let l = look().await;
    for n in 0..5_000 {
        fs::write(l.phone.path().join(format!("DCIM/IMG_{n:04}.jpg")), b"x").unwrap();
    }
    mtp_rs::rescan_virtual_device(&l.serial);
    // The pane read the folder once, as it does when it shows it.
    let explorer = Arc::clone(&l.transfer.explorer);
    explorer.list(l.storage, "/DCIM", false).await.unwrap();
    let before = explorer.full_reads();

    let started = std::time::Instant::now();
    let (fetched, _) = l.fetch(&["IMG_1234.jpg"]).await;
    eprintln!("a look in 5,000 files: {:?}", started.elapsed());

    assert_eq!(fetched.paths.len(), 1);
    assert_eq!(
        explorer.full_reads(),
        before,
        "asked about the one file, not the folder"
    );
}

fn quarantine_of(path: &str) -> Option<String> {
    let out = std::process::Command::new("xattr")
        .args(["-p", "com.apple.quarantine", path])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[tokio::test]
async fn a_fetched_copy_is_marked_as_from_elsewhere_and_locked_also_the_second_time() {
    let l = look().await;

    let (first, _) = l.fetch(&["a.jpg"]).await;
    let (again, _) = l.fetch(&["a.jpg"]).await;

    for fetched in [&first, &again] {
        let path = &fetched.paths[0];
        let mark = quarantine_of(path).expect("marked");
        assert!(
            mark.starts_with("0083;") && mark.ends_with(";open-mtp-client;"),
            "{mark}"
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o444
        );
    }
    // A locked copy that changed on the phone is still replaced.
    put(l.phone.path(), "DCIM/a.jpg", b"new", 3_000_000);
    mtp_rs::rescan_virtual_device(&l.serial);
    let (changed, _) = l.fetch(&["a.jpg"]).await;
    assert_eq!(fs::read(&changed.paths[0]).unwrap(), b"new");
}
