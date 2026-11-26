use std::path::{Path, PathBuf};
use std::sync::Arc;

use mtp_rs::StorageId;
use mtp_rs::mtp::{ObjectHandle, Storage};
use tokio::sync::Semaphore;

use super::disk::{self, DiskCache};
use super::mapper;
use super::phone_video;
use crate::dto::{Entry, Error, Thumbnail};
use crate::service::{device, local};

/// Phone images at most this big are fetched whole and shrunk here when the
/// phone has no thumbnail of its own; bigger ones would cost too much USB time.
const MAX_PHONE_FETCH: u64 = 4 * 1024 * 1024;
/// Mac images at most this big are decoded for a thumbnail.
const MAX_MAC_DECODE: u64 = 64 * 1024 * 1024;
/// A phone's own thumbnail bigger than this is not decoded.
const MAX_PHONE_THUMBNAIL: usize = 1024 * 1024;
/// Decodes running at once; each may use up to the decoder's memory limit.
const DECODES_AT_ONCE: usize = 2;
static DECODES: Semaphore = Semaphore::const_new(DECODES_AT_ONCE);

/// Small previews for the grid view.
pub struct Service {
    device: Arc<device::Service>,
    /// Phone thumbnails kept on the Mac; none in tests that don't ask.
    disk: Option<Arc<DiskCache>>,
}

impl Service {
    pub fn new(device: Arc<device::Service>) -> Self {
        Self { device, disk: None }
    }

    /// Keeps phone thumbnails in `dir`, at most `most` bytes of them, and
    /// looks there before asking the phone.
    pub fn with_disk_cache(mut self, dir: PathBuf, most: u64) -> Self {
        self.disk = Some(Arc::new(DiskCache::new(dir, most)));
        self
    }

    /// A preview of the phone file `entry` in `folder`. One kept on the Mac
    /// from before is used without asking the phone; otherwise the phone's
    /// own thumbnail, or a small enough picture fetched and shrunk here, and
    /// kept for next time. `None` when there is nothing to show.
    pub async fn phone(
        &self,
        storage_id: u64,
        folder: &str,
        entry: &Entry,
    ) -> Result<Option<Thumbnail>, Error> {
        let name = entry.name.as_str();
        if !mapper::is_phone_media(name) && !mapper::is_decodable(name) {
            return Ok(None);
        }
        // Every request below is bounded like any other: a phone that goes
        // silent while thumbnails are asked for is `Timeout`, not a grid of
        // tiles waiting for ever.
        let (session, epoch) = self.device.session_at().await?;
        let kept = self.disk.as_ref().and_then(|disk| {
            let path = format!("{}/{name}", folder.trim_end_matches('/'));
            let serial = &session.device_info().serial_number;
            disk::key(serial, storage_id, &path, entry.size_bytes, entry.modified)
                .map(|key| (Arc::clone(disk), key))
        });
        if let Some((disk, key)) = &kept {
            let (disk, key) = (Arc::clone(disk), key.clone());
            if let Ok(Some(bytes)) = tokio::task::spawn_blocking(move || disk.get(&key)).await
                && let Some(thumbnail) = decode(bytes).await
            {
                return Ok(Some(thumbnail));
            }
        }
        let found = self.ask_phone(&session, epoch, storage_id, entry).await?;
        if let (Some(thumbnail), Some((disk, key))) = (&found, kept) {
            let thumbnail = thumbnail.clone();
            let _ = tokio::task::spawn_blocking(move || disk.put(&key, &thumbnail)).await;
        }
        Ok(found)
    }

