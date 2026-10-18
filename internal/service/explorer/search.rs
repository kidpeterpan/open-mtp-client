//! A walk of the whole phone, for searching it.
//!
//! A phone describes its files one request each and has no way to list a whole
//! storage at once, so a search walks every folder. The walk is done once and
//! what it sees goes to the caller as a catalogue, piece by piece; looking for
//! a name or a kind in that catalogue happens in memory, without the phone.
//!
//! Every folder read in full is kept in the search index (`Remembering`), so
//! walking again costs one request per folder (how many items it holds) rather
//! than one per file: a folder still holding as many items, on the same
//! connection, is taken from the index, as a pane's recent folders are.
//!
//! The phone is untrusted: a folder whose name is not one plain name is left
//! out with everything in it (it could not be downloaded by path), a folder met
//! twice is not walked again, and depth and size are bounded. Hidden folders
//! (`.thumbnails`, `.trashed-…`), which can hold most of a phone's files, are
//! walked only when hidden files are shown.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mtp_rs::StorageId;
use mtp_rs::mtp::{ObjectHandle, ObjectInfo, Storage};

use super::bounded::read_all;
use super::mapper;
use super::service::{Failure, Service, join, normalize};
use crate::dto::{Cancel, Error, Found, WalkUpdate};
use crate::service::{clock, names};

/// A piece goes out after this many items or this long, whichever is first.
const BATCH_ITEMS: usize = 500;
const BATCH_TIME: Duration = Duration::from_millis(120);
/// Bounds on what a phone can make a walk do.
const MAX_DEPTH: usize = 64;
pub(super) const MAX_ITEMS: usize = 500_000;

/// Whether a walk takes an item in (and, for a folder, goes into it): its name
/// is one plain name, and it is not hidden unless hidden files are shown.
fn worth_walking(name: &str, include_hidden: bool) -> bool {
    names::is_safe_component(name) && (include_hidden || !name.starts_with('.'))
}

/// Whether a walk goes into a folder met at `depth`: not too deep, and not
/// met before (a phone can report a folder inside itself).
fn goes_into(visited: &mut HashSet<u64>, handle: u64, depth: usize) -> bool {
    depth < MAX_DEPTH && visited.insert(handle)
}

/// A walk of one storage, read a piece at a time with [`Walk::next`]. Between
/// pieces nothing is in flight, so dropping it or cancelling it never leaves
/// the phone in the middle of a request.
pub struct Walk {
    service: Arc<Service>,
    storage: Storage,
    serial: String,
    storage_id: u64,
    epoch: u64,
    /// The folder the walk started at ("/" for the whole storage).
    root: String,
    include_hidden: bool,
    cancel: Cancel,
    /// The handle of the folder the walk started at (`None` is the root).
    start: Option<ObjectHandle>,
    /// Folders still to read: path, handle (`None` is the root), depth.
    waiting: Vec<(String, Option<ObjectHandle>, usize)>,
    visited: HashSet<u64>,
    folders_read: usize,
    items: usize,
    /// Android re-keys files when its media scanner runs; the walk starts over
    /// once.
    restarted: bool,
    /// Take a folder from the index when the phone still reports as many items
    /// (off: every folder is read for real, and the index refreshed).
    trust_index: bool,
    finished: bool,
}

impl Service {
    /// Starts a walk of the whole storage `storage_id` (see the module docs).
    pub async fn walk(
        self: &Arc<Self>,
        storage_id: u64,
        include_hidden: bool,
        cancel: Cancel,
    ) -> Result<Walk, Error> {
        self.walk_from(storage_id, "/", include_hidden, cancel)
            .await
    }

