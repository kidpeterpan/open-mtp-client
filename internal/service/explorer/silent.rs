//! A phone that goes silent in the middle of an edit. Pixels do, `mtp-rs`
//! documents it, and a request to one never comes back: without a limit the
//! pane waits for ever with its dialog up. Each operation here must instead end
//! as `Timeout` soon after the limit, drop the wedged session (so the device
//! panel offers Reconnect), and work again once the phone is back.
//!
//! The virtual phone can't be made to hang, so a transfer that never gets its
//! data holds its one session (`testing::silence`), and everything behind it
//! waits, as it would behind a request that never comes back.

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::bounded::read_all;
use super::{Limits, Service, read_children};
use crate::dto::{Cancel, Error, Paste, PasteRequest};
use crate::service::device;
use crate::service::testing::{busy, silence, virtual_phone};

/// Long enough to be a limit and short enough to wait for; the real ones are a
/// minute and ten.
const ASK: Duration = Duration::from_millis(150);
const LONG: Duration = Duration::from_millis(450);
/// An operation may take a few requests' worth of limit, never more.
const AT_MOST: Duration = Duration::from_secs(4);

struct Quiet {
    _phone: tempfile::TempDir,
    device: Arc<device::Service>,
    service: Arc<Service>,
    storage: u64,
}

/// DCIM/{a.jpg, b.jpg} and Music/ on a phone whose answers get `ASK` and `LONG`.
async fn quiet() -> Quiet {
    let phone = tempfile::tempdir().unwrap();
    fs::create_dir_all(phone.path().join("DCIM")).unwrap();
    fs::create_dir_all(phone.path().join("Music")).unwrap();
    fs::write(phone.path().join("DCIM/a.jpg"), b"A").unwrap();
    fs::write(phone.path().join("DCIM/b.jpg"), b"B").unwrap();
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(phone.path()),
    )));
    device.connect().await.unwrap();
    let storage = device.storages().await.unwrap()[0].id;
    let service = Arc::new(Service::new(Arc::clone(&device)).with_limits(ASK, LONG));
    Quiet {
        _phone: phone,
        device,
        service,
        storage,
    }
}

impl Quiet {
    fn paste(&self, mode: Paste) -> PasteRequest {
        PasteRequest {
            mode,
            from_storage: self.storage,
            from: "/DCIM".into(),
            names: vec!["a.jpg".into()],
            to_storage: self.storage,
            to: "/Music".into(),
        }
    }

    /// The wedged session was dropped, and what is remembered of it distrusted.
    async fn gave_up_on_the_phone(&self, epoch: u64) {
        assert_eq!(
            self.device.session().await.err(),
            Some(Error::NotConnected),
            "the session was dropped, so Reconnect is offered"
        );
        assert!(self.device.epoch() > epoch, "and its memory is not trusted");
    }
}

/// Runs `operation` against a silent phone and checks it ended, and how fast.
async fn ends_as_a_timeout<T: std::fmt::Debug>(
    q: &Quiet,
    operation: impl std::future::Future<Output = Result<T, Error>>,
) {
    let epoch = q.device.epoch();
    let _silence = silence(&q.device, q.storage).await;
    let started = Instant::now();

    let result = tokio::time::timeout(AT_MOST * 3, operation)
        .await
        .expect("an operation on a silent phone must end, not wait for ever");

    assert_eq!(result.as_ref().err(), Some(&Error::Timeout), "{result:?}");
    let took = started.elapsed();
    assert!(took >= ASK && took < AT_MOST, "{took:?}");
    q.gave_up_on_the_phone(epoch).await;
}

#[tokio::test]
async fn a_search_walk_on_a_phone_gone_silent_ends_and_stops() {
    let q = quiet().await;
    let mut walk = q
        .service
        .walk(q.storage, false, Cancel::new())
        .await
        .unwrap();

    ends_as_a_timeout(&q, async {
        match walk.next().await {
            Some(result) => result.map(|_| ()),
            None => Ok(()),
        }
    })
    .await;

    assert!(walk.next().await.is_none(), "a failed walk is over");
}

