//! Mirroring a phone folder and a Mac folder, either way: the source's
//! contents are compared with the target's (`compare.rs`), previewed, and then
//! what is new is copied, what changed is replaced, and what is only in the
//! target is removed when asked.
//!
//! Onto the Mac nothing is lost: a changed file is copied into a hidden
//! staging folder (`.open-mtp-client-mirror`) and takes the old one's place
//! only once it has all arrived and been checked; the old one, and any extra
//! being removed, are gathered in one hidden folder that goes to the Trash in
//! one call (the Trash is a Finder script, slow per call). Without a Trash,
//! a mirror that would replace or remove anything on the Mac is refused before
//! it starts. A staging folder left by a stopped mirror is carried on from.
//!
//! Onto the phone a changed file is deleted and sent again, and extras are
//! deleted for good: the phone has no Trash, and the Mac's copy is the one
//! the mirror is made from.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mtp_rs::StorageId;
use mtp_rs::mtp::{ObjectHandle, ObjectInfo, Storage};

use super::compare::{Action, Item, compare};
use super::mapper;
use super::relay::Nowhere;
use super::service::{Failure, Outcome, Plan, Run, Service, Step, resolve_file};
use super::upload::{LocalNode, Sender, scan};
use crate::dto::{
    Cancel, Direction, Error, FileFailure, MIRROR_SAMPLE, MirrorDirection, MirrorPreview,
    MirrorRequest, Mirrored, TransferProgress, TransferSummary,
};
use crate::service::failures::whole_batch_failure;
use crate::service::{explorer, local};

/// Where changed files wait, inside the Mac folder, until they replace the old.
const STAGING: &str = ".open-mtp-client-mirror";
/// How long a preview's reading of the phone folder stands in for a new one.
const READING_LASTS: std::time::Duration = std::time::Duration::from_secs(120);

/// A phone folder as a preview read it, for the mirror run right after.
pub(super) struct Reading {
    storage_id: u64,
    folder: String,
    epoch: u64,
    at: Instant,
    phone: Vec<Item>,
    phone_objects: HashMap<String, ObjectInfo>,
    phone_root: Option<ObjectHandle>,
}

/// Both sides as `compare` sees them, and what acting on them needs.
struct Trees {
    phone: Vec<Item>,
    /// The phone's objects by relative path.
    phone_objects: HashMap<String, ObjectInfo>,
    /// The phone folder's own handle (`None` for a storage's root).
    phone_root: Option<ObjectHandle>,
    mac: Vec<Item>,
    /// The Mac folder, read whole.
    mac_root: LocalNode,
}

impl Trees {
    fn source(&self, direction: MirrorDirection) -> &[Item] {
        match direction {
            MirrorDirection::ToMac => &self.phone,
            MirrorDirection::ToPhone => &self.mac,
        }
    }

    fn target(&self, direction: MirrorDirection) -> &[Item] {
        match direction {
            MirrorDirection::ToMac => &self.mac,
            MirrorDirection::ToPhone => &self.phone,
        }
    }

    /// The phone folder at `path` below the mirrored folder, matched ignoring
    /// case as `compare` matched it.
    fn phone_folder(&self, path: &str) -> Option<ObjectHandle> {
        let wanted = path.to_lowercase();
        self.phone_objects
            .iter()
            .find(|(at, object)| object.is_folder() && at.to_lowercase() == wanted)
            .map(|(_, object)| object.handle)
    }
}

/// `path`'s parent and its own name: "a/b/c.jpg" gives ("a/b", "c.jpg").
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// The Mac item at `path` below `root`, found by name.
fn mac_node<'a>(root: &'a LocalNode, path: &str) -> Option<&'a LocalNode> {
    path.split('/').try_fold(root, |node, name| {
        node.children.iter().find(|child| child.name == name)
    })
}

/// `node` with only what the mirror means to send: what is under it in
/// `planned` (relative paths), so hidden files and conflicts stay behind.
fn only_planned(node: &LocalNode, path: &str, planned: &HashSet<String>) -> LocalNode {
    LocalNode {
        children: node
            .children
            .iter()
            .filter_map(|child| {
                let child_path = format!("{path}/{}", child.name);
                planned
                    .contains(&child_path)
                    .then(|| only_planned(child, &child_path, planned))
            })
            .collect(),
        path: node.path.clone(),
        name: node.name.clone(),
        is_folder: node.is_folder,
        size: node.size,
        modified: node.modified,
        left_out: 0,
    }
}

