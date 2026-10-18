//! Free Up Space: one walk of a storage, and what it finds
//! that could go. Copies of the same file are found by size first (one pass,
//! no request), then by the first and last 64 KB of each (two small reads,
//! where the phone reads from an offset), and only then read in full and
//! hashed: a group is a group only when every byte agrees, so nothing is
//! called a copy on a guess. Photos already imported are those whose copy
//! is still where the import put it, at the same size and date (the record
//! alone also holds photos the user deleted on the Mac). Removing re-checks
//! each file with the phone, as a move does, and never touches a folder.

use std::collections::{BTreeMap, HashMap};

use mtp_rs::StorageId;
use mtp_rs::mtp::{ByteRange, ObjectHandle, ObjectInfo, Storage};
use sha2::{Digest, Sha256};

use super::import::{DCIM, record_key};
use super::mapper;
use super::service::{
    Failure, Origin, Removed, Service, WINDOW, check_within_size, is_same_file, phone_answers,
    size_limit,
};
use crate::dto::{
    Cancel, Direction, DuplicateGroup, Error, FreeUpRequest, Kept, Removal, SpaceFile,
    SpaceProgress, SpaceReport, TransferProgress, Why,
};
use crate::service::explorer;

/// How much of each end of a file is read before the whole of it.
pub(super) const ENDS: u64 = 64 * 1024;

/// What the last look at a storage found, for removing from it: each file by
/// its path, as the phone described it then.
pub(super) struct SpaceScan {
    storage_id: u64,
    epoch: u64,
    objects: HashMap<String, ObjectInfo>,
}

type Digested = [u8; 32];

impl Service {
    /// Walks the storage and says what could go: groups of identical files,
    /// and photos already imported to this Mac. The biggest files are the
    /// search's (74), not repeated here.
    pub async fn space_report(
        &self,
        storage_id: u64,
        cancel: &Cancel,
        progress: impl Fn(SpaceProgress) + Send + Sync,
    ) -> Result<SpaceReport, Error> {
        let epoch = self.device.epoch();
        let (storage, partial_reads, serial) = self.space_storage(storage_id).await?;

        // Hidden folders (thumbnail caches, trash) are not the user's files.
        let mut walk = self
            .explorer
            .walk_from(storage_id, "/", false, cancel.clone())
            .await?;
        let mut found: Vec<(String, ObjectInfo)> = Vec::new();
        while let Some(piece) = walk.next_raw().await {
            let (update, objects) = piece?;
            if update.restart {
                found.clear();
            }
            found.extend(
                objects
                    .into_iter()
                    .filter(|(_, object)| !object.is_folder()),
            );
            progress(SpaceProgress::Walking { files: found.len() });
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }

        let imported = self.already_imported(&serial, &found).await;
        let duplicates = match self
            .duplicates(&storage, partial_reads, &found, cancel, &progress)
            .await
        {
            Ok(groups) => groups,
            Err(failure) => return Err(self.to_error(failure).await),
        };
        let report = SpaceReport {
            storage_id,
            files: found.len(),
            duplicates,
            imported,
        };
        *self.space_scan.lock().await = Some(SpaceScan {
            storage_id,
            epoch,
            objects: found
                .into_iter()
                .map(|(folder, object)| (join(&folder, &object.filename), object))
                .collect(),
        });
        Ok(report)
    }

