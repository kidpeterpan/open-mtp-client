//! Copying and moving inside the phone, end to end on the virtual phone: what
//! ends up where, and that nothing is ever overwritten or emptied.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use super::Service;
use crate::dto::{Cancel, Error, Paste, PasteRequest, PasteSummary};
use crate::service::device;
use crate::service::testing::virtual_phone;

struct Phone {
    dir: tempfile::TempDir,
    device: Arc<device::Service>,
    service: Arc<Service>,
    storage: u64,
    serial: String,
}

/// DCIM/Camera/{a.jpg "A", b.jpg "B"}, Download/{notes.txt, a.jpg "other A"},
/// an empty Music, and Trip/inner/deep.txt.
async fn phone() -> Phone {
    phone_with(|_| {}).await
}

async fn phone_with(limit: impl FnOnce(&mut device::VirtualPhone)) -> Phone {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("DCIM/Camera")).unwrap();
    fs::create_dir_all(root.join("Download")).unwrap();
    fs::create_dir_all(root.join("Music")).unwrap();
    fs::create_dir_all(root.join("Trip/inner")).unwrap();
    fs::write(root.join("DCIM/Camera/a.jpg"), b"A").unwrap();
    fs::write(root.join("DCIM/Camera/b.jpg"), b"B").unwrap();
    fs::write(root.join("Download/notes.txt"), b"notes").unwrap();
    fs::write(root.join("Download/a.jpg"), b"other A").unwrap();
    fs::write(root.join("Trip/inner/deep.txt"), b"deep").unwrap();

    let mut config = virtual_phone(root);
    limit(&mut config);
    let device = Arc::new(device::Service::new(device::Source::Virtual(config)));
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