/// The Mac items under `node`, with paths relative to the mirrored folder.
fn flatten(node: &LocalNode, prefix: &str, out: &mut Vec<Item>) {
    for child in &node.children {
        let path = if prefix.is_empty() {
            child.name.clone()
        } else {
            format!("{prefix}/{}", child.name)
        };
        out.push(Item {
            path: path.clone(),
            is_folder: child.is_folder,
            size: child.size,
            modified: child.modified,
        });
        flatten(child, &path, out);
    }
}

/// `folder` (a path the walk reported) relative to `root`, joined with `name`.
pub(super) fn relative(root: &str, folder: &str, name: &str) -> String {
    let below = folder
        .strip_prefix(root)
        .unwrap_or(folder)
        .trim_start_matches('/');
    if below.is_empty() {
        name.to_owned()
    } else {
        format!("{below}/{name}")
    }
}

impl Service {
    /// What `mirror` would do now. `seen` is told how many items of the phone
    /// folder have been read so far.
    pub async fn mirror_preview(
        &self,
        request: &MirrorRequest,
        cancel: &Cancel,
        seen: impl Fn(usize) + Send + Sync,
    ) -> Result<MirrorPreview, Error> {
        // Read for real: a file changed in place leaves its folder's item count
        // as it was, so the index would hide it.
        let trees = self.read_trees(request, cancel, &seen, false).await?;
        let sizes: HashMap<&str, u64> = trees
            .source(request.direction)
            .iter()
            .map(|item| (item.path.as_str(), item.size))
            .collect();
        let mut preview = MirrorPreview::default();
        let sample = |list: &mut Vec<String>, path: &str| {
            if list.len() < MIRROR_SAMPLE {
                list.push(path.to_owned());
            }
        };
        for action in compare(
            trees.source(request.direction),
            trees.target(request.direction),
            request.remove_extras,
        ) {
            match &action {
                Action::MakeFolder { .. } => {}
                Action::CopyNew { path } => {
                    preview.new_files += 1;
                    preview.new_bytes += sizes.get(path.as_str()).copied().unwrap_or(0);
                    sample(&mut preview.samples.new, path);
                }
                Action::Replace { path, .. } => {
                    preview.changed_files += 1;
                    preview.changed_bytes += sizes.get(path.as_str()).copied().unwrap_or(0);
                    sample(&mut preview.samples.changed, path);
                }
                Action::Extra { target, .. } => {
                    preview.extras += 1;
                    sample(&mut preview.samples.extras, target);
                }
                Action::Conflict { path, .. } => {
                    preview.conflicts += 1;
                    sample(&mut preview.samples.conflicts, path);
                }
            }
        }
        Ok(preview)
    }

    /// Makes the target folder's contents match the source's (see the module
    /// docs). Like any transfer it can be cancelled and goes on past a file
    /// that fails; the summary's `mirrored` says what was replaced, removed
    /// and left alone.
    pub async fn mirror(
        &self,
        request: &MirrorRequest,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        let started = Instant::now();
        // The preview's reading if it is of this folder, on this connection, and
        // recent; else read for real. (A file changed since is not missed for
        // good: the next mirror sees it. Nothing is removed on its strength
        // but by name.)
        let trees = self.read_trees(request, cancel, &|_| {}, true).await?;
        let actions = compare(
            trees.source(request.direction),
            trees.target(request.direction),
            request.remove_extras,
        );
        let result = match request.direction {
            MirrorDirection::ToMac => {
                self.mirror_to_mac(request, &trees, &actions, cancel, &progress, started)
                    .await
            }
            MirrorDirection::ToPhone => {
                self.mirror_to_phone(request, &trees, &actions, cancel, &progress, started)
                    .await
            }
        };
        if request.direction == MirrorDirection::ToPhone {
            // Whatever happened, the phone folder is not as it was remembered.
            self.explorer
                .forget_listings_below(request.storage_id, &request.phone_folder)
                .await;
        }
        result
    }

