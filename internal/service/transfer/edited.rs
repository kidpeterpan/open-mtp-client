//! Editing phone files on the Mac, on the virtual phone: the working copy,
//! sending it back over the phone's file or beside a changed one, and what
//! is kept across a relaunch.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{Service, copy_state};
use crate::dto::{Cancel, EditRequest, Error, MOST_EDITS, WorkingCopy};
use crate::service::testing::virtual_phone;
use crate::service::{device, explorer};

struct Edit {
    phone: tempfile::TempDir,
    support: tempfile::TempDir,
    device: Arc<device::Service>,
    explorer: Arc<explorer::Service>,
    transfer: Service,
    storage: u64,
    /// The phone's `Phone::key`.
    key: String,
}

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

fn put(root: &Path, path: &str, content: &[u8], when: SystemTime) {
    let file = root.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, content).unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

/// Documents/notes.txt and a folder Documents/Old on the phone.
async fn edit() -> Edit {
    let phone = tempfile::tempdir().unwrap();
    put(
        phone.path(),
        "Documents/notes.txt",
        b"first draft",
        at(1_000_000),
    );
    fs::create_dir_all(phone.path().join("Documents/Old")).unwrap();
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(phone.path()),
    )));
    let key = device.connect().await.unwrap().key;
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    let support = tempfile::tempdir().unwrap();
    let transfer = Service::new(Arc::clone(&device), Arc::clone(&explorer))
        .with_editing(support.path().join("editing"));
    Edit {
        phone,
        support,
        device,
        explorer,
        transfer,
        storage,
        key,
    }
}

impl Edit {
    fn request(&self, name: &str) -> EditRequest {
        EditRequest {
            phone: self.key.clone(),
            storage_id: self.storage,
            folder: "/Documents".into(),
            name: name.into(),
        }
    }

    /// The same support folder, as the app finds it after a relaunch.
    fn relaunched(&self) -> Service {
        Service::new(Arc::clone(&self.device), Arc::clone(&self.explorer))
            .with_editing(self.support.path().join("editing"))
    }
}

/// What an app does when it saves: writes the copy anew, a while later.
fn save(copy: &WorkingCopy, content: &[u8]) {
    let path = Path::new(&copy.path);
    fs::write(path, content).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at(2_000_000))
        .unwrap();
}