    /// Removes `request.paths` from the phone, each only if the last look
    /// found it and it is still that file (same name, size and date). A file
    /// that changed, or that the look didn't find, stays; a lost phone or a
    /// cancel stops between files.
    pub async fn free_up(
        &self,
        request: &FreeUpRequest,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<Removal, Error> {
        let (storage, _, _) = self.space_storage(request.storage_id).await?;
        let epoch = self.device.epoch();
        let mut scan = self.space_scan.lock().await;
        let objects = match scan.as_mut() {
            Some(scan) if scan.storage_id == request.storage_id && scan.epoch == epoch => {
                Some(&mut scan.objects)
            }
            _ => None,
        };
        let mut objects = objects;
        let mut removal = Removal::default();
        let mut state = TransferProgress {
            direction: Direction::Download,
            removing: true,
            files_total: request.paths.len(),
            ..TransferProgress::default()
        };
        progress(state.clone());
        let mut gone: BTreeMap<String, Vec<ObjectHandle>> = BTreeMap::new();
        for path in &request.paths {
            if cancel.is_cancelled() {
                removal.stopped = Some(Error::Cancelled);
                break;
            }
            let Some(object) = objects
                .as_deref()
                .and_then(|objects| objects.get(path))
                .cloned()
            else {
                removal.kept.push(Kept {
                    label: path.clone(),
                    why: Why::Changed,
                });
                continue;
            };
            let origin = Origin::file(
                object.handle,
                object.filename.clone(),
                object.size,
                object.modified,
            );
            match self.remove_one(&storage, &origin).await {
                Removed::Done => {
                    removal.removed += 1;
                    removal.freed += object.size;
                    gone.entry(parent(path)).or_default().push(object.handle);
                    if let Some(objects) = objects.as_deref_mut() {
                        objects.remove(path);
                    }
                    state.files_done += 1;
                    progress(state.clone());
                }
                Removed::Kept(why) => removal.kept.push(Kept {
                    label: path.clone(),
                    why,
                }),
                Removed::Stopped(failure) => {
                    removal.stopped = Some(self.to_error(failure).await);
                    break;
                }
            }
        }
        drop(scan);
        self.explorer
            .note_removed(request.storage_id, &gone, &[])
            .await;
        Ok(removal)
    }

    /// The storage on the session open now, whether the phone reads from an
    /// offset, and its serial (what the import record is kept under).
    async fn space_storage(&self, storage_id: u64) -> Result<(Storage, bool, String), Error> {
        let (session, epoch) = self.device.session_at().await?;
        let storage = match self
            .limits
            .ask(session.storage(StorageId(storage_id)))
            .await
        {
            Ok(storage) => storage,
            Err(err) => return Err(self.device.handle_error_at(epoch, err).await),
        };
        Ok((
            storage,
            session.capabilities().supports_partial_download,
            session.device_info().serial_number.clone(),
        ))
    }

    /// The DCIM photos and videos whose imported copy is still on this Mac
    /// where the import put it, with the same size and date.
    async fn already_imported(
        &self,
        serial: &str,
        found: &[(String, ObjectInfo)],
    ) -> Vec<SpaceFile> {
        let Some(record) = self.imports.as_ref().filter(|_| !serial.is_empty()) else {
            return Vec::new();
        };
        let targets = record.targets(serial).await;
        if targets.is_empty() {
            return Vec::new();
        }
        let mut imported = Vec::new();
        for (folder, object) in found {
            if !in_dcim(folder) {
                continue;
            }
            let Some(target) = targets.get(&record_key(folder, object)) else {
                continue;
            };
            let Some(when) = object.modified.and_then(mapper::to_system_time) else {
                continue;
            };
            let still_there = tokio::fs::symlink_metadata(target)
                .await
                .is_ok_and(|there| is_same_file(&there, object.size, when));
            if still_there {
                imported.push(space_file(folder, object));
            }
        }
        imported.sort_by(|a, b| a.path.cmp(&b.path));
        imported
    }

    /// Groups of files whose every byte agrees: by size, then by their ends,
    /// then by the whole of them.
    async fn duplicates(
        &self,
        storage: &Storage,
        partial_reads: bool,
        found: &[(String, ObjectInfo)],
        cancel: &Cancel,
        progress: &(impl Fn(SpaceProgress) + Send + Sync),
    ) -> Result<Vec<DuplicateGroup>, Failure> {
        let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
        for (index, (_, object)) in found.iter().enumerate() {
            // Nothing to free in an empty file, and no size to go by in
            // MTP's "4 GB or more".
            if object.size > 0 && object.size != u64::from(u32::MAX) {
                by_size.entry(object.size).or_default().push(index);
            }
        }
        let mut candidates: Vec<Vec<usize>> = by_size
            .into_values()
            .filter(|group| group.len() > 1)
            .collect();

        if partial_reads {
            let mut kept = Vec::new();
            for group in candidates {
                let mut by_ends: HashMap<Digested, Vec<usize>> = HashMap::new();
                for index in group {
                    if cancel.is_cancelled() {
                        return Err(Failure::Cancelled);
                    }
                    if let Some(digest) = self.ends(storage, &found[index].1).await? {
                        by_ends.entry(digest).or_default().push(index);
                    }
                }
                kept.extend(by_ends.into_values().filter(|group| group.len() > 1));
            }
            candidates = kept;
        }

        let total: u64 = candidates
            .iter()
            .flatten()
            .map(|&index| found[index].1.size)
            .sum();
        let mut done = 0;
        progress(SpaceProgress::Comparing { done, total });
        let mut groups = Vec::new();
        for group in candidates {
            let mut by_whole: HashMap<Digested, Vec<usize>> = HashMap::new();
            for index in group {
                let object = &found[index].1;
                let digest = self
                    .whole(storage, partial_reads, object, cancel, &mut |read| {
                        done += read;
                        progress(SpaceProgress::Comparing { done, total });
                    })
                    .await?;
                if let Some(digest) = digest {
                    by_whole.entry(digest).or_default().push(index);
                }
            }
            for same in by_whole.into_values().filter(|group| group.len() > 1) {
                let mut files: Vec<(Option<std::time::SystemTime>, SpaceFile)> = same
                    .into_iter()
                    .map(|index| {
                        let (folder, object) = &found[index];
                        (
                            object.modified.and_then(mapper::to_system_time),
                            space_file(folder, object),
                        )
                    })
                    .collect();
                files.sort_by(|(a_when, a), (b_when, b)| original_first(*a_when, a, *b_when, b));
                groups.push(DuplicateGroup {
                    size: files[0].1.size,
                    files: files.into_iter().map(|(_, file)| file).collect(),
                });
            }
        }
        // Most to gain first.
        groups.sort_by(|a, b| {
            let gain = |group: &DuplicateGroup| group.size * (group.files.len() as u64 - 1);
            gain(b)
                .cmp(&gain(a))
                .then_with(|| a.files[0].path.cmp(&b.files[0].path))
        });
        Ok(groups)
    }

    /// The first and last `ENDS` bytes of `object`, hashed; `None` when the
    /// phone won't read it (it then can't be told a copy).
    async fn ends(
        &self,
        storage: &Storage,
        object: &ObjectInfo,
    ) -> Result<Option<Digested>, Failure> {
        let activity = self.device.activity();
        let mut hasher = Sha256::new();
        let mut ranges = vec![(0, ENDS.min(object.size))];
        if object.size > ENDS {
            let from = object.size.saturating_sub(ENDS).max(ENDS);
            ranges.push((from, object.size - from));
        }
        for (offset, len) in ranges {
            if len == 0 {
                continue;
            }
            let read = phone_answers(
                &activity,
                self.stall_limit,
                storage.read_range(object.handle, offset, len as u32),
            )
            .await?;
            match read {
                Ok(bytes) => {
                    activity.heard();
                    hasher.update(&bytes);
                }
                Err(err) if crate::service::failures::is_about_one_object(&err) => return Ok(None),
                Err(err) => return Err(Failure::Device(err)),
            }
        }
        Ok(Some(hasher.finalize().into()))
    }

    /// The whole of `object`, hashed as it arrives (nothing is written);
    /// `None` when the phone won't read it. Stops between windows on a
    /// cancel; a phone that sends whole files only is told to stop.
    async fn whole(
        &self,
        storage: &Storage,
        partial_reads: bool,
        object: &ObjectInfo,
        cancel: &Cancel,
        read: &mut (impl FnMut(u64) + Send),
    ) -> Result<Option<Digested>, Failure> {
        let activity = self.device.activity();
        let limit = size_limit(object.size);
        let mut hasher = Sha256::new();
        let mut received = 0u64;
        let refused = |err: mtp_rs::Error| {
            if crate::service::failures::is_about_one_object(&err) {
                Ok(None)
            } else {
                Err(Failure::Device(err))
            }
        };
        if partial_reads {
            let mut download = match phone_answers(
                &activity,
                self.stall_limit,
                storage.download_windowed(object.handle, ByteRange::Full, WINDOW),
            )
            .await?
            {
                Ok(download) => download,
                Err(err) => return refused(err),
            };
            while let Some(window) =
                phone_answers(&activity, self.stall_limit, download.next_window()).await?
            {
                let window = match window {
                    Ok(window) => window,
                    Err(err) => return refused(err),
                };
                check_within_size(received, window.len(), limit)?;
                received += window.len() as u64;
                hasher.update(&window);
                activity.heard();
                read(window.len() as u64);
                if cancel.is_cancelled() {
                    return Err(Failure::Cancelled);
                }
            }
        } else {
            let mut download = match phone_answers(
                &activity,
                self.stall_limit,
                storage.download(object.handle, ByteRange::Full),
            )
            .await?
            {
                Ok(download) => download,
                Err(err) => return refused(err),
            };
            while let Some(chunk) =
                phone_answers(&activity, self.stall_limit, download.next_chunk()).await?
            {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(err) => return refused(err),
                };
                check_within_size(received, chunk.len(), limit)?;
                received += chunk.len() as u64;
                hasher.update(&chunk);
                activity.heard();
                read(chunk.len() as u64);
                if cancel.is_cancelled() {
                    let _ = download.cancel(mtp_rs::mtp::DEFAULT_CANCEL_TIMEOUT).await;
                    return Err(Failure::Cancelled);
                }
            }
        }
        // A file that came short of its size isn't known whole.
        if received != object.size {
            return Ok(None);
        }
        Ok(Some(hasher.finalize().into()))
    }
}