    async fn read_trees(
        &self,
        request: &MirrorRequest,
        cancel: &Cancel,
        seen: &(impl Fn(usize) + Sync),
        reuse: bool,
    ) -> Result<Trees, Error> {
        let mac_folder = PathBuf::from(&request.mac_folder);
        match tokio::fs::metadata(&mac_folder).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(Error::PathNotFound(request.mac_folder.clone())),
            Err(err) => return Err(local::mapper::to_error(&err, &request.mac_folder)),
        }
        let mut count = 0;
        let mac_root = match scan(mac_folder, String::new(), 0, &mut count).await {
            Ok(Some(root)) => root,
            Ok(None) => return Err(Error::PathNotFound(request.mac_folder.clone())),
            Err(failure) => return Err(failure.into_error()),
        };
        let mut mac = Vec::new();
        flatten(&mac_root, "", &mut mac);

        let root = explorer::normalize_path(&request.phone_folder);
        let epoch = self.device.epoch();
        let mut kept = self.mirror_reading.lock().await;
        if reuse
            && let Some(reading) = kept.take()
            && reading.storage_id == request.storage_id
            && reading.folder == root
            && reading.epoch == epoch
            && reading.at.elapsed() < READING_LASTS
        {
            return Ok(Trees {
                phone: reading.phone,
                phone_objects: reading.phone_objects,
                phone_root: reading.phone_root,
                mac,
                mac_root,
            });
        }
        *kept = None;
        drop(kept);

        // Hidden folders are left alone, so they need not be read; every other
        // folder is read for real.
        let mut walk = self
            .explorer
            .walk_from(
                request.storage_id,
                &request.phone_folder,
                false,
                cancel.clone(),
            )
            .await?
            .fresh();
        let phone_root = walk.start();
        let mut found: Vec<(String, ObjectInfo)> = Vec::new();
        while let Some(piece) = walk.next_raw().await {
            let (update, objects) = piece?;
            if update.restart {
                found.clear();
            }
            // A walk that stopped at its bound did not see everything, and
            // what it did not see would look like an extra to remove.
            if update.truncated {
                return Err(Error::TransferTooLarge);
            }
            found.extend(objects);
            seen(found.len());
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut phone = Vec::with_capacity(found.len());
        let mut phone_objects = HashMap::with_capacity(found.len());
        for (folder, object) in found {
            let path = relative(&root, &folder, &object.filename);
            phone.push(Item {
                path: path.clone(),
                is_folder: object.is_folder(),
                size: if object.is_folder() { 0 } else { object.size },
                modified: object.modified.and_then(mapper::to_system_time),
            });
            phone_objects.insert(path, object);
        }
        if !reuse {
            *self.mirror_reading.lock().await = Some(Reading {
                storage_id: request.storage_id,
                folder: root,
                epoch,
                at: Instant::now(),
                phone: phone.clone(),
                phone_objects: phone_objects.clone(),
                phone_root,
            });
        }
        Ok(Trees {
            phone,
            phone_objects,
            phone_root,
            mac,
            mac_root,
        })
    }

    /// The phone storage and whether it reads from an offset, on the session
    /// open now.
    async fn phone_storage(&self, storage_id: u64) -> Result<(Storage, bool), Error> {
        let (session, epoch) = self.device.session_at().await?;
        match self
            .limits
            .ask(session.storage(StorageId(storage_id)))
            .await
        {
            Ok(storage) => Ok((storage, session.capabilities().supports_partial_download)),
            Err(err) => Err(self.device.handle_error_at(epoch, err).await),
        }
    }

