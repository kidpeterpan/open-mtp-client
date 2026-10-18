//! Room before a byte moves, on the virtual phone: a transfer that would not
//! fit is refused before anything is written, and one that fits goes ahead.

use std::fs;
use std::sync::Arc;

use super::Service;
use super::room::Room;
use crate::dto::{
    Cancel, Conflict, DownloadRequest, Error, MirrorDirection, MirrorRequest, Paste, PasteRequest,
    UploadRequest,
};
use crate::service::device;
use crate::service::explorer;
use crate::service::testing::virtual_phone;

const KIB: u64 = 1024;

struct Phone {
    dir: tempfile::TempDir,
    mac: tempfile::TempDir,
    device: Arc<device::Service>,
    explorer: Arc<explorer::Service>,
    storage: u64,
}

/// A phone holding DCIM/a.jpg (40 KiB) and DCIM/b.jpg (10 KiB), whose
/// storage is `capacity` bytes.
async fn phone(capacity: u64) -> Phone {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("DCIM")).unwrap();
    fs::write(
        dir.path().join("DCIM/a.jpg"),
        vec![1u8; (40 * KIB) as usize],
    )
    .unwrap();
    fs::write(
        dir.path().join("DCIM/b.jpg"),
        vec![2u8; (10 * KIB) as usize],
    )
    .unwrap();
    let mut phone = virtual_phone(dir.path());
    phone.capacity = capacity;
    let device = Arc::new(device::Service::new(device::Source::Virtual(phone)));
    device.connect().await.unwrap();
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    Phone {
        dir,
        mac: tempfile::tempdir().unwrap(),
        device,
        explorer,
        storage,
    }
}

impl Phone {
    fn transfer(&self) -> Service {
        Service::new(Arc::clone(&self.device), Arc::clone(&self.explorer))
    }

    fn download(&self, names: &[&str]) -> DownloadRequest {
        DownloadRequest {
            storage_id: self.storage,
            source: "/DCIM".into(),
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            destination: self.mac.path().to_string_lossy().into_owned(),
        }
    }
}

fn mac_with(free: u64) -> Room {
    Room::free(free, "on your Mac's disk")
}

