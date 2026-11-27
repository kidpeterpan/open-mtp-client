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

/// What the decoder made of a video, and whether more of the file might make it better.
#[derive(Debug, PartialEq)]
pub struct Seen {
    pub thumbnail: Thumbnail,
    /// The first picture is dark (a fade-in) and a later one, which would be better,
    /// could not be made: its data may be in a part of the file not read yet. A
    /// whole file never says so; a stand-in for a video on a phone may.
    pub more_data_may_help: bool,
}

/// The first frame of the video at `path`, at most `THUMBNAIL_SIZE` a side and
/// the right way up. `None` when no frame can be made or `timeout` passes.
pub fn first_frame(path: &Path, timeout: Duration) -> Option<Thumbnail> {
    first_look(path, timeout).map(|seen| seen.thumbnail)
}

/// `first_frame`, and whether reading more of the file could give a better one.
/// A picture that starts dark (a clip that fades in from black) is replaced by
/// one a moment in, when that can be made and is brighter.
pub fn first_look(path: &Path, timeout: Duration) -> Option<Seen> {
    let path = path.to_path_buf();
    with_deadline(timeout, move || decode(&path)).flatten()
}

/// A picture with a mean brightness below this (of 255) is "dark": black, or a
/// fade from it.
const DARK_BELOW: u64 = 20;

/// Whether `picture` is dark: its mean brightness, with the weights the eye gives
/// red, green and blue, is under `DARK_BELOW`. A picture with no pixels is dark.
fn is_dark(picture: &Thumbnail) -> bool {
    let pixels = (picture.rgba.len() / 4) as u64;
    if pixels == 0 {
        return true;
    }
    brightness(picture) < DARK_BELOW * 1000 * pixels
}

/// Where in a clip of `length` seconds to look when its start is dark: one second
/// in, or halfway for a clip under two. None for one too short or of no known length.
fn later_time(length: f64) -> Option<f64> {
    (length.is_finite() && length >= 0.4).then(|| (length / 2.0).min(1.0))
}

/// What came of looking at the later moment.
enum Later {
    /// The clip is too short to have one.
    NoMoment,
    /// There is one, and no picture could be made of it.
    Missing,
    Frame(Thumbnail),
}

/// The first picture, or the later one when it is brighter.
fn settle(first: Thumbnail, later: Later) -> Seen {
    match later {
        Later::NoMoment => Seen {
            thumbnail: first,
            more_data_may_help: false,
        },
        Later::Missing => Seen {
            thumbnail: first,
            more_data_may_help: true,
        },
        Later::Frame(later) => Seen {
            thumbnail: if brightness(&later) > brightness(&first) {
                later
            } else {
                first
            },
            more_data_may_help: false,
        },
    }
}

/// The sum of the brightness of every pixel (only compared between pictures).
fn brightness(picture: &Thumbnail) -> u64 {
    picture
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| 299 * u64::from(p[0]) + 587 * u64::from(p[1]) + 114 * u64::from(p[2]))
        .sum()
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
fn decode(_path: &Path) -> Option<Seen> {
    None
}

#[cfg(target_os = "macos")]
mod av {
    use super::{Later, Seen, Thumbnail, is_dark, later_time, settle};
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

    pub(super) fn decode(path: &Path) -> Option<Seen> {
        autoreleasepool(|_| decode_in_pool(path))
    }

    #[allow(deprecated)] // the synchronous calls, so the deadline above is the only wait
    fn decode_in_pool(path: &Path) -> Option<Seen> {
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path.to_str()?), false);
        // SAFETY: plain Objective-C calls with valid arguments; no pointers are kept.
        let (asset, generator) = unsafe {
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
            let generator = AVAssetImageGenerator::assetImageGeneratorWithAsset(&asset);
            generator.setAppliesPreferredTrackTransform(true);
            let side = f64::from(THUMBNAIL_SIZE);
            generator.setMaximumSize(CGSize {
                width: side,
                height: side,
            });
            (asset, generator)
        };
        // The default tolerances (any frame near the time asked), not zero: a
        // phone's clip often starts a little after 0 or after an edit.
        let frame_at = |seconds: f64| -> Option<Thumbnail> {
            // SAFETY: as above; the image is only read while it is alive.
            let image = unsafe {
                let time = CMTime::new((seconds * 600.0).round() as i64, 600);
                generator
                    .copyCGImageAtTime_actualTime_error(time, std::ptr::null_mut())
                    .ok()?
            };
            rgba_of(&image)
        };

        let first = frame_at(0.0)?;
        if !is_dark(&first) {
            return Some(Seen {
                thumbnail: first,
                more_data_may_help: false,
            });
        }
        // A fade-in from black: look a moment into the clip instead.
        // SAFETY: a plain property read.
        let length = seconds_of(unsafe { asset.duration() });
        let later = match later_time(length) {
            None => Later::NoMoment,
            Some(at) => frame_at(at).map_or(Later::Missing, Later::Frame),
        };
        Some(settle(first, later))
    }

    /// `time` in seconds; NaN for one that is invalid, indefinite or infinite.
    fn seconds_of(time: CMTime) -> f64 {
        // kCMTimeFlags_Valid is 1; positive and negative infinity and indefinite are
        // 4, 8 and 16.
        if time.flags.0 & 0b1_1101 != 1 || time.timescale <= 0 {
            return f64::NAN;
        }
        time.value as f64 / f64::from(time.timescale)
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
        // The fourth byte of each pixel was skipped by the context: opaque.
        for alpha in rgba.iter_mut().skip(3).step_by(4) {
            *alpha = 255;
        }
        Some(Thumbnail {
            width: u32::try_from(width).ok()?,
            height: u32::try_from(height).ok()?,
            rgba,
        })
    }
}