    async fn mirror_to_mac(
        &self,
        request: &MirrorRequest,
        trees: &Trees,
        actions: &[Action],
        cancel: &Cancel,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
        started: Instant,
    ) -> Result<TransferSummary, Error> {
        let mac = PathBuf::from(&request.mac_folder);
        let touches_mac = actions.iter().any(|action| {
            matches!(
                action,
                Action::Replace { .. } | Action::Extra { remove: true, .. }
            )
        });
        let trash = match (&self.trash, touches_mac) {
            (Some(trash), _) => Some(Arc::clone(trash)),
            (None, false) => None,
            // Nothing on the Mac is replaced or removed without a Trash to
            // put it in; refused before anything is copied.
            (None, true) => return Err(Error::Unsupported),
        };
        let (storage, partial_reads) = self.phone_storage(request.storage_id).await?;
        let staging = mac.join(STAGING);

        // The steps, and for each the change it makes: a new file is placed
        // where it goes; a changed one is staged, to replace `target` later.
        let mut steps = Vec::new();
        let mut replacing: Vec<Option<(String, String)>> = Vec::new();
        let mut staged_folders: HashSet<PathBuf> = HashSet::new();
        let mut claimed: HashSet<String> = HashSet::new();
        let mut first_item = None;
        // Folders that can't be made because something else holds the name
        // (a link to a folder elsewhere would take the copies out of the Mac
        // folder): reported, and nothing under them is done.
        let mut blocked: Vec<String> = Vec::new();
        let mut failures = Vec::new();
        for action in actions {
            let path = match action {
                Action::MakeFolder { path }
                | Action::CopyNew { path }
                | Action::Replace { path, .. } => path,
                _ => continue,
            };
            if blocked
                .iter()
                .any(|folder| path.to_lowercase().starts_with(&format!("{folder}/")))
            {
                continue;
            }
            let (path, staged_for) = match action {
                Action::MakeFolder { path } => {
                    let folder = mac.join(path);
                    if let Ok(there) = tokio::fs::symlink_metadata(&folder).await
                        && !there.is_dir()
                    {
                        blocked.push(path.to_lowercase());
                        failures.push(FileFailure {
                            label: path.clone(),
                            error: Error::NameTaken(path.clone()),
                        });
                        continue;
                    }
                    steps.push(Step::Folder(folder));
                    replacing.push(None);
                    continue;
                }
                Action::CopyNew { path } => (path, None),
                Action::Replace { path, target } => (path, Some(target)),
                Action::Extra { .. } | Action::Conflict { .. } => continue,
            };
            let Some(object) = trees.phone_objects.get(path) else {
                continue;
            };
            let base = match staged_for {
                None => mac.join(path),
                Some(_) => {
                    let staged = staging.join(path);
                    if let Some(parent) = staged.parent()
                        && staged_folders.insert(parent.to_path_buf())
                    {
                        steps.push(Step::Folder(parent.to_path_buf()));
                        replacing.push(None);
                    }
                    staged
                }
            };
            let (target, how) =
                resolve_file(&base, object.size, object.modified, partial_reads, &claimed).await;
            claimed.insert(target.to_string_lossy().to_lowercase());
            first_item.get_or_insert_with(|| path.clone());
            steps.push(Step::File {
                handle: object.handle,
                target,
                label: path.clone(),
                size: object.size,
                modified: object.modified,
                how,
            });
            replacing.push(staged_for.map(|target| (path.clone(), target.clone())));
        }
        let plan = Plan {
            steps,
            origins: Vec::new(),
            items: Vec::new(),
            unwalked: false,
            renamed: 0,
            first_item,
            replaces: Vec::new(),
            skipped: Vec::new(),
        };
        let executed = self
            .run_steps::<Nowhere>(
                &plan,
                Run {
                    storage: &storage,
                    partial_reads,
                    destination: &mac,
                    moving: false,
                    started,
                    cancel,
                    onward: None,
                },
                progress,
            )
            .await?;

        failures.extend(executed.failures);
        let mut mirrored = Mirrored {
            conflicts: blocked.len(),
            ..Mirrored::default()
        };
        // Whatever leaves the folder goes here first, and to the Trash as one.
        let outgoing = mac.join(format!(
            ".open-mtp-client-replaced-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_millis())
        ));
        let mut put_aside = 0;
        for ((step, change), outcome) in plan.steps.iter().zip(&replacing).zip(&executed.outcomes) {
            let (Step::File { target: staged, .. }, Some((path, old))) = (step, change) else {
                if matches!(step, Step::File { .. }) && *outcome != Outcome::NotCopied {
                    mirrored.copied += 1;
                }
                continue;
            };
            match outcome {
                Outcome::Verified => {}
                // Not whole, or not known to be: the old one stays.
                Outcome::Unchecked | Outcome::NotCopied => continue,
            }
            match swap_in(staged, &mac.join(old), &outgoing.join(old), &mac.join(path)).await {
                Ok(()) => {
                    mirrored.replaced += 1;
                    put_aside += 1;
                }
                Err(err) => failures.push(FileFailure {
                    label: path.clone(),
                    error: local::mapper::to_error(&err, path),
                }),
            }
        }
        for action in actions {
            match action {
                Action::Extra {
                    target,
                    remove: true,
                    ..
                } => match set_aside(&mac.join(target), &outgoing.join(target)).await {
                    Ok(()) => {
                        mirrored.removed += 1;
                        put_aside += 1;
                    }
                    Err(err) => failures.push(FileFailure {
                        label: target.clone(),
                        error: local::mapper::to_error(&err, target),
                    }),
                },
                Action::Extra { remove: false, .. } => mirrored.kept += 1,
                Action::Conflict { .. } => mirrored.conflicts += 1,
                _ => {}
            }
        }
        if put_aside > 0
            && let Some(trash) = trash
        {
            let folder = outgoing.clone();
            let trashed = tokio::task::spawn_blocking(move || trash(&folder))
                .await
                .unwrap_or_else(|_| Err(std::io::Error::other("the Trash did not answer")));
            if let Err(err) = trashed {
                failures.push(FileFailure {
                    label: outgoing
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    error: local::mapper::to_error(&err, &outgoing.to_string_lossy()),
                });
            }
        }
        // What is staged and not swapped in (a part to carry on from) stays.
        remove_empty_folders(&staging).await;

        let copied = executed.state.files_done - executed.already_there - executed.not_copied;
        finish(
            request,
            Direction::Download,
            copied,
            executed.state.bytes_done,
            executed.already_there,
            executed.resumed,
            failures,
            mirrored,
            plan.first_item.clone(),
        )
    }

