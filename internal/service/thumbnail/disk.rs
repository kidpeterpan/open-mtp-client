//! Phone thumbnails kept on the Mac, so a folder opened again, even after a
//! relaunch, shows its tiles without asking the phone. Each is one small file
//! named by a hash of what identifies the phone's file (serial, storage,
//! path, size and date: a changed file is a new key) and holding that key in
//! full, so a hash that two keys share is a miss, never the wrong picture.
//! The folder is capped: past `most` bytes, the thumbnails used longest ago
//! go first. Everything here blocks on the disk; the service calls it from
//! `spawn_blocking`.

use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::dto::{Thumbnail, Timestamp};

/// How much the thumbnails on disk may take: some 20,000 photos' worth.
pub const DISK_CACHE_MOST: u64 = 200 * 1024 * 1024;
/// What a thumbnail file starts with.
// 2: thumbnails made at 192 px; 3: turned the way the photo says. A file
// written before either is a miss, and the thumbnail made again.
const MAGIC: &[u8; 8] = b"OMCTHMB3";
/// A cleanup goes down to this share of `most`, so the next one is a while
/// off rather than at the very next thumbnail.
const AFTER_CLEANUP: f64 = 0.9;
const EXTENSION: &str = "thumb";

pub struct DiskCache {
    dir: PathBuf,
    most: u64,
    /// What the folder holds, in bytes, near enough: read from the disk
    /// once, then added to as thumbnails are written, and read again by each
    /// cleanup.
    used: Mutex<Option<u64>>,
}

/// What identifies a phone file's picture: `None` for a phone without a
/// serial (two such phones would share keys) or a file without a date (an
/// edit that kept its size would go unseen).
pub fn key(
    serial: &str,
    storage_id: u64,
    path: &str,
    size: u64,
    modified: Option<Timestamp>,
) -> Option<String> {
    let when = modified?;
    if serial.is_empty() {
        return None;
    }
    Some(format!(
        "{serial}\n{storage_id}\n{path}\n{size}\n{:04}-{:02}-{:02} {:02}:{:02}",
        when.year, when.month, when.day, when.hour, when.minute
    ))
}

/// FNV-1a: the same on every run and every build, unlike std's hasher.
fn hash(key: &str) -> u64 {
    key.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The picture as stored: JPEG for an opaque one (a photo's thumbnail is a
/// few kilobytes), PNG when it has see-through parts.
fn encode(thumbnail: &Thumbnail) -> Option<Vec<u8>> {
    let image =
        image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.rgba.clone())?;
    let mut bytes = Cursor::new(Vec::new());
    if image.pixels().all(|pixel| pixel.0[3] == u8::MAX) {
        let rgb = image::DynamicImage::ImageRgba8(image).to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
            .encode_image(&rgb)
            .ok()?;
    } else {
        image.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
    }
    Some(bytes.into_inner())
}

impl DiskCache {
    pub fn new(dir: PathBuf, most: u64) -> Self {
        Self {
            dir,
            most,
            used: Mutex::new(None),
        }
    }

    fn file(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{:016x}.{EXTENSION}", hash(key)))
    }

    /// The stored picture's bytes (JPEG or PNG) for `key`, marked as just
    /// used; `None` when there is none, or the file is someone else's.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.file(key);
        let bytes = fs::read(&path).ok()?;
        let header = MAGIC.len() + 4;
        if bytes.len() < header || &bytes[..MAGIC.len()] != MAGIC {
            return None;
        }
        let length = u32::from_le_bytes(bytes[MAGIC.len()..header].try_into().ok()?) as usize;
        let stored = bytes.get(header..header + length)?;
        if stored != key.as_bytes() {
            return None;
        }
        // Used now: the cleanup drops what was used longest ago.
        if let Ok(file) = fs::File::options().write(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }
        Some(bytes[header + length..].to_vec())
    }

    /// Keeps `thumbnail` for `key`, then makes room if the folder holds too
    /// much. A failure only means it is fetched again next time.
    pub fn put(&self, key: &str, thumbnail: &Thumbnail) {
        let Some(picture) = encode(thumbnail) else {
            return;
        };
        if fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + key.len() + picture.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(key.len() as u32).to_le_bytes());
        bytes.extend_from_slice(key.as_bytes());
        bytes.extend_from_slice(&picture);
        if write_whole(&self.file(key), &bytes).is_err() {
            return;
        }
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // A thumbnail written again over its old file (after that one could
        // not be read) is counted twice: the cleanup counts the disk afresh.
        let total = match *used {
            Some(total) => total + bytes.len() as u64,
            None => self.on_disk(),
        };
        *used = Some(if total > self.most {
            self.clean_up()
        } else {
            total
        });
    }

    /// Every thumbnail file, with its size and when it was last used.
    fn files(&self) -> Vec<(PathBuf, u64, SystemTime)> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == EXTENSION))
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                Some((
                    entry.path(),
                    metadata.len(),
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                ))
            })
            .collect()
    }

    fn on_disk(&self) -> u64 {
        self.files().iter().map(|(_, size, _)| size).sum()
    }

    /// Drops the thumbnails used longest ago until the folder is well under
    /// its cap; returns what it holds then.
    fn clean_up(&self) -> u64 {
        let mut files = self.files();
        files.sort_by_key(|(_, _, used)| *used);
        let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
        let goal = (self.most as f64 * AFTER_CLEANUP) as u64;
        for (path, size, _) in files {
            if total <= goal {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= size;
            }
        }
        total
    }
}

