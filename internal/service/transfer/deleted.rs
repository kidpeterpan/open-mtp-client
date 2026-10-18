//! Phone deletes that keep a copy in the Trash, on the virtual phone: what
//! reaches the Trash byte for byte before it leaves the phone, and what
//! stays when it can't.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::Service;
use crate::dto::{Cancel, DeleteRequest, Error, TransferProgress, Why};
use crate::service::testing::virtual_phone;
use crate::service::{device, explorer};

struct Delete {
    phone: tempfile::TempDir,
    /// Where the Trash recorder puts what it is given: the test's own.
    bin: tempfile::TempDir,
    trashed: Arc<Mutex<Vec<PathBuf>>>,
    transfer: Service,
    storage: u64,
}

/// The phone: notes.txt, Trip/a.jpg and Trip/b.jpg, keep.txt.
async fn delete_with(refuse: bool) -> Delete {
    let phone = tempfile::tempdir().unwrap();
    fs::write(phone.path().join("notes.txt"), b"some notes").unwrap();
    fs::create_dir_all(phone.path().join("Trip")).unwrap();
    fs::write(phone.path().join("Trip/a.jpg"), b"aaaa").unwrap();
    fs::write(phone.path().join("Trip/b.jpg"), b"bbbbbb").unwrap();
    fs::write(phone.path().join("keep.txt"), b"keep").unwrap();
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(phone.path()),
    )));
    device.connect().await.unwrap();
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    let bin = tempfile::tempdir().unwrap();
    let trashed = Arc::new(Mutex::new(Vec::new()));
    let (log, into) = (Arc::clone(&trashed), bin.path().to_path_buf());
    // Never the user's Trash: each item goes into a folder of the test's.
    let transfer = Service::new(device, explorer).with_trash(move |path: &Path| {
        if refuse {
            return Err(std::io::Error::other("the Trash said no"));
        }
        let mut log = log.lock().unwrap();
        log.push(path.to_path_buf());
        fs::rename(path, into.join(path.file_name().unwrap()))
    });
    Delete {
        phone,
        bin,
        trashed,
        transfer,
        storage,
    }
}

impl Delete {
    fn request(&self, names: &[&str]) -> DeleteRequest {
        DeleteRequest {
            storage_id: self.storage,
            folder: "/".into(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
        }
    }
}

#[tokio::test]
async fn a_file_and_a_folder_are_in_the_trash_byte_for_byte_before_they_leave_the_phone() {
    let d = delete_with(false).await;
    let syncs = || super::service::SYNCS.with(std::cell::Cell::get);
    let before = syncs();

    let summary = d
        .transfer
        .delete_keeping_copy(&d.request(&["notes.txt", "Trip"]), &Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(
        syncs() - before,
        3,
        "each copy on the disk before its original goes"
    );
    assert_eq!(
        fs::read(d.bin.path().join("notes.txt")).unwrap(),
        b"some notes"
    );
    assert_eq!(fs::read(d.bin.path().join("Trip/a.jpg")).unwrap(), b"aaaa");
    assert_eq!(
        fs::read(d.bin.path().join("Trip/b.jpg")).unwrap(),
        b"bbbbbb"
    );
    assert_eq!(d.trashed.lock().unwrap().len(), 2, "one Trash call an item");
    assert!(!d.phone.path().join("notes.txt").exists());
    assert!(!d.phone.path().join("Trip").exists());
    assert!(
        d.phone.path().join("keep.txt").exists(),
        "only what was chosen"
    );
    let removal = summary.removal.unwrap();
    assert_eq!(
        removal.removed, 3,
        "the files counted; the folder, emptied, went too"
    );
    assert!(removal.kept.is_empty());
}

#[tokio::test]
async fn an_item_whose_copy_fails_stays_on_the_phone_and_says_why() {
    let d = delete_with(false).await;
    let unreadable = d.phone.path().join("Trip/b.jpg");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

    let summary = d
        .transfer
        .delete_keeping_copy(&d.request(&["notes.txt", "Trip"]), &Cancel::new(), |_| {})
        .await;
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
    let removal = summary.unwrap().removal.unwrap();

    assert!(
        !d.phone.path().join("notes.txt").exists(),
        "the other item went"
    );
    assert!(
        d.phone.path().join("Trip/a.jpg").exists(),
        "nothing of the folder left"
    );
    assert!(d.phone.path().join("Trip/b.jpg").exists());
    assert_eq!(removal.kept[0].label, "Trip");
    assert_eq!(removal.kept[0].why, Why::NotCopied);
    assert_eq!(
        d.trashed.lock().unwrap().len(),
        1,
        "only the whole one reached the Trash"
    );
}

#[tokio::test]
async fn an_item_the_trash_refuses_stays_on_the_phone() {
    let d = delete_with(true).await;

    let removal = d
        .transfer
        .delete_keeping_copy(&d.request(&["notes.txt"]), &Cancel::new(), |_| {})
        .await
        .unwrap()
        .removal
        .unwrap();

    assert!(d.phone.path().join("notes.txt").exists());
    assert_eq!(removal.removed, 0);
    assert!(
        matches!(&removal.kept[0].why, Why::Refused(_)),
        "{:?}",
        removal.kept
    );
}

#[tokio::test]
async fn a_cancel_while_copying_deletes_nothing() {
    let d = delete_with(false).await;
    let cancel = Cancel::new();
    let trigger = cancel.clone();
    let seen = Mutex::new(Vec::<TransferProgress>::new());

    let result = d
        .transfer
        .delete_keeping_copy(&d.request(&["notes.txt", "Trip"]), &cancel, |p| {
            if p.files_done > 0 {
                trigger.cancel();
            }
            seen.lock().unwrap().push(p);
        })
        .await;

    assert_eq!(result.map(|_| ()), Err(Error::Cancelled));
    assert!(d.phone.path().join("notes.txt").exists());
    assert!(d.phone.path().join("Trip/a.jpg").exists());
    assert!(d.trashed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn without_a_trash_nothing_is_copied_or_deleted() {
    let d = delete_with(false).await;
    let plain = Service::new(
        Arc::clone(&d.transfer.device),
        Arc::clone(&d.transfer.explorer),
    );

    let result = plain
        .delete_keeping_copy(&d.request(&["notes.txt"]), &Cancel::new(), |_| {})
        .await;

    assert_eq!(result.map(|_| ()), Err(Error::Unsupported));
    assert!(d.phone.path().join("notes.txt").exists());
}