#[tokio::test]
async fn a_download_that_would_not_fit_on_the_mac_writes_nothing() {
    let p = phone(1 << 30).await;
    let transfer = p.transfer().with_room(mac_with(45 * KIB));

    let refused = transfer
        .download(&p.download(&["a.jpg", "b.jpg"]), &Cancel::new(), |_| {})
        .await;

    assert_eq!(
        refused.unwrap_err(),
        Error::NotEnoughRoom {
            needed: 50 * KIB,
            free: 45 * KIB,
            place: "on your Mac's disk".into(),
        }
    );
    assert_eq!(
        fs::read_dir(p.mac.path()).unwrap().count(),
        0,
        "not a file, not a part file"
    );

    let fits = transfer
        .download(&p.download(&["a.jpg"]), &Cancel::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(fits.files, 1, "40 KiB fits in 45");
}

#[tokio::test]
async fn what_is_already_there_needs_no_room_again() {
    let p = phone(1 << 30).await;
    p.transfer()
        .download(&p.download(&["a.jpg"]), &Cancel::new(), |_| {})
        .await
        .unwrap();
    // a.jpg is there: only b.jpg (10 KiB) is still to copy.
    let full = p.transfer().with_room(mac_with(10 * KIB));
    let again = full
        .download(&p.download(&["a.jpg", "b.jpg"]), &Cancel::new(), |_| {})
        .await
        .unwrap();
    assert_eq!((again.files, again.already_there), (1, 1));

    let nothing = p.transfer().with_room(mac_with(0));
    let done = nothing
        .download(&p.download(&["a.jpg", "b.jpg"]), &Cancel::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(done.already_there, 2, "nothing to write: no room needed");
}

#[tokio::test]
async fn an_upload_that_would_not_fit_on_the_phone_sends_nothing() {
    // The phone holds 50 KiB of a 70 KiB storage: 20 KiB free.
    let p = phone(70 * KIB).await;
    let big = p.mac.path().join("movie.mov");
    fs::write(&big, vec![4u8; (30 * KIB) as usize]).unwrap();
    let request = UploadRequest {
        sources: vec![big.to_string_lossy().into_owned()],
        storage_id: p.storage,
        destination: "/".into(),
    };

    let refused = p
        .transfer()
        .upload(&request, Conflict::Skip, &Cancel::new(), |_| {})
        .await;

    match refused {
        Err(Error::NotEnoughRoom {
            needed,
            free,
            place,
        }) => {
            assert_eq!((needed, free), (30 * KIB, 20 * KIB));
            assert_eq!(place, "on the phone's “Internal shared storage”");
        }
        other => panic!("refused for room, not {other:?}"),
    }
    assert!(!p.dir.path().join("movie.mov").exists());
}

#[tokio::test]
async fn replacing_a_file_counts_the_room_it_gives_back() {
    // 20 KiB free; a.jpg (40 KiB) is replaced by a 50 KiB one: 10 KiB more.
    let p = phone(70 * KIB).await;
    fs::create_dir(p.mac.path().join("in")).unwrap();
    let newer = p.mac.path().join("in/a.jpg");
    fs::write(&newer, vec![5u8; (50 * KIB) as usize]).unwrap();
    let request = UploadRequest {
        sources: vec![newer.to_string_lossy().into_owned()],
        storage_id: p.storage,
        destination: "/DCIM".into(),
    };

    let summary = p
        .transfer()
        .upload(&request, Conflict::Replace, &Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(summary.replaced, 1);
    assert_eq!(
        fs::metadata(p.dir.path().join("DCIM/a.jpg")).unwrap().len(),
        50 * KIB
    );
}

#[tokio::test]
async fn a_copy_through_the_mac_needs_room_for_one_file_there_and_all_of_it_on_the_phone() {
    let request = |p: &Phone| PasteRequest {
        mode: Paste::Copy,
        from_storage: p.storage,
        from: "/DCIM".into(),
        names: vec!["a.jpg".into(), "b.jpg".into()],
        to_storage: p.storage,
        to: "/".into(),
    };

    // The Mac has room for a.jpg, the biggest, and no more: enough.
    let roomy = phone(1 << 30).await;
    let summary = roomy
        .transfer()
        .with_room(mac_with(40 * KIB))
        .paste_through_mac(&request(&roomy), false, &Cancel::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(summary.files, 2);

    // A byte less on the Mac: refused.
    let tight = phone(1 << 30).await;
    assert!(matches!(
        tight
            .transfer()
            .with_room(mac_with(40 * KIB - 1))
            .paste_through_mac(&request(&tight), false, &Cancel::new(), |_| {})
            .await,
        Err(Error::NotEnoughRoom { needed, .. }) if needed == 40 * KIB
    ));

    // 50 KiB to copy onto a phone with 20 KiB free: refused before a read.
    let full = phone(70 * KIB).await;
    assert!(matches!(
        full.transfer()
            .paste_through_mac(&request(&full), false, &Cancel::new(), |_| {})
            .await,
        Err(Error::NotEnoughRoom { needed, free, .. }) if (needed, free) == (50 * KIB, 20 * KIB)
    ));
    assert!(!full.dir.path().join("a.jpg").exists());
}

#[tokio::test]
async fn a_mirror_onto_the_phone_counts_what_removing_extras_gives_back() {
    // 20 KiB free. The Mac folder holds a new 30 KiB file and not a.jpg (40
    // KiB) or b.jpg, which are extras on the phone.
    let p = phone(70 * KIB).await;
    fs::write(
        p.mac.path().join("clip.mp4"),
        vec![6u8; (30 * KIB) as usize],
    )
    .unwrap();
    let request = |remove_extras| MirrorRequest {
        storage_id: p.storage,
        phone_folder: "/DCIM".into(),
        mac_folder: p.mac.path().to_string_lossy().into_owned(),
        direction: MirrorDirection::ToPhone,
        remove_extras,
    };

    let refused = p
        .transfer()
        .mirror(&request(false), &Cancel::new(), |_| {})
        .await;
    assert!(
        matches!(refused, Err(Error::NotEnoughRoom { needed, .. }) if needed == 30 * KIB),
        "{refused:?}"
    );
    assert!(p.dir.path().join("DCIM/a.jpg").exists(), "nothing touched");

    let mirrored = p
        .transfer()
        .mirror(&request(true), &Cancel::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(mirrored.files, 1, "the 50 KiB of extras make room for it");
    assert!(p.dir.path().join("DCIM/clip.mp4").exists());
}

#[tokio::test]
async fn a_retried_move_through_the_mac_finds_what_it_sent_and_needs_no_room_for_it() {
    // The first try sent both files to / and stopped before removing the
    // originals: the phone holds 100 KiB of 110, 10 KiB free.
    let p = phone(110 * KIB).await;
    for name in ["a.jpg", "b.jpg"] {
        let original = p.dir.path().join("DCIM").join(name);
        let sent = p.dir.path().join(name);
        fs::copy(&original, &sent).unwrap();
        let when = fs::metadata(&original).unwrap().modified().unwrap();
        fs::File::options()
            .write(true)
            .open(&sent)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
    mtp_rs::rescan_virtual_device(&p.device.connect().await.unwrap().serial_number);
    let request = PasteRequest {
        mode: Paste::Move,
        from_storage: p.storage,
        from: "/DCIM".into(),
        names: vec!["a.jpg".into(), "b.jpg".into()],
        to_storage: p.storage,
        to: "/".into(),
    };

    let retried = p
        .transfer()
        .paste_through_mac(&request, true, &Cancel::new(), |_| {})
        .await;

    assert!(
        retried.is_ok(),
        "the plan's 50 KiB were sent already: {retried:?}"
    );
    assert!(p.dir.path().join("a.jpg").exists() && p.dir.path().join("b.jpg").exists());
}

// What the Mac's disk can take besides room: a disk that can't be written,
// and FAT32's 4 GB per file.

fn mac_kind(read_only: bool, most_per_file: Option<u64>, place: &str) -> Room {
    Room {
        read_only,
        most_per_file,
        ..Room::free(1 << 40, place)
    }
}

#[tokio::test]
async fn a_read_only_disk_is_refused_before_anything_is_written() {
    let p = phone(1 << 30).await;
    let transfer = p.transfer().with_room(mac_kind(true, None, "on “BACKUP”"));

    let refused = transfer
        .download(&p.download(&["a.jpg", "b.jpg"]), &Cancel::new(), |_| {})
        .await;

    assert_eq!(
        refused.unwrap_err(),
        Error::VolumeReadOnly("on “BACKUP”".into())
    );
    assert_eq!(fs::read_dir(p.mac.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn on_fat32_a_file_of_4_gb_or_more_is_left_out_by_name_and_the_rest_go() {
    let p = phone(1 << 40).await;
    // Sparse: 4.7 GB that costs no disk, and is never read.
    fs::File::create(p.dir.path().join("DCIM/VID.mp4"))
        .unwrap()
        .set_len(4_700_000_000)
        .unwrap();
    let serial = p.device.connect().await.unwrap().serial_number;
    mtp_rs::rescan_virtual_device(&serial);
    let transfer = p
        .transfer()
        .with_room(mac_kind(false, Some(0xFFFF_FFFF), "on “KINGSTON”"));

    let summary = transfer
        .download(
            &p.download(&["a.jpg", "VID.mp4", "b.jpg"]),
            &Cancel::new(),
            |_| {},
        )
        .await
        .unwrap();

    assert_eq!(summary.files, 2, "a.jpg and b.jpg");
    assert_eq!(summary.bytes, 50 * KIB, "the video's bytes never moved");
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].label, "VID.mp4");
    assert_eq!(
        summary.failures[0].error,
        Error::TooBigForVolume("on “KINGSTON”".into())
    );
    assert!(!p.mac.path().join("VID.mp4").exists());
    let mut names: Vec<String> = fs::read_dir(p.mac.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["a.jpg", "b.jpg"], "no part file either");
}

#[tokio::test]
async fn on_fat32_a_selection_of_only_big_files_says_so_and_copies_nothing() {
    let p = phone(1 << 40).await;
    fs::File::create(p.dir.path().join("DCIM/VID.mp4"))
        .unwrap()
        .set_len(4_700_000_000)
        .unwrap();
    let serial = p.device.connect().await.unwrap().serial_number;
    mtp_rs::rescan_virtual_device(&serial);
    let transfer = p
        .transfer()
        .with_room(mac_kind(false, Some(0xFFFF_FFFF), "on “KINGSTON”"));

    let refused = transfer
        .download(&p.download(&["VID.mp4"]), &Cancel::new(), |_| {})
        .await;

    assert_eq!(
        refused.unwrap_err(),
        Error::TooBigForVolume("on “KINGSTON”".into()),
        "one file, so the whole download's answer"
    );
}

/// The phone with a sparse 4.7 GB DCIM/VID.mp4 beside a.jpg and b.jpg.
async fn phone_with_video() -> Phone {
    let p = phone(1 << 40).await;
    fs::File::create(p.dir.path().join("DCIM/VID.mp4"))
        .unwrap()
        .set_len(4_700_000_000)
        .unwrap();
    let serial = p.device.connect().await.unwrap().serial_number;
    mtp_rs::rescan_virtual_device(&serial);
    p
}

#[tokio::test]
async fn a_file_too_big_for_fat32_needs_no_room_there_since_it_isnt_copied() {
    let p = phone_with_video().await;
    let fat = Room {
        most_per_file: Some(0xFFFF_FFFF),
        ..Room::free(60 * KIB, "on “KINGSTON”")
    };

    let summary = p
        .transfer()
        .with_room(fat)
        .download(
            &p.download(&["a.jpg", "VID.mp4", "b.jpg"]),
            &Cancel::new(),
            |_| {},
        )
        .await
        .unwrap();

    assert_eq!(
        summary.files, 2,
        "50 KiB fit in 60; the video was never going"
    );
}

#[tokio::test]
async fn a_part_left_of_a_file_too_big_for_fat32_is_not_carried_on_either() {
    let p = phone_with_video().await;
    // What an earlier try left, stamped with the phone's date as a part is.
    let modified = fs::metadata(p.dir.path().join("DCIM/VID.mp4"))
        .unwrap()
        .modified()
        .unwrap();
    let whole_seconds = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(
            modified
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
    let part = p.mac.path().join(".VID.mp4.open-mtp-client-part");
    fs::write(&part, vec![0u8; 1024]).unwrap();
    fs::File::options()
        .write(true)
        .open(&part)
        .unwrap()
        .set_modified(whole_seconds)
        .unwrap();

    let refused = p
        .transfer()
        .with_room(mac_kind(false, Some(0xFFFF_FFFF), "on “KINGSTON”"))
        .download(&p.download(&["VID.mp4"]), &Cancel::new(), |_| {})
        .await;

    assert_eq!(
        refused.unwrap_err(),
        Error::TooBigForVolume("on “KINGSTON”".into())
    );
    assert_eq!(
        fs::metadata(&part).unwrap().len(),
        1024,
        "nothing added to it"
    );
}