/// Written beside its place and renamed into it, so a reader never sees half
/// a thumbnail.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = path.with_extension(format!("{}-{id}.tmp", std::process::id()));
    let result = fs::File::create(&temporary)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn when() -> Option<Timestamp> {
        Some(Timestamp {
            year: 2026,
            month: 10,
            day: 17,
            hour: 9,
            minute: 30,
        })
    }

    fn photo(shade: u8) -> Thumbnail {
        Thumbnail {
            width: 4,
            height: 3,
            rgba: [shade, 0, 0, u8::MAX].repeat(12),
        }
    }

    fn cache(most: u64) -> (tempfile::TempDir, DiskCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().join("thumbnails"), most);
        (dir, cache)
    }

    fn picture(bytes: &[u8]) -> Thumbnail {
        super::super::mapper::to_thumbnail(bytes).unwrap()
    }

    #[test]
    fn a_kept_thumbnail_comes_back_for_the_same_file_only() {
        let (_dir, cache) = cache(DISK_CACHE_MOST);
        let key = key("ABC", 1, "/DCIM/a.jpg", 100, when()).unwrap();
        assert_eq!(cache.get(&key), None);

        cache.put(&key, &photo(200));

        let back = picture(&cache.get(&key).unwrap());
        assert_eq!((back.width, back.height), (4, 3));
        assert!(
            back.rgba[0].abs_diff(200) < 8,
            "close to what went in: {}",
            back.rgba[0]
        );
        for other in [
            super::key("XYZ", 1, "/DCIM/a.jpg", 100, when()),
            super::key("ABC", 2, "/DCIM/a.jpg", 100, when()),
            super::key("ABC", 1, "/DCIM/b.jpg", 100, when()),
            super::key("ABC", 1, "/DCIM/a.jpg", 101, when()),
            super::key("ABC", 1, "/DCIM/a.jpg", 100, None).or(Some("x".into())),
        ] {
            assert_eq!(cache.get(&other.unwrap()), None);
        }
    }

    #[test]
    fn no_key_without_a_serial_or_a_date() {
        assert_eq!(key("", 1, "/a.jpg", 1, when()), None);
        assert_eq!(key("ABC", 1, "/a.jpg", 1, None), None);
        let one = key("ABC", 1, "/a.jpg", 1, when()).unwrap();
        let later = Timestamp {
            minute: 31,
            ..when().unwrap()
        };
        assert_ne!(Some(one), key("ABC", 1, "/a.jpg", 1, Some(later)));
    }

    #[test]
    fn a_file_whose_name_another_key_shares_is_not_taken_for_it() {
        let (_dir, cache) = cache(DISK_CACHE_MOST);
        let key = key("ABC", 1, "/a.jpg", 1, when()).unwrap();
        cache.put(&key, &photo(10));
        // Another key's picture written where this key's would be (as two
        // keys with one hash would): it is not this key's.
        let path = cache.file(&key);
        let mut bytes = fs::read(&path).unwrap();
        let at = MAGIC.len() + 4;
        bytes[at] ^= 1;
        fs::write(&path, &bytes).unwrap();

        assert_eq!(cache.get(&key), None);
        fs::write(&path, b"short").unwrap();
        assert_eq!(cache.get(&key), None, "not a thumbnail file");
    }

    #[test]
    fn a_thumbnail_kept_by_an_older_version_is_not_used() {
        let (_dir, cache) = cache(DISK_CACHE_MOST);
        let key = key("ABC", 1, "/a.jpg", 1, when()).unwrap();
        // At 160 px, then at 192 px before photos were turned upright.
        for older in [b"OMCTHMB1", b"OMCTHMB2"] {
            cache.put(&key, &photo(10));
            assert!(cache.get(&key).is_some());
            let path = cache.file(&key);
            let mut bytes = fs::read(&path).unwrap();
            bytes[..MAGIC.len()].copy_from_slice(older);
            fs::write(&path, &bytes).unwrap();

            assert_eq!(cache.get(&key), None, "made again");
        }
    }

    #[test]
    fn a_picture_with_see_through_parts_is_kept_as_png() {
        let (_dir, cache) = cache(DISK_CACHE_MOST);
        let key = key("ABC", 1, "/icon.png", 1, when()).unwrap();
        let mut icon = photo(50);
        icon.rgba[3] = 0;

        cache.put(&key, &icon);

        let bytes = cache.get(&key).unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
        assert_eq!(picture(&bytes).rgba, icon.rgba, "exact");
        let opaque = super::key("ABC", 1, "/a.jpg", 1, when()).unwrap();
        cache.put(&opaque, &photo(50));
        assert!(
            cache.get(&opaque).unwrap().starts_with(&[0xFF, 0xD8]),
            "JPEG"
        );
    }

    #[test]
    fn past_its_cap_the_thumbnails_used_longest_ago_go_first() {
        let (_dir, probe) = cache(DISK_CACHE_MOST);
        let first = key("ABC", 1, "/0.jpg", 1, when()).unwrap();
        probe.put(&first, &photo(0));
        let one = fs::metadata(probe.file(&first)).unwrap().len();
        // Room for about four.
        let (_dir, cache) = cache(one * 4 + one / 2);
        let keys: Vec<String> = (0..4)
            .map(|n| key("ABC", 1, &format!("/{n}.jpg"), 1, when()).unwrap())
            .collect();
        for (n, key) in keys.iter().enumerate() {
            cache.put(key, &photo(0));
            // Each older than the next by a minute, as if used in turn.
            let at = SystemTime::now() - std::time::Duration::from_secs(600 - 60 * n as u64);
            let file = fs::File::options()
                .write(true)
                .open(cache.file(key))
                .unwrap();
            file.set_modified(at).unwrap();
        }
        // The oldest is looked at, so it is used most recently now.
        assert!(cache.get(&keys[0]).is_some());

        let fifth = key("ABC", 1, "/4.jpg", 1, when()).unwrap();
        cache.put(&fifth, &photo(0));

        assert!(cache.get(&keys[0]).is_some(), "just used, kept");
        assert!(cache.get(&keys[1]).is_none(), "used longest ago, gone");
        assert!(cache.get(&fifth).is_some());
        assert!(cache.on_disk() <= one * 4 + one / 2);
    }

    /// The size of one `photo(0)` file, and a cache with room for `room` of
    /// them.
    fn sized(room: f64) -> (tempfile::TempDir, DiskCache, u64) {
        let (dir, probe) = cache(DISK_CACHE_MOST);
        let first = key("ABC", 1, "/00.jpg", 1, when()).unwrap();
        probe.put(&first, &photo(0));
        let one = fs::metadata(probe.file(&first)).unwrap().len();
        let cache = DiskCache::new(dir.path().join("sized"), (one as f64 * room) as u64);
        (dir, cache, one)
    }

    #[test]
    fn a_cleanup_makes_room_for_a_while_not_for_one_more() {
        let (_dir, cache, one) = sized(10.5);
        for n in 0..11 {
            cache.put(
                &key("ABC", 1, &format!("/{n:02}.jpg"), 1, when()).unwrap(),
                &photo(0),
            );
        }

        assert_eq!(cache.on_disk(), 9 * one, "down to 90% of the cap");
    }

    #[test]
    fn keeping_a_thumbnail_again_takes_no_more_room() {
        let (_dir, cache, _) = sized(3.5);
        let keys: Vec<String> = (0..3)
            .map(|n| key("ABC", 1, &format!("/{n:02}.jpg"), 1, when()).unwrap())
            .collect();
        for key in &keys {
            cache.put(key, &photo(0));
        }
        for _ in 0..10 {
            cache.put(&keys[2], &photo(0));
        }

        assert!(
            keys.iter().all(|key| cache.get(key).is_some()),
            "none dropped"
        );
    }

    #[test]
    fn a_folder_that_cannot_be_made_keeps_nothing_and_breaks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("file");
        fs::write(&blocked, b"x").unwrap();
        let cache = DiskCache::new(blocked.join("thumbnails"), DISK_CACHE_MOST);
        let key = key("ABC", 1, "/a.jpg", 1, when()).unwrap();

        cache.put(&key, &photo(1));

        assert_eq!(cache.get(&key), None);
    }

    #[test]
    fn the_hash_is_the_same_on_every_run() {
        assert_eq!(hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
