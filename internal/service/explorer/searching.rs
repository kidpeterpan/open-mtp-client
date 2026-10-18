//! Walking the whole phone for a search: what it finds, what it leaves out,
//! and what it costs the second time.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use super::Service;
use crate::dto::{Cancel, Error, Found, WalkUpdate};
use crate::service::device;
use crate::service::testing::virtual_phone;

struct Phone {
    root: tempfile::TempDir,
    device: Arc<device::Service>,
    service: Arc<Service>,
    storage: u64,
    serial: String,
}

/// A phone with DCIM/Camera/{a.jpg, b.mp4}, Music/song.mp3, notes.txt,
/// .thumbnails/t1.jpg and DCIM/.hidden.jpg.
async fn phone() -> Phone {
    let root = tempfile::tempdir().unwrap();
    for (path, content) in [
        ("DCIM/Camera/a.jpg", "aa"),
        ("DCIM/Camera/b.mp4", "bbb"),
        ("Music/song.mp3", "s"),
        ("notes.txt", "n"),
        (".thumbnails/t1.jpg", "t"),
        ("DCIM/.hidden.jpg", "h"),
    ] {
        let at = root.path().join(path);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        fs::write(at, content).unwrap();
    }
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(root.path()),
    )));
    let serial = device.connect().await.unwrap().serial_number;
    let storage = device.storages().await.unwrap()[0].id;
    let service = Arc::new(Service::new(Arc::clone(&device)));
    Phone {
        root,
        device,
        service,
        storage,
        serial,
    }
}

impl Phone {
    fn rescan(&self) {
        mtp_rs::rescan_virtual_device(&self.serial);
    }

    async fn walk(&self, hidden: bool) -> Vec<WalkUpdate> {
        let mut walk = self
            .service
            .walk(self.storage, hidden, Cancel::new())
            .await
            .unwrap();
        let mut pieces = Vec::new();
        while let Some(piece) = walk.next().await {
            pieces.push(piece.unwrap());
        }
        pieces
    }

    /// Everything a walk saw, as "folder/name", and how many full reads it took.
    async fn seen(&self, hidden: bool) -> (BTreeSet<String>, usize) {
        let before = self.service.full_reads();
        let mut seen = BTreeSet::new();
        for piece in self.walk(hidden).await {
            if piece.restart {
                seen.clear();
            }
            seen.extend(piece.found.iter().map(path_of));
        }
        (seen, self.service.full_reads() - before)
    }
}