impl Phone {
    fn request(&self, mode: Paste, from: &str, names: &[&str], to: &str) -> PasteRequest {
        PasteRequest {
            mode,
            from_storage: self.storage,
            from: from.into(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
            to_storage: self.storage,
            to: to.into(),
        }
    }

    async fn paste(
        &self,
        mode: Paste,
        from: &str,
        names: &[&str],
        to: &str,
    ) -> Result<PasteSummary, Error> {
        self.service
            .paste(&self.request(mode, from, names, to))
            .await
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn names(&self, relative: &str) -> BTreeSet<String> {
        fs::read_dir(self.path(relative))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn read(&self, relative: &str) -> Vec<u8> {
        fs::read(self.path(relative)).unwrap()
    }

    /// What the explorer shows in a folder.
    async fn shows(&self, path: &str) -> BTreeSet<String> {
        self.service
            .list(self.storage, path, true)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect()
    }
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

#[tokio::test]
async fn copying_a_file_to_another_folder_leaves_the_original() {
    let p = phone().await;

    let summary = p
        .paste(Paste::Copy, "/DCIM/Camera", &["b.jpg"], "/Music")
        .await
        .unwrap();

    assert_eq!(p.read("DCIM/Camera/b.jpg"), b"B");
    assert_eq!(p.read("Music/b.jpg"), b"B");
    assert_eq!((summary.done, summary.renamed), (1, 0));
    assert_eq!(summary.names, ["b.jpg"]);
    assert_eq!(summary.into, "/Music");
    assert_eq!(p.shows("/Music").await, set(&["b.jpg"]));
}

#[tokio::test]
async fn copying_a_file_into_its_own_folder_makes_a_numbered_copy_and_keeps_the_original_whole() {
    // The virtual phone would copy the file onto itself, which can empty it.
    let p = phone().await;

    let summary = p
        .paste(Paste::Copy, "/DCIM/Camera", &["a.jpg"], "/DCIM/Camera")
        .await
        .unwrap();

    assert_eq!(
        p.read("DCIM/Camera/a.jpg"),
        b"A",
        "the original is untouched"
    );
    assert_eq!(p.read("DCIM/Camera/a (1).jpg"), b"A");
    assert_eq!((summary.done, summary.renamed), (1, 1));
    assert_eq!(summary.names, ["a (1).jpg"]);
    assert_eq!(
        p.names("DCIM/Camera"),
        set(&["a.jpg", "a (1).jpg", "b.jpg"])
    );
}

#[tokio::test]
async fn copying_the_same_file_again_takes_the_next_number() {
    let p = phone().await;
    for _ in 0..2 {
        p.paste(Paste::Copy, "/DCIM/Camera", &["a.jpg"], "/DCIM/Camera")
            .await
            .unwrap();
    }

    assert_eq!(
        p.names("DCIM/Camera"),
        set(&["a.jpg", "a (1).jpg", "a (2).jpg", "b.jpg"])
    );
    assert_eq!(p.read("DCIM/Camera/a.jpg"), b"A");
}

#[tokio::test]
async fn copying_onto_a_different_file_with_the_same_name_keeps_both() {
    let p = phone().await;

    let summary = p
        .paste(Paste::Copy, "/DCIM/Camera", &["a.jpg"], "/Download")
        .await
        .unwrap();

    assert_eq!(
        p.read("Download/a.jpg"),
        b"other A",
        "what was there is untouched"
    );
    assert_eq!(
        p.read("Download/a (1).jpg"),
        b"A",
        "the copy is the source's content"
    );
    assert_eq!(summary.renamed, 1);
}

#[tokio::test]
async fn several_items_with_a_folder_copy_together_with_everything_inside() {
    let p = phone().await;

    let summary = p
        .paste(Paste::Copy, "/", &["Trip", "Download"], "/Music")
        .await
        .unwrap();

    assert_eq!(p.read("Music/Trip/inner/deep.txt"), b"deep");
    assert_eq!(p.read("Music/Download/notes.txt"), b"notes");
    assert!(p.path("Trip/inner/deep.txt").exists(), "the originals stay");
    assert_eq!(summary.done, 2);
    assert_eq!(p.names("Music"), set(&["Trip", "Download"]));
}

#[tokio::test]
async fn no_scratch_folder_is_left_behind() {
    let p = phone().await;
    p.paste(
        Paste::Copy,
        "/DCIM/Camera",
        &["a.jpg", "b.jpg"],
        "/DCIM/Camera",
    )
    .await
    .unwrap();
    p.paste(Paste::Copy, "/DCIM/Camera", &["a.jpg"], "/Download")
        .await
        .unwrap();

    for folder in ["DCIM/Camera", "Download"] {
        assert!(
            p.names(folder)
                .iter()
                .all(|n| !n.starts_with(".open-mtp-client")),
            "{folder}: {:?}",
            p.names(folder)
        );
    }
    assert!(
        p.shows("/DCIM/Camera")
            .await
            .iter()
            .all(|n| !n.starts_with(".open-mtp-client"))
    );
}

#[tokio::test]
async fn the_number_goes_before_the_extension_whatever_the_file_is() {
    // Clashes ignoring case are covered against the fake phone; the virtual
    // phone's folder is case-sensitive on disk, so this checks the naming.
    let p = phone().await;

    let summary = p
        .paste(Paste::Copy, "/Download", &["notes.txt"], "/Download")
        .await
        .unwrap();

    assert_eq!(p.read("Download/notes.txt"), b"notes");
    assert_eq!(p.read("Download/notes (1).txt"), b"notes");
    assert_eq!(summary.names, ["notes (1).txt"]);
    assert_eq!(summary.renamed, 1);
}

#[tokio::test]
async fn a_hidden_file_already_there_is_never_copied_or_moved_over() {
    // `.nomedia` is on every Android phone; hidden names must count as taken.
    for mode in [Paste::Copy, Paste::Move] {
        let p = phone().await;
        fs::write(p.path("DCIM/Camera/.nomedia"), b"camera").unwrap();
        fs::write(p.path("Download/.nomedia"), b"downloads").unwrap();
        mtp_rs::rescan_virtual_device(&p.serial);

        let summary = p
            .paste(mode, "/DCIM/Camera", &[".nomedia"], "/Download")
            .await
            .unwrap();

        assert_eq!(p.read("Download/.nomedia"), b"downloads", "{mode:?}");
        assert_eq!(p.read("Download/.nomedia (1)"), b"camera", "{mode:?}");
        assert_eq!(summary.names, [".nomedia (1)"], "{mode:?}");
        assert_eq!(p.path("DCIM/Camera/.nomedia").exists(), mode == Paste::Copy);
    }
}

#[tokio::test]
async fn moving_a_file_takes_it_out_of_its_folder() {
    let p = phone().await;

    let summary = p
        .paste(Paste::Move, "/DCIM/Camera", &["b.jpg"], "/Music")
        .await
        .unwrap();

    assert!(!p.path("DCIM/Camera/b.jpg").exists());
    assert_eq!(p.read("Music/b.jpg"), b"B");
    assert_eq!((summary.done, summary.renamed), (1, 0));
    assert_eq!(p.shows("/Music").await, set(&["b.jpg"]));
    assert_eq!(p.shows("/DCIM/Camera").await, set(&["a.jpg"]));
}

#[tokio::test]
async fn moving_onto_a_taken_name_keeps_what_was_there_and_numbers_the_arrival() {
    let p = phone().await;

    let summary = p
        .paste(Paste::Move, "/DCIM/Camera", &["a.jpg"], "/Download")
        .await
        .unwrap();

    assert_eq!(p.read("Download/a.jpg"), b"other A", "never overwritten");
    assert_eq!(p.read("Download/a (1).jpg"), b"A");
    assert!(!p.path("DCIM/Camera/a.jpg").exists());
    assert!(
        !p.path("DCIM/Camera/a (1).jpg").exists(),
        "the rename moved with it"
    );
    assert_eq!(summary.renamed, 1);
    assert_eq!(summary.names, ["a (1).jpg"]);
}

#[tokio::test]
async fn moving_into_the_folder_it_is_already_in_does_nothing() {
    let p = phone().await;

    let summary = p
        .paste(Paste::Move, "/DCIM/Camera", &["a.jpg"], "/DCIM/Camera")
        .await
        .unwrap();

    assert_eq!(summary.done, 0);
    assert_eq!(p.read("DCIM/Camera/a.jpg"), b"A");
    assert_eq!(p.names("DCIM/Camera"), set(&["a.jpg", "b.jpg"]));
}

#[tokio::test]
async fn moving_a_folder_takes_everything_in_it_and_the_old_path_is_gone() {
    let p = phone().await;
    // The explorer has seen what is inside before.
    p.shows("/Trip/inner").await;

    let summary = p
        .paste(Paste::Move, "/", &["Trip"], "/Music")
        .await
        .unwrap();

    assert!(!p.path("Trip").exists());
    assert_eq!(p.read("Music/Trip/inner/deep.txt"), b"deep");
    assert_eq!(summary.done, 1);
    assert_eq!(p.shows("/Music/Trip/inner").await, set(&["deep.txt"]));
    assert!(matches!(
        p.service.list(p.storage, "/Trip/inner", true).await,
        Err(Error::PathNotFound(_))
    ));
}

#[tokio::test]
async fn a_folder_cannot_go_into_itself_or_below_itself_either_way() {
    let p = phone().await;

    for mode in [Paste::Copy, Paste::Move] {
        for to in ["/Trip", "/Trip/inner"] {
            let result = p.paste(mode, "/", &["Trip"], to).await;

            assert_eq!(
                result,
                Err(Error::IntoItself("Trip".into())),
                "{mode:?} into {to}"
            );
        }
    }
    assert_eq!(p.names("Trip"), set(&["inner"]), "nothing was changed");
    assert_eq!(p.names("Trip/inner"), set(&["deep.txt"]));
}

#[tokio::test]
async fn a_folder_whose_name_is_a_prefix_of_the_target_is_not_inside_it() {
    let p = phone().await;
    fs::create_dir_all(p.path("Trip2")).unwrap();
    mtp_rs::rescan_virtual_device(&p.serial);

    let summary = p
        .paste(Paste::Copy, "/", &["Trip"], "/Trip2")
        .await
        .unwrap();

    assert_eq!(summary.done, 1);
    assert_eq!(p.read("Trip2/Trip/inner/deep.txt"), b"deep");
}

#[tokio::test]
async fn a_name_that_is_not_there_is_an_error_and_nothing_is_done() {
    let p = phone().await;

    let result = p
        .paste(
            Paste::Copy,
            "/DCIM/Camera",
            &["b.jpg", "nope.jpg"],
            "/Music",
        )
        .await;

    assert!(matches!(result, Err(Error::PathNotFound(_))), "{result:?}");
    assert!(p.names("Music").is_empty());
}

#[tokio::test]
async fn pasting_nothing_does_nothing() {
    let p = phone().await;

    let summary = p
        .paste(Paste::Copy, "/DCIM/Camera", &[], "/Music")
        .await
        .unwrap();

    assert_eq!(summary.done, 0);
    assert!(p.names("Music").is_empty());
}

#[tokio::test]
async fn a_read_only_target_refuses_before_anything_is_touched() {
    let p = phone_with(|phone| phone.read_only = true).await;

    for mode in [Paste::Copy, Paste::Move] {
        let result = p.paste(mode, "/DCIM/Camera", &["b.jpg"], "/Music").await;

        assert_eq!(result, Err(Error::ReadOnly), "{mode:?}");
    }
    assert_eq!(p.read("DCIM/Camera/b.jpg"), b"B");
}

#[tokio::test]
async fn without_a_phone_connected_it_says_so() {
    let p = phone().await;
    p.device.disconnect().await.unwrap();

    let result = p
        .paste(Paste::Copy, "/DCIM/Camera", &["b.jpg"], "/Music")
        .await;

    assert_eq!(result, Err(Error::NotConnected));
}

#[tokio::test]
async fn a_phone_that_resets_part_way_is_a_lost_phone_not_a_list_of_failures() {
    let p = phone().await;
    p.shows("/DCIM/Camera").await;
    p.shows("/Music").await;
    mtp_rs::force_operation_wedge(&p.serial);

    let result = p
        .paste(Paste::Copy, "/DCIM/Camera", &["a.jpg", "b.jpg"], "/Music")
        .await;

    assert_eq!(result, Err(Error::Disconnected));
}

#[tokio::test]
async fn after_a_paste_both_folders_show_without_asking_the_phone_about_every_file() {
    let p = phone().await;
    p.shows("/DCIM/Camera").await;
    p.shows("/Music").await;
    let reads = p.service.full_reads();

    p.paste(Paste::Move, "/DCIM/Camera", &["a.jpg", "b.jpg"], "/Music")
        .await
        .unwrap();
    let mut listing = p
        .service
        .open_listing(p.storage, "/Music", false, Cancel::new(), true)
        .await
        .unwrap();
    let mut shown = BTreeSet::new();
    while let Some(update) = listing.next().await {
        shown.extend(update.unwrap().entries.into_iter().map(|e| e.name));
    }

    assert_eq!(shown, set(&["a.jpg", "b.jpg"]));
    assert_eq!(p.shows("/DCIM/Camera").await, set(&[]));
    assert_eq!(
        p.service.full_reads(),
        reads,
        "the remembered folders were kept true by the paste"
    );
}

#[tokio::test]
async fn a_second_paste_from_the_remembered_source_checks_only_the_items_asked_for() {
    let p = phone().await;
    p.shows("/DCIM/Camera").await;
    let reads = p.service.full_reads();

    p.paste(Paste::Copy, "/DCIM/Camera", &["a.jpg"], "/Music")
        .await
        .unwrap();
    p.paste(Paste::Copy, "/DCIM/Camera", &["b.jpg"], "/Music")
        .await
        .unwrap();

    // The source was remembered, and /Music was read once (it was not yet).
    assert_eq!(p.service.full_reads(), reads + 1);
}

#[tokio::test]
async fn pasting_into_and_out_of_a_big_folder_does_not_read_it_again() {
    // 5,000 photos: reading one back costs a USB round trip each, so a paste
    // must work from what the explorer already remembers of the folder.
    let p = phone().await;
    fs::create_dir_all(p.path("DCIM/Big")).unwrap();
    for i in 0..5_000 {
        fs::write(p.path(&format!("DCIM/Big/IMG_{i:05}.jpg")), b"x").unwrap();
    }
    mtp_rs::rescan_virtual_device(&p.serial);
    let listed = p.shows("/DCIM/Big").await;
    assert_eq!(listed.len(), 5_000);
    let reads = p.service.full_reads();

    let started = std::time::Instant::now();
    // Out of the big folder, into a small one, and a copy into itself.
    p.paste(Paste::Copy, "/DCIM/Big", &["IMG_00042.jpg"], "/Music")
        .await
        .unwrap();
    let summary = p
        .paste(Paste::Copy, "/DCIM/Big", &["IMG_00042.jpg"], "/DCIM/Big")
        .await
        .unwrap();
    p.paste(Paste::Move, "/Music", &["IMG_00042.jpg"], "/Download")
        .await
        .unwrap();
    let took = started.elapsed();

    assert_eq!(summary.names, ["IMG_00042 (1).jpg"]);
    assert_eq!(
        p.service.full_reads(),
        reads + 2,
        "only /Music and /Download were read, once each; the 5,000-file folder never again"
    );
    // Showing the big folder again, with its new copy, is not a re-read either.
    let mut listing = p
        .service
        .open_listing(p.storage, "/DCIM/Big", false, Cancel::new(), true)
        .await
        .unwrap();
    let mut shown = 0;
    while let Some(update) = listing.next().await {
        shown = update.unwrap().total;
    }
    assert_eq!(shown, 5_001);
    assert_eq!(p.service.full_reads(), reads + 2);
    assert_eq!(p.read("DCIM/Big/IMG_00042.jpg"), b"x");
    assert_eq!(p.read("DCIM/Big/IMG_00042 (1).jpg"), b"x");
    // The virtual phone answers "how many items are in this folder" by
    // reading the directory (about 0.3 s per question at 5,000 files in a
    // release build, 1 ms at 50; a real phone answers from its database), and
    // a paste asks it a few times. The bound only catches a paste that starts
    // costing a round trip per file, which would be minutes.
    assert!(took < std::time::Duration::from_secs(20), "{took:?}");
}

/// What the phone says it can do, less what a test takes away.
async fn capabilities(
    p: &Phone,
    change: impl FnOnce(&mut mtp_rs::mtp::Capabilities),
) -> mtp_rs::mtp::Capabilities {
    let mut capabilities = *p.device.session().await.unwrap().capabilities();
    change(&mut capabilities);
    capabilities
}

#[tokio::test]
async fn a_phone_that_cannot_go_round_a_clash_hands_over_a_copy_into_its_own_folder() {
    // It can copy, but not rename or move: a copy beside the original needs both.
    let p = phone().await;
    let caps = capabilities(&p, |c| {
        c.can_rename = false;
        c.can_move = false;
    })
    .await;

    let result = p
        .service
        .paste_with(
            &p.request(Paste::Copy, "/DCIM/Camera", &["a.jpg"], "/DCIM/Camera"),
            &caps,
        )
        .await;

    assert_eq!(result, Err(Error::Unsupported));
    assert_eq!(
        p.names("DCIM/Camera"),
        set(&["a.jpg", "b.jpg"]),
        "nothing was touched"
    );
    assert_eq!(p.read("DCIM/Camera/a.jpg"), b"A");
}

#[tokio::test]
async fn that_phone_still_copies_to_a_free_name_by_itself() {
    let p = phone().await;
    let caps = capabilities(&p, |c| {
        c.can_rename = false;
        c.can_move = false;
    })
    .await;

    let summary = p
        .service
        .paste_with(
            &p.request(Paste::Copy, "/DCIM/Camera", &["b.jpg"], "/Music"),
            &caps,
        )
        .await
        .unwrap();

    assert_eq!(summary.done, 1);
    assert_eq!(p.read("Music/b.jpg"), b"B");
}

#[tokio::test]
async fn a_phone_that_cannot_rename_hands_over_a_move_onto_a_taken_name_but_not_a_free_one() {
    let p = phone().await;
    let caps = capabilities(&p, |c| c.can_rename = false).await;

    let clash = p
        .service
        .paste_with(
            &p.request(Paste::Move, "/DCIM/Camera", &["a.jpg"], "/Download"),
            &caps,
        )
        .await;
    let free = p
        .service
        .paste_with(
            &p.request(Paste::Move, "/DCIM/Camera", &["b.jpg"], "/Download"),
            &caps,
        )
        .await
        .unwrap();

    assert_eq!(clash, Err(Error::Unsupported), "Download has an a.jpg");
    assert_eq!(p.read("DCIM/Camera/a.jpg"), b"A", "left where it was");
    assert_eq!(
        p.read("Download/a.jpg"),
        b"other A",
        "and the other untouched"
    );
    assert_eq!(free.done, 1);
    assert_eq!(p.read("Download/b.jpg"), b"B");
}

#[tokio::test]
async fn a_phone_that_cannot_make_folders_hands_over_a_clashing_copy_but_a_move_goes_ahead() {
    let p = phone().await;
    let caps = capabilities(&p, |c| c.can_create_folder = false).await;

    let copy = p
        .service
        .paste_with(
            &p.request(Paste::Copy, "/DCIM/Camera", &["a.jpg"], "/Download"),
            &caps,
        )
        .await;
    let mv = p
        .service
        .paste_with(
            &p.request(Paste::Move, "/DCIM/Camera", &["a.jpg"], "/Download"),
            &caps,
        )
        .await
        .unwrap();

    assert_eq!(copy, Err(Error::Unsupported));
    assert_eq!(mv.names, ["a (1).jpg"], "a move only needs to rename");
    assert_eq!(p.read("Download/a (1).jpg"), b"A");
    assert_eq!(p.read("Download/a.jpg"), b"other A");
}
