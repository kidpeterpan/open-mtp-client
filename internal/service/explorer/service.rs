use std::collections::HashMap;
use std::sync::Arc;

use mtp_rs::StorageId;
use mtp_rs::mtp::{MtpDevice, ObjectHandle, ObjectInfo, ObjectListing, Storage};
use tokio::sync::Mutex;

use super::bounded::{Limits, read_all, read_children};
use super::listing::Listing;
use super::mapper;
use super::memory::{FolderKey, Remembering};
use super::natural::natural_cmp;
use crate::dto::{Cancel, Entry, Error, Sort, SortKey};
use crate::service::{clock, device, names};

/// Browses device folders by path. MTP itself addresses
/// objects by handle, so resolved folder handles are cached per path.
pub struct Service {
    pub(super) device: Arc<device::Service>,
    pub(super) folders: Mutex<HashMap<FolderKey, ObjectHandle>>,
    /// The last full reading of recently used folders: see `memory.rs`.
    pub(super) listings: Mutex<Remembering>,
    /// How long the phone gets to answer each request: see `bounded.rs`.
    pub(super) limits: Limits,
    /// How many times a folder has been read in full (every item asked about),
    /// which is what remembering exists to avoid; tests count it.
    #[cfg(test)]
    full_reads: std::sync::atomic::AtomicUsize,
}

/// An open device folder: what it is, where it lives, and what is in it.
pub(crate) struct Folder {
    pub(crate) session: MtpDevice,
    pub(crate) storage: Storage,
    pub(crate) handle: Option<ObjectHandle>,
    pub(crate) children: Vec<ObjectInfo>,
    /// Which folder this is, for keeping what is remembered of it right.
    pub(crate) key: FolderKey,
    /// `children` is what was read earlier, not read just now. Anyone about to
    /// act on a child checks it with the phone first (see
    /// `Service::open_folder_confirmed`).
    pub(crate) from_memory: bool,
    /// How long the phone gets to answer a request about this folder.
    pub(crate) limits: Limits,
    /// The session it was opened on: see `device::Service::handle_error_at`.
    pub(crate) epoch: u64,
    /// Where each child is by name, built the first time a name is asked for.
    index: std::sync::OnceLock<names::Index>,
}

impl Folder {
    pub(crate) fn ensure_writable(&self) -> Result<(), Error> {
        if self.storage.info().is_writable {
            Ok(())
        } else {
            Err(Error::ReadOnly)
        }
    }

    /// A child whose name matches ignoring case, as the device would see it.
    pub(crate) fn child_named(&self, name: &str) -> Option<&ObjectInfo> {
        let index = self.index.get_or_init(|| {
            names::Index::of(self.children.iter().map(|child| child.filename.as_str()))
        });
        index.find(name).map(|position| &self.children[position])
    }

    /// The child with exactly this name; the user picked it from a listing.
    pub(crate) fn exact_child(&self, parent: &str, name: &str) -> Result<ObjectInfo, Error> {
        self.children
            .iter()
            .find(|child| child.filename == name)
            .cloned()
            .ok_or_else(|| Error::PathNotFound(join(&normalize(parent), name)))
    }
}

pub(super) enum Failure {
    Client(mtp_rs::Error),
    PathNotFound,
}

