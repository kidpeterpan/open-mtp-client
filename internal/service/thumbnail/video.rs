//! A video's first frame, made by the Mac's own decoder (AVFoundation).
//!
//! It is handed a path to a file that may be a sparse stand-in for a video on a
//! phone (`sparse.rs`): the index and the start are real and the rest is a
//! hole. A decoder asked for the first picture reads only those, so the answer
//! is the same as from the whole file. The bytes come from a phone and are not
//! trusted, so the decoder runs on its own thread under a deadline; a stuck one
//! is left to finish and its answer is dropped.

use crate::dto::Thumbnail;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// How long the decoder gets for one frame.
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// The first frame of the video at `path`, at most `THUMBNAIL_SIZE` a side and
/// the right way up. `None` when no frame can be made or `timeout` passes.
pub fn first_frame(path: &Path, timeout: Duration) -> Option<Thumbnail> {
    let path = path.to_path_buf();
    with_deadline(timeout, move || decode(&path)).flatten()
}

/// Threads left running past their deadline, and the most there may be: a
/// decoder stuck on a bad file does not come back, and a folder of them must
/// not pile up threads. Past the limit no new work is started at all.
static ABANDONED: AtomicUsize = AtomicUsize::new(0);
const MOST_ABANDONED: usize = 8;

/// Runs `work` on its own thread. `None` when it has not answered within
/// `timeout` (the thread is left to finish and its answer is dropped), or when
/// too many earlier ones are still stuck.
pub fn with_deadline<T: Send + 'static>(
    timeout: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    with_deadline_counting(&ABANDONED, MOST_ABANDONED, timeout, work)
}

/// `with_deadline` with its own count of abandoned threads, so a test can fill it.
fn with_deadline_counting<T: Send + 'static>(
    abandoned: &'static AtomicUsize,
    most: usize,
    timeout: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    if abandoned.load(Ordering::Acquire) >= most {
        return None;
    }
    let (send, receive) = mpsc::channel();
    let late = Arc::new(AtomicBool::new(false));
    let worker_late = Arc::clone(&late);
    std::thread::spawn(move || {
        let answer = work();
        // The receiver may be gone by now; that is the point of a deadline.
        let _ = send.send(answer);
        // Gave up on, and now over: no longer stuck.
        if worker_late.swap(true, Ordering::AcqRel) {
            abandoned.fetch_sub(1, Ordering::AcqRel);
        }
    });
    match receive.recv_timeout(timeout) {
        Ok(answer) => Some(answer),
        Err(_) => {
            // If the worker has not ended, count it; if it ended in the
            // meantime it has already flipped the flag and we must not count.
            if !late.swap(true, Ordering::AcqRel) {
                abandoned.fetch_add(1, Ordering::AcqRel);
            }
            None
        }
    }
}

/// The decoder: AVFoundation, on a Mac. Anywhere else there is none, and no
/// video has a frame.
#[cfg(target_os = "macos")]
use av::decode;

#[cfg(not(target_os = "macos"))]
fn decode(_path: &Path) -> Option<Thumbnail> {
    None
}

#[cfg(target_os = "macos")]
mod av {
    use super::Thumbnail;
    use crate::dto::THUMBNAIL_SIZE;
    use av_core_foundation::{CGPoint, CGRect, CGSize};
    use av_core_graphics::{
        CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, kCGColorSpaceSRGB,
    };
    use av_core_media::CMTime;
    use av_foundation::{AVAssetImageGenerator, AVURLAsset};
    use av_foundation_types::{NSString, NSURL};
    use av_objc2::rc::autoreleasepool;
    use std::path::Path;

    /// `kCGImageAlphaNoneSkipLast`: R, G, B and a byte nothing is stored in
    /// (set to 255 below). A frame of a video is opaque, so it is not premultiplied.
    const RGB_SKIP_LAST: u32 = 5;

    pub(super) fn decode(path: &Path) -> Option<Thumbnail> {
        autoreleasepool(|_| decode_in_pool(path))
    }

