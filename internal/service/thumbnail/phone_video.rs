//! The first frame of a video on a phone, without copying the video.
//!
//! A phone's camera writes the index of a video (`moov`) after its picture
//! data, and a decoder needs only the index and the start of the data. This
//! reads the box headers to learn where those are, reads just those ranges
//! (1 MiB requests, so a lost phone is noticed between them), puts them at
//! their own offsets in a sparse temporary file (`sparse.rs`) and has the Mac's
//! decoder make the frame (`video.rs`). A first picture that does not fit in
//! the start read is tried again with more of it, up to `mp4::HEAD_TIERS`.
//!
//! Everything the phone says is checked: sizes against the file's length,
//! the walk and the reads against fixed bounds. Whatever is wrong with one
//! video is `Ok(None)` (no thumbnail); only a phone that is gone is an error.

use std::future::Future;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

use super::mp4::{HEAD_TIERS, HEADER_PEEK, Layout, Walker, missing, read_plan};
use super::sparse::{Piece, write_sparse};
use super::video;
use crate::dto::{Error, Thumbnail};

/// A video claiming to be bigger than this is not tried.
pub(super) const MAX_VIDEO_BYTES: u64 = 1 << 40;
/// One request to the phone: small enough to notice a lost phone between two.
const CHUNK: u64 = 1 << 20;
/// How long the Mac's decoder gets for one frame.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// Videos made at once. The phone has one session and a video takes a dozen
/// requests; two at once would only take turns.
const VIDEOS_AT_ONCE: usize = 1;
static VIDEOS: Semaphore = Semaphore::const_new(VIDEOS_AT_ONCE);

/// How many videos are in the middle of being made, and the most at once.
#[cfg(test)]
static NOW: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(super) static HIGH_WATER: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// A video of `size` bytes, as the phone claims, is worth a try.
pub(super) fn is_worth_trying(size: u64) -> bool {
    (1..=MAX_VIDEO_BYTES).contains(&size)
}

/// The first frame of a video of `size` bytes. `read(offset, len)` asks the
/// phone for a range: `Ok(None)` when it refuses this file, `Err` when the
/// phone is gone.
pub(super) async fn first_frame_of<R, Fut>(size: u64, read: R) -> Result<Option<Thumbnail>, Error>
where
    R: Fn(u64, u32) -> Fut,
    Fut: Future<Output = Result<Option<Vec<u8>>, Error>>,
{
    first_frame_in(&std::env::temp_dir(), size, read).await
}

/// `first_frame_of`, with its one temporary folder made inside `parent`.
async fn first_frame_in<R, Fut>(
    parent: &std::path::Path,
    size: u64,
    read: R,
) -> Result<Option<Thumbnail>, Error>
where
    R: Fn(u64, u32) -> Fut,
    Fut: Future<Output = Result<Option<Vec<u8>>, Error>>,
{
    if !is_worth_trying(size) {
        return Ok(None);
    }
    let Ok(_turn) = VIDEOS.acquire().await else {
        return Ok(None);
    };
    #[cfg(test)]
    let _counted = Counted::enter();
    // Removed when this ends, whichever way.
    let Ok(folder) = tempfile::Builder::new()
        .prefix("open-mtp-client-vt-")
        .tempdir_in(parent)
    else {
        return Ok(None);
    };
    let Some(layout) = walk(size, &read).await? else {
        return Ok(None);
    };

    let mut have: Vec<Range<u64>> = Vec::new();
    let mut pieces: Arc<Vec<(u64, Vec<u8>)>> = Arc::new(Vec::new());
    for tier in 0..HEAD_TIERS.len() {
        let Some(plan) = read_plan(&layout, size, tier) else {
            break;
        };
        let wanted = missing(&have, &plan);
        if wanted.is_empty() && tier > 0 {
            break; // the bigger start is no bigger: the file ended
        }
        for range in wanted {
            let Some(bytes) = read_exact(&read, range.start, range.end - range.start).await? else {
                return Ok(None);
            };
            Arc::make_mut(&mut pieces).push((range.start, bytes));
            have.push(range);
        }
        let path = folder.path().join(format!("tier-{tier}.mp4"));
        if let Some(thumbnail) = frame_from(path, size, Arc::clone(&pieces)).await {
            return Ok(Some(thumbnail));
        }
    }
    Ok(None)
}

/// Reads the top-level box headers until `moov` and `mdat` are both found.
async fn walk<R, Fut>(size: u64, read: &R) -> Result<Option<Layout>, Error>
where
    R: Fn(u64, u32) -> Fut,
    Fut: Future<Output = Result<Option<Vec<u8>>, Error>>,
{
    let mut walker = Walker::new(size);
    loop {
        let at = walker.next_offset();
        let want = u64::from(HEADER_PEEK).min(size - at);
        let Some(header) = read_exact(read, at, want).await? else {
            return Ok(None);
        };
        match walker.feed(&header) {
            Ok(Some(layout)) => return Ok(Some(layout)),
            Ok(None) => {}
            Err(_) => return Ok(None),
        }
    }
}

