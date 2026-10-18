//! What the explorer does with folders it remembers: it should not ask the
//! phone about every file again to find three of them, or to show the same
//! folder twice, and it must never act on a file that has changed.
//!
//! Each test counts `full_reads`: how many times a whole folder was read.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use super::Service;
use crate::dto::{Cancel, Error};
use crate::service::device;
use crate::service::testing::virtual_phone;

struct Phone {
    dir: tempfile::TempDir,
    device: Arc<device::Service>,
    service: Arc<Service>,
    storage: u64,
    serial: String,
}

const CAMERA: &str = "/DCIM/Camera";

/// A phone with `photos` files in DCIM/Camera (IMG_0000.jpg…), an empty Music
/// folder, and notes.txt at the root.
async fn phone(photos: usize) -> Phone {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("DCIM/Camera")).unwrap();
    fs::create_dir_all(dir.path().join("Music")).unwrap();
    fs::write(dir.path().join("notes.txt"), b"notes").unwrap();
    for i in 0..photos {
        fs::write(
            dir.path().join(format!("DCIM/Camera/IMG_{i:04}.jpg")),
            format!("photo {i}"),
        )
        .unwrap();
    }
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(dir.path()),
    )));
    let serial = device.connect().await.unwrap().serial_number;
    let storage = device.storages().await.unwrap()[0].id;
    Phone {
        dir,
        service: Arc::new(Service::new(Arc::clone(&device))),
        device,
        storage,
        serial,
    }
}

fn camera_file(i: usize) -> String {
    format!("IMG_{i:04}.jpg")
}

impl Phone {
    fn reads(&self) -> usize {
        self.service.full_reads()
    }

    /// The phone looks again after something changed behind the app's back.
    fn rescan(&self) {
        mtp_rs::rescan_virtual_device(&self.serial);
    }

    /// Shows a folder as the pane would: the names in it, and in how many pieces.
    async fn show(
        &self,
        path: &str,
        allow_remembered: bool,
    ) -> Result<(Vec<String>, usize), Error> {
        self.show_with_hidden(path, false, allow_remembered).await
    }

    async fn show_with_hidden(
        &self,
        path: &str,
        show_hidden: bool,
        allow_remembered: bool,
    ) -> Result<(Vec<String>, usize), Error> {
        let mut listing = self
            .service
            .open_listing(
                self.storage,
                path,
                show_hidden,
                Cancel::new(),
                allow_remembered,
            )
            .await?;
        let (mut names, mut pieces) = (Vec::new(), 0);
        while let Some(update) = listing.next().await {
            let update = update?;
            pieces += 1;
            names.extend(update.entries.into_iter().map(|entry| entry.name));
        }
        names.sort();
        Ok((names, pieces))
    }

    fn path(&self, relative: &str) -> std::path::PathBuf {
        self.dir.path().join(relative)
    }
}

fn camera_names(from: usize, to: usize) -> Vec<String> {
    (from..to).map(camera_file).collect()
}

#[tokio::test]
async fn reading_a_folder_in_full_remembers_it() {
    let p = phone(30).await;
    let (first, _) = p.show(CAMERA, true).await.unwrap();
    assert_eq!(p.reads(), 1);

    let (again, pieces) = p.show(CAMERA, true).await.unwrap();

    assert_eq!(again, first);
    assert_eq!(first, camera_names(0, 30));
    assert_eq!(
        p.reads(),
        1,
        "the second showing asked the phone about no file"
    );
    assert_eq!(pieces, 1, "handed over in one piece");
}

#[tokio::test]
async fn a_folder_that_gained_or_lost_an_item_is_read_again() {
    let p = phone(10).await;
    p.show(CAMERA, true).await.unwrap();

    fs::write(p.path("DCIM/Camera/NEW.jpg"), b"new").unwrap();
    p.rescan();
    let (with_new, _) = p.show(CAMERA, true).await.unwrap();
    assert!(with_new.contains(&"NEW.jpg".to_owned()));
    assert_eq!(p.reads(), 2, "the count changed, so it was read for real");

    fs::remove_file(p.path("DCIM/Camera/IMG_0003.jpg")).unwrap();
    p.rescan();
    let (without, _) = p.show(CAMERA, true).await.unwrap();
    assert!(!without.contains(&camera_file(3)));
    assert_eq!(p.reads(), 3);
}

#[tokio::test]
async fn asking_to_see_the_phone_as_it_is_now_always_reads() {
    let p = phone(10).await;
    p.show(CAMERA, true).await.unwrap();

    p.show(CAMERA, false).await.unwrap();

    assert_eq!(p.reads(), 2);
    // And what it read is remembered again.
    p.show(CAMERA, true).await.unwrap();
    assert_eq!(p.reads(), 2);
}

#[tokio::test]
async fn a_new_session_does_not_trust_what_an_old_one_read() {
    let p = phone(10).await;
    p.show(CAMERA, true).await.unwrap();

    p.device.disconnect().await.unwrap();
    p.device.connect().await.unwrap();
    p.show(CAMERA, true).await.unwrap();

    assert_eq!(
        p.reads(),
        2,
        "Android may have re-keyed every file in between"
    );
}