fn path_of(found: &Found) -> String {
    format!(
        "{}/{}",
        found.folder.trim_end_matches('/'),
        found.entry.name
    )
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[tokio::test]
async fn a_walk_sees_every_file_and_folder_once_with_where_it_is() {
    let p = phone().await;

    let pieces = p.walk(false).await;

    let seen: Vec<String> = pieces
        .iter()
        .flat_map(|piece| piece.found.iter().map(path_of))
        .collect();
    assert_eq!(
        seen.iter().cloned().collect::<BTreeSet<_>>(),
        set(&[
            "/DCIM",
            "/DCIM/Camera",
            "/DCIM/Camera/a.jpg",
            "/DCIM/Camera/b.mp4",
            "/Music",
            "/Music/song.mp3",
            "/notes.txt",
        ])
    );
    assert_eq!(seen.len(), 7, "nothing twice");
    let last = pieces.last().unwrap();
    assert!(last.done && !last.truncated);
    assert_eq!(
        (last.folders_read, last.folders_left, last.items),
        (4, 0, 7)
    );
    let b = pieces
        .iter()
        .flat_map(|piece| &piece.found)
        .find(|found| found.entry.name == "b.mp4")
        .unwrap();
    assert_eq!(b.entry.size_bytes, 3);
    assert!(!b.entry.is_folder && b.entry.id != 0);
}

#[tokio::test]
async fn hidden_folders_and_files_are_walked_only_when_hidden_files_are_shown() {
    let p = phone().await;

    let (shown, _) = p.seen(true).await;

    assert!(shown.contains("/.thumbnails/t1.jpg"));
    assert!(shown.contains("/DCIM/.hidden.jpg"));
    let (hidden, _) = p.seen(false).await;
    assert!(!hidden.iter().any(|path| path.contains("/.")), "{hidden:?}");
}

#[tokio::test]
async fn walking_again_reads_no_folder_in_full_that_has_not_changed() {
    let p = phone().await;
    let (first, reads) = p.seen(false).await;
    assert_eq!(reads, 4, "every folder, once");

    let (again, reads) = p.seen(false).await;

    assert_eq!(again, first);
    assert_eq!(reads, 0, "one request per folder, how many items it holds");
}

#[tokio::test]
async fn a_folder_that_changed_on_the_phone_is_read_again_and_only_it() {
    let p = phone().await;
    p.seen(false).await;
    fs::write(p.root.path().join("Music/new.mp3"), b"n").unwrap();
    p.rescan();

    let (seen, reads) = p.seen(false).await;

    assert!(seen.contains("/Music/new.mp3"));
    // Music, and the root: Android counts everything on the storage as the
    // root's (it holds a few dozen items, so that costs little).
    assert_eq!(reads, 2, "Music and the root, not DCIM or Camera");
}

#[tokio::test]
async fn a_new_connection_does_not_trust_the_index() {
    let p = phone().await;
    p.seen(false).await;
    p.device.disconnect().await.unwrap();
    p.device.connect().await.unwrap();

    let (_, reads) = p.seen(false).await;

    assert_eq!(reads, 4, "files may have new handles: everything again");
}

#[tokio::test]
async fn what_the_app_renames_or_deletes_is_kept_true_in_the_index() {
    // A rename keeps the folder's count, so only the app can say what changed;
    // a stale name found by a search could not be downloaded.
    let p = phone().await;
    p.seen(false).await;
    p.service
        .rename(p.storage, "/DCIM/Camera", "a.jpg", "renamed.jpg")
        .await
        .unwrap();

    let (renamed, reads) = p.seen(false).await;

    assert!(renamed.contains("/DCIM/Camera/renamed.jpg"));
    assert!(!renamed.contains("/DCIM/Camera/a.jpg"));
    assert_eq!(reads, 0, "nothing had to be read again");

    p.service
        .delete(p.storage, "/Music", &["song.mp3".into()])
        .await
        .unwrap();
    let (deleted, reads) = p.seen(false).await;

    assert!(!deleted.contains("/Music/song.mp3"));
    assert_eq!(
        reads, 1,
        "only the root, whose count is the whole storage's"
    );
}

#[tokio::test]
async fn a_renamed_folder_is_walked_under_its_new_name() {
    let p = phone().await;
    p.seen(false).await;
    p.service
        .rename(p.storage, "/", "Music", "Songs")
        .await
        .unwrap();

    let (seen, _) = p.seen(false).await;

    assert!(seen.contains("/Songs/song.mp3"), "{seen:?}");
    assert!(!seen.iter().any(|path| path.starts_with("/Music")));
}

#[tokio::test]
async fn opening_a_folder_a_walk_has_read_needs_no_full_read() {
    // Downloading a search result opens its folder: from the index, not by
    // asking the phone about every file in it again.
    let p = phone().await;
    for i in 0..300 {
        fs::write(
            p.root.path().join(format!("DCIM/Camera/IMG_{i:04}.jpg")),
            b"x",
        )
        .unwrap();
    }
    p.rescan();
    p.seen(false).await;
    let before = p.service.full_reads();

    let folder = p
        .service
        .open_folder_confirmed(p.storage, "/DCIM/Camera", &["IMG_0042.jpg".into()], true)
        .await
        .unwrap();

    assert_eq!(p.service.full_reads(), before);
    assert!(folder.child_named("IMG_0042.jpg").is_some());
}

#[tokio::test]
async fn a_cancelled_walk_ends_between_folders() {
    let p = phone().await;
    let cancel = Cancel::new();
    let mut walk = p
        .service
        .walk(p.storage, false, cancel.clone())
        .await
        .unwrap();
    cancel.cancel();

    assert!(walk.next().await.is_none());
    assert!(walk.next().await.is_none(), "and stays over");
}

#[tokio::test]
async fn a_walk_comes_in_pieces_and_says_how_far_it_is() {
    let p = phone().await;
    for folder in 0..12 {
        for i in 0..100 {
            let at = p.root.path().join(format!("Big/F{folder:02}/{i:03}.jpg"));
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, b"x").unwrap();
        }
    }
    p.rescan();

    let pieces = p.walk(false).await;

    assert!(pieces.len() > 1, "more than one piece");
    assert!(pieces.iter().all(|piece| piece.found.len() <= 600));
    assert!(pieces.windows(2).all(|pair| pair[0].items <= pair[1].items));
    assert!(pieces[..pieces.len() - 1].iter().all(|piece| !piece.done));
    assert_eq!(pieces.last().unwrap().items, 7 + 1 + 12 + 1_200);
}