    /// Starts a walk of the folder `path` and everything below it. A folder
    /// that is not there is `PathNotFound`.
    pub async fn walk_from(
        self: &Arc<Self>,
        storage_id: u64,
        path: &str,
        include_hidden: bool,
        cancel: Cancel,
    ) -> Result<Walk, Error> {
        let root = normalize(path);
        let (session, epoch) = self.device.session_at().await?;
        let storage = match self
            .limits
            .ask(session.storage(StorageId(storage_id)))
            .await
        {
            Ok(storage) => storage,
            Err(err) => return Err(self.device.handle_error_at(epoch, err).await),
        };
        let serial = session.device_info().serial_number.clone();
        let start = match self.resolve(&storage, &serial, storage_id, &root).await {
            Ok(handle) => handle,
            Err(Failure::PathNotFound) => return Err(Error::PathNotFound(root)),
            Err(Failure::Client(err)) => return Err(self.device.handle_error_at(epoch, err).await),
        };
        Ok(Walk {
            service: Arc::clone(self),
            storage,
            serial,
            storage_id,
            epoch,
            include_hidden,
            cancel,
            waiting: vec![(root.clone(), start, 0)],
            start,
            root,
            visited: HashSet::new(),
            folders_read: 0,
            items: 0,
            restarted: false,
            trust_index: true,
            finished: false,
        })
    }
}

impl Walk {
    /// The same walk, reading every folder for real: a file changed in place
    /// leaves its folder's item count as it was, so the index would hide the
    /// change. What is read refreshes the index.
    pub(crate) fn fresh(mut self) -> Self {
        self.trust_index = false;
        self
    }

    /// The handle of the folder the walk started at (`None` for the storage's
    /// root), as found again after a restart.
    pub(crate) fn start(&self) -> Option<ObjectHandle> {
        self.start
    }

    /// The next piece, or `None` once the walk is over (it ended, failed, or
    /// was cancelled). The last piece has `done` set.
    pub async fn next(&mut self) -> Option<Result<WalkUpdate, Error>> {
        Some(self.next_raw().await?.map(|(mut update, seen)| {
            update.found = seen
                .into_iter()
                .map(|(folder, object)| Found {
                    folder,
                    entry: mapper::to_entry(&object, clock::local_offset_minutes),
                })
                .collect();
            update
        }))
    }

    /// `next`, with what was seen as the phone described it (its handle and
    /// its own date), each with the folder it is in; the update's `found` is
    /// left empty.
    pub(crate) async fn next_raw(
        &mut self,
    ) -> Option<Result<(WalkUpdate, Vec<(String, ObjectInfo)>), Error>> {
        if self.finished {
            return None;
        }
        let started = Instant::now();
        let mut update = WalkUpdate::default();
        let mut seen: Vec<(String, ObjectInfo)> = Vec::new();
        while let Some((path, handle, depth)) = self.waiting.pop() {
            if self.cancel.is_cancelled() {
                self.finished = true;
                return None;
            }
            let children = match self.read(&path, handle).await {
                Ok(children) => children,
                Err(mtp_rs::Error::StaleHandle | mtp_rs::Error::NotFound) if !self.restarted => {
                    // The phone re-keyed its files: everything seen so far may
                    // be addressed wrongly. Start over, once.
                    self.restarted = true;
                    self.service.forget_folders().await;
                    // What was kept of this storage holds the old handles.
                    self.service.listings.lock().await.forget_below(
                        &self.serial,
                        self.storage_id,
                        "/",
                    );
                    // Where the walk started is found again: its handle too
                    // may have changed.
                    let start = match self
                        .service
                        .resolve(&self.storage, &self.serial, self.storage_id, &self.root)
                        .await
                    {
                        Ok(handle) => handle,
                        Err(failure) => {
                            self.finished = true;
                            return Some(Err(match failure {
                                Failure::PathNotFound => Error::PathNotFound(self.root.clone()),
                                Failure::Client(err) => {
                                    self.service.handle_error_at(self.epoch, err).await
                                }
                            }));
                        }
                    };
                    self.waiting = vec![(self.root.clone(), start, 0)];
                    self.start = start;
                    seen.clear();
                    self.visited.clear();
                    self.folders_read = 0;
                    self.items = 0;
                    update = WalkUpdate {
                        restart: true,
                        ..WalkUpdate::default()
                    };
                    continue;
                }
                Err(err) => {
                    self.finished = true;
                    return Some(Err(self.service.handle_error_at(self.epoch, err).await));
                }
            };
            self.folders_read += 1;
            for child in children {
                let name = child.filename.clone();
                if !worth_walking(&name, self.include_hidden) {
                    continue;
                }
                if child.is_folder() && goes_into(&mut self.visited, child.handle.0, depth) {
                    self.waiting
                        .push((join(&path, &name), Some(child.handle), depth + 1));
                }
                self.items += 1;
                seen.push((path.clone(), child));
            }
            if self.items >= MAX_ITEMS {
                self.waiting.clear();
                update.truncated = true;
                break;
            }
            if seen.len() >= BATCH_ITEMS || started.elapsed() >= BATCH_TIME {
                break;
            }
        }
        update.folders_read = self.folders_read;
        update.folders_left = self.waiting.len();
        update.items = self.items;
        if self.waiting.is_empty() {
            update.done = true;
            self.finished = true;
        }
        Some(Ok((update, seen)))
    }