/// The one to keep first: the oldest, then the camera's own (under
/// `DCIM/Camera`), then by path; a file without a date comes after one with.
fn original_first(
    a_when: Option<std::time::SystemTime>,
    a: &SpaceFile,
    b_when: Option<std::time::SystemTime>,
    b: &SpaceFile,
) -> std::cmp::Ordering {
    let camera = |file: &SpaceFile| !file.path.to_lowercase().starts_with("/dcim/camera/");
    (a_when.is_none(), a_when, camera(a), &a.path).cmp(&(
        b_when.is_none(),
        b_when,
        camera(b),
        &b.path,
    ))
}

fn in_dcim(folder: &str) -> bool {
    let lower = folder.to_lowercase();
    let dcim = DCIM.to_lowercase();
    lower == dcim || lower.starts_with(&format!("{dcim}/"))
}

fn space_file(folder: &str, object: &ObjectInfo) -> SpaceFile {
    SpaceFile {
        path: join(folder, &object.filename),
        size: object.size,
        modified: explorer::shown_date(object),
    }
}

fn join(folder: &str, name: &str) -> String {
    if folder.ends_with('/') {
        format!("{folder}{name}")
    } else {
        format!("{folder}/{name}")
    }
}

/// The folder `path` is in, as the explorer's memory names it.
fn parent(path: &str) -> String {
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/".to_owned(),
        Some((folder, _)) => folder.to_owned(),
    }
}