#[tokio::test]
async fn hidden_files_are_shown_or_hidden_from_the_same_reading() {
    let p = phone(5).await;
    fs::write(p.path("DCIM/Camera/.thumbnails"), b"x").unwrap();
    p.rescan();
    let (shown, _) = p.show_with_hidden(CAMERA, false, true).await.unwrap();
    assert!(!shown.contains(&".thumbnails".to_owned()));

    let (with_hidden, _) = p.show_with_hidden(CAMERA, true, true).await.unwrap();

    assert!(with_hidden.contains(&".thumbnails".to_owned()));
    assert_eq!(
        p.reads(),
        1,
        "toggling hidden files doesn't ask the phone again"
    );
}

#[tokio::test]
async fn a_cancelled_reading_is_not_remembered() {
    let p = phone(600).await;
    let cancel = Cancel::new();
    let mut listing = p
        .service
        .open_listing(p.storage, CAMERA, false, cancel.clone(), true)
        .await
        .unwrap();
    assert!(!listing.next().await.unwrap().unwrap().done);
    cancel.cancel();
    assert!(listing.next().await.is_none());

    p.show(CAMERA, true).await.unwrap();

    assert_eq!(
        p.reads(),
        1,
        "the unfinished reading was not kept, so this one read in full"
    );
}

#[tokio::test]
async fn renaming_in_a_remembered_folder_asks_about_one_file_not_all() {
    let p = phone(40).await;
    p.show(CAMERA, true).await.unwrap();
    assert_eq!(p.reads(), 1);

    let renamed = p
        .service
        .rename(p.storage, CAMERA, &camera_file(7), "sunset.jpg")
        .await
        .unwrap();

    assert_eq!(renamed, "sunset.jpg");
    assert_eq!(p.reads(), 1, "no whole-folder read to find one file");
    assert!(p.path("DCIM/Camera/sunset.jpg").exists());
    // The refresh that follows is served from the amended memory.
    let (after, _) = p.show(CAMERA, true).await.unwrap();
    assert_eq!(p.reads(), 1);
    assert!(after.contains(&"sunset.jpg".to_owned()));
    assert!(!after.contains(&camera_file(7)));
}

#[tokio::test]
async fn a_name_clash_is_found_in_what_is_remembered() {
    let p = phone(10).await;
    p.show(CAMERA, true).await.unwrap();

    let clash = p
        .service
        .rename(p.storage, CAMERA, &camera_file(1), &camera_file(2))
        .await;
    let new_folder = p
        .service
        .make_folder(p.storage, CAMERA, &camera_file(3).to_uppercase())
        .await;

    assert_eq!(clash, Err(Error::NameTaken(camera_file(2))));
    assert!(
        matches!(new_folder, Err(Error::NameTaken(_))),
        "{new_folder:?}"
    );
    assert_eq!(p.reads(), 1);
}

#[tokio::test]
async fn deleting_from_a_remembered_folder_asks_about_the_chosen_files_only() {
    let p = phone(40).await;
    p.show(CAMERA, true).await.unwrap();

    p.service
        .delete(p.storage, CAMERA, &[camera_file(2), camera_file(5)])
        .await
        .unwrap();

    assert_eq!(p.reads(), 1);
    assert!(!p.path("DCIM/Camera/IMG_0002.jpg").exists());
    assert!(!p.path("DCIM/Camera/IMG_0005.jpg").exists());
    let (after, pieces) = p.show(CAMERA, true).await.unwrap();
    assert_eq!(p.reads(), 1, "the refresh after a delete reads nothing");
    assert_eq!(pieces, 1);
    assert_eq!(after.len(), 38);
}

#[tokio::test]
async fn a_folder_made_in_a_remembered_folder_shows_without_reading_it_again() {
    let p = phone(20).await;
    p.show(CAMERA, true).await.unwrap();

    p.service
        .make_folder(p.storage, CAMERA, "Trip")
        .await
        .unwrap();

    assert_eq!(p.reads(), 1);
    let (after, _) = p.show(CAMERA, true).await.unwrap();
    assert_eq!(p.reads(), 1);
    assert!(after.contains(&"Trip".to_owned()));
}

#[tokio::test]
async fn deleting_a_folder_forgets_the_folders_inside_it() {
    let p = phone(5).await;
    fs::create_dir_all(p.path("DCIM/Camera/Old")).unwrap();
    fs::write(p.path("DCIM/Camera/Old/a.jpg"), b"a").unwrap();
    p.rescan();
    p.show("/DCIM/Camera/Old", true).await.unwrap();
    p.show(CAMERA, true).await.unwrap();
    let reads = p.reads();

    p.service
        .delete(p.storage, CAMERA, &["Old".to_owned()])
        .await
        .unwrap();

    // Bring a different folder up under the same name: what was remembered
    // under the old one must not come back.
    fs::create_dir_all(p.path("DCIM/Camera/Old")).unwrap();
    fs::write(p.path("DCIM/Camera/Old/b.jpg"), b"b").unwrap();
    p.rescan();
    let (names, _) = p.show("/DCIM/Camera/Old", true).await.unwrap();
    assert_eq!(names, ["b.jpg"]);
    assert_eq!(p.reads(), reads + 1, "read afresh, not recalled");
}

