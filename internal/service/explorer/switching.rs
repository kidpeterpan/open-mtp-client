//! Switching phones while a request to the first is still out. The first
//! phone's session is closed by the switch, so whatever was reading from it
//! fails late; that failure must not close the second phone's session.

use std::fs;
use std::sync::Arc;

use super::Service;
use crate::dto::Cancel;
use crate::service::device::{self, Source};
use crate::service::testing::virtual_phone;

struct Two {
    _dirs: (tempfile::TempDir, tempfile::TempDir),
    device: Arc<device::Service>,
    service: Arc<Service>,
    a: String,
    b: String,
}

/// Phone "a" has DCIM with 1,200 photos; phone "b" has Music/song.mp3.
async fn two_phones() -> Two {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    fs::create_dir_all(one.path().join("DCIM")).unwrap();
    for i in 0..1_200 {
        fs::write(one.path().join(format!("DCIM/IMG_{i:04}.jpg")), b"x").unwrap();
    }
    fs::create_dir_all(two.path().join("Music")).unwrap();
    fs::write(two.path().join("Music/song.mp3"), b"s").unwrap();
    let (a, b) = (virtual_phone(one.path()), virtual_phone(two.path()));
    let (a_key, b_key) = (a.serial.clone(), b.serial.clone());
    let device = Arc::new(device::Service::new(Source::Phones(vec![a, b])));
    let service = Arc::new(Service::new(Arc::clone(&device)));
    Two {
        _dirs: (one, two),
        device,
        service,
        a: a_key,
        b: b_key,
    }
}

async fn storage(device: &device::Service) -> u64 {
    device.storages().await.unwrap()[0].id
}

async fn open_serial(device: &device::Service) -> String {
    device
        .session()
        .await
        .unwrap()
        .device_info()
        .serial_number
        .clone()
}

#[tokio::test]
async fn a_listing_of_the_first_phone_failing_after_a_switch_leaves_the_second_open() {
    let two = two_phones().await;
    two.device.connect_to(Some(&two.a)).await.unwrap();
    let on_a = storage(&two.device).await;
    let mut listing = two
        .service
        .open_listing(on_a, "/DCIM", false, Cancel::new(), false)
        .await
        .unwrap();
    let first = listing.next().await.unwrap().unwrap();
    assert!(!first.done, "the listing is still going");

    two.device.connect_to(Some(&two.b)).await.unwrap();
    let on_b = two.device.epoch();
    let mut late = None;
    while let Some(piece) = listing.next().await {
        if let Err(err) = piece {
            late = Some(err);
        }
    }

    assert!(late.is_some(), "the first phone's session is gone");
    assert_eq!(open_serial(&two.device).await, two.b);
    assert_eq!(
        two.device.epoch(),
        on_b,
        "the second phone's session stands"
    );
    let on_b_storage = storage(&two.device).await;
    let music = two
        .service
        .list(on_b_storage, "/Music", false)
        .await
        .unwrap();
    assert_eq!(music[0].name, "song.mp3");
}

#[tokio::test]
async fn what_was_remembered_of_one_phone_is_not_shown_for_the_other() {
    let two = two_phones().await;
    two.device.connect_to(Some(&two.a)).await.unwrap();
    let on_a = storage(&two.device).await;
    two.service.list(on_a, "/", false).await.unwrap();

    two.device.connect_to(Some(&two.b)).await.unwrap();
    let on_b = storage(&two.device).await;
    let root = two.service.list(on_b, "/", false).await.unwrap();

    let names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Music"]);
}

#[tokio::test]
async fn a_search_of_the_first_phone_failing_after_a_switch_leaves_the_second_open() {
    let two = two_phones().await;
    two.device.connect_to(Some(&two.a)).await.unwrap();
    let on_a = storage(&two.device).await;
    let mut walk = two.service.walk(on_a, false, Cancel::new()).await.unwrap();

    two.device.connect_to(Some(&two.b)).await.unwrap();
    let on_b = two.device.epoch();
    let mut failed = false;
    while let Some(piece) = walk.next().await {
        failed |= piece.is_err();
    }

    assert!(failed);
    assert_eq!(open_serial(&two.device).await, two.b);
    assert_eq!(two.device.epoch(), on_b);
    assert_eq!(two.device.connect().await.map(|d| d.key), Ok(two.b.clone()));
}

#[tokio::test]
async fn the_first_phones_event_watch_ends_after_a_switch_and_leaves_the_second_open() {
    let two = two_phones().await;
    two.device.connect_to(Some(&two.a)).await.unwrap();
    // The device panel always has this waiting while a phone is open.
    let watching = tokio::spawn({
        let device = Arc::clone(&two.device);
        async move { device.next_change().await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    two.device.connect_to(Some(&two.b)).await.unwrap();
    let on_b = two.device.epoch();
    let ended = tokio::time::timeout(std::time::Duration::from_secs(5), watching)
        .await
        .expect("the old phone's watch ends rather than waiting on a closed session")
        .unwrap();

    assert!(ended.is_err(), "{ended:?}");
    assert_eq!(open_serial(&two.device).await, two.b);
    assert_eq!(two.device.epoch(), on_b);
}