impl Service {
    pub fn new(device: Arc<device::Service>) -> Self {
        Self {
            limits: Limits::tied_to(device.activity()),
            device,
            folders: Mutex::new(HashMap::new()),
            listings: Mutex::new(Remembering::default()),
            #[cfg(test)]
            full_reads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The same service giving a phone `ask` and `long` to answer in, so a test
    /// of a silent one doesn't wait a minute.
    #[cfg(test)]
    pub(crate) fn with_limits(
        mut self,
        ask: std::time::Duration,
        long: std::time::Duration,
    ) -> Self {
        self.limits = Limits::tied(self.device.activity(), ask, long);
        self
    }

    /// Lists one folder in the order the device reports it; see [`sort`].
    /// `path` is absolute; "/" is the root of the storage.
    /// Which of `paths` are folders on storage `storage_id`. Each is walked
    /// from the top as a folder is opened, one listing per folder on the way
    /// (most are small, and remembered for the next), never a whole
    /// storage. A path the phone re-keyed is looked up again once.
    pub async fn folders_present(
        &self,
        storage_id: u64,
        paths: &[String],
    ) -> Result<Vec<String>, Error> {
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
        let mut present = Vec::new();
        for path in paths {
            let path = normalize(path);
            let mut found = self.resolve(&storage, &serial, storage_id, &path).await;
            if let Err(Failure::Client(mtp_rs::Error::StaleHandle | mtp_rs::Error::NotFound)) =
                found
            {
                self.folders.lock().await.clear();
                found = self.resolve(&storage, &serial, storage_id, &path).await;
            }
            match found {
                Ok(_) => present.push(path),
                Err(Failure::PathNotFound) => {}
                Err(Failure::Client(err)) => {
                    return Err(self.device.handle_error_at(epoch, err).await);
                }
            }
        }
        Ok(present)
    }

    /// The folders remembered in `path`, from what this connection has read:
    /// no request to the phone, so it can be asked as a path is typed. Empty
    /// for a folder not read lately (or no phone).
    pub async fn known_folders(&self, storage_id: u64, path: &str) -> Vec<String> {
        let Ok((session, epoch)) = self.device.session_at().await else {
            return Vec::new();
        };
        let key = (
            session.device_info().serial_number.clone(),
            storage_id,
            normalize(path),
        );
        self.listings
            .lock()
            .await
            .peek(&key, epoch)
            .map(|children| {
                children
                    .iter()
                    .filter(|child| child.is_folder())
                    .map(|child| child.filename.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn list(
        &self,
        storage_id: u64,
        path: &str,
        show_hidden: bool,
    ) -> Result<Vec<Entry>, Error> {
        let folder = self.open_folder(storage_id, path).await?;
        Ok(folder
            .children
            .iter()
            .map(|object| mapper::to_entry(object, clock::local_offset_minutes))
            .filter(|entry| show_hidden || !entry.is_hidden())
            .collect())
    }

    /// Starts reading one folder piece by piece, so a camera folder with
    /// thousands of photos shows its first files at once instead of after the
    /// phone has described the last one. `cancel` stops it between files.
    ///
    /// With `allow_remembered`, a folder the phone still reports as holding
    /// as many items as when it was last read is handed over from memory,
    /// without asking the phone about each file again. Pass false when the
    /// user asked to see the phone as it is now.
    /// Reads the folder `path` from the phone, never from memory, and times
    /// it: Troubleshooting's "how quick is this phone" check. Hidden items
    /// count; every request is bounded as for a listing; `cancel` stops it
    /// between items.
    pub async fn time_reading(
        self: &Arc<Self>,
        storage_id: u64,
        path: &str,
        cancel: Cancel,
    ) -> Result<crate::dto::ReadTiming, Error> {
        let started = std::time::Instant::now();
        let mut listing = self
            .open_listing(storage_id, path, true, cancel, false)
            .await?;
        let mut items = 0;
        while let Some(piece) = listing.next().await {
            let piece = piece?;
            items = arrived(items, &piece);
            if piece.done {
                break;
            }
        }
        Ok(crate::dto::ReadTiming {
            items,
            took: started.elapsed(),
        })
    }

    pub async fn open_listing(
        self: &Arc<Self>,
        storage_id: u64,
        path: &str,
        show_hidden: bool,
        cancel: Cancel,
        allow_remembered: bool,
    ) -> Result<Listing, Error> {
        let path = normalize(path);
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

        let mut started = self
            .start_listing(&storage, &serial, storage_id, &path)
            .await;
        // The same recovery as `open_folder`: re-resolve from the root once.
        if let Err(Failure::Client(mtp_rs::Error::StaleHandle | mtp_rs::Error::NotFound)) = started
        {
            self.forget_folders().await;
            started = self
                .start_listing(&storage, &serial, storage_id, &path)
                .await;
        }
        match started {
            Ok((_folder, objects)) => {
                let key = (serial.clone(), storage_id, path.clone());
                let remembered = if allow_remembered {
                    self.listings
                        .lock()
                        .await
                        .recall(&key, epoch, objects.total())
                        .cloned()
                } else {
                    None
                };
                if let Some(children) = &remembered {
                    self.remember(&serial, storage_id, &path, children).await;
                }
                Ok(Listing::new(
                    Arc::clone(self),
                    storage,
                    serial,
                    storage_id,
                    path,
                    show_hidden,
                    cancel,
                    objects,
                    epoch,
                    remembered,
                ))
            }
            Err(Failure::PathNotFound) => Err(Error::PathNotFound(path)),
            Err(Failure::Client(err)) => Err(self.device.handle_error_at(epoch, err).await),
        }
    }

    /// Creates `name` inside `parent` and returns the name it was given.
    pub async fn make_folder(
        &self,
        storage_id: u64,
        parent: &str,
        name: &str,
    ) -> Result<String, Error> {
        let name = names::validate(name)?;
        let folder = self.open_folder(storage_id, parent).await?;
        folder.ensure_writable()?;
        if folder.child_named(name).is_some() {
            return Err(Error::NameTaken(name.to_owned()));
        }

        match folder
            .limits
            .ask(folder.storage.create_folder(folder.handle, name))
            .await
        {
            Ok(made) => {
                // The folder is in the listing now; remember it so the
                // refresh that follows needn't read the whole folder again.
                match folder
                    .limits
                    .ask(folder.storage.get_object_info(made))
                    .await
                {
                    Ok(info) => {
                        self.amend(&folder.key, |kept| kept.added(info.clone()))
                            .await
                    }
                    Err(_) => self.forget_listing(&folder.key).await,
                }
                Ok(name.to_owned())
            }
            Err(err) => Err(self.device.handle_error(err).await),
        }
    }

    /// Renames `old` inside `parent` and returns the new name. A change of
    /// case alone is allowed even though names otherwise ignore case.
    pub async fn rename(
        &self,
        storage_id: u64,
        parent: &str,
        old: &str,
        new: &str,
    ) -> Result<String, Error> {
        let new = names::validate(new)?;
        if new == old {
            return Ok(new.to_owned());
        }
        let folder = self
            .open_folder_confirmed(storage_id, parent, &[old.to_owned()], true)
            .await?;
        folder.ensure_writable()?;
        if !folder.session.supports_rename() {
            return Err(Error::Unsupported);
        }
        let target = folder.exact_child(parent, old)?;
        if folder
            .child_named(new)
            .is_some_and(|other| other.handle != target.handle)
        {
            return Err(Error::NameTaken(new.to_owned()));
        }

        // A folder's rename has the phone rewrite everything inside it.
        if let Err(err) = folder
            .limits
            .scaled(
                target.is_folder(),
                folder.storage.rename(target.handle, new),
            )
            .await
        {
            return Err(self.device.handle_error(err).await);
        }
        self.amend(&folder.key, |kept| kept.renamed(target.handle, new))
            .await;
        if target.is_folder() {
            // Every cached path below the old name is now wrong.
            self.folders.lock().await.clear();
            self.forget_below(&folder.key, &join(&normalize(parent), old))
                .await;
        }
        Ok(new.to_owned())
    }

    /// Deletes `names` inside `parent`, folders with everything in them.
    /// Every name is found before anything is deleted, so a typo deletes
    /// nothing.
    pub async fn delete(
        &self,
        storage_id: u64,
        parent: &str,
        names: &[String],
    ) -> Result<(), Error> {
        let folder = self
            .open_folder_confirmed(storage_id, parent, names, true)
            .await?;
        folder.ensure_writable()?;
        let targets = names
            .iter()
            .map(|name| folder.exact_child(parent, name))
            .collect::<Result<Vec<_>, _>>()?;

        let mut deleted = Vec::new();
        let mut failure = None;
        for target in &targets {
            match delete_object(
                &folder.storage,
                &folder.limits,
                target.handle,
                target.is_folder(),
            )
            .await
            {
                Ok(()) => deleted.push(target.handle),
                Err(err) => {
                    failure = Some(err);
                    break;
                }
            }
        }
        // What was deleted is gone from the remembered folder, even when a
        // later one failed.
        self.amend(&folder.key, |kept| kept.removed(&deleted)).await;
        for target in targets.iter().filter(|target| target.is_folder()) {
            self.forget_below(&folder.key, &join(&normalize(parent), &target.filename))
                .await;
        }
        if targets.iter().any(|target| target.is_folder()) {
            self.folders.lock().await.clear();
        }
        match failure {
            Some(err) => Err(self.device.handle_error(err).await),
            None => Ok(()),
        }
    }

    /// Resolves `path` and reads its children, with the stale-handle retry.
    /// A folder the phone still reports as holding as many items as when it
    /// was last read comes from memory instead of one request per file.
    pub(crate) async fn open_folder(&self, storage_id: u64, path: &str) -> Result<Folder, Error> {
        self.open_folder_with(storage_id, path, true).await
    }

    /// Like `open_folder`, for someone about to act on the children called
    /// `names` (rename, delete, copy, replace). When the folder came from
    /// memory, each of them is checked with the phone (one request each, not
    /// one per file in the folder) and replaced by what the phone says now; if
    /// any is gone or has changed, the folder is read for real. With
    /// `must_exist`, a name that isn't remembered counts as a change too (it
    /// may have been added since); without it, an unknown name is just new.
    pub(crate) async fn open_folder_confirmed(
        &self,
        storage_id: u64,
        path: &str,
        names: &[String],
        must_exist: bool,
    ) -> Result<Folder, Error> {
        let mut folder = self.open_folder(storage_id, path).await?;
        if folder.from_memory && !self.confirm(&mut folder, names, must_exist).await? {
            folder = self.open_folder_with(storage_id, path, false).await?;
        }
        Ok(folder)
    }

    /// Checks the remembered children called `names` against the phone and
    /// takes what it says. False if one is missing or no longer the same item.
    async fn confirm(
        &self,
        folder: &mut Folder,
        names: &[String],
        must_exist: bool,
    ) -> Result<bool, Error> {
        for name in names {
            let Some(at) = folder
                .children
                .iter()
                .position(|child| child.filename == *name)
            else {
                if must_exist {
                    return Ok(false);
                }
                continue;
            };
            let remembered = &folder.children[at];
            match folder
                .limits
                .ask(folder.storage.get_object_info(remembered.handle))
                .await
            {
                Ok(info)
                    if info.filename == *name && info.is_folder() == remembered.is_folder() =>
                {
                    folder.children[at] = info;
                }
                Ok(_) | Err(mtp_rs::Error::StaleHandle | mtp_rs::Error::NotFound) => {
                    return Ok(false);
                }
                Err(err) => return Err(self.device.handle_error_at(folder.epoch, err).await),
            }
        }
        Ok(true)
    }

    async fn open_folder_with(
        &self,
        storage_id: u64,
        path: &str,
        reuse: bool,
    ) -> Result<Folder, Error> {
        let path = normalize(path);
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
        let key = (serial.clone(), storage_id, path.clone());

        let mut result = self.read_folder(&storage, &key, epoch, reuse).await;
        // Android re-keys object handles when its media index rescans, which
        // invalidates the cache. Re-resolving the path from the root once is
        // the documented recovery, reading the folder for real.
        if let Err(Failure::Client(mtp_rs::Error::StaleHandle | mtp_rs::Error::NotFound)) = result {
            self.folders.lock().await.clear();
            self.forget_listing(&key).await;
            result = self.read_folder(&storage, &key, epoch, false).await;
        }

        match result {
            Ok((handle, children, from_memory)) => Ok(Folder {
                session,
                storage,
                handle,
                children,
                key,
                from_memory,
                limits: self.limits.clone(),
                epoch,
                index: std::sync::OnceLock::new(),
            }),
            Err(Failure::PathNotFound) => Err(Error::PathNotFound(path)),
            Err(Failure::Client(err)) => Err(self.device.handle_error_at(epoch, err).await),
        }
    }

    /// The folder's handle and children: from memory when `reuse` allows it
    /// and the phone still reports as many items, else read from the phone
    /// (and remembered).
    async fn read_folder(
        &self,
        storage: &Storage,
        key: &FolderKey,
        epoch: u64,
        reuse: bool,
    ) -> Result<(Option<ObjectHandle>, Vec<ObjectInfo>, bool), Failure> {
        let (serial, storage_id, path) = (&key.0, key.1, &key.2);
        let (folder, listing) = self
            .start_listing(storage, serial, storage_id, path)
            .await?;
        let total = listing.total();
        if reuse {
            let remembered = self
                .listings
                .lock()
                .await
                .recall(key, epoch, total)
                .cloned();
            if let Some(children) = remembered {
                self.remember(serial, storage_id, path, &children).await;
                return Ok((folder, children, true));
            }
        }
        let children = read_all(listing, &self.limits)
            .await
            .map_err(Failure::Client)?;
        self.remember(serial, storage_id, path, &children).await;
        #[cfg(test)]
        self.full_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.listings
            .lock()
            .await
            .keep(key.clone(), epoch, total, children.clone());
        Ok((folder, children, false))
    }

    /// Keeps what is remembered of a folder in step with a change just made to it.
    pub(super) async fn amend(
        &self,
        key: &FolderKey,
        change: impl Fn(&mut super::memory::Remembered),
    ) {
        self.listings.lock().await.amend(key, change);
    }

    pub(super) async fn keep_listing(
        &self,
        key: FolderKey,
        epoch: u64,
        total: usize,
        children: Vec<ObjectInfo>,
    ) {
        #[cfg(test)]
        self.full_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.listings.lock().await.keep(key, epoch, total, children);
    }

    #[cfg(test)]
    pub(super) fn count_full_read(&self) {
        self.full_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// How many full readings of folders there have been.
    #[cfg(test)]
    pub(crate) fn full_reads(&self) -> usize {
        self.full_reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(super) async fn forget_listing(&self, key: &FolderKey) {
        self.listings.lock().await.forget(key);
    }

    /// Forgets what is remembered of the folder at `path` and everything under it.
    pub(super) async fn forget_below(&self, key: &FolderKey, path: &str) {
        self.listings.lock().await.forget_below(&key.0, key.1, path);
    }

    /// Something other than this service changed the phone's folder at `path`
    /// (a copy into it, say): what is remembered of it and of every folder
    /// under it is no longer right.
    pub(crate) async fn forget_listings_below(&self, storage_id: u64, path: &str) {
        let Ok(session) = self.device.session().await else {
            return;
        };
        let serial = session.device_info().serial_number.clone();
        self.listings
            .lock()
            .await
            .forget_below(&serial, storage_id, &normalize(path));
    }

    /// A move copied files away from phone folders and removed the originals
    /// itself, handle by handle: `removed` lists them by the folder they were in
    /// (a path), and `folders` the folders that went too. What is remembered
    /// of each folder is kept true, so showing it needs no per-file reads, and
    /// what is remembered of a removed folder is dropped.
    pub(crate) async fn note_removed(
        &self,
        storage_id: u64,
        removed: &std::collections::BTreeMap<String, Vec<ObjectHandle>>,
        folders: &[String],
    ) {
        let Ok(session) = self.device.session().await else {
            return;
        };
        let serial = session.device_info().serial_number.clone();
        {
            let mut listings = self.listings.lock().await;
            for (parent, handles) in removed {
                let key: FolderKey = (serial.clone(), storage_id, normalize(parent));
                listings.amend(&key, |kept| kept.removed(handles));
            }
            for folder in folders {
                listings.forget_below(&serial, storage_id, &normalize(folder));
            }
        }
        if !folders.is_empty() {
            self.folders.lock().await.clear();
        }
    }

    /// Resolves `path` and asks the phone for the handles of what is in it;
    /// the details of each item are fetched as the listing is read.
    pub(super) async fn start_listing(
        &self,
        storage: &Storage,
        serial: &str,
        storage_id: u64,
        path: &str,
    ) -> Result<(Option<ObjectHandle>, ObjectListing), Failure> {
        let folder = self.resolve(storage, serial, storage_id, path).await?;
        let listing = self
            .limits
            .ask(storage.list_objects_stream(folder))
            .await
            .map_err(Failure::Client)?;
        if listing.total() == 0
            && let Some(handle) = folder
        {
            ensure_still_folder(storage, &self.limits, handle, path).await?;
        }
        Ok((folder, listing))
    }

    /// Drops every remembered folder handle, e.g. after the phone re-keyed them.
    pub(super) async fn forget_folders(&self) {
        self.folders.lock().await.clear();
    }

    /// Handles an error on the session of `epoch`, leaving a newer session
    /// (another phone, opened since) alone.
    pub(super) async fn handle_error_at(&self, epoch: u64, err: mtp_rs::Error) -> Error {
        self.device.handle_error_at(epoch, err).await
    }

    /// Walks `path` from the storage root to a folder handle. `None` is the root.
    pub(super) async fn resolve(
        &self,
        storage: &Storage,
        serial: &str,
        storage_id: u64,
        path: &str,
    ) -> Result<Option<ObjectHandle>, Failure> {
        let mut parent = None;
        let mut walked = String::from("/");

        for segment in path.split('/').filter(|segment| !segment.is_empty()) {
            let child = join(&walked, segment);
            let key = (serial.to_owned(), storage_id, child.clone());
            let cached = self.folders.lock().await.get(&key).copied();

            parent = match cached {
                Some(handle) => Some(handle),
                None => {
                    let children = read_children(storage, parent, &self.limits)
                        .await
                        .map_err(Failure::Client)?;
                    self.remember(serial, storage_id, &walked, &children).await;
                    let folder = children
                        .iter()
                        .find(|object| object.is_folder() && object.filename == segment)
                        .ok_or(Failure::PathNotFound)?;
                    Some(folder.handle)
                }
            };
            walked = child;
        }

        Ok(parent)
    }

    pub(super) async fn remember(
        &self,
        serial: &str,
        storage_id: u64,
        path: &str,
        children: &[ObjectInfo],
    ) {
        let mut folders = self.folders.lock().await;
        for child in children.iter().filter(|child| child.is_folder()) {
            let key = (serial.to_owned(), storage_id, join(path, &child.filename));
            folders.insert(key, child.handle);
        }
    }
}

/// Orders a listing like Finder: by the chosen key, with names compared
/// case-insensitively and numbers as numbers (`IMG_2` before `IMG_10`).
/// Folders stay on top when asked, and
/// ties are broken by name so the order is stable. Folders have no size, so
/// they tie on size. Accented letters are not folded into their plain letter
/// (see `natural.rs`).
pub fn sort(entries: &mut [Entry], sort: Sort) {
    let order = |a: &Entry, b: &Entry, by_key: std::cmp::Ordering| {
        let by_key = by_key.then_with(|| natural_cmp(&a.name, &b.name));
        let by_key = if sort.descending {
            by_key.reverse()
        } else {
            by_key
        };
        if sort.folders_first {
            b.is_folder.cmp(&a.is_folder).then(by_key)
        } else {
            by_key
        }
    };
    if sort.key == SortKey::Kind {
        // Each entry's kind worked out once, not at every comparison: this
        // runs on the UI thread as a big folder arrives.
        let kinds: Vec<(usize, String)> = entries
            .iter()
            .map(|entry| (kind_rank(entry), extension(entry)))
            .collect();
        let mut at: Vec<usize> = (0..entries.len()).collect();
        at.sort_by(|&i, &j| order(&entries[i], &entries[j], kinds[i].cmp(&kinds[j])));
        let sorted: Vec<Entry> = at.iter().map(|&i| entries[i].clone()).collect();
        entries.clone_from_slice(&sorted);
        return;
    }
    entries.sort_by(|a, b| {
        let by_key = match sort.key {
            SortKey::Name | SortKey::Kind => std::cmp::Ordering::Equal,
            SortKey::Size => a.size_bytes.cmp(&b.size_bytes),
            SortKey::Modified => a.modified.cmp(&b.modified),
        };
        order(a, b, by_key)
    });
}

/// Where an entry's kind comes in a sort by kind: folders, the kinds a search
/// knows in their order, other files with an extension, then files without.
fn kind_rank(entry: &Entry) -> usize {
    if entry.is_folder {
        return 0;
    }
    match crate::dto::FileKind::of(&entry.name) {
        Some(kind) => {
            1 + crate::dto::FileKind::ALL
                .iter()
                .position(|known| *known == kind)
                .unwrap_or(0)
        }
        None if extension(entry).is_empty() => 2 + crate::dto::FileKind::ALL.len(),
        None => 1 + crate::dto::FileKind::ALL.len(),
    }
}

/// A file's extension in lower case, which the Kind column shows ("JPG
/// file"); empty for a folder or a name without one.
fn extension(entry: &Entry) -> String {
    match entry.name.rsplit_once('.') {
        Some((stem, extension)) if !entry.is_folder && !stem.is_empty() => extension.to_lowercase(),
        _ => String::new(),
    }
}

/// What deleting needs from a device storage. A seam so the fallback for
/// devices that refuse non-empty folders can be tested without one.
pub(crate) trait Objects: Sync {
    fn delete(
        &self,
        handle: ObjectHandle,
    ) -> impl Future<Output = Result<(), mtp_rs::Error>> + Send;

    /// Each child's handle and whether it is a folder.
    fn children(
        &self,
        handle: ObjectHandle,
    ) -> impl Future<Output = Result<Vec<(ObjectHandle, bool)>, mtp_rs::Error>> + Send;
}

/// Deletes one object. Android deletes a folder and its contents in one
/// request; devices that refuse a non-empty folder get it emptied first.
///
/// Each request has its own limit (`limits`): deleting a folder is one request
/// that takes as long as what is in it, so it gets the long one, and so does
/// listing what is in a folder this way.
pub(crate) async fn delete_object(
    objects: &impl Objects,
    limits: &Limits,
    handle: ObjectHandle,
    is_folder: bool,
) -> Result<(), mtp_rs::Error> {
    match delete_one(objects, limits, handle, is_folder).await {
        Ok(()) => Ok(()),
        Err(err) if is_folder => {
            let children = limits.long(objects.children(handle)).await?;
            if children.is_empty() {
                return Err(err);
            }
            for (child, child_is_folder) in children {
                Box::pin(delete_object(objects, limits, child, child_is_folder)).await?;
            }
            delete_one(objects, limits, handle, true).await
        }
        Err(err) => Err(err),
    }
}

/// One delete request: quick for a file, as long as its contents for a folder.
async fn delete_one(
    objects: &impl Objects,
    limits: &Limits,
    handle: ObjectHandle,
    is_folder: bool,
) -> Result<(), mtp_rs::Error> {
    if is_folder {
        limits.long(objects.delete(handle)).await
    } else {
        limits.ask(objects.delete(handle)).await
    }
}

/// Many devices list a stale folder handle as empty instead of failing, so an
/// empty listing is only trusted once the handle is confirmed to still be the
/// folder at `path`. Anything else is reported as a stale handle to trigger
/// the re-resolve.
async fn ensure_still_folder(
    storage: &Storage,
    limits: &Limits,
    handle: ObjectHandle,
    path: &str,
) -> Result<(), Failure> {
    let expected_name = path.rsplit('/').next().unwrap_or_default();
    match limits.ask(storage.get_object_info(handle)).await {
        Ok(info) if info.is_folder() && info.filename == expected_name => Ok(()),
        Ok(_) | Err(mtp_rs::Error::StaleHandle | mtp_rs::Error::NotFound) => {
            Err(Failure::Client(mtp_rs::Error::StaleHandle))
        }
        Err(err) => Err(Failure::Client(err)),
    }
}

/// "/", "/DCIM", "/DCIM/Camera": leading slash, no trailing slash, no empty segments.
pub(super) fn normalize(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    format!("/{}", segments.join("/"))
}

pub(super) fn join(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// How many items a timed reading has seen once `piece` is in: the entries
/// that arrive, not the phone's own count (Android's root folder reports the
/// whole storage's), started again when the listing starts over.
fn arrived(before: usize, piece: &crate::dto::ListingUpdate) -> usize {
    let before = if piece.restart { 0 } else { before };
    before + piece.entries.len()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::service::testing::virtual_phone;

    /// A phone with DCIM/Camera/photo.jpg, Download/notes.txt, an empty Music folder, music.mp3,
    /// alpha.txt, and a hidden .nomedia file at the root.
    async fn phone() -> (tempfile::TempDir, Service) {
        phone_with(|_| {}).await
    }

    async fn phone_with(
        limit: impl FnOnce(&mut device::VirtualPhone),
    ) -> (tempfile::TempDir, Service) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("DCIM/Camera")).unwrap();
        fs::create_dir_all(dir.path().join("Download")).unwrap();
        fs::create_dir_all(dir.path().join("Music")).unwrap();
        fs::write(dir.path().join("DCIM/Camera/photo.jpg"), b"jpeg").unwrap();
        fs::write(dir.path().join("Download/notes.txt"), b"notes").unwrap();
        fs::write(dir.path().join("music.mp3"), b"mp3").unwrap();
        fs::write(dir.path().join("alpha.txt"), b"a").unwrap();
        fs::write(dir.path().join(".nomedia"), b"").unwrap();

        let mut phone = virtual_phone(dir.path());
        limit(&mut phone);
        let device = Arc::new(device::Service::new(device::Source::Virtual(phone)));
        device.connect().await.unwrap();
        (dir, Service::new(device))
    }

    async fn storage_id(service: &Service) -> u64 {
        service.device.storages().await.unwrap()[0].id
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    #[test]
    fn a_listing_that_starts_over_is_counted_from_the_start_again() {
        let piece = |entries: usize, restart: bool| crate::dto::ListingUpdate {
            entries: (0..entries)
                .map(|n| Entry {
                    id: n as u64,
                    name: format!("{n}"),
                    is_folder: false,
                    size_bytes: 1,
                    modified: None,
                })
                .collect(),
            restart,
            ..crate::dto::ListingUpdate::default()
        };

        let seen = arrived(arrived(0, &piece(500, false)), &piece(300, false));
        assert_eq!(seen, 800);
        assert_eq!(arrived(seen, &piece(0, true)), 0, "re-keyed: forget it");
        assert_eq!(arrived(0, &piece(200, false)), 200);
    }

    #[tokio::test]
    async fn which_of_some_paths_are_folders_is_found_without_reading_the_storage() {
        let (dir, service) = phone().await;
        fs::create_dir_all(dir.path().join("Android/media/com.whatsapp/WhatsApp/Media")).unwrap();
        let storage = storage_id(&service).await;
        let asked: Vec<String> = [
            "/DCIM/Camera",
            "/DCIM/Screenshots",
            "/Download/",
            "/Android/media/com.whatsapp/WhatsApp/Media",
            "/music.mp3",
            "/Nowhere/At/All",
        ]
        .iter()
        .map(|path| (*path).to_owned())
        .collect();

        let present = service.folders_present(storage, &asked).await.unwrap();

        assert_eq!(
            present,
            [
                "/DCIM/Camera",
                "/Download",
                "/Android/media/com.whatsapp/WhatsApp/Media"
            ],
            "folders only, in the order asked"
        );
        assert_eq!(service.full_reads(), 0, "no folder read as a listing is");
    }

    #[tokio::test]
    async fn without_a_phone_no_folder_is_present() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;
        service.device.disconnect().await.unwrap();

        let result = service
            .folders_present(storage, &["/DCIM".to_owned()])
            .await;

        assert_eq!(result, Err(Error::NotConnected));
    }

    #[tokio::test]
    async fn the_folders_known_in_a_folder_come_from_memory_without_asking_the_phone() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;
        assert!(
            service.known_folders(storage, "/").await.is_empty(),
            "not read yet"
        );
        let _ = service.list(storage, "/", false).await.unwrap();

        // A phone that answers nothing: any request would hang.
        let _silence = crate::service::testing::silence(&service.device, storage).await;
        let mut known = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.known_folders(storage, "/"),
        )
        .await
        .expect("no request to the phone");

        known.sort();
        assert_eq!(known, ["DCIM", "Download", "Music"], "folders only");
        assert!(service.known_folders(storage, "/DCIM").await.is_empty());
        assert!(service.known_folders(storage + 1, "/").await.is_empty());
    }

    #[tokio::test]
    async fn the_folders_known_are_forgotten_with_the_connection() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;
        let _ = service.list(storage, "/", false).await.unwrap();

        service.device.disconnect().await.unwrap();
        assert!(
            service.known_folders(storage, "/").await.is_empty(),
            "no phone"
        );
        service.device.connect().await.unwrap();

        assert!(
            service.known_folders(storage, "/").await.is_empty(),
            "another connection may have re-keyed it"
        );
    }

    #[tokio::test]
    async fn a_timed_reading_counts_every_item_read_from_the_phone() {
        let (dir, service) = phone().await;
        let service = Arc::new(service);
        let storage = storage_id(&service).await;
        let camera = dir.path().join("DCIM/Camera");
        for n in 0..5_000 {
            fs::write(camera.join(format!("IMG_{n:04}.jpg")), b"j").unwrap();
        }
        let _ = service.list(storage, "/DCIM/Camera", false).await.unwrap();
        let before = service.full_reads();

        let timing = service
            .time_reading(storage, "/DCIM/Camera", Cancel::new())
            .await
            .unwrap();

        assert_eq!(timing.items, 5_001, "photo.jpg and the 5,000 new ones");
        assert_eq!(
            service.full_reads(),
            before + 1,
            "read from the phone, not remembered"
        );
        let root = service
            .time_reading(storage, "/", Cancel::new())
            .await
            .unwrap();
        assert_eq!(
            root.items, 6,
            "hidden .nomedia counts too, and the root's count is its own"
        );
        assert!(
            service
                .time_reading(storage, "/nowhere", Cancel::new())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn root_lists_every_visible_entry_with_its_details() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let mut entries = service.list(storage, "/", false).await.unwrap();
        sort(&mut entries, Sort::default());

        assert_eq!(
            names(&entries),
            ["DCIM", "Download", "Music", "alpha.txt", "music.mp3"]
        );
        assert!(entries[0].is_folder);
        assert_eq!(entries[4].size_bytes, 3);
    }

    #[tokio::test]
    async fn hidden_files_are_listed_only_when_asked() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let hidden = service.list(storage, "/", false).await.unwrap();
        let shown = service.list(storage, "/", true).await.unwrap();

        assert!(!names(&hidden).contains(&".nomedia"));
        assert!(names(&shown).contains(&".nomedia"));
    }

    #[tokio::test]
    async fn nested_folder_lists_its_children() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let entries = service.list(storage, "/DCIM/Camera", false).await.unwrap();

        assert_eq!(names(&entries), ["photo.jpg"]);
    }

    #[tokio::test]
    async fn path_is_normalized() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let entries = service.list(storage, "DCIM//Camera/", false).await.unwrap();

        assert_eq!(names(&entries), ["photo.jpg"]);
    }

    #[tokio::test]
    async fn missing_folder_is_path_not_found() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let result = service.list(storage, "/DCIM/Screenshots", false).await;

        assert_eq!(result, Err(Error::PathNotFound("/DCIM/Screenshots".into())));
    }

    #[tokio::test]
    async fn a_file_is_not_a_folder() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let result = service.list(storage, "/alpha.txt", false).await;

        assert_eq!(result, Err(Error::PathNotFound("/alpha.txt".into())));
    }