    /// The phone's own thumbnail, or the picture itself if small enough.
    async fn ask_phone(
        &self,
        session: &mtp_rs::MtpDevice,
        epoch: u64,
        storage_id: u64,
        entry: &Entry,
    ) -> Result<Option<Thumbnail>, Error> {
        let (id, name, size_bytes) = (entry.id, entry.name.as_str(), entry.size_bytes);
        let storage = self
            .device
            .ask(epoch, session.storage(StorageId(storage_id)))
            .await?;
        let handle = ObjectHandle(id);

        if session.capabilities().supports_thumbnails {
            // Through `ask`, like every request here: a silent phone is
            // `Timeout`; a phone that has no thumbnail for this file answers
            // with a refusal, and the picture itself is tried.
            let own = self
                .device
                .ask(epoch, async {
                    Ok::<_, mtp_rs::Error>(storage.thumbnail(handle).await)
                })
                .await?;
            if let Ok(bytes) = own
                && bytes.len() <= MAX_PHONE_THUMBNAIL
            {
                // The photo's own orientation, from its start, where the
                // phone can read part of a file: the thumbnail it sent is
                // stored as the camera's sensor was held.
                let head = if wants_head(name, session.capabilities().supports_partial_download) {
                    self.device.ask(epoch, read_head(&storage, handle)).await?
                } else {
                    None
                };
                if let Some(thumbnail) = decode_phone(bytes, head).await {
                    return Ok(Some(thumbnail));
                }
            }
        }
        // The reported size is the phone's word; the reads below enforce it.
        match fallback_for(
            name,
            size_bytes,
            session.capabilities().supports_partial_download,
        ) {
            Fallback::Video => {
                phone_video::first_frame_of(size_bytes, |offset, len| {
                    self.device
                        .ask(epoch, read_part(&storage, handle, offset, len))
                })
                .await
            }
            Fallback::Still => match self
                .device
                .ask(epoch, read_at_most(&storage, handle, MAX_PHONE_FETCH))
                .await?
            {
                Some(bytes) => Ok(decode(bytes).await),
                None => Ok(None),
            },
            Fallback::Nothing => Ok(None),
        }
    }

    /// A preview of a Mac picture, or of a HEIC photo, video or PDF via
    /// Quick Look; `None` for anything else.
    pub async fn mac(&self, path: &str) -> Result<Option<Thumbnail>, Error> {
        let name = Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let previewable = mapper::is_quick_look(&name) || mapper::is_decodable(&name);
        if !previewable {
            return Ok(None);
        }
        // Checked first: Quick Look waits out its timeout on a missing file.
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|err| local::mapper::to_error(&err, path))?;
        if mapper::is_quick_look(&name) {
            return Ok(quick_look(path, &name, QUICK_LOOK_TIMEOUT).await);
        }
        if metadata.len() > MAX_MAC_DECODE {
            return Ok(None);
        }
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|err| local::mapper::to_error(&err, path))?;
        Ok(decode(bytes).await)
    }
}

/// How long Quick Look gets for one preview: `qlmanage` can hang for good on
/// a damaged file, so it is stopped after this.
const QUICK_LOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Quick Look runs at once, so a folder of damaged videos can't pile up
/// stuck processes.
static QUICK_LOOKS: Semaphore = Semaphore::const_new(2);

/// Asks macOS's Quick Look (`qlmanage`) for a preview PNG and decodes it with
/// the usual limits. Any failure just means no thumbnail.
async fn quick_look(path: &str, name: &str, timeout: std::time::Duration) -> Option<Thumbnail> {
    let _permit = QUICK_LOOKS.acquire().await.ok()?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let folder =
        std::env::temp_dir().join(format!("open-mtp-client-ql-{}-{id}", std::process::id()));
    tokio::fs::create_dir_all(&folder).await.ok()?;

    let size = crate::dto::THUMBNAIL_SIZE.to_string();
    let run = tokio::process::Command::new("qlmanage")
        .args(["-t", "-s", &size, "-o"])
        .arg(&folder)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    let ran = matches!(
        tokio::time::timeout(timeout, run).await,
        Ok(Ok(status)) if status.success()
    );
    let png = folder.join(format!("{name}.png"));
    let bytes = if ran {
        tokio::fs::read(&png).await.ok()
    } else {
        None
    };
    let _ = tokio::fs::remove_dir_all(&folder).await;
    decode(bytes?).await
}

/// What to make a thumbnail from when the phone has none of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fallback {
    /// The first frame of an MP4 or QuickTime video, from its start and index.
    Video,
    /// A small picture, fetched whole and shrunk here.
    Still,
    Nothing,
}

/// `size_bytes` is what the phone says the file is; `partial_reads` is whether
/// it can send part of a file (a video is never fetched whole).
fn fallback_for(name: &str, size_bytes: u64, partial_reads: bool) -> Fallback {
    if mapper::is_phone_video(name) {
        if partial_reads && phone_video::is_worth_trying(size_bytes) {
            Fallback::Video
        } else {
            Fallback::Nothing
        }
    } else if mapper::is_decodable(name) && size_bytes <= MAX_PHONE_FETCH {
        Fallback::Still
    } else {
        Fallback::Nothing
    }
}