#[cfg(test)]
mod choosing_tests {
    use super::*;

    fn grey(value: u8, pixels: usize) -> Thumbnail {
        Thumbnail {
            width: pixels as u32,
            height: 1,
            rgba: [value, value, value, 255].repeat(pixels),
        }
    }

    #[test]
    fn black_is_dark_and_the_middle_of_the_range_is_not() {
        assert!(is_dark(&grey(0, 100)));
        assert!(!is_dark(&grey(128, 100)));
        assert!(!is_dark(&grey(255, 100)));
    }

    #[test]
    fn dark_ends_at_a_mean_brightness_of_20() {
        assert!(is_dark(&grey(19, 100)));
        assert!(!is_dark(&grey(20, 100)));
    }

    #[test]
    fn half_black_and_half_white_is_not_dark() {
        let mut half = grey(0, 50);
        half.rgba.extend(grey(255, 50).rgba);
        half.width = 100;
        assert!(!is_dark(&half));
    }

    #[test]
    fn colour_counts_by_how_bright_the_eye_finds_it() {
        // Pure blue is dark to the eye (0.114), pure green is not (0.587).
        let blue = Thumbnail {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 255, 255],
        };
        let green = Thumbnail {
            width: 1,
            height: 1,
            rgba: vec![0, 255, 0, 255],
        };
        assert!(!is_dark(&blue), "blue is 29 of 255");
        assert!(!is_dark(&green));
        let navy = Thumbnail {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 150, 255],
        };
        assert!(is_dark(&navy), "17 of 255");
    }

    #[test]
    fn an_empty_picture_is_dark_not_a_division_by_zero() {
        let none = Thumbnail {
            width: 0,
            height: 0,
            rgba: Vec::new(),
        };
        assert!(is_dark(&none));
    }

    #[test]
    fn the_later_moment_is_one_second_in_or_halfway_for_a_short_clip() {
        assert_eq!(later_time(2.0), Some(1.0));
        assert_eq!(later_time(600.0), Some(1.0));
        assert_eq!(later_time(0.5), Some(0.25));
        assert_eq!(later_time(0.4), Some(0.2));
    }

    #[test]
    fn a_clip_too_short_or_of_no_known_length_has_no_later_moment() {
        for length in [0.0, 0.1, 0.39, -3.0, f64::NAN, f64::INFINITY] {
            assert_eq!(later_time(length), None, "{length}");
        }
    }

    #[test]
    fn a_brighter_later_frame_wins_and_settles_it() {
        let seen = settle(grey(0, 4), Later::Frame(grey(90, 4)));
        assert_eq!(seen.thumbnail, grey(90, 4));
        assert!(!seen.more_data_may_help);
    }

    #[test]
    fn a_later_frame_darker_than_the_first_loses() {
        let seen = settle(grey(10, 4), Later::Frame(grey(2, 4)));
        assert_eq!(seen.thumbnail, grey(10, 4));
        assert!(!seen.more_data_may_help);
    }

    #[test]
    fn a_later_frame_that_could_not_be_made_keeps_the_first_and_asks_for_more() {
        let seen = settle(grey(0, 4), Later::Missing);
        assert_eq!(seen.thumbnail, grey(0, 4));
        assert!(seen.more_data_may_help);
    }

    #[test]
    fn no_later_moment_to_try_settles_on_the_first() {
        let seen = settle(grey(0, 4), Later::NoMoment);
        assert_eq!(seen.thumbnail, grey(0, 4));
        assert!(!seen.more_data_may_help);
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
    fn a_video_that_starts_black_shows_a_later_frame() {
        let dir = tempfile::tempdir().unwrap();
        let seen = first_look(&fixture(dir.path(), "dark_start.mp4"), LONG).unwrap();
        assert_upright(&seen.thumbnail);
        assert!(!seen.more_data_may_help);
        // The plain call gives the same picture.
        let plain = first_frame(&fixture(dir.path(), "dark_start.mp4"), LONG).unwrap();
        assert_eq!(plain, seen.thumbnail);
    }

    #[test]
    fn a_video_that_is_black_throughout_gives_a_black_frame_and_nothing_more_to_try() {
        let dir = tempfile::tempdir().unwrap();
        let seen = first_look(&fixture(dir.path(), "black.mp4"), LONG).unwrap();
        assert!(is_dark(&seen.thumbnail));
        assert!(
            !seen.more_data_may_help,
            "the later frame was made, and is black too"
        );
    }

    #[test]
    fn a_video_with_a_picture_at_the_start_is_not_looked_at_twice() {
        let dir = tempfile::tempdir().unwrap();
        let seen = first_look(&fixture(dir.path(), "moov_at_end.mp4"), LONG).unwrap();
        assert!(!is_dark(&seen.thumbnail));
        assert!(!seen.more_data_may_help);
    }

    #[test]
    fn a_later_frame_whose_data_is_not_there_yet_keeps_the_black_one_and_asks_for_more() {
        // `dark_start.mp4`'s second chunk (frames 30-59, the key frame at 1 s) is a
        // hole, as when only the start of a phone's video has been read.
        let mut whole = bytes("dark_start.mp4");
        let mdat_end = 28 + 2464;
        whole[1023..mdat_end].fill(0);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cut.mp4");
        std::fs::write(&path, &whole).unwrap();

        let seen = first_look(&path, LONG).unwrap();

        assert!(is_dark(&seen.thumbnail));
        assert!(seen.more_data_may_help);
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
