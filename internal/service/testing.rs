//! Test helpers shared by the service tests.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use super::device::{Source, VirtualPhone};

/// A virtual device over `dir` with a serial no other test is using, so
/// mtp-rs's serial-keyed test hooks reach the right device.
pub fn virtual_source(dir: &Path) -> Source {
    Source::Virtual(virtual_phone(dir))
}

/// The phone behind [`virtual_source`], for tests that change its limits.
pub fn virtual_phone(dir: &Path) -> VirtualPhone {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    VirtualPhone::new(
        dir.to_path_buf(),
        format!("test-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
    )
}

/// A phone that has gone silent. A transfer that never gets its data holds the
/// phone's one session, and every request behind it waits for ever: the same
/// thing a request that never comes back does to the ones after it. Dropping
/// this lets go.
pub struct Silence(tokio::task::JoinHandle<()>);

impl Drop for Silence {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Makes `device`'s phone go silent, once it has really gone quiet: when this
/// returns, a request to it does not come back.
pub async fn silence(device: &super::device::Service, storage_id: u64) -> Silence {
    use std::time::Duration;

    use futures::StreamExt;

    let session = device.session().await.expect("a connected phone");
    let hold = session
        .storage(mtp_rs::StorageId(storage_id))
        .await
        .expect("the storage");
    let probe = session
        .storage(mtp_rs::StorageId(storage_id))
        .await
        .expect("the storage");
    let held = tokio::spawn(async move {
        let never = futures::stream::pending::<Result<bytes::Bytes, std::io::Error>>().boxed();
        let _ = hold
            .upload_with_progress(
                None,
                mtp_rs::mtp::NewObjectInfo::file("never-finishes.bin", 100),
                never,
                |_| std::ops::ControlFlow::Continue(()),
            )
            .await;
    });
    let silence = Silence(held);
    // Until a request is seen not to come back, the transfer has not got hold yet.
    for _ in 0..200 {
        let asked =
            tokio::time::timeout(Duration::from_millis(25), probe.list_objects_stream(None)).await;
        if asked.is_err() {
            return silence;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the phone would not go silent");
}

/// A phone busy with a transfer that is making progress: a big file is on its
/// way, a little at a time, and every other request has to wait its turn. The
/// phone is not silent, only busy; `chunks` of data go over, one every `gap`.
/// Dropping this stops the transfer.
pub async fn busy(
    device: &super::device::Service,
    storage_id: u64,
    chunks: usize,
    gap: std::time::Duration,
) -> Silence {
    use futures::StreamExt;

    let session = device.session().await.expect("a connected phone");
    let hold = session
        .storage(mtp_rs::StorageId(storage_id))
        .await
        .expect("the storage");
    let size = (chunks * 4) as u64;
    // What a transfer does with each chunk it sends: tell the session the phone
    // is alive.
    let activity = device.activity();
    let held = tokio::spawn(async move {
        let trickle = futures::stream::unfold(0, move |sent| async move {
            if sent == chunks {
                return None;
            }
            tokio::time::sleep(gap).await;
            Some((
                Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data")),
                sent + 1,
            ))
        })
        .boxed();
        let _ = hold
            .upload_with_progress(
                None,
                mtp_rs::mtp::NewObjectInfo::file("big-video.bin", size),
                trickle,
                move |_| {
                    activity.heard();
                    std::ops::ControlFlow::Continue(())
                },
            )
            .await;
    });
    // Give it a moment to take hold of the session.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    Silence(held)
}