    #[tokio::test]
    async fn listing_without_a_device_is_not_connected() {
        let device = Arc::new(device::Service::new(device::Source::Usb));
        let service = Service::new(device);

        assert_eq!(service.list(1, "/", false).await, Err(Error::NotConnected));
    }

    #[tokio::test]
    async fn a_re_keyed_folder_is_re_resolved_once() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;
        service.list(storage, "/DCIM/Camera", false).await.unwrap();
        let serial = service.device.connect().await.unwrap().serial_number;

        // What Android's media rescan does: same folder, new handle.
        let rekeyed = mtp_rs::rekey_virtual_object(&serial, std::path::Path::new("DCIM/Camera"));
        assert!(
            rekeyed.is_some(),
            "the virtual device should track DCIM/Camera"
        );

        let entries = service.list(storage, "/DCIM/Camera", false).await.unwrap();
        assert_eq!(names(&entries), ["photo.jpg"]);
    }

    #[tokio::test]
    async fn listing_a_folder_caches_its_subfolders() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        service.list(storage, "/", false).await.unwrap();

        let folders = service.folders.lock().await;
        let paths: Vec<&str> = folders.keys().map(|(_, _, path)| path.as_str()).collect();
        assert!(paths.contains(&"/DCIM"));
        assert!(paths.contains(&"/Download"));
        assert!(!paths.contains(&"/alpha.txt"));
    }

    #[test]
    fn normalize_and_join_build_absolute_paths() {
        assert_eq!(normalize(""), "/");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("a/b/"), "/a/b");
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join("/a", "b"), "/a/b");
    }

    #[tokio::test]
    async fn an_empty_folder_stays_empty_when_reached_from_the_cache() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;
        service.list(storage, "/", false).await.unwrap();

        let entries = service.list(storage, "/Music", false).await.unwrap();

        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn a_re_keyed_empty_folder_is_re_resolved() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;
        service.list(storage, "/Music", false).await.unwrap();
        let serial = service.device.connect().await.unwrap().serial_number;
        let (_, new_handle) =
            mtp_rs::rekey_virtual_object(&serial, std::path::Path::new("Music")).unwrap();

        let entries = service.list(storage, "/Music", false).await.unwrap();

        assert!(entries.is_empty());
        let key = (serial, storage, "/Music".to_owned());
        let cached = service.folders.lock().await.get(&key).copied();
        assert_eq!(cached.map(|handle| handle.0), Some(u64::from(new_handle.0)));
    }

    #[tokio::test]
    async fn modified_times_are_shown_in_mac_local_time() {
        use chrono::{Datelike, Local, TimeZone, Timelike};

        let (dir, service) = phone().await;
        let instant = 1_791_376_200; // 2026-10-07 12:30 UTC
        let file = fs::File::options()
            .write(true)
            .open(dir.path().join("alpha.txt"))
            .unwrap();
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(instant))
            .unwrap();
        let storage = storage_id(&service).await;

        let entries = service.list(storage, "/", false).await.unwrap();

        let alpha = entries.iter().find(|e| e.name == "alpha.txt").unwrap();
        let local = Local.timestamp_opt(instant as i64, 0).unwrap();
        let stamp = alpha.modified.unwrap();
        assert_eq!(
            (stamp.year, stamp.month, stamp.day, stamp.hour, stamp.minute),
            (
                local.year() as u16,
                local.month() as u8,
                local.day() as u8,
                local.hour() as u8,
                local.minute() as u8
            )
        );
    }

    fn entry(name: &str, is_folder: bool, size_bytes: u64, day: Option<u8>) -> Entry {
        Entry {
            id: 0,
            name: name.into(),
            is_folder,
            size_bytes,
            modified: day.map(|day| crate::dto::Timestamp {
                year: 2026,
                month: 10,
                day,
                hour: 9,
                minute: 0,
            }),
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            entry("b.jpg", false, 300, Some(3)),
            entry("Music", true, 0, Some(9)),
            entry("a.txt", false, 100, None),
            entry("DCIM", true, 0, Some(1)),
            entry("C.mp4", false, 200, Some(5)),
        ]
    }

    fn sorted(key: SortKey, descending: bool, folders_first: bool) -> Vec<String> {
        let mut entries = sample();
        sort(
            &mut entries,
            Sort {
                key,
                descending,
                folders_first,
            },
        );
        entries.into_iter().map(|e| e.name).collect()
    }

    #[test]
    fn sort_by_name_ignores_case_and_keeps_folders_first() {
        assert_eq!(
            sorted(SortKey::Name, false, true),
            ["DCIM", "Music", "a.txt", "b.jpg", "C.mp4"]
        );
    }

    #[test]
    fn sort_by_name_descending_reverses_within_each_group() {
        assert_eq!(
            sorted(SortKey::Name, true, true),
            ["Music", "DCIM", "C.mp4", "b.jpg", "a.txt"]
        );
    }

    #[test]
    fn sort_by_size_puts_folders_as_zero_and_breaks_ties_by_name() {
        assert_eq!(
            sorted(SortKey::Size, false, false),
            ["DCIM", "Music", "a.txt", "C.mp4", "b.jpg"]
        );
    }

    #[test]
    fn sort_by_date_puts_undated_entries_first() {
        assert_eq!(
            sorted(SortKey::Modified, false, false),
            ["a.txt", "DCIM", "b.jpg", "C.mp4", "Music"]
        );
    }

    #[test]
    fn sort_by_date_descending_shows_newest_first() {
        assert_eq!(
            sorted(SortKey::Modified, true, true),
            ["Music", "DCIM", "C.mp4", "b.jpg", "a.txt"]
        );
    }

    #[test]
    fn default_sort_is_name_ascending_with_folders_first() {
        assert_eq!(
            Sort::default(),
            Sort {
                key: SortKey::Name,
                descending: false,
                folders_first: true
            }
        );
    }

    async fn listed(service: &Service, path: &str) -> Vec<String> {
        let storage = storage_id(service).await;
        let mut entries = service.list(storage, path, true).await.unwrap();
        sort(&mut entries, Sort::default());
        entries.into_iter().map(|e| e.name).collect()
    }

    #[tokio::test]
    async fn make_folder_creates_it_on_the_device() {
        let (dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let name = service
            .make_folder(storage, "/DCIM", "  Trip  ")
            .await
            .unwrap();

        assert_eq!(name, "Trip");
        assert!(dir.path().join("DCIM/Trip").is_dir());
        assert_eq!(listed(&service, "/DCIM").await, ["Camera", "Trip"]);
    }

    #[tokio::test]
    async fn make_folder_at_the_root() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        service.make_folder(storage, "/", "Backups").await.unwrap();

        assert!(listed(&service, "/").await.contains(&"Backups".to_owned()));
    }

    #[tokio::test]
    async fn make_folder_refuses_a_taken_name_ignoring_case() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let result = service.make_folder(storage, "/", "dcim").await;

        assert_eq!(result, Err(Error::NameTaken("dcim".into())));
    }

    #[tokio::test]
    async fn make_folder_refuses_an_invalid_name_before_touching_the_device() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let result = service.make_folder(storage, "/No/Such/Folder", "a:b").await;

        assert_eq!(
            result,
            Err(Error::InvalidName(
                crate::dto::NameProblem::IllegalCharacters
            ))
        );
    }

    #[tokio::test]
    async fn make_folder_in_a_missing_folder_is_path_not_found() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let result = service.make_folder(storage, "/Nope", "Trip").await;

        assert_eq!(result, Err(Error::PathNotFound("/Nope".into())));
    }

    #[tokio::test]
    async fn a_read_only_storage_refuses_changes() {
        let (_dir, service) = phone_with(|phone| phone.read_only = true).await;
        let storage = storage_id(&service).await;

        assert_eq!(
            service.make_folder(storage, "/", "Trip").await,
            Err(Error::ReadOnly)
        );
        assert_eq!(
            service.rename(storage, "/", "alpha.txt", "beta.txt").await,
            Err(Error::ReadOnly)
        );
        assert_eq!(
            service.delete(storage, "/", &["alpha.txt".into()]).await,
            Err(Error::ReadOnly)
        );
    }

    #[tokio::test]
    async fn rename_a_file() {
        let (dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let name = service
            .rename(storage, "/", "alpha.txt", "beta.txt")
            .await
            .unwrap();

        assert_eq!(name, "beta.txt");
        assert!(dir.path().join("beta.txt").exists());
        assert!(!dir.path().join("alpha.txt").exists());
    }

    #[tokio::test]
    async fn rename_a_folder_and_browse_into_it_by_its_new_path() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;
        listed(&service, "/DCIM/Camera").await;

        service
            .rename(storage, "/", "DCIM", "Photos")
            .await
            .unwrap();

        assert_eq!(listed(&service, "/Photos/Camera").await, ["photo.jpg"]);
        let gone = service.list(storage, "/DCIM/Camera", false).await;
        assert_eq!(gone, Err(Error::PathNotFound("/DCIM/Camera".into())));
    }

    #[tokio::test]
    async fn rename_may_change_only_the_case() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let name = service.rename(storage, "/", "alpha.txt", "Alpha.txt").await;

        assert_eq!(name, Ok("Alpha.txt".into()));
    }

    #[tokio::test]
    async fn rename_to_the_same_name_does_nothing() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let name = service
            .rename(storage, "/Nope", "alpha.txt", "alpha.txt")
            .await;

        assert_eq!(name, Ok("alpha.txt".into()));
    }

    #[tokio::test]
    async fn rename_refuses_a_name_another_entry_has() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let result = service.rename(storage, "/", "alpha.txt", "MUSIC.mp3").await;

        assert_eq!(result, Err(Error::NameTaken("MUSIC.mp3".into())));
    }

    #[tokio::test]
    async fn rename_of_a_missing_entry_is_path_not_found() {
        let (_dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let result = service.rename(storage, "/", "ghost.txt", "x.txt").await;

        assert_eq!(result, Err(Error::PathNotFound("/ghost.txt".into())));
    }

    #[tokio::test]
    async fn rename_on_a_device_without_rename_is_unsupported() {
        let (_dir, service) = phone_with(|phone| phone.supports_rename = false).await;
        let storage = storage_id(&service).await;

        let result = service.rename(storage, "/", "alpha.txt", "beta.txt").await;

        assert_eq!(result, Err(Error::Unsupported));
    }

    #[tokio::test]
    async fn delete_files_and_a_folder_with_its_contents() {
        let (dir, service) = phone().await;
        let storage = storage_id(&service).await;

        service
            .delete(storage, "/", &["alpha.txt".into(), "DCIM".into()])
            .await
            .unwrap();

        assert!(!dir.path().join("alpha.txt").exists());
        assert!(!dir.path().join("DCIM").exists());
        assert_eq!(
            listed(&service, "/").await,
            ["Download", "Music", ".nomedia", "music.mp3"]
        );
    }

    #[tokio::test]
    async fn delete_with_a_missing_name_deletes_nothing() {
        let (dir, service) = phone().await;
        let storage = storage_id(&service).await;

        let result = service
            .delete(storage, "/", &["alpha.txt".into(), "ghost".into()])
            .await;

        assert_eq!(result, Err(Error::PathNotFound("/ghost".into())));
        assert!(dir.path().join("alpha.txt").exists());
    }

    /// A device that refuses to delete a folder that still has children,
    /// with a tree of handle → children (handle, is_folder).
    struct Fussy {
        tree: std::sync::Mutex<HashMap<u64, Vec<(u64, bool)>>>,
        deleted: std::sync::Mutex<Vec<u64>>,
    }

    impl Fussy {
        fn new(tree: &[(u64, &[(u64, bool)])]) -> Self {
            Self {
                tree: std::sync::Mutex::new(
                    tree.iter().map(|(h, kids)| (*h, kids.to_vec())).collect(),
                ),
                deleted: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn deleted(&self) -> Vec<u64> {
            self.deleted.lock().unwrap().clone()
        }
    }

    impl Objects for Fussy {
        async fn delete(&self, handle: ObjectHandle) -> Result<(), mtp_rs::Error> {
            let mut tree = self.tree.lock().unwrap();
            if tree.get(&handle.0).is_some_and(|kids| !kids.is_empty()) {
                return Err(mtp_rs::Error::Other {
                    detail: "folder not empty".into(),
                });
            }
            tree.remove(&handle.0);
            for kids in tree.values_mut() {
                kids.retain(|(kid, _)| *kid != handle.0);
            }
            self.deleted.lock().unwrap().push(handle.0);
            Ok(())
        }

        async fn children(
            &self,
            handle: ObjectHandle,
        ) -> Result<Vec<(ObjectHandle, bool)>, mtp_rs::Error> {
            let tree = self.tree.lock().unwrap();
            Ok(tree
                .get(&handle.0)
                .map(|kids| kids.iter().map(|(h, f)| (ObjectHandle(*h), *f)).collect())
                .unwrap_or_default())
        }
    }

    #[tokio::test]
    async fn a_fussy_device_gets_folders_emptied_bottom_up() {
        // 1 = DCIM { 2 = Camera { 3 = a.jpg }, 4 = b.jpg }
        let fussy = Fussy::new(&[(1, &[(2, true), (4, false)]), (2, &[(3, false)])]);

        delete_object(&fussy, &Limits::default(), ObjectHandle(1), true)
            .await
            .unwrap();

        assert_eq!(fussy.deleted(), [3, 2, 4, 1]);
    }

    #[tokio::test]
    async fn a_refused_empty_folder_reports_the_devices_error() {
        struct Refuses;
        impl Objects for Refuses {
            async fn delete(&self, _: ObjectHandle) -> Result<(), mtp_rs::Error> {
                Err(mtp_rs::Error::AccessDenied)
            }
            async fn children(
                &self,
                _: ObjectHandle,
            ) -> Result<Vec<(ObjectHandle, bool)>, mtp_rs::Error> {
                Ok(vec![])
            }
        }

        let folder = delete_object(&Refuses, &Limits::default(), ObjectHandle(1), true).await;
        let file = delete_object(&Refuses, &Limits::default(), ObjectHandle(2), false).await;

        assert!(matches!(folder, Err(mtp_rs::Error::AccessDenied)));
        assert!(matches!(file, Err(mtp_rs::Error::AccessDenied)));
    }

    fn camera_folder() -> Vec<Entry> {
        let names = [
            "IMG_10.jpg",
            "IMG_2.jpg",
            "IMG_1.jpg",
            "IMG_100.jpg",
            "IMG_20.jpg",
            "Trip 2",
            "Trip 10",
        ];
        names
            .iter()
            .map(|name| entry(name, name.starts_with("Trip"), 0, None))
            .collect()
    }

    fn order(mut entries: Vec<Entry>, sort_by: Sort) -> Vec<String> {
        sort(&mut entries, sort_by);
        entries.into_iter().map(|e| e.name).collect()
    }

    #[test]
    fn names_sort_with_numbers_as_numbers() {
        let by_name = Sort {
            key: SortKey::Name,
            descending: false,
            folders_first: true,
        };

        assert_eq!(
            order(camera_folder(), by_name),
            [
                "Trip 2",
                "Trip 10",
                "IMG_1.jpg",
                "IMG_2.jpg",
                "IMG_10.jpg",
                "IMG_20.jpg",
                "IMG_100.jpg"
            ]
        );
    }

    #[test]
    fn descending_names_are_the_exact_reverse_within_each_group() {
        let by_name = Sort {
            key: SortKey::Name,
            descending: true,
            folders_first: true,
        };

        assert_eq!(
            order(camera_folder(), by_name),
            [
                "Trip 10",
                "Trip 2",
                "IMG_100.jpg",
                "IMG_20.jpg",
                "IMG_10.jpg",
                "IMG_2.jpg",
                "IMG_1.jpg"
            ]
        );
    }

    #[test]
    fn files_that_tie_on_size_are_ordered_by_natural_name() {
        let by_size = Sort {
            key: SortKey::Size,
            descending: false,
            folders_first: false,
        };

        assert_eq!(
            order(camera_folder(), by_size),
            [
                "IMG_1.jpg",
                "IMG_2.jpg",
                "IMG_10.jpg",
                "IMG_20.jpg",
                "IMG_100.jpg",
                "Trip 2",
                "Trip 10"
            ]
        );
    }

    #[test]
    fn sort_by_kind_groups_folders_then_each_kind_then_by_extension_and_name() {
        let mut entries: Vec<Entry> = [
            ("README", false),
            ("x.zip", false),
            ("Zed", true),
            ("b.mp4", false),
            ("IMG_10.jpg", false),
            ("c.PNG", false),
            ("song.mp3", false),
            ("notes.txt", false),
            ("app.apk", false),
            ("model.stl", false),
            (".nomedia", false),
            ("IMG_2.jpg", false),
            ("Album", true),
        ]
        .iter()
        .map(|(name, is_folder)| entry(name, *is_folder, 1, None))
        .collect();
        let by_kind = |descending, folders_first| Sort {
            key: SortKey::Kind,
            descending,
            folders_first,
        };
        let names =
            |entries: &[Entry]| -> Vec<String> { entries.iter().map(|e| e.name.clone()).collect() };

        sort(&mut entries, by_kind(false, false));
        assert_eq!(
            names(&entries),
            [
                "Album",
                "Zed",
                "IMG_2.jpg",
                "IMG_10.jpg",
                "c.PNG",
                "b.mp4",
                "song.mp3",
                "notes.txt",
                "app.apk",
                "x.zip",
                "model.stl",
                ".nomedia",
                "README"
            ],
            "folders; photos (JPG then PNG, any case); video; music; document; archives (APK, ZIP); other kinds; no extension (a dot-file's name is not one)"
        );
        sort(&mut entries, by_kind(true, true));
        assert_eq!(
            names(&entries)[..3],
            ["Zed", "Album", "README"],
            "reversed, folders still first"
        );
    }

    #[test]
    fn sorting_a_big_folder_by_kind_is_quick() {
        let kinds = ["jpg", "mp4", "png", "heic", "txt", "mov"];
        let mut entries: Vec<Entry> = (0..20_000u64)
            .map(|i| {
                let name = format!("IMG_{:05}.{}", (i * 7919) % 20_000, kinds[i as usize % 6]);
                entry(&name, false, i, None)
            })
            .collect();
        let started = std::time::Instant::now();

        sort(
            &mut entries,
            Sort {
                key: SortKey::Kind,
                ..Sort::default()
            },
        );

        assert!(entries[0].name.ends_with(".heic"), "{}", entries[0].name);
        assert!(entries[19_999].name.ends_with(".txt"));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn sorting_a_big_camera_folder_is_quick() {
        // 20,000 shuffled names, sorted on the UI thread whenever a piece of
        // the folder arrives: it must not cost more than a few frames.
        let mut entries: Vec<Entry> = (0..20_000u64)
            .map(|i| {
                entry(
                    &format!("IMG_{:05}.jpg", (i * 7919) % 20_000),
                    false,
                    i,
                    None,
                )
            })
            .collect();
        let started = std::time::Instant::now();

        sort(&mut entries, Sort::default());

        assert!(
            entries
                .windows(2)
                .all(|w| { natural_cmp(&w[0].name, &w[1].name) != std::cmp::Ordering::Greater })
        );
        // Tens of milliseconds in a debug build; the bound only catches a
        // change that makes the comparison allocate or go quadratic.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    // ---- a phone that never answers a delete ----

    const QUICK: std::time::Duration = std::time::Duration::from_millis(40);
    const SLOW: std::time::Duration = std::time::Duration::from_millis(240);

    /// A phone that never answers `delete`, or never says what is in a folder.
    struct Silent {
        /// Never answers a delete of anything (else only of a file inside).
        on_delete: bool,
        /// What `children` answers, or `None` for no answer at all.
        children: Option<Vec<(u64, bool)>>,
    }

    impl Objects for Silent {
        async fn delete(&self, handle: ObjectHandle) -> Result<(), mtp_rs::Error> {
            // Handle 1 is the folder: refused while it has anything in it.
            if self.on_delete || handle.0 != 1 {
                std::future::pending::<()>().await;
            }
            Err(mtp_rs::Error::Other {
                detail: "folder not empty".into(),
            })
        }

        async fn children(
            &self,
            _: ObjectHandle,
        ) -> Result<Vec<(ObjectHandle, bool)>, mtp_rs::Error> {
            let Some(children) = &self.children else {
                std::future::pending::<()>().await;
                unreachable!("pending never returns");
            };
            Ok(children
                .iter()
                .map(|(handle, folder)| (ObjectHandle(*handle), *folder))
                .collect())
        }
    }

    /// Deletes `handle` from a silent phone: how it ended, and how long it took.
    /// A phone that hangs it for ever is a failure of the test, not a hang.
    async fn deleted_while_silent(
        phone: &Silent,
        handle: u64,
        is_folder: bool,
    ) -> (Result<(), mtp_rs::Error>, std::time::Duration) {
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            SLOW * 10,
            delete_object(
                phone,
                &Limits::new(QUICK, SLOW),
                ObjectHandle(handle),
                is_folder,
            ),
        )
        .await
        .expect("a phone that never answers must not hang a delete");
        (result, started.elapsed())
    }

    #[tokio::test]
    async fn deleting_a_file_nobody_answers_ends_at_the_quick_limit() {
        let phone = Silent {
            on_delete: true,
            children: Some(vec![]),
        };

        let (result, took) = deleted_while_silent(&phone, 2, false).await;

        assert!(matches!(result, Err(mtp_rs::Error::Timeout)));
        assert!(took >= QUICK && took < SLOW, "{took:?}");
    }

    #[tokio::test]
    async fn deleting_a_folder_nobody_answers_gets_as_long_as_what_is_in_it_may_take() {
        // The folder's delete is never answered; asking what is in it is (nothing).
        let phone = Silent {
            on_delete: true,
            children: Some(vec![]),
        };

        let (result, took) = deleted_while_silent(&phone, 1, true).await;

        assert!(matches!(result, Err(mtp_rs::Error::Timeout)));
        assert!(took >= SLOW, "the long limit, not the quick one: {took:?}");
    }

    #[tokio::test]
    async fn emptying_a_folder_first_ends_at_the_long_limit_if_the_phone_stops_listing_it() {
        // Refuses the delete (not empty), then says nothing about what is in it.
        let phone = Silent {
            on_delete: false,
            children: None,
        };

        let (result, took) = deleted_while_silent(&phone, 1, true).await;

        assert!(matches!(result, Err(mtp_rs::Error::Timeout)));
        assert!(took >= SLOW, "{took:?}");
    }

    #[tokio::test]
    async fn emptying_a_folder_ends_if_the_phone_stops_answering_inside_it() {
        // Refuses the folder, lists a file in it, and never answers the file's delete.
        let phone = Silent {
            on_delete: false,
            children: Some(vec![(2, false)]),
        };

        let (result, took) = deleted_while_silent(&phone, 1, true).await;

        assert!(matches!(result, Err(mtp_rs::Error::Timeout)));
        assert!(
            took >= QUICK && took < SLOW,
            "a file's delete is quick: {took:?}"
        );
    }
}