    async fn mirror_to_phone(
        &self,
        request: &MirrorRequest,
        trees: &Trees,
        actions: &[Action],
        cancel: &Cancel,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
        started: Instant,
    ) -> Result<TransferSummary, Error> {
        let (storage, _) = self.phone_storage(request.storage_id).await?;
        let planned: HashSet<String> = actions
            .iter()
            .filter_map(|action| match action {
                Action::MakeFolder { path } | Action::CopyNew { path } => Some(path.clone()),
                _ => None,
            })
            .collect();
        let mut sender = Sender::new(
            &storage,
            &self.limits,
            cancel,
            progress,
            started,
            false,
            self.stall_limit,
            self.device.activity(),
        );
        // What a replaced file or a removed extra held is given back first.
        let mut freed = 0u64;
        for action in actions {
            if let Action::CopyNew { path } | Action::Replace { path, .. } = action
                && let Some(node) = mac_node(&trees.mac_root, path)
            {
                sender.state.files_total += 1;
                sender.state.bytes_total += node.size;
            }
            match action {
                Action::Replace { target, .. }
                | Action::Extra {
                    target,
                    remove: true,
                    ..
                } => {
                    if let Some(object) = trees.phone_objects.get(target) {
                        freed += object.size;
                    }
                }
                _ => {}
            }
        }
        let info = storage.info();
        super::room::ensure(
            sender.state.bytes_total.saturating_sub(freed),
            Some(&super::room::phone_room(info.free_space, &info.description)),
        )?;

        let mut mirrored = Mirrored::default();
        // New folders go over whole, with what is planned inside them.
        let mut sent_folders: Vec<String> = Vec::new();
        let mut first_item = None;
        for action in actions {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let path = match action {
                Action::MakeFolder { path }
                | Action::CopyNew { path }
                | Action::Replace { path, .. } => path,
                Action::Extra {
                    target,
                    is_folder,
                    remove,
                } => {
                    if !remove {
                        mirrored.kept += 1;
                        continue;
                    }
                    let Some(object) = trees.phone_objects.get(target) else {
                        continue;
                    };
                    let removed =
                        explorer::delete_object(&storage, &self.limits, object.handle, *is_folder)
                            .await
                            .map_err(Failure::Device);
                    let failed_before = sender.failures.len();
                    if let Err(failure) =
                        sender.carry_on(removed, target, &LocalNode::standing_for(target))
                    {
                        return Err(self.to_error(failure).await);
                    }
                    if sender.failures.len() == failed_before {
                        mirrored.removed += 1;
                    }
                    continue;
                }
                Action::Conflict { .. } => {
                    mirrored.conflicts += 1;
                    continue;
                }
            };
            if sent_folders
                .iter()
                .any(|folder| path.starts_with(&format!("{folder}/")))
            {
                continue;
            }
            let (parent, name) = split(path);
            let Some(node) = mac_node(&trees.mac_root, path) else {
                continue;
            };
            // The folder it goes in: the mirrored folder itself, or one of the
            // phone's folders below it (a new one went over whole, above).
            let into = if parent.is_empty() {
                trees.phone_root
            } else {
                match trees.phone_folder(parent) {
                    Some(handle) => Some(handle),
                    None => continue,
                }
            };
            first_item.get_or_insert_with(|| path.clone());
            let failed_before = sender.failures.len();
            let result = match action {
                Action::MakeFolder { .. } => {
                    sent_folders.push(path.clone());
                    let whole = only_planned(node, path, &planned);
                    sender.put(&whole, into, name, path.clone()).await
                }
                Action::Replace { target, .. } => match trees.phone_objects.get(target) {
                    Some(old) => {
                        match explorer::delete_object(&storage, &self.limits, old.handle, false)
                            .await
                        {
                            Ok(()) => sender.put(node, into, name, path.clone()).await,
                            Err(err) => Err(Failure::Device(err)),
                        }
                    }
                    None => continue,
                },
                _ => sender.put(node, into, name, path.clone()).await,
            };
            if let Err(failure) = sender.carry_on(result, path, node) {
                return Err(self.to_error(failure).await);
            }
            // Replaced only if the new one went over (a failure about the one
            // file is noted by the sender, not returned).
            if matches!(action, Action::Replace { .. }) && sender.failures.len() == failed_before {
                mirrored.replaced += 1;
            }
        }

        let failures = std::mem::take(&mut sender.failures);
        let mut state = sender.state;
        // Every file that went over is new or replaced (a replace is counted
        // only once its file went over).
        mirrored.copied = state.files_done.saturating_sub(mirrored.replaced);
        state.elapsed = started.elapsed();
        progress(state.clone());
        finish(
            request,
            Direction::Upload,
            state.files_done,
            state.bytes_done,
            sender.already_there,
            0,
            failures,
            mirrored,
            first_item,
        )
    }
}

