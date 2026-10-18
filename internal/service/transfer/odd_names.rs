//! Files with awkward names must copy, list, rename and delete exactly as they
//! are named, whatever `?`, `"`, `:`, `#` or emoji they hold. Silently renaming
//! them would be data loss, and is no fix.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use super::Service;
use crate::dto::{Cancel, Conflict, DownloadRequest, UploadRequest};
use crate::service::device;
use crate::service::explorer;
use crate::service::testing::virtual_phone;

/// What each name is about; every one is legal on macOS and on Android.
const ODD: &[&str] = &[
    "what?.jpg",
    "say \"hi\".txt",
    "100% sure.txt",
    "file #1.jpg",
    "a:b.txt",
    "a*b.bin",
    "<tag>.xml",
    "a|b.txt",
    "back\\slash.txt",
    "😀 trip 🌴.jpg",
    "ภาพถ่าย วันหยุด.jpg",
    "日本語のファイル.txt",
    "e\u{301}clair.txt",
    " spaces around ",
    "ends with a dot.",
    ".hidden file",
    "-rf",
    "$HOME",
    "`touch pwned`",
    "$(touch pwned).txt",
    "semi;colon&amp.txt",
    "tab\tinside.txt",
    "new\nline.txt",
    "~",
    "...",
];

/// Names the virtual phone refuses to create (its guard for the folder behind
/// it: no `..`, `/` or `\\`). A real phone has rules of its own, and its answer
/// to a name it dislikes is what the test below uses these for.
const REFUSED_BY_THE_SIMULATOR: &[&str] = &["...", "back\\slash.txt"];

struct Odd {
    phone: tempfile::TempDir,
    mac: tempfile::TempDir,
    transfer: Service,
    explorer: Arc<explorer::Service>,
    storage: u64,
}

/// A name of exactly 255 bytes, the most a file system allows, mixing 1-, 2-,
/// 3- and 4-byte characters.
fn longest_name() -> String {
    let mut name = "x".repeat(40);
    name.push_str(&"é".repeat(20));
    name.push_str(&"ก".repeat(20));
    name.push_str(&"😀".repeat(10));
    let missing = 255 - name.len() - ".jpg".len();
    name.push_str(&"y".repeat(missing));
    name.push_str(".jpg");
    assert_eq!(name.len(), 255);
    name
}

fn content(name: &str) -> Vec<u8> {
    format!("the content of {name}").into_bytes()
}

/// Writes every odd name into `folder` with content that says whose it is.
fn fill(folder: &Path) {
    fs::create_dir_all(folder).unwrap();
    for name in ODD.iter().copied().chain([longest_name().as_str()]) {
        fs::write(folder.join(name), content(name)).unwrap();
    }
}

async fn odd() -> Odd {
    let phone = tempfile::tempdir().unwrap();
    fill(&phone.path().join("Odd"));
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(phone.path()),
    )));
    device.connect().await.unwrap();
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    Odd {
        phone,
        mac: tempfile::tempdir().unwrap(),
        transfer: Service::new(device, Arc::clone(&explorer)),
        explorer,
        storage,
    }
}