#[tokio::test]
async fn a_working_copy_is_writable_kept_with_where_it_came_from_and_sent_back_over_the_original() {
    let e = edit().await;

    let copy = e
        .transfer
        .edit_on_mac(&e.request("notes.txt"))
        .await
        .unwrap();

    assert_eq!(fs::read(&copy.path).unwrap(), b"first draft");
    assert!(
        copy.path.starts_with(
            &e.support
                .path()
                .join("editing")
                .to_string_lossy()
                .into_owned()
        ),
        "{}",
        copy.path
    );
    let mode = fs::metadata(&copy.path).unwrap().permissions().mode();
    assert_ne!(mode & 0o200, 0, "the app can save it");
    let folder = Path::new(&copy.path).parent().unwrap();
    assert_eq!(
        fs::metadata(folder).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!((copy.phone_size, copy.name.as_str()), (11, "notes.txt"));
    assert_eq!(copy_state(Path::new(&copy.path)), Some(copy.fetched));

    save(&copy, b"second draft, longer");
    assert_ne!(
        copy_state(Path::new(&copy.path)),
        Some(copy.fetched),
        "an edit shows"
    );
    let summary = e
        .transfer
        .send_edit(&copy, &Cancel::new(), |_| {})
        .await
        .unwrap();

    let on_phone = e.phone.path().join("Documents/notes.txt");
    assert_eq!(fs::read(&on_phone).unwrap(), b"second draft, longer");
    let sent = fs::metadata(&on_phone).unwrap().modified().unwrap();
    assert!(
        sent.duration_since(at(2_000_000)).unwrap_or_default() < Duration::from_secs(2)
            && at(2_000_000).duration_since(sent).unwrap_or_default() < Duration::from_secs(2),
        "with the Mac's date"
    );
    assert_eq!(summary.replaced, 1);
    assert!(!folder.exists(), "the copy goes once the phone has it");
    assert!(e.transfer.working_copies().await.is_empty());
}

#[tokio::test]
async fn a_phone_file_changed_meanwhile_keeps_its_change_and_the_edit_goes_beside_it() {
    let e = edit().await;
    let copy = e
        .transfer
        .edit_on_mac(&e.request("notes.txt"))
        .await
        .unwrap();
    save(&copy, b"edited on the Mac");
    // Someone changed it on the phone too.
    put(
        e.phone.path(),
        "Documents/notes.txt",
        b"edited on the phone",
        at(1_500_000),
    );

    let summary = e
        .transfer
        .send_edit(&copy, &Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(
        fs::read(e.phone.path().join("Documents/notes.txt")).unwrap(),
        b"edited on the phone"
    );
    assert_eq!(
        fs::read(e.phone.path().join("Documents/notes (1).txt")).unwrap(),
        b"edited on the Mac"
    );
    assert_eq!(summary.replaced, 0);
    assert_eq!(summary.renamed, 1);
}

#[tokio::test]
async fn an_edit_goes_back_only_to_the_phone_it_came_from() {
    let e = edit().await;
    let copy = e
        .transfer
        .edit_on_mac(&e.request("notes.txt"))
        .await
        .unwrap();
    save(&copy, b"for the other phone? no");
    let elsewhere = WorkingCopy {
        phone: "ANOTHER".into(),
        ..copy.clone()
    };

    let refused = e
        .transfer
        .send_edit(&elsewhere, &Cancel::new(), |_| {})
        .await;

    assert_eq!(refused.map(|_| ()), Err(Error::OtherPhone));
    assert_eq!(
        fs::read(e.phone.path().join("Documents/notes.txt")).unwrap(),
        b"first draft",
        "nothing sent"
    );
    assert!(
        Path::new(&copy.path).exists(),
        "the copy waits for its phone"
    );
}

#[tokio::test]
async fn a_phone_file_changed_to_the_same_size_is_still_changed() {
    let e = edit().await;
    let copy = e
        .transfer
        .edit_on_mac(&e.request("notes.txt"))
        .await
        .unwrap();
    save(&copy, b"edited on the Mac");
    // As long as before, but saved again on the phone later.
    put(
        e.phone.path(),
        "Documents/notes.txt",
        b"first DRAFT",
        at(1_500_000),
    );

    e.transfer
        .send_edit(&copy, &Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(
        fs::read(e.phone.path().join("Documents/notes.txt")).unwrap(),
        b"first DRAFT",
        "its date says it isn't the file fetched"
    );
    assert!(e.phone.path().join("Documents/notes (1).txt").exists());
}

#[tokio::test]
async fn editing_the_same_file_again_keeps_the_edits_and_copies_come_back_after_a_relaunch() {
    let e = edit().await;
    let copy = e
        .transfer
        .edit_on_mac(&e.request("notes.txt"))
        .await
        .unwrap();
    save(&copy, b"unsent edit");

    let again = e
        .transfer
        .edit_on_mac(&e.request("notes.txt"))
        .await
        .unwrap();
    assert_eq!(again, copy);
    assert_eq!(
        fs::read(&again.path).unwrap(),
        b"unsent edit",
        "not fetched again"
    );

    let after = e.relaunched().working_copies().await;
    assert_eq!(after, std::slice::from_ref(&copy));

    // A copy whose file has gone, and an odd manifest, are left out.
    let odd = e.support.path().join("editing/odd");
    fs::create_dir_all(&odd).unwrap();
    fs::write(odd.join("manifest.json"), b"{ not json").unwrap();
    // What the escaping manifest names is there: only the name rule stops it.
    fs::write(e.support.path().join("secret"), b"not the app's").unwrap();
    let escaping = e.support.path().join("editing/escaping");
    fs::create_dir_all(&escaping).unwrap();
    fs::write(
        escaping.join("manifest.json"),
        br#"{"version":1,"phone":"ABC123","storage":1,"folder":"/Documents","name":"../../secret","fetchedSize":1,"fetchedMs":1,"phoneSize":1,"phoneDate":""}"#,
    )
    .unwrap();
    assert_eq!(
        e.relaunched().working_copies().await,
        std::slice::from_ref(&copy)
    );

    // A copy whose file was deleted is left out though its manifest stays.
    let notes = e.transfer.edit_on_mac(&e.request("other.txt")).await;
    assert!(notes.is_err(), "no such file on the phone");
    put(e.phone.path(), "Documents/other.txt", b"o", at(1_000_000));
    let other = e
        .transfer
        .edit_on_mac(&e.request("other.txt"))
        .await
        .unwrap();
    fs::remove_file(&other.path).unwrap();
    assert_eq!(
        e.relaunched().working_copies().await,
        std::slice::from_ref(&copy)
    );

    e.transfer.discard_edit(&copy).await.unwrap();
    assert!(!Path::new(&copy.path).exists());
    assert!(e.relaunched().working_copies().await.is_empty());
}

#[tokio::test]
async fn only_files_can_be_edited_and_no_more_than_twenty_at_once() {
    let e = edit().await;
    assert_eq!(
        e.transfer.edit_on_mac(&e.request("Old")).await,
        Err(Error::NotEditable)
    );
    assert!(matches!(
        e.transfer.edit_on_mac(&e.request("missing.txt")).await,
        Err(Error::PathNotFound(_))
    ));

    for i in 0..MOST_EDITS {
        put(
            e.phone.path(),
            &format!("Documents/{i}.txt"),
            b"x",
            at(1_000_000),
        );
    }
    for i in 0..MOST_EDITS {
        e.transfer
            .edit_on_mac(&e.request(&format!("{i}.txt")))
            .await
            .unwrap();
    }
    assert_eq!(
        e.transfer.edit_on_mac(&e.request("notes.txt")).await,
        Err(Error::TooManyEdits)
    );
}

#[tokio::test]
async fn without_a_place_for_working_copies_nothing_is_edited_and_a_tampered_copy_is_refused() {
    let e = edit().await;
    let plain = Service::new(Arc::clone(&e.device), Arc::clone(&e.explorer));
    assert_eq!(
        plain.edit_on_mac(&e.request("notes.txt")).await,
        Err(Error::Unsupported)
    );

    let copy = e
        .transfer
        .edit_on_mac(&e.request("notes.txt"))
        .await
        .unwrap();
    let elsewhere = WorkingCopy {
        path: "/etc/hosts".into(),
        ..copy.clone()
    };
    assert_eq!(
        e.transfer.discard_edit(&elsewhere).await,
        Err(Error::NotEditable)
    );
    let climbing = WorkingCopy {
        id: "..".into(),
        ..copy
    };
    assert_eq!(
        e.transfer
            .send_edit(&climbing, &Cancel::new(), |_| {})
            .await
            .map(|_| ()),
        Err(Error::NotEditable)
    );
}