#[tokio::test]
async fn listing_a_folder_on_a_silent_phone_ends() {
    let q = quiet().await;

    ends_as_a_timeout(&q, q.service.list(q.storage, "/DCIM", true)).await;
}

#[tokio::test]
async fn opening_a_streamed_listing_on_a_silent_phone_ends() {
    let q = quiet().await;

    ends_as_a_timeout(&q, async {
        q.service
            .open_listing(q.storage, "/DCIM", true, Cancel::new(), false)
            .await
            .map(|_| ())
    })
    .await;
}

#[tokio::test]
async fn a_listing_that_goes_quiet_part_way_ends_instead_of_waiting_for_the_next_file() {
    let q = quiet().await;
    let mut listing = q
        .service
        .open_listing(q.storage, "/DCIM", true, Cancel::new(), false)
        .await
        .unwrap();
    let epoch = q.device.epoch();
    let _silence = silence(&q.device, q.storage).await;
    let started = Instant::now();

    let next = tokio::time::timeout(AT_MOST * 3, listing.next())
        .await
        .expect("a listing on a silent phone must end, not wait for ever");

    assert!(matches!(next, Some(Err(Error::Timeout))), "{next:?}");
    assert!(started.elapsed() < AT_MOST);
    q.gave_up_on_the_phone(epoch).await;
    assert!(listing.next().await.is_none(), "and it stays over");
}

#[tokio::test]
async fn a_new_folder_on_a_silent_phone_ends() {
    let q = quiet().await;

    ends_as_a_timeout(&q, q.service.make_folder(q.storage, "/", "Trip")).await;
}

#[tokio::test]
async fn a_rename_on_a_silent_phone_ends() {
    let q = quiet().await;

    ends_as_a_timeout(&q, q.service.rename(q.storage, "/DCIM", "a.jpg", "z.jpg")).await;
}

#[tokio::test]
async fn deleting_on_a_silent_phone_ends_for_a_file_and_for_a_folder() {
    let q = quiet().await;
    ends_as_a_timeout(&q, q.service.delete(q.storage, "/DCIM", &["a.jpg".into()])).await;

    let q = quiet().await;
    ends_as_a_timeout(&q, q.service.delete(q.storage, "/", &["DCIM".into()])).await;
}

#[tokio::test]
async fn a_copy_or_a_move_on_a_silent_phone_ends() {
    let q = quiet().await;
    ends_as_a_timeout(&q, q.service.paste(&q.paste(Paste::Copy))).await;

    let q = quiet().await;
    ends_as_a_timeout(&q, q.service.paste(&q.paste(Paste::Move))).await;
}

#[tokio::test]
async fn checking_a_paste_on_a_silent_phone_ends() {
    let q = quiet().await;

    ends_as_a_timeout(&q, q.service.check_paste(&q.paste(Paste::Copy))).await;
}

#[tokio::test]
async fn once_the_phone_answers_again_a_reconnect_brings_everything_back() {
    let q = quiet().await;
    {
        let _silence = silence(&q.device, q.storage).await;
        assert_eq!(
            q.service.list(q.storage, "/DCIM", true).await.err(),
            Some(Error::Timeout)
        );
    }

    q.device.connect().await.unwrap();
    let names: std::collections::BTreeSet<_> = q
        .service
        .list(q.storage, "/DCIM", true)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();

    assert_eq!(names.len(), 2);
    assert!(names.contains("a.jpg") && names.contains("b.jpg"));
}

#[tokio::test]
async fn a_phone_that_is_only_slow_is_not_given_up_on() {
    // Every request here takes a while to come back (the transfer ahead lets
    // go after a moment), but inside the limit: nothing times out.
    let q = quiet().await;
    let silence = silence(&q.device, q.storage).await;
    let letting_go = tokio::spawn(async move {
        tokio::time::sleep(ASK / 3).await;
        drop(silence);
    });

    let listed = q.service.list(q.storage, "/DCIM", true).await;
    letting_go.await.unwrap();

    assert_eq!(listed.unwrap().len(), 2);
    assert!(q.device.session().await.is_ok(), "the session is kept");
}

