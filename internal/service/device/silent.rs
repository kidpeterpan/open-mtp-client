//! The device's own waits on a phone that goes silent: opening it, its
//! storage list, and the helpers every other bounded request goes through.
//! A silent phone ends as `Timeout`, its session dropped so Reconnect is
//! offered; a busy one (a transfer making progress) is waited for.
//!
//! The virtual phone can't be made to hang, so a transfer that never gets its
//! data holds its one session (`testing::silence`) and everything behind it
//! waits; opening a phone can't be held that way, so it is tested on the
//! helper it goes through.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::Service;
use crate::dto::Error;
use crate::service::testing::{busy, silence, virtual_phone};

/// Long enough to be a limit and short enough to wait for; the real one is a
/// minute.
const LIMIT: Duration = Duration::from_millis(150);
const AT_MOST: Duration = Duration::from_secs(4);

async fn connected() -> (tempfile::TempDir, Arc<Service>, u64) {
    let phone = tempfile::tempdir().unwrap();
    std::fs::write(phone.path().join("a.jpg"), b"A").unwrap();
    let device = Arc::new(
        Service::new(super::Source::Virtual(virtual_phone(phone.path()))).answering_within(LIMIT),
    );
    device.connect().await.unwrap();
    let storage = device.storages().await.unwrap()[0].id;
    (phone, device, storage)
}

#[tokio::test]
async fn the_storage_list_of_a_silent_phone_ends_as_a_timeout_and_drops_the_session() {
    let (_phone, device, storage) = connected().await;
    let epoch = device.epoch();
    let _silence = silence(&device, storage).await;
    let started = Instant::now();

    let result = device.storages().await;

    assert_eq!(result.err(), Some(Error::Timeout));
    assert!(started.elapsed() >= LIMIT && started.elapsed() < AT_MOST);
    assert_eq!(device.session().await.err(), Some(Error::NotConnected));
    assert!(device.epoch() > epoch);
}

#[tokio::test]
async fn the_storage_list_behind_a_busy_transfer_is_waited_for() {
    let (_phone, device, storage) = connected().await;
    // Ten chunks, a limit's worth apart in total several times over, but
    // never quiet for a whole limit.
    let _busy = busy(&device, storage, 10, LIMIT / 3).await;

    let result = device.storages().await;

    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn opening_a_phone_that_never_answers_ends_as_a_timeout() {
    let device = Service::new(super::Source::Phones(Vec::new())).answering_within(LIMIT);
    let started = Instant::now();

    let result: Result<(), Error> = device.opening(std::future::pending()).await;

    assert_eq!(result, Err(Error::Timeout));
    assert!(started.elapsed() >= LIMIT && started.elapsed() < AT_MOST);
    let opened: Result<u8, Error> = device.opening(async { Ok(7) }).await;
    assert_eq!(opened, Ok(7), "a phone that answers opens");
}

#[tokio::test]
async fn ask_keeps_answers_and_errors_and_turns_silence_into_a_timeout() {
    let (_phone, device, _) = connected().await;
    let epoch = device.epoch();

    let answered = device.ask(epoch, async { Ok::<_, mtp_rs::Error>(7) }).await;
    let silent = device
        .ask(epoch, std::future::pending::<Result<u8, mtp_rs::Error>>())
        .await;

    assert_eq!(answered, Ok(7));
    assert_eq!(silent, Err(Error::Timeout));
    assert_eq!(
        device.session().await.err(),
        Some(Error::NotConnected),
        "a silent phone's session is dropped"
    );
}