    #[allow(deprecated)] // the synchronous call, so the deadline above is the only wait
    fn decode_in_pool(path: &Path) -> Option<Thumbnail> {
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path.to_str()?), false);
        // SAFETY: plain Objective-C calls with valid arguments; no pointers are kept.
        let image = unsafe {
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
            let generator = AVAssetImageGenerator::assetImageGeneratorWithAsset(&asset);
            generator.setAppliesPreferredTrackTransform(true);
            let side = f64::from(THUMBNAIL_SIZE);
            generator.setMaximumSize(CGSize {
                width: side,
                height: side,
            });
            // The default tolerances (any frame near the time asked), not zero: a
            // phone's clip often starts a little after 0 or after an edit.
            generator
                .copyCGImageAtTime_actualTime_error(CMTime::new(0, 1), std::ptr::null_mut())
                .ok()?
        };
        rgba_of(&image)
    }

    /// The pixels of `image` as opaque sRGB RGBA, top row first, shrunk to fit
    /// `THUMBNAIL_SIZE` if the decoder gave a pixel or two more.
    fn rgba_of(image: &CGImage) -> Option<Thumbnail> {
        let (full_width, full_height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
        if full_width == 0 || full_height == 0 {
            return None;
        }
        let side = THUMBNAIL_SIZE as usize;
        let (width, height) = if full_width <= side && full_height <= side {
            (full_width, full_height)
        } else {
            let scale = side as f64 / full_width.max(full_height) as f64;
            (
                ((full_width as f64 * scale).round() as usize).clamp(1, side),
                ((full_height as f64 * scale).round() as usize).clamp(1, side),
            )
        };
        let mut rgba = vec![0u8; width * height * 4];
        // sRGB, not the device's: a wide-gamut clip is shown as the rest of the app shows colour.
        // SAFETY: a constant that Core Graphics defines.
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
            .or_else(CGColorSpace::new_device_rgb)?;
        // SAFETY: `rgba` outlives `context`, which is dropped before it is read,
        // and is exactly `height` rows of `width * 4` bytes.
        let context = unsafe {
            CGBitmapContextCreate(
                rgba.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * 4,
                Some(&space),
                RGB_SKIP_LAST,
            )
        }?;
        let rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: width as f64,
                height: height as f64,
            },
        };
        CGContext::draw_image(Some(&context), rect, Some(image));
        drop(context);
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        Some(Thumbnail {
            width: u32::try_from(width).ok()?,
            height: u32::try_from(height).ok()?,
            rgba,
        })
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::dto::THUMBNAIL_SIZE;
    use crate::service::thumbnail::fixtures::bytes;
    use crate::service::thumbnail::mp4::{HEADER_PEEK, Walker, read_plan};
    use crate::service::thumbnail::sparse::{Piece, write_sparse};
    use std::time::Instant;

    const LONG: Duration = Duration::from_secs(20);

    fn fixture(dir: &Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes(name)).unwrap();
        path
    }

    fn pixel(thumb: &Thumbnail, x: f32, y: f32) -> [u8; 3] {
        let (px, py) = (
            (thumb.width as f32 * x) as usize,
            (thumb.height as f32 * y) as usize,
        );
        let at = (py * thumb.width as usize + px) * 4;
        [thumb.rgba[at], thumb.rgba[at + 1], thumb.rgba[at + 2]]
    }

    // H.264 and its colour conversion shift a pure colour by a few tens of levels
    // (blue comes back as about 30, 60, 247); the quadrants are still far apart.
    fn is_red([r, g, b]: [u8; 3]) -> bool {
        r > 200 && g < 90 && b < 90
    }

    fn is_blue([r, g, b]: [u8; 3]) -> bool {
        b > 200 && r < 90 && g < 90
    }

    /// Red top-left as seen, blue in the other three quadrants.
    fn assert_upright(thumb: &Thumbnail) {
        assert_eq!(thumb.rgba.len(), (thumb.width * thumb.height * 4) as usize);
        assert!(
            is_red(pixel(thumb, 0.25, 0.25)),
            "top-left {:?}",
            pixel(thumb, 0.25, 0.25)
        );
        for (x, y) in [(0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
            assert!(
                is_blue(pixel(thumb, x, y)),
                "({x}, {y}) {:?}",
                pixel(thumb, x, y)
            );
        }
    }

    fn assert_aspect(thumb: &Thumbnail, wide: u32, high: u32) {
        assert!(thumb.width <= THUMBNAIL_SIZE && thumb.height <= THUMBNAIL_SIZE);
        let expected = f64::from(thumb.width) * f64::from(high) / f64::from(wide);
        assert!(
            (expected - f64::from(thumb.height)).abs() <= 1.0,
            "{}x{}",
            thumb.width,
            thumb.height
        );
    }

    #[test]
    fn the_first_frame_of_a_moov_last_video_is_made() {
        let dir = tempfile::tempdir().unwrap();
        let thumb = first_frame(&fixture(dir.path(), "moov_at_end.mp4"), LONG).unwrap();
        assert_aspect(&thumb, 160, 96);
        assert_upright(&thumb);
    }

    #[test]
    fn a_faststart_video_gives_the_same_picture() {
        let dir = tempfile::tempdir().unwrap();
        let a = first_frame(&fixture(dir.path(), "moov_at_end.mp4"), LONG).unwrap();
        let b = first_frame(&fixture(dir.path(), "faststart.mp4"), LONG).unwrap();
        assert_aspect(&b, 160, 96);
        assert_upright(&b);
        assert_eq!((a.width, a.height), (b.width, b.height));
    }

    #[test]
    fn a_portrait_video_comes_out_upright() {
        let dir = tempfile::tempdir().unwrap();
        let thumb = first_frame(&fixture(dir.path(), "portrait.mp4"), LONG).unwrap();
        assert!(
            thumb.height > thumb.width,
            "{}x{}",
            thumb.width,
            thumb.height
        );
        assert_aspect(&thumb, 96, 160);
        assert_upright(&thumb);
    }

    /// The pieces `read_plan` asks for, cut from `whole`, as one sparse file.
    fn sparse_of(dir: &Path, whole: &[u8], keep_head: bool) -> std::path::PathBuf {
        let len = whole.len() as u64;
        let mut walker = Walker::new(len);
        let layout = loop {
            let at = walker.next_offset() as usize;
            let end = (at + HEADER_PEEK as usize).min(whole.len());
            if let Some(layout) = walker.feed(&whole[at..end]).unwrap() {
                break layout;
            }
        };
        let mut plan = read_plan(&layout, len, 0).unwrap();
        if !keep_head {
            plan = vec![layout.moov.clone()];
        }
        let pieces: Vec<Piece> = plan
            .iter()
            .map(|range| Piece {
                offset: range.start,
                bytes: &whole[range.start as usize..range.end as usize],
            })
            .collect();
        let path = dir.join(if keep_head {
            "sparse.mp4"
        } else {
            "no-head.mp4"
        });
        write_sparse(&path, len, &pieces).unwrap();
        path
    }

    #[test]
    fn a_video_with_only_its_start_and_its_index_is_enough() {
        let dir = tempfile::tempdir().unwrap();
        let whole = bytes("moov_at_end.mp4");
        let full = first_frame(&fixture(dir.path(), "moov_at_end.mp4"), LONG).unwrap();
        let sparse = first_frame(&sparse_of(dir.path(), &whole, true), LONG).unwrap();
        assert_eq!(full, sparse, "the same picture, byte for byte");
    }

    #[test]
    fn a_hole_in_the_middle_of_the_data_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let whole = crate::service::thumbnail::fixtures::padded_at_end(6 << 20);
        let path = dir.path().join("padded.mp4");
        std::fs::write(&path, &whole).unwrap();
        let full = first_frame(&path, LONG).unwrap();
        assert_upright(&full);

        let sparse = sparse_of(dir.path(), &whole, true);
        // Past the first tier the file is a hole: zeros, not the filler.
        let hole = std::fs::read(&sparse).unwrap();
        assert!(
            hole[(3 << 20)..(5 << 20)].iter().all(|&byte| byte == 0),
            "the filler was not copied"
        );
        assert!(whole[(3 << 20)..(5 << 20)].iter().all(|&byte| byte == 0xAB));
        assert_eq!(first_frame(&sparse, LONG), Some(full));
    }

    #[test]
    fn a_video_without_its_start_has_no_frame() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let thumb = first_frame(
            &sparse_of(dir.path(), &bytes("moov_at_end.mp4"), false),
            LONG,
        );
        assert_eq!(thumb, None);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn garbage_and_empty_files_have_no_frame() {
        let dir = tempfile::tempdir().unwrap();
        let noise: Vec<u8> = (0..1024u32)
            .map(|n| (n.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        let cases: [(&str, Vec<u8>); 3] = [
            ("empty.mp4", Vec::new()),
            ("noise.mp4", noise),
            (
                "picture.mp4",
                crate::service::thumbnail::mapper::png(32, 32),
            ),
        ];
        for (name, content) in cases {
            let path = dir.path().join(name);
            std::fs::write(&path, content).unwrap();
            assert_eq!(first_frame(&path, LONG), None, "{name}");
        }
    }

    #[test]
    fn a_missing_file_has_no_frame() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(first_frame(&dir.path().join("gone.mp4"), LONG), None);
    }

    #[test]
    fn work_stuck_past_its_deadline_is_counted_and_stops_new_work_until_it_ends() {
        static STUCK: AtomicUsize = AtomicUsize::new(0);
        let release = Arc::new(AtomicBool::new(false));
        let stuck = |release: Arc<AtomicBool>| {
            move || {
                while !release.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                1
            }
        };
        let wait = Duration::from_millis(30);

        assert_eq!(
            with_deadline_counting(&STUCK, 2, wait, stuck(Arc::clone(&release))),
            None
        );
        assert_eq!(
            with_deadline_counting(&STUCK, 2, wait, stuck(Arc::clone(&release))),
            None
        );
        assert_eq!(STUCK.load(Ordering::SeqCst), 2);
        // Full: this is refused without starting anything, so it cannot answer.
        let started = Instant::now();
        assert_eq!(
            with_deadline_counting(&STUCK, 2, Duration::from_secs(5), || 7),
            None
        );
        assert!(started.elapsed() < Duration::from_millis(500));

        release.store(true, Ordering::Release);
        for _ in 0..200 {
            if STUCK.load(Ordering::SeqCst) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(STUCK.load(Ordering::SeqCst), 0, "ended: no longer counted");
        assert_eq!(
            with_deadline_counting(&STUCK, 2, Duration::from_secs(5), || 7),
            Some(7)
        );
    }

    #[test]
    fn work_that_ends_in_time_is_never_counted() {
        static QUICK: AtomicUsize = AtomicUsize::new(0);
        for _ in 0..20 {
            assert_eq!(
                with_deadline_counting(&QUICK, 1, Duration::from_secs(5), || 1),
                Some(1)
            );
        }
        assert_eq!(QUICK.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn with_deadline_returns_the_answer_in_time() {
        assert_eq!(with_deadline(Duration::from_secs(5), || 7), Some(7));
    }

    #[test]
    fn with_deadline_gives_up_on_a_slow_worker() {
        let started = Instant::now();
        let answer = with_deadline(Duration::from_millis(100), || {
            std::thread::sleep(Duration::from_secs(2));
            7
        });
        assert_eq!(answer, None);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn two_hundred_frames_in_a_row_all_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(dir.path(), "moov_at_end.mp4");
        let started = Instant::now();
        for n in 0..200 {
            assert!(first_frame(&path, LONG).is_some(), "call {n}");
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{:?}",
            started.elapsed()
        );
    }
}