#[tokio::test]
async fn renaming_a_folder_forgets_the_folders_inside_it() {
    let p = phone(2).await;
    fs::create_dir_all(p.path("DCIM/Camera/Old")).unwrap();
    fs::write(p.path("DCIM/Camera/Old/a.jpg"), b"a").unwrap();
    p.rescan();
    p.show("/DCIM/Camera/Old", true).await.unwrap();
    p.show(CAMERA, true).await.unwrap();

    p.service
        .rename(p.storage, CAMERA, "Old", "Older")
        .await
        .unwrap();

    let (names, _) = p.show("/DCIM/Camera/Older", true).await.unwrap();
    assert_eq!(names, ["a.jpg"]);
    assert!(matches!(
        p.show("/DCIM/Camera/Old", true).await,
        Err(Error::PathNotFound(_))
    ));
}

#[tokio::test]
async fn a_file_that_vanished_is_not_acted_on_from_memory() {
    let p = phone(10).await;
    p.show(CAMERA, true).await.unwrap();
    // IMG_0003 goes and another file appears: the count is the same.
    fs::remove_file(p.path("DCIM/Camera/IMG_0003.jpg")).unwrap();
    fs::write(p.path("DCIM/Camera/swap.jpg"), b"swap").unwrap();
    p.rescan();

    let result = p
        .service
        .rename(p.storage, CAMERA, &camera_file(3), "x.jpg")
        .await;

    assert!(matches!(result, Err(Error::PathNotFound(_))), "{result:?}");
    assert_eq!(
        p.reads(),
        2,
        "found out by asking about that file, then read for real"
    );
}

#[tokio::test]
async fn a_file_the_phone_re_keyed_is_found_again() {
    let p = phone(10).await;
    p.show(CAMERA, true).await.unwrap();
    // The media scanner gives the file a new handle; the count is unchanged.
    assert!(
        mtp_rs::rekey_virtual_object(&p.serial, Path::new("DCIM/Camera/IMG_0004.jpg")).is_some()
    );

    let renamed = p
        .service
        .rename(p.storage, CAMERA, &camera_file(4), "fresh.jpg")
        .await
        .unwrap();

    assert_eq!(renamed, "fresh.jpg");
    assert!(p.path("DCIM/Camera/fresh.jpg").exists());
    assert_eq!(
        p.reads(),
        2,
        "the stale handle was noticed, and the folder read for real"
    );
}

#[tokio::test]
async fn a_change_to_some_other_file_does_not_force_a_read() {
    let p = phone(10).await;
    p.show(CAMERA, true).await.unwrap();
    // IMG_0009 is re-keyed, but IMG_0001 is the one being renamed.
    assert!(
        mtp_rs::rekey_virtual_object(&p.serial, Path::new("DCIM/Camera/IMG_0009.jpg")).is_some()
    );

    p.service
        .rename(p.storage, CAMERA, &camera_file(1), "one.jpg")
        .await
        .unwrap();

    assert_eq!(p.reads(), 1, "only the file being acted on was checked");
}

#[tokio::test]
async fn deleting_a_name_that_was_never_there_is_an_error_after_a_real_read() {
    let p = phone(5).await;
    p.show(CAMERA, true).await.unwrap();

    let result = p
        .service
        .delete(p.storage, CAMERA, &["nope.jpg".to_owned()])
        .await;

    assert!(matches!(result, Err(Error::PathNotFound(_))));
    assert_eq!(
        p.reads(),
        2,
        "a name that isn't remembered may be new: read to be sure"
    );
}

#[tokio::test]
async fn a_folder_that_was_never_read_is_read_when_an_edit_needs_it() {
    let p = phone(5).await;

    p.service
        .rename(p.storage, CAMERA, &camera_file(0), "zero.jpg")
        .await
        .unwrap();

    assert!(p.reads() >= 1);
    let reads = p.reads();
    // That reading is remembered, so a second edit costs none.
    p.service
        .rename(p.storage, CAMERA, &camera_file(1), "one.jpg")
        .await
        .unwrap();
    assert_eq!(p.reads(), reads);
}

#[tokio::test]
async fn the_remembered_folder_is_not_confused_with_another_folder_of_the_same_name() {
    let p = phone(3).await;
    fs::create_dir_all(p.path("Music/Camera")).unwrap();
    fs::write(p.path("Music/Camera/song.mp3"), b"x").unwrap();
    p.rescan();
    p.show(CAMERA, true).await.unwrap();

    let (music, _) = p.show("/Music/Camera", true).await.unwrap();

    assert_eq!(music, ["song.mp3"]);
}