#[tokio::test]
async fn files_re_keyed_during_a_walk_make_it_start_over_once() {
    let p = phone().await;
    for folder in ["A", "B", "C"] {
        for i in 0..600 {
            let at = p.root.path().join(format!("{folder}/{i:03}.jpg"));
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, b"x").unwrap();
        }
    }
    p.rescan();
    let mut walk = p
        .service
        .walk(p.storage, false, Cancel::new())
        .await
        .unwrap();
    let first = walk.next().await.unwrap().unwrap();
    assert!(!first.done);
    // What Android's media scanner does: every folder gets a new handle.
    for folder in ["A", "B", "C", "DCIM", "Music"] {
        mtp_rs::rekey_virtual_object(&p.serial, Path::new(folder));
    }

    let mut rest = Vec::new();
    while let Some(piece) = walk.next().await {
        rest.push(piece.unwrap());
    }

    assert!(
        rest.iter().any(|piece| piece.restart),
        "told to forget what came before"
    );
    let after: Vec<String> = rest
        .iter()
        .skip_while(|piece| !piece.restart)
        .flat_map(|piece| piece.found.iter().map(path_of))
        .collect();
    assert_eq!(
        after.len(),
        7 + 3 + 1_800,
        "everything once, after the restart"
    );
}

#[tokio::test]
async fn a_walk_without_a_phone_says_so() {
    let p = phone().await;
    p.device.disconnect().await.unwrap();

    let result = p.service.walk(p.storage, false, Cancel::new()).await;

    assert!(matches!(result, Err(Error::NotConnected)));
}

#[tokio::test]
async fn five_thousand_files_are_walked_and_the_second_walk_reads_no_file() {
    let p = phone().await;
    for folder in 0..50 {
        for i in 0..100 {
            let at = p
                .root
                .path()
                .join(format!("DCIM/F{folder:02}/IMG_{i:03}.jpg"));
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, b"x").unwrap();
        }
    }
    p.rescan();
    let started = std::time::Instant::now();
    let (first, reads) = p.seen(false).await;
    eprintln!(
        "first walk of 5,000 files in 50 folders: {:?}",
        started.elapsed()
    );
    let again_started = std::time::Instant::now();
    let (again, again_reads) = p.seen(false).await;
    eprintln!("second walk: {:?}", again_started.elapsed());

    assert!(first.len() > 5_000);
    assert_eq!(again, first);
    assert_eq!((reads, again_reads), (54, 0));
}

#[tokio::test]
async fn a_walk_from_a_folder_sees_only_what_is_below_it() {
    let p = phone().await;

    let mut walk = p
        .service
        .walk_from(p.storage, "/DCIM", false, Cancel::new())
        .await
        .unwrap();
    let mut seen = BTreeSet::new();
    while let Some(piece) = walk.next().await {
        seen.extend(piece.unwrap().found.iter().map(path_of));
    }

    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        ["/DCIM/Camera", "/DCIM/Camera/a.jpg", "/DCIM/Camera/b.mp4"]
    );
}

#[tokio::test]
async fn a_walk_from_a_folder_that_is_not_there_says_so() {
    let p = phone().await;

    let walk = p
        .service
        .walk_from(p.storage, "/Nowhere", false, Cancel::new())
        .await;

    assert!(matches!(walk, Err(Error::PathNotFound(path)) if path == "/Nowhere"));
}

#[tokio::test]
async fn a_walk_from_a_folder_starts_over_there_when_the_phone_re_keys() {
    let p = phone().await;
    for folder in ["A", "B"] {
        for i in 0..600 {
            let at = p.root.path().join(format!("DCIM/{folder}/{i:04}.jpg"));
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, b"x").unwrap();
        }
    }
    p.rescan();
    let mut walk = p
        .service
        .walk_from(p.storage, "/DCIM", false, Cancel::new())
        .await
        .unwrap();
    let first = walk.next_raw().await.unwrap().unwrap();
    assert!(!first.0.done);
    for folder in ["DCIM", "DCIM/A", "DCIM/B", "DCIM/Camera"] {
        mtp_rs::rekey_virtual_object(&p.serial, Path::new(folder));
    }

    let mut after = BTreeSet::new();
    let mut restarted = false;
    while let Some(piece) = walk.next_raw().await {
        let (update, seen) = piece.unwrap();
        if update.restart {
            restarted = true;
            after.clear();
        }
        after.extend(
            seen.iter()
                .map(|(folder, o)| format!("{folder}/{}", o.filename)),
        );
    }

    assert!(restarted);
    assert_eq!(
        after.len(),
        3 + 1_200 + 2,
        "A, B, Camera, and what is in them"
    );
    assert!(after.iter().all(|path| path.starts_with("/DCIM/")));
}