    /// What is in one folder: from the index if the phone still reports as
    /// many items there on this connection, else read in full and indexed.
    async fn read(
        &self,
        path: &str,
        handle: Option<ObjectHandle>,
    ) -> Result<Vec<ObjectInfo>, mtp_rs::Error> {
        let limits = &self.service.limits;
        let listing = limits.ask(self.storage.list_objects_stream(handle)).await?;
        let total = listing.total();
        // A folder the phone re-keyed lists as empty rather than failing: an
        // empty folder is asked whether it is still that folder.
        if total == 0
            && let Some(handle) = handle
            && !limits
                .ask(self.storage.get_object_info(handle))
                .await?
                .is_folder()
        {
            return Err(mtp_rs::Error::StaleHandle);
        }
        let key = (self.serial.clone(), self.storage_id, path.to_owned());
        let kept = match self.trust_index {
            true => self
                .service
                .listings
                .lock()
                .await
                .recall(&key, self.epoch, total)
                .cloned(),
            false => None,
        };
        let children = match kept {
            Some(children) => children,
            None => {
                let children = read_all(listing, limits).await?;
                #[cfg(test)]
                self.service.count_full_read();
                self.service
                    .listings
                    .lock()
                    .await
                    .index(key, self.epoch, total, children.clone());
                children
            }
        };
        self.service
            .remember(&self.serial, self.storage_id, path, &children)
            .await;
        Ok(children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_gone_into_once_and_not_too_deep() {
        let mut visited = HashSet::new();

        assert!(goes_into(&mut visited, 7, 0));
        assert!(!goes_into(&mut visited, 7, 3), "met again, below itself");
        assert!(goes_into(&mut visited, 8, MAX_DEPTH - 1));
        assert!(!goes_into(&mut visited, 9, MAX_DEPTH));
    }

    #[test]
    fn only_plain_names_are_walked_and_hidden_ones_only_when_shown() {
        // (A phone can report any name; the virtual one can only make the
        // names its Mac folder can hold.)
        let nul = format!("bad{}name", '\0');
        for (name, shown, left_out) in [
            ("DCIM", true, true),
            ("Beach 2026", true, true),
            (".thumbnails", true, false),
            ("..", false, false),
            (".", false, false),
            ("a/b", false, false),
            ("../escape", false, false),
            ("", false, false),
            (nul.as_str(), false, false),
        ] {
            assert_eq!(worth_walking(name, true), shown, "{name:?}, hidden shown");
            assert_eq!(
                worth_walking(name, false),
                left_out,
                "{name:?}, hidden left out"
            );
        }
    }
}