fn names_in(folder: &Path) -> BTreeSet<String> {
    fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

fn all_names() -> BTreeSet<String> {
    ODD.iter()
        .map(|name| (*name).to_owned())
        .chain([longest_name()])
        .collect()
}

#[tokio::test]
async fn the_phone_lists_every_awkward_name_exactly() {
    let s = odd().await;

    let listed = s.explorer.list(s.storage, "/Odd", true).await.unwrap();

    let names: BTreeSet<String> = listed.into_iter().map(|e| e.name).collect();
    assert_eq!(names, all_names());
}

#[tokio::test]
async fn every_awkward_name_downloads_with_its_name_and_bytes_intact() {
    let s = odd().await;
    let request = DownloadRequest {
        storage_id: s.storage,
        source: "/Odd".into(),
        names: all_names().into_iter().collect(),
        destination: s.mac.path().to_string_lossy().into_owned(),
    };

    let summary = s
        .transfer
        .download(&request, &Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(summary.failures, []);
    assert_eq!(summary.renamed, 0, "nothing was renamed to be copyable");
    assert_eq!(summary.files, all_names().len());
    assert_eq!(names_in(s.mac.path()), all_names());
    for name in all_names() {
        assert_eq!(
            fs::read(s.mac.path().join(&name)).unwrap(),
            content(&name),
            "{name:?}"
        );
    }
    assert!(
        !s.mac.path().join("pwned").exists(),
        "a name is never run as a command"
    );
}

#[tokio::test]
async fn awkward_names_download_as_a_whole_folder_too() {
    let s = odd().await;
    let request = DownloadRequest {
        storage_id: s.storage,
        source: "/".into(),
        names: vec!["Odd".into()],
        destination: s.mac.path().to_string_lossy().into_owned(),
    };

    let summary = s
        .transfer
        .download(&request, &Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(summary.failures, []);
    assert_eq!(names_in(&s.mac.path().join("Odd")), all_names());
}

/// Every awkward name the virtual phone will take.
fn uploadable() -> BTreeSet<String> {
    all_names()
        .into_iter()
        .filter(|name| !REFUSED_BY_THE_SIMULATOR.contains(&name.as_str()))
        .collect()
}

#[tokio::test]
async fn every_awkward_name_uploads_with_its_name_and_bytes_intact() {
    let s = odd().await;
    let mac_folder = s.mac.path().join("From Mac");
    fill(&mac_folder);
    let sources = uploadable()
        .into_iter()
        .map(|name| mac_folder.join(name).to_string_lossy().into_owned())
        .collect();
    let request = UploadRequest {
        sources,
        storage_id: s.storage,
        destination: "/".into(),
    };

    let taken = s.transfer.existing(&request).await.unwrap();
    let summary = s
        .transfer
        .upload(&request, Conflict::Skip, &Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(taken, Vec::<String>::new());
    assert_eq!(summary.failures, []);
    assert_eq!(summary.renamed, 0);
    assert_eq!(summary.files, uploadable().len());
    let on_phone: BTreeSet<String> = names_in(s.phone.path())
        .into_iter()
        .filter(|name| name != "Odd" && name != "From Mac")
        .collect();
    assert_eq!(on_phone, uploadable());
    for name in uploadable() {
        assert_eq!(
            fs::read(s.phone.path().join(&name)).unwrap(),
            content(&name),
            "{name:?}"
        );
    }
}

#[tokio::test]
async fn a_folder_with_an_awkward_name_downloads_and_uploads_with_its_files() {
    let s = odd().await;
    let folder = "Trip: Rome? #1 \"best\" 🍕";
    fs::create_dir_all(s.phone.path().join(folder)).unwrap();
    fs::write(s.phone.path().join(folder).join("pic?.jpg"), b"pic").unwrap();
    mtp_rs::rescan_virtual_device(&s.transfer.device.connect().await.unwrap().serial_number);

    let down = s
        .transfer
        .download(
            &DownloadRequest {
                storage_id: s.storage,
                source: "/".into(),
                names: vec![folder.into()],
                destination: s.mac.path().to_string_lossy().into_owned(),
            },
            &Cancel::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(down.failures, []);
    assert_eq!(
        fs::read(s.mac.path().join(folder).join("pic?.jpg")).unwrap(),
        b"pic"
    );

    let up = s
        .transfer
        .upload(
            &UploadRequest {
                sources: vec![s.mac.path().join(folder).to_string_lossy().into_owned()],
                storage_id: s.storage,
                destination: "/Odd".into(),
            },
            Conflict::Skip,
            &Cancel::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(up.failures, []);
    assert_eq!(
        fs::read(s.phone.path().join("Odd").join(folder).join("pic?.jpg")).unwrap(),
        b"pic"
    );
}

#[tokio::test]
async fn awkward_names_can_be_renamed_deleted_and_made_on_the_phone() {
    let s = odd().await;
    let phone = "/Odd";

    // Rename to a name with a % and a #, which are fine everywhere.
    let renamed = s
        .explorer
        .rename(s.storage, phone, "what?.jpg", "100% done #2 😀.jpg")
        .await
        .unwrap();
    assert_eq!(renamed, "100% done #2 😀.jpg");
    assert!(s.phone.path().join("Odd/100% done #2 😀.jpg").exists());
    assert!(!s.phone.path().join("Odd/what?.jpg").exists());

    // Rename away from an awkward name.
    s.explorer
        .rename(s.storage, phone, "$(touch pwned).txt", "plain.txt")
        .await
        .unwrap();
    assert!(s.phone.path().join("Odd/plain.txt").exists());

    // Delete several awkward names at once.
    s.explorer
        .delete(
            s.storage,
            phone,
            &[
                "a:b.txt".into(),
                "😀 trip 🌴.jpg".into(),
                "back\\slash.txt".into(),
            ],
        )
        .await
        .unwrap();
    assert!(!s.phone.path().join("Odd/a:b.txt").exists());
    assert!(!s.phone.path().join("Odd/😀 trip 🌴.jpg").exists());
    assert!(!s.phone.path().join("Odd/back\\slash.txt").exists());

    // A new folder with a percent sign.
    s.explorer
        .make_folder(s.storage, phone, "100% new")
        .await
        .unwrap();
    assert!(s.phone.path().join("Odd/100% new").is_dir());
    assert!(!s.phone.path().join("pwned").exists());
}

#[tokio::test]
async fn names_with_characters_a_phone_rejects_are_still_refused_when_typed() {
    let s = odd().await;

    for typed in [
        "a?b", "a:b", "a*b", "a|b", "a\"b", "a<b", "a>b", "a\\b", "a/b",
    ] {
        let result = s.explorer.make_folder(s.storage, "/Odd", typed).await;
        assert!(
            matches!(
                result,
                Err(crate::dto::Error::InvalidName(
                    crate::dto::NameProblem::IllegalCharacters
                ))
            ),
            "{typed:?}: {result:?}"
        );
    }
}

#[tokio::test]
async fn two_phone_files_whose_names_differ_only_by_case_both_arrive_on_the_mac() {
    // A phone's file system tells `a.jpg` from `A.JPG`; the Mac's does not, so
    // the second must get a number instead of replacing the first.
    let mac = tempfile::tempdir().unwrap();
    let first = mac.path().join(".a.jpg.part1");
    let second = mac.path().join(".A.JPG.part2");
    fs::write(&first, b"first").unwrap();
    fs::write(&second, b"second").unwrap();

    let one = super::service::place(&first, &mac.path().join("a.jpg"))
        .await
        .unwrap();
    let two = super::service::place(&second, &mac.path().join("A.JPG"))
        .await
        .unwrap();

    assert_ne!(one, two);
    assert_eq!(fs::read(&one).unwrap(), b"first");
    assert_eq!(fs::read(&two).unwrap(), b"second");
    assert_eq!(fs::read_dir(mac.path()).unwrap().count(), 2);
}

#[tokio::test]
async fn a_name_the_phone_refuses_is_one_failure_and_the_rest_still_upload() {
    let s = odd().await;
    let mac_folder = s.mac.path().join("From Mac");
    fill(&mac_folder);
    let sources = all_names()
        .into_iter()
        .map(|name| mac_folder.join(name).to_string_lossy().into_owned())
        .collect();
    let request = UploadRequest {
        sources,
        storage_id: s.storage,
        destination: "/".into(),
    };

    let summary = s
        .transfer
        .upload(&request, Conflict::Skip, &Cancel::new(), |_| {})
        .await
        .unwrap();

    // The phone answered `GeneralError` for these two; that is about those
    // files, not about the connection, so nothing else is held up.
    let mut refused: Vec<_> = summary.failures.iter().map(|f| f.label.clone()).collect();
    refused.sort();
    let mut expected: Vec<String> = REFUSED_BY_THE_SIMULATOR
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    expected.sort();
    assert_eq!(refused, expected);
    assert_eq!(summary.files, uploadable().len());
    let on_phone: BTreeSet<String> = names_in(s.phone.path())
        .into_iter()
        .filter(|name| name != "Odd" && name != "From Mac")
        .collect();
    assert_eq!(on_phone, uploadable(), "everything else arrived, unchanged");
}

#[tokio::test]
async fn a_phone_that_refuses_the_only_file_reports_that_as_the_error() {
    let s = odd().await;
    let mac_folder = s.mac.path().join("From Mac");
    fill(&mac_folder);
    let request = UploadRequest {
        sources: vec![mac_folder.join("...").to_string_lossy().into_owned()],
        storage_id: s.storage,
        destination: "/".into(),
    };

    let result = s
        .transfer
        .upload(&request, Conflict::Skip, &Cancel::new(), |_| {})
        .await;

    assert!(
        matches!(result, Err(crate::dto::Error::General(_))),
        "{result:?}"
    );
}