/// The summary of a mirror, or the one error that stands for it when nothing
/// was done and every failure was the same.
#[allow(clippy::too_many_arguments)]
fn finish(
    request: &MirrorRequest,
    direction: Direction,
    copied: usize,
    bytes: u64,
    already_there: usize,
    resumed: usize,
    failures: Vec<FileFailure>,
    mirrored: Mirrored,
    first_item: Option<String>,
) -> Result<TransferSummary, Error> {
    if let Some(error) = whole_batch_failure(copied, already_there, &failures) {
        return Err(error);
    }
    Ok(TransferSummary {
        direction,
        files: copied,
        bytes,
        replaced: mirrored.replaced,
        already_there,
        resumed,
        failures,
        destination: match direction {
            Direction::Upload => request.phone_folder.clone(),
            _ => request.mac_folder.clone(),
        },
        first_item,
        mirrored: Some(mirrored),
        ..TransferSummary::default()
    })
}

/// Puts `staged` (a whole new copy) where `old` is, after moving `old` to
/// `aside`. If the new one can't be put in place, the old one goes back.
pub(super) async fn swap_in(
    staged: &Path,
    old: &Path,
    aside: &Path,
    place: &Path,
) -> std::io::Result<()> {
    // A link where the old file was is not written through.
    if tokio::fs::symlink_metadata(old).await?.is_symlink() {
        return Err(std::io::Error::other("a link is in the way"));
    }
    set_aside(old, aside).await?;
    if let Err(err) = tokio::fs::rename(staged, place).await {
        let _ = tokio::fs::rename(aside, old).await;
        return Err(err);
    }
    Ok(())
}

/// Moves `item` to `aside` (inside the same folder, so on the same disk),
/// never over something already there: what is set aside is on its way to
/// the Trash, and a rename over it would lose it for good.
async fn set_aside(item: &Path, aside: &Path) -> std::io::Result<()> {
    if let Some(parent) = aside.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if tokio::fs::symlink_metadata(aside).await.is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "something is already set aside there",
        ));
    }
    tokio::fs::rename(item, aside).await
}

/// Removes `folder` and the folders in it that hold nothing; a part file
/// waiting to be carried on keeps its folders.
pub(super) async fn remove_empty_folders(folder: &Path) {
    let Ok(mut reader) = tokio::fs::read_dir(folder).await else {
        return;
    };
    while let Ok(Some(entry)) = reader.next_entry().await {
        if entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
            Box::pin(remove_empty_folders(&entry.path())).await;
        }
    }
    let _ = tokio::fs::remove_dir(folder).await;
}
