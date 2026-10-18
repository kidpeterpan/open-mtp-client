//! What the phone says about its free space as files come and go. Deleting
//! must free space, so the numbers are checked from what the storage itself
//! reports after each change.

use std::fs;
use std::sync::Arc;

use super::Service;
use crate::dto::{Cancel, Conflict, UploadRequest};
use crate::service::device;
use crate::service::explorer;
use crate::service::testing::virtual_phone;

const MIB: u64 = 1024 * 1024;

struct Phone {
    dir: tempfile::TempDir,
    mac: tempfile::TempDir,
    device: Arc<device::Service>,
    explorer: Arc<explorer::Service>,
    transfer: Service,
    storage: u64,
}

async fn phone() -> Phone {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("movie.mp4"), vec![7u8; (3 * MIB) as usize]).unwrap();
    fs::create_dir(dir.path().join("Music")).unwrap();
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(dir.path()),
    )));
    device.connect().await.unwrap();
    let storage = device.storages().await.unwrap()[0].id;
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    Phone {
        dir,
        mac: tempfile::tempdir().unwrap(),
        transfer: Service::new(Arc::clone(&device), Arc::clone(&explorer)),
        device,
        explorer,
        storage,
    }
}

impl Phone {
    /// Free bytes as the storage reports them now.
    async fn free(&self) -> u64 {
        self.device.storages().await.unwrap()[0].free_bytes
    }
}

#[tokio::test]
async fn deleting_a_file_frees_exactly_its_size() {
    let p = phone().await;
    let before = p.free().await;

    p.explorer
        .delete(p.storage, "/", &["movie.mp4".into()])
        .await
        .unwrap();

    assert!(!p.dir.path().join("movie.mp4").exists());
    assert_eq!(p.free().await, before + 3 * MIB);
}

#[tokio::test]
async fn uploading_takes_space_and_deleting_the_copy_gives_it_back() {
    let p = phone().await;
    let from_mac = p.mac.path().join("holiday.mov");
    fs::write(&from_mac, vec![1u8; (2 * MIB) as usize]).unwrap();
    let before = p.free().await;
    let request = UploadRequest {
        sources: vec![from_mac.to_string_lossy().into_owned()],
        storage_id: p.storage,
        destination: "/".into(),
    };

    p.transfer
        .upload(&request, Conflict::Skip, &Cancel::new(), |_| {})
        .await
        .unwrap();
    let after_upload = p.free().await;
    p.explorer
        .delete(p.storage, "/", &["holiday.mov".into()])
        .await
        .unwrap();

    assert_eq!(after_upload, before - 2 * MIB);
    assert_eq!(p.free().await, before);
}

#[tokio::test]
async fn deleting_a_folder_frees_everything_inside_it() {
    let p = phone().await;
    fs::write(p.dir.path().join("Music/a.mp3"), vec![2u8; MIB as usize]).unwrap();
    fs::write(p.dir.path().join("Music/b.mp3"), vec![3u8; MIB as usize]).unwrap();
    mtp_rs::rescan_virtual_device(&p.device.connect().await.unwrap().serial_number);
    let before = p.free().await;

    p.explorer
        .delete(p.storage, "/", &["Music".into()])
        .await
        .unwrap();

    assert_eq!(p.free().await, before + 2 * MIB);
}

#[tokio::test]
async fn making_a_folder_takes_no_space_worth_counting() {
    let p = phone().await;
    let before = p.free().await;

    p.explorer
        .make_folder(p.storage, "/", "Trip")
        .await
        .unwrap();

    assert_eq!(p.free().await, before);
}

#[tokio::test]
async fn the_storage_reports_what_is_used_and_free_adding_up_to_its_capacity() {
    let p = phone().await;

    let storage = p.device.storages().await.unwrap().remove(0);

    assert_eq!(
        storage.used_bytes() + storage.free_bytes,
        storage.capacity_bytes
    );
    assert!(
        storage.used_bytes() >= 3 * MIB,
        "the movie is counted as used"
    );
}