#[tokio::test]
async fn reading_a_folder_item_by_item_ends_if_the_phone_stops_describing_its_files() {
    let q = quiet().await;
    let session = q.device.session().await.unwrap();
    let storage = session.storage(mtp_rs::StorageId(q.storage)).await.unwrap();
    // The phone has said how many items there are; then it goes quiet.
    let listing = storage.list_objects_stream(None).await.unwrap();
    assert!(listing.total() > 0, "something to describe");
    let _silence = silence(&q.device, q.storage).await;
    let started = Instant::now();

    let read = tokio::time::timeout(AT_MOST * 3, read_all(listing, &Limits::new(ASK, LONG)))
        .await
        .expect("a folder read from a silent phone must end");

    assert!(matches!(read, Err(mtp_rs::Error::Timeout)), "{read:?}");
    assert!(started.elapsed() >= ASK && started.elapsed() < AT_MOST);
}

#[tokio::test]
async fn asking_for_a_folders_files_of_a_silent_phone_ends_at_the_first_request() {
    let q = quiet().await;
    let session = q.device.session().await.unwrap();
    let storage = session.storage(mtp_rs::StorageId(q.storage)).await.unwrap();
    let _silence = silence(&q.device, q.storage).await;

    let read = tokio::time::timeout(
        AT_MOST * 3,
        read_children(&storage, None, &Limits::new(ASK, LONG)),
    )
    .await
    .expect("a folder asked of a silent phone must end");

    assert!(matches!(read, Err(mtp_rs::Error::Timeout)), "{read:?}");
}

#[tokio::test]
async fn a_folder_of_a_working_phone_is_read_whatever_its_size() {
    // The limit is for each request, not for the folder: 600 files take far
    // longer than one request's limit, and nothing times out.
    let q = quiet().await;
    for i in 0..600 {
        fs::write(q._phone.path().join(format!("Music/{i:04}.mp3")), b"x").unwrap();
    }
    mtp_rs::rescan_virtual_device(&q.device.connect().await.unwrap().serial_number);
    let session = q.device.session().await.unwrap();
    let storage = session.storage(mtp_rs::StorageId(q.storage)).await.unwrap();
    let music = read_children(&storage, None, &Limits::new(ASK, LONG))
        .await
        .unwrap()
        .into_iter()
        .find(|object| object.filename == "Music")
        .unwrap();

    let started = Instant::now();
    let files = read_children(
        &storage,
        Some(music.handle),
        &Limits::new(Duration::from_millis(1500), LONG),
    )
    .await
    .unwrap();

    assert_eq!(files.len(), 600);
    // (A whole-folder limit this small would have cut it short.)
    eprintln!("600 files read in {:?}", started.elapsed());
}

#[tokio::test]
async fn the_explorers_limits_count_on_the_devices_own_clock() {
    // The tie is what makes a request queued behind a transfer wait for it.
    let phone = tempfile::tempdir().unwrap();
    let device = Arc::new(device::Service::new(device::Source::Virtual(
        virtual_phone(phone.path()),
    )));

    let service = Service::new(Arc::clone(&device));

    assert!(service.limits.counts_on(&device.activity()));
    assert!(!service.limits.counts_on(
        &device::Service::new(device::Source::Virtual(virtual_phone(phone.path()))).activity()
    ));
}

#[tokio::test]
async fn a_phone_busy_with_a_big_transfer_is_waited_for_not_given_up_on() {
    // A 3 GB video is going to the phone, a little at a time, for several times
    // the limit; meanwhile the user browses. The listing queues behind the
    // upload, which holds the phone's one session, and must get its answer when
    // the upload is done, not a "your phone stopped responding" after a minute.
    let q = quiet().await;
    let _upload = busy(&q.device, q.storage, 30, ASK / 6).await;
    let started = Instant::now();

    let listed = q.service.list(q.storage, "/DCIM", true).await;

    assert!(
        started.elapsed() > ASK * 3,
        "the upload really did hold it up for several limits: {:?}",
        started.elapsed()
    );
    assert_eq!(listed.map(|entries| entries.len()), Ok(2));
    assert!(q.device.session().await.is_ok(), "the session is kept");
}