/// Exactly `len` bytes at `offset`, in requests of `CHUNK`. `Ok(None)` when
/// the phone refuses or has fewer than it claimed.
async fn read_exact<R, Fut>(read: &R, offset: u64, len: u64) -> Result<Option<Vec<u8>>, Error>
where
    R: Fn(u64, u32) -> Fut,
    Fut: Future<Output = Result<Option<Vec<u8>>, Error>>,
{
    let mut out = Vec::new();
    while (out.len() as u64) < len {
        let want = CHUNK.min(len - out.len() as u64);
        let Some(bytes) = read(offset + out.len() as u64, want as u32).await? else {
            return Ok(None);
        };
        if bytes.len() as u64 != want {
            return Ok(None);
        }
        out.extend_from_slice(&bytes);
    }
    Ok(Some(out))
}

/// The frame of the file made of `pieces` at `path`, decoded off the async threads.
async fn frame_from(
    path: std::path::PathBuf,
    size: u64,
    pieces: Arc<Vec<(u64, Vec<u8>)>>,
) -> Option<Thumbnail> {
    tokio::task::spawn_blocking(move || {
        let parts: Vec<Piece<'_>> = pieces
            .iter()
            .map(|(offset, bytes)| Piece {
                offset: *offset,
                bytes,
            })
            .collect();
        write_sparse(&path, size, &parts).ok()?;
        video::first_frame(&path, FRAME_TIMEOUT)
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
struct Counted;

#[cfg(test)]
impl Counted {
    fn enter() -> Self {
        use std::sync::atomic::Ordering;
        let now = NOW.fetch_add(1, Ordering::SeqCst) + 1;
        HIGH_WATER.fetch_max(now, Ordering::SeqCst);
        Self
    }
}

#[cfg(test)]
impl Drop for Counted {
    fn drop(&mut self) {
        NOW.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::thumbnail::fixtures::{bytes, padded_at_end, padded_at_start};
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    /// A phone holding `bytes`: it answers what it has, and fewer bytes at the end.
    /// Its temporary folders are made in `scratch`, so a test can count them alone.
    struct Phone {
        scratch: tempfile::TempDir,
        bytes: Vec<u8>,
        asked: Mutex<Vec<(u64, u32)>>,
        fail_after: Option<usize>,
    }

    impl Phone {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                scratch: tempfile::tempdir().unwrap(),
                bytes,
                asked: Mutex::new(Vec::new()),
                fail_after: None,
            }
        }

        async fn read(&self, offset: u64, len: u32) -> Result<Option<Vec<u8>>, Error> {
            let mut asked = self.asked.lock().unwrap();
            if self.fail_after.is_some_and(|n| asked.len() >= n) {
                return Err(Error::Timeout);
            }
            asked.push((offset, len));
            let start = (offset as usize).min(self.bytes.len());
            let end = (start + len as usize).min(self.bytes.len());
            Ok(Some(self.bytes[start..end].to_vec()))
        }

        fn requests(&self) -> usize {
            self.asked.lock().unwrap().len()
        }

        fn bytes_sent(&self) -> u64 {
            self.asked
                .lock()
                .unwrap()
                .iter()
                .map(|&(offset, len)| {
                    (self.bytes.len() as u64)
                        .saturating_sub(offset)
                        .min(u64::from(len))
                })
                .sum()
        }

        async fn frame(&self) -> Result<Option<Thumbnail>, Error> {
            self.frame_claiming(self.bytes.len() as u64).await
        }

        async fn frame_claiming(&self, size: u64) -> Result<Option<Thumbnail>, Error> {
            first_frame_in(self.scratch.path(), size, |offset, len| {
                self.read(offset, len)
            })
            .await
        }
    }

    /// Folders the phone's videos left behind.
    fn left_behind(phone: &Phone) -> usize {
        std::fs::read_dir(phone.scratch.path()).unwrap().count()
    }

    fn is_red([r, g, b]: [u8; 3]) -> bool {
        r > 200 && g < 90 && b < 90
    }

    fn top_left(thumb: &Thumbnail) -> [u8; 3] {
        let (x, y) = (thumb.width as usize / 4, thumb.height as usize / 4);
        let at = (y * thumb.width as usize + x) * 4;
        [thumb.rgba[at], thumb.rgba[at + 1], thumb.rgba[at + 2]]
    }

    #[tokio::test]
    async fn a_moov_last_video_gets_its_first_frame_from_a_few_megabytes() {
        // 40 MiB of video: reading it all would take 40 requests.
        let phone = Phone::new(padded_at_end(40 << 20));
        let thumb = phone.frame().await.unwrap().expect("a frame");
        assert!(is_red(top_left(&thumb)));
        assert!(phone.requests() <= 12, "{} requests", phone.requests());
        assert!(phone.bytes_sent() < 3 << 20, "{} bytes", phone.bytes_sent());
    }

    #[tokio::test]
    async fn a_faststart_video_gets_its_first_frame() {
        let phone = Phone::new(bytes("faststart.mp4"));
        assert!(is_red(top_left(
            &phone.frame().await.unwrap().expect("a frame")
        )));
    }

    #[tokio::test]
    async fn a_portrait_video_is_upright() {
        let thumb = Phone::new(bytes("portrait.mp4"))
            .frame()
            .await
            .unwrap()
            .expect("a frame");
        assert!(thumb.height > thumb.width);
        assert!(is_red(top_left(&thumb)));
    }

    #[tokio::test]
    async fn a_head_that_is_too_small_is_read_again_larger() {
        // The first picture lies 3 MiB into the data; the first tier stops at 2.
        let phone = Phone::new(padded_at_start(3 << 20));
        let thumb = phone
            .frame()
            .await
            .unwrap()
            .expect("a frame from the second tier");
        assert!(is_red(top_left(&thumb)));
        let asked = phone.asked.lock().unwrap().clone();
        // 3 box headers, the first tier's start (3 pieces of 1 MiB) and the index (1),
        // then only the bytes the first tier lacked (2), never the index again.
        assert_eq!(asked.len(), 3 + 3 + 1 + 2, "{asked:?}");
        let whole = &phone.bytes;
        let moov_at = whole.windows(4).position(|w| w == b"moov").unwrap() as u64 - 4;
        // (a 16-byte look at its header is the walk, not a read of it)
        let index_reads = asked
            .iter()
            .filter(|&&(at, len)| at == moov_at && len > 16)
            .count();
        assert_eq!(index_reads, 1, "the index is read once: {asked:?}");
    }

    #[tokio::test]
    async fn a_phone_unplugged_halfway_is_an_error_and_leaves_no_temp_folder() {
        let mut phone = Phone::new(padded_at_end(8 << 20));
        phone.fail_after = Some(5);
        assert!(matches!(phone.frame().await, Err(Error::Timeout)));
        assert_eq!(
            phone.requests(),
            5,
            "it got as far as the start of the picture data"
        );
        assert_eq!(left_behind(&phone), 0);
    }

    #[tokio::test]
    async fn no_temp_folders_are_left_after_many_videos() {
        let phone = Phone::new(bytes("moov_at_end.mp4"));
        for _ in 0..50 {
            assert!(phone.frame().await.unwrap().is_some());
        }
        let garbage = Phone::new(vec![7; 4096]);
        for _ in 0..20 {
            assert_eq!(garbage.frame().await.unwrap(), None);
        }
        assert_eq!((left_behind(&phone), left_behind(&garbage)), (0, 0));
    }

    #[tokio::test]
    async fn a_video_whose_boxes_lie_gets_none() {
        let good = bytes("moov_at_end.mp4");
        let moov_at = good.windows(4).position(|w| w == b"moov").unwrap() - 4;
        let patch = |at: usize, value: u32| {
            let mut bad = good.clone();
            bad[at..at + 4].copy_from_slice(&value.to_be_bytes());
            bad
        };
        let cases = [
            ("a 2 GB moov", patch(moov_at, 2_000_000_000)),
            ("a moov past the end", patch(moov_at, good.len() as u32)),
            ("an mdat that runs to the end", {
                let mut bad = good.clone();
                bad[28..32].copy_from_slice(&0u32.to_be_bytes());
                bad
            }),
            ("a size below its header", patch(moov_at, 3)),
        ];
        for (what, bytes) in cases {
            let phone = Phone::new(bytes);
            assert_eq!(phone.frame().await.unwrap(), None, "{what}");
            assert!(
                phone.requests() <= 6,
                "{what}: {} requests",
                phone.requests()
            );
        }
    }

    #[tokio::test]
    async fn a_file_shorter_than_the_phone_claimed_gets_none() {
        let phone = Phone::new(bytes("moov_at_end.mp4"));
        let claimed = phone.bytes.len() as u64 + (1 << 20);
        assert_eq!(phone.frame_claiming(claimed).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_video_claiming_more_than_a_terabyte_is_refused_untried() {
        let phone = Phone::new(bytes("moov_at_end.mp4"));
        assert_eq!(
            phone.frame_claiming(MAX_VIDEO_BYTES + 1).await.unwrap(),
            None
        );
        assert_eq!(phone.requests(), 0);
        assert_eq!(phone.frame_claiming(0).await.unwrap(), None);
        assert_eq!(phone.requests(), 0);
    }

    #[tokio::test]
    async fn videos_are_made_one_at_a_time() {
        let phone = std::sync::Arc::new(Phone::new(bytes("moov_at_end.mp4")));
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let phone = std::sync::Arc::clone(&phone);
                tokio::spawn(async move { phone.frame().await.unwrap().is_some() })
            })
            .collect();
        for task in tasks {
            assert!(task.await.unwrap());
        }
        assert_eq!(HIGH_WATER.load(Ordering::SeqCst), 1);
    }
}