/// `len` bytes of a phone file at `offset`; `None` when the phone won't send
/// them (about this file, not about the phone), an error when the phone is gone.
async fn read_part(
    storage: &Storage,
    handle: ObjectHandle,
    offset: u64,
    len: u32,
) -> Result<Option<Vec<u8>>, mtp_rs::Error> {
    match storage.read_range(handle, offset, len).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if crate::service::failures::is_about_one_object(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

/// The start of a phone file, or `None` if it is longer than `cap` (or the
/// phone can't read part of a file): asking for `cap + 1` bytes bounds what
/// is received whatever size the phone claimed.
async fn read_at_most(
    storage: &Storage,
    handle: ObjectHandle,
    cap: u64,
) -> Result<Option<Vec<u8>>, mtp_rs::Error> {
    let want = u32::try_from(cap + 1).unwrap_or(u32::MAX);
    match storage.read_range(handle, 0, want).await {
        Ok(bytes) if bytes.len() as u64 > cap => Ok(None),
        Ok(bytes) => Ok(Some(bytes)),
        Err(mtp_rs::Error::Unsupported) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Whether a photo's start is worth reading for its orientation: a JPEG
/// (where a camera writes EXIF), on a phone that can read part of a file.
fn wants_head(name: &str, partial_reads: bool) -> bool {
    partial_reads
        && name
            .rsplit_once('.')
            .is_some_and(|(_, ext)| matches!(ext.to_ascii_lowercase().as_str(), "jpg" | "jpeg"))
}

/// The start of a phone file (`mapper::HEAD_BYTES` at most), or `None` when
/// the phone won't read part of it; a lost phone is still an error.
async fn read_head(
    storage: &Storage,
    handle: ObjectHandle,
) -> Result<Option<Vec<u8>>, mtp_rs::Error> {
    match storage.read_range(handle, 0, mapper::HEAD_BYTES).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if crate::service::failures::is_about_one_object(&err) => Ok(None),
        Err(mtp_rs::Error::Unsupported) => Ok(None),
        Err(err) => Err(err),
    }
}

/// `decode`, for a phone's own thumbnail turned as its photo's start says.
async fn decode_phone(bytes: Vec<u8>, head: Option<Vec<u8>>) -> Option<Thumbnail> {
    let _permit = DECODES.acquire().await.ok()?;
    tokio::task::spawn_blocking(move || mapper::to_phone_thumbnail(&bytes, head.as_deref()))
        .await
        .ok()
        .flatten()
}

/// Decoding is CPU work, so it runs off the async threads, a couple at a
/// time so a screenful of tiles can't stack up decoder memory.
async fn decode(bytes: Vec<u8>) -> Option<Thumbnail> {
    let _permit = DECODES.acquire().await.ok()?;
    tokio::task::spawn_blocking(move || mapper::to_thumbnail(&bytes))
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::service::explorer;
    use crate::service::testing::virtual_source;

    async fn phone() -> (tempfile::TempDir, Service, explorer::Service, u64) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("photo.png"), mapper::png(320, 320)).unwrap();
        fs::write(dir.path().join("notes.txt"), b"hello").unwrap();
        fs::write(dir.path().join("broken.png"), b"not really").unwrap();
        fs::write(
            dir.path().join("huge.png"),
            vec![0u8; (MAX_PHONE_FETCH + 1) as usize],
        )
        .unwrap();
        let device = Arc::new(device::Service::new(virtual_source(dir.path())));
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = explorer::Service::new(Arc::clone(&device));
        (dir, Service::new(device), explorer, storage_id)
    }

    async fn thumbnail_of(name: &str) -> Result<Option<Thumbnail>, Error> {
        let (_dir, service, explorer, storage_id) = phone().await;
        let entries = explorer.list(storage_id, "/", false).await.unwrap();
        let entry = entries.iter().find(|e| e.name == name).unwrap();
        service.phone(storage_id, "/", entry).await
    }

    /// A phone with `photo.png` in DCIM, and where its thumbnails are kept.
    async fn kept() -> (tempfile::TempDir, Arc<device::Service>, u64, Entry, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("phone/DCIM")).unwrap();
        fs::write(
            dir.path().join("phone/DCIM/photo.png"),
            mapper::png(320, 240),
        )
        .unwrap();
        let device = Arc::new(device::Service::new(virtual_source(
            &dir.path().join("phone"),
        )));
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let entry = explorer::Service::new(Arc::clone(&device))
            .list(storage_id, "/DCIM", false)
            .await
            .unwrap()
            .remove(0);
        let cache = dir.path().join("cache");
        (dir, device, storage_id, entry, cache)
    }

    #[tokio::test]
    async fn a_thumbnail_kept_on_the_mac_is_shown_after_a_relaunch_without_the_phone() {
        let (_dir, device, storage_id, entry, cache) = kept().await;
        let first = Service::new(Arc::clone(&device)).with_disk_cache(cache.clone(), 1 << 20);
        let fetched = first
            .phone(storage_id, "/DCIM", &entry)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 1);

        // The app again, and a phone that answers nothing at all: any request
        // would end as a timeout.
        let relaunched = Service::new(Arc::clone(&device)).with_disk_cache(cache, 1 << 20);
        let _silence = crate::service::testing::silence(&device, storage_id).await;
        let shown = relaunched.phone(storage_id, "/DCIM", &entry).await;

        let shown = shown.unwrap().unwrap();
        assert_eq!((shown.width, shown.height), (fetched.width, fetched.height));
    }

    #[tokio::test]
    async fn a_changed_file_or_one_without_a_date_asks_the_phone() {
        let (_dir, device, storage_id, entry, cache) = kept().await;
        let service = Service::new(Arc::clone(&device)).with_disk_cache(cache.clone(), 1 << 20);
        let _ = service.phone(storage_id, "/DCIM", &entry).await.unwrap();

        // Kept under the file's old size: the phone's file now, at its new
        // size, is fetched afresh (its bytes are not a picture any more).
        fs::write(device_dir(&cache).join("phone/DCIM/photo.png"), b"no").unwrap();
        let changed = Entry {
            size_bytes: 2,
            ..entry.clone()
        };
        assert_eq!(service.phone(storage_id, "/DCIM", &changed).await, Ok(None));

        let undated = Entry {
            modified: None,
            ..entry
        };
        let before = fs::read_dir(&cache).unwrap().count();
        let _ = service.phone(storage_id, "/DCIM", &undated).await;
        assert_eq!(fs::read_dir(&cache).unwrap().count(), before, "not kept");
    }

    #[tokio::test]
    async fn a_file_of_the_same_name_in_another_folder_is_another_thumbnail() {
        let (_dir, device, storage_id, entry, cache) = kept().await;
        let service = Service::new(Arc::clone(&device)).with_disk_cache(cache.clone(), 1 << 20);

        let _ = service.phone(storage_id, "/DCIM", &entry).await.unwrap();
        let _ = service
            .phone(storage_id, "/DCIM/Trip", &entry)
            .await
            .unwrap();

        assert_eq!(fs::read_dir(&cache).unwrap().count(), 2);
    }

    fn device_dir(cache: &Path) -> PathBuf {
        cache.parent().unwrap().to_path_buf()
    }

    #[tokio::test]
    async fn listed_phone_entries_carry_distinct_object_ids() {
        let (_dir, _service, explorer, storage_id) = phone().await;

        let mut ids: Vec<u64> = explorer
            .list(storage_id, "/", false)
            .await
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect();
        ids.sort_unstable();
        ids.dedup();

        assert_eq!(ids.len(), 4);
        assert!(!ids.contains(&0));
    }

    #[tokio::test]
    async fn a_phone_picture_without_a_phone_thumbnail_is_shrunk_here() {
        let thumbnail = thumbnail_of("photo.png").await.unwrap().unwrap();

        assert_eq!(
            (thumbnail.width, thumbnail.height),
            (crate::dto::THUMBNAIL_SIZE, crate::dto::THUMBNAIL_SIZE)
        );
    }

    #[tokio::test]
    async fn a_phone_photo_shrunk_here_is_turned_the_way_it_was_taken() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("portrait.jpg"),
            mapper::jpeg_with_orientation(400, 300, 6),
        )
        .unwrap();
        let device = Arc::new(device::Service::new(virtual_source(dir.path())));
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let files = explorer::Service::new(Arc::clone(&device));
        let entries = files.list(storage_id, "/", false).await.unwrap();
        let entry = entries.iter().find(|e| e.name == "portrait.jpg").unwrap();

        let thumbnail = Service::new(device)
            .phone(storage_id, "/", entry)
            .await
            .unwrap()
            .unwrap();

        assert!(thumbnail.height > thumbnail.width, "upright: {thumbnail:?}");
    }

    #[tokio::test]
    async fn a_photos_start_is_read_in_one_request_and_only_for_a_jpeg_on_a_phone_that_can() {
        assert!(wants_head("IMG_0001.JPG", true));
        assert!(wants_head("a.jpeg", true));
        assert!(!wants_head("a.jpg", false), "no partial reads");
        assert!(!wants_head("clip.mp4", true));
        assert!(!wants_head("photo.heic", true));

        let dir = tempfile::tempdir().unwrap();
        let big: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        fs::write(dir.path().join("big.jpg"), &big).unwrap();
        let device = Arc::new(device::Service::new(virtual_source(dir.path())));
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let files = explorer::Service::new(Arc::clone(&device));
        let entries = files.list(storage_id, "/", false).await.unwrap();
        let entry = entries.iter().find(|e| e.name == "big.jpg").unwrap();
        let (session, _) = device.session_at().await.unwrap();
        let storage = session.storage(StorageId(storage_id)).await.unwrap();

        let head = read_head(&storage, ObjectHandle(entry.id))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(head.len(), mapper::HEAD_BYTES as usize);
        assert_eq!(head[..], big[..head.len()]);
        let missing = read_head(&storage, ObjectHandle(0xDEAD_BEEF))
            .await
            .unwrap();
        assert_eq!(missing, None, "no such file: nothing to turn by");
    }

    #[tokio::test]
    async fn phone_files_that_are_not_pictures_have_no_thumbnail() {
        assert_eq!(thumbnail_of("notes.txt").await, Ok(None));
        assert_eq!(thumbnail_of("broken.png").await, Ok(None));
    }

    #[tokio::test]
    async fn big_phone_pictures_are_not_fetched_for_a_thumbnail() {
        assert_eq!(thumbnail_of("huge.png").await, Ok(None));
    }

    #[tokio::test]
    async fn phone_thumbnails_need_a_connected_phone() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::new(Arc::new(device::Service::new(virtual_source(dir.path()))));

        let entry = Entry {
            id: 1,
            name: "a.jpg".into(),
            is_folder: false,
            size_bytes: 10,
            modified: None,
        };
        let result = service.phone(1, "/", &entry).await;

        assert_eq!(result, Err(Error::NotConnected));
    }

    // ---- Videos on a phone that gives no thumbnail of its own ----

    const MIB: u64 = 1 << 20;

    /// `fallback_for` as a table: what to try when the phone has no thumbnail.
    #[test]
    fn what_to_try_when_the_phone_has_no_thumbnail() {
        let cases: [(&str, u64, bool, Fallback); 14] = [
            ("a.mp4", 5 * MIB, true, Fallback::Video),
            ("A.MOV", 5 * MIB, true, Fallback::Video),
            ("a.m4v", 5 * MIB, true, Fallback::Video),
            ("a.3gp", 5 * MIB, true, Fallback::Video),
            ("a.mp4", 5 * MIB, false, Fallback::Nothing),
            ("a.mp4", 0, true, Fallback::Nothing),
            ("a.mp4", 2 << 40, true, Fallback::Nothing),
            ("a.mkv", 5 * MIB, true, Fallback::Nothing),
            ("a.webm", 5 * MIB, true, Fallback::Nothing),
            ("a.jpg", MIB, true, Fallback::Still),
            ("a.jpg", MIB, false, Fallback::Still),
            ("a.jpg", 9 * MIB, true, Fallback::Nothing),
            ("a.heic", MIB, true, Fallback::Nothing),
            ("notes.txt", 10, true, Fallback::Nothing),
        ];
        for (name, size, partial, expected) in cases {
            assert_eq!(
                fallback_for(name, size, partial),
                expected,
                "{name} {size} {partial}"
            );
        }
        assert_eq!(
            fallback_for("a.mp4", 1 << 40, true),
            Fallback::Video,
            "a terabyte is the limit"
        );
    }

    /// A phone holding `files`, its service, and the entries by name.
    struct VideoPhone {
        _dir: tempfile::TempDir,
        device: Arc<device::Service>,
        service: Service,
        storage_id: u64,
        entries: Vec<Entry>,
    }

    impl VideoPhone {
        async fn new(files: &[(&str, Vec<u8>)], partial_reads: bool) -> Self {
            Self::build(files, partial_reads, None, None).await
        }

        async fn build(
            files: &[(&str, Vec<u8>)],
            partial_reads: bool,
            cache: Option<&Path>,
            limit: Option<std::time::Duration>,
        ) -> Self {
            let dir = tempfile::tempdir().unwrap();
            for (name, bytes) in files {
                fs::write(dir.path().join(name), bytes).unwrap();
            }
            let mut phone = crate::service::testing::virtual_phone(dir.path());
            phone.partial_reads = partial_reads;
            let device = device::Service::new(device::Source::Virtual(phone));
            let device = Arc::new(match limit {
                Some(limit) => device.answering_within(limit),
                None => device,
            });
            device.connect().await.unwrap();
            let storage_id = device.storages().await.unwrap()[0].id;
            let entries = explorer::Service::new(Arc::clone(&device))
                .list(storage_id, "/", false)
                .await
                .unwrap();
            let mut service = Service::new(Arc::clone(&device));
            if let Some(cache) = cache {
                service = service.with_disk_cache(cache.to_path_buf(), 8 * MIB);
            }
            Self {
                _dir: dir,
                device,
                service,
                storage_id,
                entries,
            }
        }

        fn entry(&self, name: &str) -> Entry {
            self.entries
                .iter()
                .find(|e| e.name == name)
                .unwrap()
                .clone()
        }

        /// The thumbnail of `entry`, and how many requests the phone heard for it.
        async fn ask(&self, entry: &Entry) -> (Result<Option<Thumbnail>, Error>, usize) {
            let before = self.device.activity().times_heard();
            let found = self.service.phone(self.storage_id, "/", entry).await;
            (found, self.device.activity().times_heard() - before)
        }
    }

    fn red_top_left(thumb: &Thumbnail) -> bool {
        let at =
            ((thumb.height as usize / 4) * thumb.width as usize + thumb.width as usize / 4) * 4;
        thumb.rgba[at] > 200 && thumb.rgba[at + 1] < 90 && thumb.rgba[at + 2] < 90
    }

    fn video(name: &str) -> Vec<u8> {
        crate::service::thumbnail::fixtures::bytes(name)
    }

    /// The requests a file the phone gives nothing for takes (just the thumbnail ask).
    async fn baseline_asks() -> usize {
        let phone = VideoPhone::new(&[("clip.mkv", video("moov_at_end.mp4"))], true).await;
        let (found, asks) = phone.ask(&phone.entry("clip.mkv")).await;
        assert_eq!(found, Ok(None));
        asks
    }

    #[tokio::test]
    async fn a_phone_video_without_a_phone_thumbnail_gets_its_first_frame() {
        let big = crate::service::thumbnail::fixtures::padded_at_end(30 << 20);
        let phone = VideoPhone::new(
            &[
                ("a.mp4", big),
                ("b.mp4", video("faststart.mp4")),
                ("c.mp4", video("portrait.mp4")),
            ],
            true,
        )
        .await;

        let (a, asks) = phone.ask(&phone.entry("a.mp4")).await;
        let a = a.unwrap().expect("a frame");
        assert!(red_top_left(&a));
        assert!(
            asks <= 12 + baseline_asks().await,
            "{asks} requests for a 30 MiB video"
        );

        let b = phone
            .ask(&phone.entry("b.mp4"))
            .await
            .0
            .unwrap()
            .expect("a frame");
        assert!(red_top_left(&b));
        let c = phone
            .ask(&phone.entry("c.mp4"))
            .await
            .0
            .unwrap()
            .expect("a frame");
        assert!(c.height > c.width && red_top_left(&c), "upright");
    }

    #[tokio::test]
    async fn a_second_look_at_a_cached_video_asks_the_phone_nothing() {
        let cache = tempfile::tempdir().unwrap();
        let phone = VideoPhone::build(
            &[("a.mp4", video("moov_at_end.mp4"))],
            true,
            Some(cache.path()),
            None,
        )
        .await;
        let entry = phone.entry("a.mp4");

        let (first, asks) = phone.ask(&entry).await;
        assert!(asks > 0);
        let (second, asks) = phone.ask(&entry).await;

        assert_eq!(asks, 0);
        let (first, second) = (first.unwrap().unwrap(), second.unwrap().unwrap());
        assert_eq!((first.width, first.height), (second.width, second.height));
        assert!(red_top_left(&second), "the picture that was kept");
    }

    #[tokio::test]
    async fn a_phone_that_cannot_read_part_of_a_file_gets_no_video_frame_and_no_request() {
        let baseline = baseline_asks().await;
        let phone = VideoPhone::new(&[("a.mp4", video("moov_at_end.mp4"))], false).await;

        let (found, asks) = phone.ask(&phone.entry("a.mp4")).await;

        assert_eq!(found, Ok(None));
        assert_eq!(asks, baseline, "only the thumbnail ask");
    }

    #[tokio::test]
    async fn mkv_webm_and_non_video_files_are_as_before() {
        let baseline = baseline_asks().await;
        let phone = VideoPhone::new(
            &[
                ("a.mkv", video("moov_at_end.mp4")),
                ("a.webm", video("moov_at_end.mp4")),
                ("a.heic", video("moov_at_end.mp4")),
            ],
            true,
        )
        .await;
        for name in ["a.mkv", "a.webm", "a.heic"] {
            let (found, asks) = phone.ask(&phone.entry(name)).await;
            assert_eq!(found, Ok(None), "{name}");
            assert_eq!(asks, baseline, "{name}");
        }
    }

    #[tokio::test]
    async fn a_video_that_is_not_a_video_gets_none() {
        let phone = VideoPhone::new(
            &[
                ("noise.mp4", vec![9; 5000]),
                ("empty.mp4", Vec::new()),
                ("png.mov", mapper::png(40, 40)),
            ],
            true,
        )
        .await;
        for name in ["noise.mp4", "empty.mp4", "png.mov"] {
            assert_eq!(phone.ask(&phone.entry(name)).await.0, Ok(None), "{name}");
        }
    }

    #[tokio::test]
    async fn a_video_claiming_more_than_a_terabyte_is_refused_untried() {
        let baseline = baseline_asks().await;
        let phone = VideoPhone::new(&[("a.mp4", video("moov_at_end.mp4"))], true).await;
        let mut entry = phone.entry("a.mp4");
        entry.size_bytes = (1 << 40) + 1;

        let (found, asks) = phone.ask(&entry).await;

        assert_eq!(found, Ok(None));
        assert_eq!(asks, baseline);
    }

    #[tokio::test]
    async fn videos_asked_for_together_each_get_their_frame() {
        let phone = Arc::new(
            VideoPhone::new(
                &[
                    ("a.mp4", video("moov_at_end.mp4")),
                    ("b.mp4", video("faststart.mp4")),
                    ("c.mp4", video("portrait.mp4")),
                    ("d.mov", video("moov_at_end.mp4")),
                ],
                true,
            )
            .await,
        );
        let asks: Vec<_> = ["a.mp4", "b.mp4", "c.mp4", "d.mov"]
            .into_iter()
            .map(|name| {
                let phone = Arc::clone(&phone);
                tokio::spawn(async move { phone.ask(&phone.entry(name)).await.0 })
            })
            .collect();
        for ask in asks {
            assert!(ask.await.unwrap().unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn thumbnails_from_a_silent_phone_end_as_a_timeout_for_videos_too() {
        let limit = std::time::Duration::from_millis(150);
        let phone = VideoPhone::build(
            &[("a.mp4", video("moov_at_end.mp4"))],
            true,
            None,
            Some(limit),
        )
        .await;
        let entry = phone.entry("a.mp4");
        let _silence = crate::service::testing::silence(&phone.device, phone.storage_id).await;

        let found = phone.service.phone(phone.storage_id, "/", &entry).await;

        assert_eq!(found, Err(Error::Timeout));
    }

    #[tokio::test]
    async fn mac_pictures_get_thumbnails_and_other_files_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("photo.png"), mapper::png(100, 300)).unwrap();
        fs::write(dir.path().join("notes.txt"), b"hello").unwrap();
        let service = Service::new(Arc::new(device::Service::new(virtual_source(dir.path()))));
        let path = |name: &str| dir.path().join(name).to_string_lossy().into_owned();

        let photo = service.mac(&path("photo.png")).await.unwrap().unwrap();
        let notes = service.mac(&path("notes.txt")).await.unwrap();

        assert_eq!((photo.width, photo.height), (64, 192));
        assert_eq!(notes, None);
    }

    #[tokio::test]
    async fn a_mac_photo_is_turned_the_way_it_was_taken() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("portrait.jpg"),
            mapper::jpeg_with_orientation(400, 300, 8),
        )
        .unwrap();
        let service = Service::new(Arc::new(device::Service::new(virtual_source(dir.path()))));

        let thumbnail = service
            .mac(&dir.path().join("portrait.jpg").to_string_lossy())
            .await
            .unwrap()
            .unwrap();

        assert_eq!((thumbnail.width, thumbnail.height), (144, 192));
    }

    #[tokio::test]
    async fn a_missing_mac_picture_is_path_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::new(Arc::new(device::Service::new(virtual_source(dir.path()))));
        let path = dir.path().join("gone.jpg").to_string_lossy().into_owned();

        let result = service.mac(&path).await;

        assert_eq!(result, Err(Error::PathNotFound(path)));
    }

    #[tokio::test]
    async fn reading_stops_past_the_cap_whatever_size_was_claimed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("photo.png"), mapper::png(320, 320)).unwrap();
        let device = Arc::new(device::Service::new(virtual_source(dir.path())));
        device.connect().await.unwrap();
        let session = device.session().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let storage = session.storage(StorageId(storage_id)).await.unwrap();
        let explorer = explorer::Service::new(Arc::clone(&device));
        let id = explorer.list(storage_id, "/", false).await.unwrap()[0].id;
        let size = fs::metadata(dir.path().join("photo.png")).unwrap().len();

        let under = read_at_most(&storage, ObjectHandle(id), size)
            .await
            .unwrap();
        let over = read_at_most(&storage, ObjectHandle(id), size - 1)
            .await
            .unwrap();

        assert_eq!(under.map(|b| b.len() as u64), Some(size));
        assert_eq!(over, None);
    }

    #[tokio::test]
    async fn a_phone_that_cannot_read_part_of_a_file_gets_no_fetched_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("photo.png"), mapper::png(320, 320)).unwrap();
        let mut phone = crate::service::testing::virtual_phone(dir.path());
        phone.partial_reads = false;
        let device = Arc::new(device::Service::new(device::Source::Virtual(phone)));
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let entries = explorer::Service::new(Arc::clone(&device))
            .list(storage_id, "/", false)
            .await
            .unwrap();
        let service = Service::new(device);

        let thumbnail = service.phone(storage_id, "/", &entries[0]).await;

        assert_eq!(thumbnail, Ok(None));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn heic_photos_on_the_mac_get_quick_look_thumbnails() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("photo.png"), mapper::png(400, 300)).unwrap();
        let converted = std::process::Command::new("sips")
            .args(["-s", "format", "heic"])
            .arg(dir.path().join("photo.png"))
            .arg("--out")
            .arg(dir.path().join("photo.heic"))
            .output()
            .unwrap();
        assert!(converted.status.success(), "sips makes a HEIC");
        let service = Service::new(Arc::new(device::Service::new(virtual_source(dir.path()))));

        let thumbnail = service
            .mac(&dir.path().join("photo.heic").to_string_lossy())
            .await
            .unwrap();

        let thumbnail = thumbnail.expect("Quick Look previews HEIC");
        let most = crate::dto::THUMBNAIL_SIZE;
        assert!(thumbnail.width <= most && thumbnail.height <= most);
    }

    #[tokio::test]
    async fn quick_look_hanging_on_a_damaged_file_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake.mov");
        fs::write(&fake, b"not a movie").unwrap();
        let started = std::time::Instant::now();

        let thumbnail = quick_look(
            &fake.to_string_lossy(),
            "fake.mov",
            std::time::Duration::from_millis(500),
        )
        .await;

        assert_eq!(thumbnail, None);
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
    }

    #[tokio::test]
    async fn a_missing_file_has_no_quick_look_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::new(Arc::new(device::Service::new(virtual_source(dir.path()))));

        let path = dir.path().join("gone.heic").to_string_lossy().into_owned();
        let started = std::time::Instant::now();

        let thumbnail = service.mac(&path).await;

        assert_eq!(thumbnail, Err(Error::PathNotFound(path)));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn thumbnails_from_a_silent_phone_end_as_a_timeout_and_drop_the_session_once() {
        let limit = std::time::Duration::from_millis(150);
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("photo.png"), mapper::png(64, 64)).unwrap();
        let device =
            Arc::new(device::Service::new(virtual_source(dir.path())).answering_within(limit));
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = explorer::Service::new(Arc::clone(&device));
        let entry = explorer
            .list(storage_id, "/", false)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.name == "photo.png")
            .unwrap();
        let service = Service::new(Arc::clone(&device));
        let epoch = device.epoch();
        let _silence = crate::service::testing::silence(&device, storage_id).await;
        let started = std::time::Instant::now();

        // A grid asks for a batch at once.
        let asked = (0..8).map(|_| service.phone(storage_id, "/", &entry));
        let results = futures::future::join_all(asked).await;

        assert!(
            results.iter().all(|r| matches!(r, Err(Error::Timeout))),
            "{results:?}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
        assert_eq!(device.session().await.err(), Some(Error::NotConnected));
        assert_eq!(
            device.epoch(),
            epoch + 1,
            "eight timeouts drop the one session once"
        );
    }
}
