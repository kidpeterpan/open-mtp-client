//! The Mac → phone half of the transfer service.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use mtp_rs::mtp::{NewObjectInfo, ObjectHandle, ObjectInfo, Storage};
use tokio_util::io::ReaderStream;

use super::mapper;
use super::service::{
    Failure, MAX_DEPTH, MAX_ITEMS, Service, phone_answers, safe_name, size_is_known,
};
use crate::dto::{
    Cancel, Conflict, Direction, Error, FileFailure, FileOutcome, FileReport, REPORT_MOST,
    TransferProgress, TransferSummary, UploadRequest,
};
use crate::service::failures::{is_about_one_object, whole_batch_failure};
use crate::service::{device, explorer, names};

/// Bytes read from the Mac file per chunk sent.
const CHUNK: usize = 1024 * 1024;

/// A Mac file or folder to copy, read before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LocalNode {
    pub(super) path: PathBuf,
    pub(super) name: String,
    pub(super) is_folder: bool,
    pub(super) size: u64,
    pub(super) modified: Option<SystemTime>,
    pub(super) children: Vec<LocalNode>,
    /// Entries of this folder (not Mac clutter) that were left out because
    /// they can't be copied: a link to a folder, a socket, a pipe.
    pub(super) left_out: usize,
}

impl LocalNode {
    /// An item that exists on the Mac only in passing: a copy through the Mac
    /// reads and sends one file at a time, so no item is ever whole there.
    /// What is known of it is its name.
    pub(super) fn standing_for(name: &str) -> Self {
        Self {
            path: PathBuf::new(),
            name: name.to_owned(),
            is_folder: false,
            size: 0,
            modified: None,
            children: Vec::new(),
            left_out: 0,
        }
    }

    /// A file or an empty folder of this name, for tests that need a tree
    /// without a disk.
    #[cfg(test)]
    pub(super) fn for_test(name: &str, is_folder: bool, size: u64) -> Self {
        Self {
            path: PathBuf::from("/mac").join(name),
            name: name.to_owned(),
            is_folder,
            size,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000)),
            children: Vec::new(),
            left_out: 0,
        }
    }

    pub(super) fn files_and_bytes(&self) -> (usize, u64) {
        if !self.is_folder {
            return (1, self.size);
        }
        self.children
            .iter()
            .map(LocalNode::files_and_bytes)
            .fold((0, 0), |(files, bytes), (f, b)| (files + f, bytes + b))
    }
}

/// Whether the phone already has this file: the same size and date. A folder
/// is never "the same"; its files are compared one by one when it is merged.
fn is_on_phone(local: &LocalNode, phone: &ObjectInfo) -> bool {
    same_file(local.is_folder, local.size, local.modified, phone)
}

/// The phone already holds this Mac file as it is, under its own name or under
/// the numbered name an earlier try gave a copy of it (see `Children::copy_of`).
fn has_a_copy(folder: &explorer::Folder, node: &LocalNode) -> bool {
    !node.is_folder
        && (0..)
            .map_while(|n| folder.child_named(&mapper::numbered(&node.name, n, false)))
            .any(|on_phone| is_on_phone(node, on_phone))
}

/// The Mac's file is newer than the phone's, by more than the two seconds
/// either side may round to; an unknown date on either side is not newer.
pub(super) fn is_newer(modified: Option<SystemTime>, phone: &ObjectInfo) -> bool {
    let on_phone = phone.modified.and_then(mapper::to_system_time);
    matches!(
        (modified, on_phone),
        (Some(here), Some(there)) if here > there && !mapper::same_moment(here, there)
    )
}

pub(super) fn same_file(
    is_folder: bool,
    size: u64,
    modified: Option<SystemTime>,
    phone: &ObjectInfo,
) -> bool {
    let on_phone = phone.modified.and_then(mapper::to_system_time);
    !is_folder
        && !phone.is_folder()
        && phone.size == size
        && matches!((modified, on_phone), (Some(here), Some(there)) if mapper::same_moment(here, there))
}

/// How an upload treats what the phone already has, and whether it reads
/// every file back.
#[derive(Debug, Clone, Copy)]
pub(super) struct Options {
    /// A file the phone already has (same name, size and date) is left alone
    /// rather than copied again: right for retrying, wrong for a copy that is
    /// meant to make another one beside the original.
    pub(super) skip_identical: bool,
    /// Ask the phone about each file after sending it and compare its size, so
    /// a move can trust the copy before it removes the original.
    pub(super) verify: bool,
}

/// What an upload did with one top-level item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Delivered {
    /// The item's name on the Mac, as it was given.
    pub(super) name: String,
    /// A file in it failed to send (the failures list says which).
    pub(super) failed: bool,
    /// The user chose to leave it be because its name was taken: nothing was
    /// sent, and nothing is wrong.
    pub(super) skipped: bool,
    /// Every file in it is on the phone and was read back, or was already
    /// there. False when anything failed, was left alone, or could not be
    /// confirmed.
    pub(super) confirmed: bool,
    /// The item as it was read before sending: where it is on the Mac and
    /// what was in it, so a move can tell afterwards that nothing changed.
    pub(super) node: Arc<LocalNode>,
}

/// An upload that has run: the summary, and how each item fared.
pub(super) struct Uploaded {
    pub(super) summary: TransferSummary,
    pub(super) delivered: Vec<Delivered>,
    /// For each line of `summary.report`, the selected Mac item it is (or
    /// is inside), by its own name: what a move matches its originals by.
    pub(super) report_tops: Vec<String>,
}

/// What reading a sent file back showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadBack {
    /// The phone has it, at the size that was sent.
    Matches,
    /// The phone has it, at another size: what it kept is not what was sent.
    Differs,
    /// The phone gave no size (some do not for 4 GB or more), so it can't be
    /// told.
    Unknown,
}

/// Compares the size the phone reports for a file just sent with the size sent.
pub(super) fn read_back(sent: u64, reported: u64) -> ReadBack {
    if !size_is_known(reported) {
        ReadBack::Unknown
    } else if reported == sent {
        ReadBack::Matches
    } else {
        ReadBack::Differs
    }
}

/// What a phone folder holds, and the names this run has taken in it. Looked
/// up by name without comparing with each (a camera folder holds thousands).
pub(super) struct Children {
    existing: Vec<ObjectInfo>,
    index: names::Index,
    /// Lower-cased names this run has given to new items in the folder.
    added: std::collections::HashSet<String>,
}

impl Children {
    pub(super) fn new(existing: Vec<ObjectInfo>) -> Self {
        let index = names::Index::of(existing.iter().map(|child| child.filename.as_str()));
        Self {
            existing,
            index,
            added: std::collections::HashSet::new(),
        }
    }

    /// The child of this name, ignoring case.
    pub(super) fn named(&self, name: &str) -> Option<&ObjectInfo> {
        self.index.find(name).map(|at| &self.existing[at])
    }

    /// The very file `name` as it is (same size and date), or the numbered copy
    /// of it that an earlier try made beside a different file of that name
    /// (`a (1).jpg`, `a (2).jpg`: numbers are given in order, so the search
    /// ends at the first gap).
    pub(super) fn copy_of(
        &self,
        name: &str,
        size: u64,
        modified: Option<SystemTime>,
    ) -> Option<&ObjectInfo> {
        (0..)
            .map_while(|n| self.named(&mapper::numbered(name, n, false)))
            .find(|candidate| same_file(false, size, modified, candidate))
    }

    /// A name for something new called `name`: itself if nothing has it, else
    /// the first numbered one. The name is taken from now on.
    pub(super) fn claim(&mut self, name: &str, is_folder: bool) -> String {
        let free = self.index.find(name).is_none() && !self.added.contains(&name.to_lowercase());
        let chosen = if free {
            name.to_owned()
        } else {
            let mut taken: Vec<String> = self.existing.iter().map(|c| c.filename.clone()).collect();
            taken.extend(self.added.iter().cloned());
            names::free_name(&taken, name, is_folder)
        };
        self.added.insert(chosen.to_lowercase());
        chosen
    }
}

/// What the phone's answer about a file just sent comes to, for a move to
/// decide whether to trust the copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Judged {
    /// The phone has it at the size that was sent.
    Confirmed,
    /// It could not be checked: the phone gave no size (some do not for 4 GB
    /// or more) or no answer about this one object. A move keeps its original.
    Unconfirmed,
    /// The phone kept something of another size.
    Wrong,
    /// The phone as a whole failed to answer: not about this file.
    Failed,
}

/// Judges `answer` (the size the phone reports, or why it did not) against the
/// `sent` bytes.
pub(super) fn judge(sent: u64, answer: Result<u64, &mtp_rs::Error>) -> Judged {
    match answer {
        Ok(reported) => match read_back(sent, reported) {
            ReadBack::Matches => Judged::Confirmed,
            ReadBack::Unknown => Judged::Unconfirmed,
            ReadBack::Differs => Judged::Wrong,
        },
        Err(err) if is_about_one_object(err) => Judged::Unconfirmed,
        Err(_) => Judged::Failed,
    }
}

/// What happens to one top-level item.
enum Placement {
    /// Copied under this name; nothing there has it.
    New(String),
    /// Copied after the existing item is deleted.
    Replace(ObjectInfo),
    /// A folder copied into the existing folder of the same name.
    Merge(ObjectHandle),
}

impl Service {
    /// Which of the sources' names are taken in the phone folder by something
    /// different, ignoring case as the phone does. Asked before an upload so
    /// the user can choose what to do with them. A file the phone already has
    /// (same size and date) is not a conflict: there is nothing to decide, and
    /// the upload leaves it be.
    pub async fn existing(&self, request: &UploadRequest) -> Result<Vec<String>, Error> {
        let folder = self
            .explorer
            .open_folder(request.storage_id, &request.destination)
            .await?;
        let mut taken = Vec::new();
        for source in &request.sources {
            let Ok(name) = source_name(source) else {
                continue;
            };
            let Some(on_phone) = folder.child_named(name) else {
                continue;
            };
            let identical = match tokio::fs::metadata(source).await {
                Ok(here) => same_file(here.is_dir(), here.len(), here.modified().ok(), on_phone),
                Err(_) => false,
            };
            if !identical {
                taken.push(name.to_owned());
            }
        }
        Ok(taken)
    }

    /// Copies `request.sources` from the Mac to the phone, folders with
    /// everything in them. A name already taken on the phone is handled as
    /// `conflict` says. A cancelled or failed file is deleted from the phone
    /// rather than left half-written. Files keep their Mac modified date.
    pub async fn upload(
        &self,
        request: &UploadRequest,
        conflict: Conflict,
        cancel: &Cancel,
        progress: impl Fn(TransferProgress) + Send + Sync,
    ) -> Result<TransferSummary, Error> {
        let options = Options {
            skip_identical: true,
            verify: false,
        };
        let uploaded = self
            .upload_checked(request, conflict, options, cancel, &progress)
            .await?;
        Ok(uploaded.summary)
    }

    /// `upload`, with the options a move through the Mac needs, and how each
    /// item fared.
    pub(super) async fn upload_checked(
        &self,
        request: &UploadRequest,
        conflict: Conflict,
        options: Options,
        cancel: &Cancel,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
    ) -> Result<Uploaded, Error> {
        let result = self
            .send(request, conflict, options, cancel, progress)
            .await;
        // Whatever happened, the phone's folder is not as it was remembered.
        self.explorer
            .forget_listings_below(request.storage_id, &request.destination)
            .await;
        result
    }

    async fn send(
        &self,
        request: &UploadRequest,
        conflict: Conflict,
        options: Options,
        cancel: &Cancel,
        progress: &(impl Fn(TransferProgress) + Send + Sync),
    ) -> Result<Uploaded, Error> {
        let started = Instant::now();
        // Names that are already in the folder are checked with the phone
        // before one is deleted or merged into; new names need no check.
        let names: Vec<String> = request
            .sources
            .iter()
            .filter_map(|source| source_name(source).ok())
            .map(str::to_owned)
            .collect();
        let folder = self
            .explorer
            .open_folder_confirmed(request.storage_id, &request.destination, &names, false)
            .await?;
        folder.ensure_writable()?;

        let mut nodes = Vec::new();
        let mut count = 0;
        for source in &request.sources {
            let scanned = match source_name(source) {
                Ok(name) => scan(PathBuf::from(source), name.to_owned(), 0, &mut count).await,
                Err(failure) => Err(failure),
            };
            match scanned {
                Ok(Some(node)) => nodes.push(Arc::new(node)),
                Ok(None) => {}
                Err(failure) => return Err(self.to_error(failure).await),
            }
        }

        let mut summary = TransferSummary {
            direction: Direction::Upload,
            destination: request.destination.clone(),
            ..TransferSummary::default()
        };
        let mut taken: Vec<String> = folder.children.iter().map(|c| c.filename.clone()).collect();
        let mut work = Vec::new();
        let mut delivered = Vec::new();
        // Files the phone has already, left alone at the top: part of what
        // was meant to go, and there.
        let mut identical = (0, 0);
        // The report's lines for items decided here, before anything is sent.
        let mut top_lines: Vec<(FileReport, String)> = Vec::new();
        let top_line = |node: &LocalNode, outcome| {
            (
                FileReport {
                    path: node.name.clone(),
                    from: None,
                    outcome,
                    original: None,
                },
                node.name.clone(),
            )
        };
        for node in nodes {
            // The phone has this very file already: nothing to copy or decide.
            if options.skip_identical
                && (folder
                    .child_named(&node.name)
                    .is_some_and(|on_phone| is_on_phone(&node, on_phone))
                    || (conflict == Conflict::Merge && has_a_copy(&folder, &node)))
            {
                summary.already_there += 1;
                let (files, bytes) = node.files_and_bytes();
                identical = (identical.0 + files, identical.1 + bytes);
                top_lines.push(top_line(&node, FileOutcome::AlreadyThere));
                delivered.push(Delivered {
                    name: node.name.clone(),
                    failed: false,
                    skipped: false,
                    confirmed: true,
                    node: Arc::clone(&node),
                });
                continue;
            }
            let placement = match (folder.child_named(&node.name), conflict) {
                (None, _) => Placement::New(node.name.clone()),
                (Some(_), Conflict::Skip) => {
                    summary.skipped += 1;
                    top_lines.push(top_line(&node, FileOutcome::Skipped));
                    delivered.push(Delivered {
                        name: node.name.clone(),
                        failed: false,
                        skipped: true,
                        confirmed: false,
                        node: Arc::clone(&node),
                    });
                    continue;
                }
                (Some(_), Conflict::KeepBoth) => {
                    summary.renamed += 1;
                    Placement::New(free_name(&taken, &node.name, node.is_folder))
                }
                (Some(existing), Conflict::Replace) => {
                    summary.replaced += 1;
                    if existing.is_folder() && node.is_folder {
                        Placement::Merge(existing.handle)
                    } else {
                        Placement::Replace(existing.clone())
                    }
                }
                (Some(existing), Conflict::ReplaceOlder) => {
                    if existing.is_folder() && node.is_folder {
                        summary.merged += 1;
                        Placement::Merge(existing.handle)
                    } else if !existing.is_folder()
                        && !node.is_folder
                        && is_newer(node.modified, existing)
                    {
                        summary.replaced += 1;
                        Placement::Replace(existing.clone())
                    } else {
                        // As new or newer on the phone, or not a file on one
                        // side: what is there stays.
                        summary.skipped += 1;
                        top_lines.push(top_line(&node, FileOutcome::Skipped));
                        delivered.push(Delivered {
                            name: node.name.clone(),
                            failed: false,
                            skipped: true,
                            confirmed: false,
                            node: Arc::clone(&node),
                        });
                        continue;
                    }
                }
                (Some(existing), Conflict::Merge) => {
                    if existing.is_folder() && node.is_folder {
                        summary.merged += 1;
                        Placement::Merge(existing.handle)
                    } else {
                        // A different file, or a file where a folder is: never
                        // replaced, so it goes in beside under another name.
                        summary.renamed += 1;
                        Placement::New(free_name(&taken, &node.name, node.is_folder))
                    }
                }
            };
            let name = match &placement {
                Placement::New(name) => name.clone(),
                Placement::Replace(_) | Placement::Merge(_) => node.name.clone(),
            };
            taken.push(name.clone());
            summary.first_item.get_or_insert_with(|| name.clone());
            work.push((node, placement, name));
        }

        let (files_total, bytes_total) = work
            .iter()
            .map(|(node, _, _)| node.files_and_bytes())
            .fold((0, 0), |(files, bytes), (f, b)| (files + f, bytes + b));
        // Room on the phone first. A folder merged into is left out: what is
        // already in it is skipped as it is reached, so counting it would
        // refuse a retry that fits. What a replaced item held is given back.
        let needed = work
            .iter()
            .map(|(node, placement, _)| match placement {
                Placement::New(_) => node.files_and_bytes().1,
                Placement::Replace(existing) => {
                    node.files_and_bytes().1.saturating_sub(existing.size)
                }
                Placement::Merge(_) => 0,
            })
            .sum();
        let info = folder.storage.info();
        super::room::ensure(
            needed,
            Some(&super::room::phone_room(info.free_space, &info.description)),
        )?;
        let mut sender = Sender::new(
            &folder.storage,
            &self.limits,
            cancel,
            progress,
            started,
            options.verify,
            self.stall_limit,
            self.device.activity(),
        );
        sender.state.files_total = files_total;
        sender.state.bytes_total = bytes_total;
        sender.keep_different = conflict == Conflict::Merge;
        sender.only_newer = conflict == Conflict::ReplaceOlder;

        for (node, placement, name) in &work {
            let (failed_before, unconfirmed_before) = (sender.failures.len(), sender.unconfirmed);
            sender.top = node.name.clone();
            sender.renamed = (*name != node.name).then(|| (name.clone(), node.name.clone()));
            let result = match placement {
                Placement::New(_) => sender.put(node, folder.handle, name, name.clone()).await,
                Placement::Replace(existing) => {
                    match explorer::delete_object(
                        &folder.storage,
                        &folder.limits,
                        existing.handle,
                        existing.is_folder(),
                    )
                    .await
                    {
                        Ok(()) => {
                            sender.replacing = true;
                            let put = sender.put(node, folder.handle, name, name.clone()).await;
                            sender.replacing = false;
                            put
                        }
                        Err(err) => Err(Failure::Device(err)),
                    }
                }
                Placement::Merge(into) => sender.merge(node, *into, name.clone()).await,
            };
            if let Err(failure) = sender.carry_on(result, name, node) {
                return Err(self.to_error(failure).await);
            }
            let failed = sender.failures.len() > failed_before;
            delivered.push(Delivered {
                name: node.name.clone(),
                failed,
                skipped: false,
                confirmed: !failed && sender.unconfirmed == unconfirmed_before,
                node: Arc::clone(node),
            });
        }

        summary.already_there += sender.already_there;
        let failures = std::mem::take(&mut sender.failures);
        let mut state = sender.state;
        state.elapsed = started.elapsed();
        progress(state.clone());
        if let Some(error) = whole_batch_failure(state.files_done, summary.already_there, &failures)
        {
            return Err(error);
        }
        summary.failures = failures;
        summary.files = state.files_done;
        summary.bytes = state.bytes_done;
        // Sent, or found on the phone as it is (at the top, or inside a folder
        // merged into). Only a read-back says a sent file is whole; without
        // one the phone accepted each file, which is all that is known.
        let there = state.files_done + summary.already_there;
        // Files left as the phone had them, not being older there, were never
        // meant to arrive.
        summary.skipped += sender.kept.0;
        summary.arrival = Some(crate::dto::Arrival {
            files: (files_total + identical.0).saturating_sub(sender.kept.0),
            bytes: (bytes_total + identical.1).saturating_sub(sender.kept.1),
            there,
            checked: options
                .verify
                .then(|| there.saturating_sub(sender.unconfirmed)),
            complete: true,
        });
        let (report, report_tops, left_out) = join_report(
            top_lines,
            std::mem::take(&mut sender.report),
            std::mem::take(&mut sender.report_tops),
            sender.report_left_out,
        );
        summary.report = report;
        summary.report_left_out = left_out;
        Ok(Uploaded {
            summary,
            delivered,
            report_tops,
        })
    }
}

/// Adds `item` to `list` while it holds fewer than `REPORT_MOST`, else
/// counts it in `left_out` (a million-file upload would otherwise hold its
/// whole report until the end); says whether it was added.
fn push_capped<T>(list: &mut Vec<T>, item: T, left_out: &mut usize) -> bool {
    if list.len() >= REPORT_MOST {
        *left_out += 1;
        false
    } else {
        list.push(item);
        true
    }
}

/// What became of a file the phone took: put in place of one it deleted,
/// read back (checked when the phone gave the size sent), or just taken.
fn sent_outcome(replacing: bool, read_back: bool, confirmed: bool) -> FileOutcome {
    if replacing {
        FileOutcome::ReplacedOnPhone
    } else if read_back {
        FileOutcome::Copied { checked: confirmed }
    } else {
        FileOutcome::Sent
    }
}

/// The upload's report: the items decided before anything was sent, then
/// what the sender noted, at most `REPORT_MOST` lines in all (the rest
/// counted), each with the selected item it is in.
fn join_report(
    top_lines: Vec<(FileReport, String)>,
    sent: Vec<FileReport>,
    sent_tops: Vec<String>,
    sent_left_out: usize,
) -> (Vec<FileReport>, Vec<String>, usize) {
    let total = top_lines.len() + sent.len() + sent_left_out;
    let (mut report, mut tops): (Vec<FileReport>, Vec<String>) = top_lines
        .into_iter()
        .chain(sent.into_iter().zip(sent_tops))
        .take(REPORT_MOST)
        .unzip();
    report.shrink_to_fit();
    tops.shrink_to_fit();
    let left_out = total - report.len();
    (report, tops, left_out)
}

/// The name a Mac source gets on the phone: its last path component, which
/// must be a plain name.
fn source_name(source: &str) -> Result<&str, Failure> {
    match Path::new(source).file_name().and_then(|name| name.to_str()) {
        Some(name) => safe_name(name),
        None => Err(Failure::UnsafeName(source.to_owned())),
    }
}

/// Mac clutter that has no place on a phone.
fn is_mac_clutter(name: &str) -> bool {
    name == ".DS_Store" || name.starts_with("._")
}

/// Reads a Mac item and everything under it, bounded like downloads.
/// Symlinked files are copied as the file they point to; symlinked folders
/// are skipped (a link can point back up the tree); anything that is
/// neither a file nor a folder (sockets, pipes) and Mac clutter is skipped.
pub(super) async fn scan(
    path: PathBuf,
    name: String,
    depth: usize,
    count: &mut usize,
) -> Result<Option<LocalNode>, Failure> {
    if depth > MAX_DEPTH || *count >= MAX_ITEMS {
        return Err(Failure::TooLarge);
    }
    let mac = |err| Failure::Mac(err, path.clone());
    let link = tokio::fs::symlink_metadata(&path).await.map_err(mac)?;
    let metadata = if link.is_symlink() {
        match tokio::fs::metadata(&path).await {
            Ok(target) if target.is_file() => target,
            _ => return Ok(None),
        }
    } else {
        link
    };
    if !metadata.is_dir() && !metadata.is_file() {
        return Ok(None);
    }
    *count += 1;

    let mut node = LocalNode {
        path: path.clone(),
        name,
        is_folder: metadata.is_dir(),
        size: if metadata.is_dir() { 0 } else { metadata.len() },
        modified: metadata.modified().ok(),
        children: Vec::new(),
        left_out: 0,
    };
    if node.is_folder {
        let mut reader = tokio::fs::read_dir(&path).await.map_err(mac)?;
        while let Some(entry) = reader.next_entry().await.map_err(mac)? {
            let child = entry.file_name().to_string_lossy().into_owned();
            if is_mac_clutter(&child) {
                continue;
            }
            match Box::pin(scan(entry.path(), child, depth + 1, count)).await? {
                Some(child) => node.children.push(child),
                None => node.left_out += 1,
            }
        }
    }
    Ok(Some(node))
}

/// The first of `name (1)`, `name (2)`… not in `taken`, ignoring case.
fn free_name(taken: &[String], name: &str, is_folder: bool) -> String {
    names::free_name(taken, name, is_folder)
}

/// Sends files to one storage and keeps the progress running.
pub(super) struct Sender<'a, P> {
    pub(super) storage: &'a Storage,
    /// What the requests around the sends (merging folders, clearing a name)
    /// are made under.
    limits: &'a explorer::Limits,
    cancel: &'a Cancel,
    progress: &'a P,
    started: Instant,
    /// Read every file back after sending it (see `Options::verify`).
    verify: bool,
    /// Files sent that could not be read back to a size: no size from the
    /// phone, or no answer about this one object.
    pub(super) unconfirmed: usize,
    /// Files inside merged folders that the phone already had.
    pub(super) already_there: usize,
    /// Files and folders that could not be sent while the rest went on.
    pub(super) failures: Vec<FileFailure>,
    /// Bytes of the file being sent that are already counted as done, until
    /// it is finished or fails.
    unfinished: u64,
    /// How long to wait on a phone that has stopped answering.
    pub(super) stall_limit: Duration,
    /// Told with every chunk sent, so requests queued behind this upload know
    /// the phone is working and not silent.
    pub(super) heard: device::Activity,
    /// Merging into a folder never deletes a different file of the same name:
    /// the new one goes in beside it under a numbered name.
    pub(super) keep_different: bool,
    /// Merging into a folder replaces a file only with a newer one; the rest
    /// of the phone's stay (`Conflict::ReplaceOlder`).
    pub(super) only_newer: bool,
    /// Files inside merged folders left as the phone had them because they
    /// were not older there: how many, and their bytes.
    pub(super) kept: (usize, u64),
    pub(super) state: TransferProgress,
    /// What became of each file, as the phone names it (at most
    /// `REPORT_MOST`; the rest counted in `report_left_out`).
    pub(super) report: Vec<FileReport>,
    /// For each line, the selected item it is in (`top` when it was noted).
    pub(super) report_tops: Vec<String>,
    pub(super) report_left_out: usize,
    /// The selected item being sent, by its own name.
    pub(super) top: String,
    /// The selected item went under a new name on the phone: (that name,
    /// its own), for the report's "was".
    pub(super) renamed: Option<(String, String)>,
    /// What is sent replaces what was there.
    pub(super) replacing: bool,
}

impl<'a, P: Fn(TransferProgress) + Sync> Sender<'a, P> {
    /// A sender that has sent nothing yet. `state` counts what it sends from
    /// here on; `verify` reads each file back from the phone.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        storage: &'a Storage,
        limits: &'a explorer::Limits,
        cancel: &'a Cancel,
        progress: &'a P,
        started: Instant,
        verify: bool,
        stall_limit: Duration,
        heard: device::Activity,
    ) -> Self {
        Self {
            storage,
            limits,
            cancel,
            progress,
            started,
            verify,
            unconfirmed: 0,
            already_there: 0,
            failures: Vec::new(),
            unfinished: 0,
            stall_limit,
            heard,
            keep_different: false,
            only_newer: false,
            kept: (0, 0),
            state: TransferProgress {
                direction: Direction::Upload,
                ..TransferProgress::default()
            },
            report: Vec::new(),
            report_tops: Vec::new(),
            report_left_out: 0,
            top: String::new(),
            renamed: None,
            replacing: false,
        }
    }

    /// Notes what became of the file (or folder) at `path` for the report.
    fn note(&mut self, path: &str, outcome: FileOutcome) {
        // Every path under a selected item starts with its name: under one
        // that took a new name, its own name was.
        let from = self.renamed.as_ref().and_then(|(new, own)| {
            path.strip_prefix(new.as_str())
                .map(|rest| format!("{own}{rest}"))
        });
        let line = FileReport {
            path: path.to_owned(),
            from,
            outcome,
            original: None,
        };
        if push_capped(&mut self.report, line, &mut self.report_left_out) {
            self.report_tops.push(self.top.clone());
        }
    }

    /// Forgets the bytes of a file that did not go through, so the next file
    /// starts from what was really sent.
    pub(super) fn abandon(&mut self) {
        self.state.bytes_done = self.state.bytes_done.saturating_sub(self.unfinished);
        self.state.file_bytes_done = 0;
        self.unfinished = 0;
    }

    /// A failure about one file, or one folder with all that was to go in it,
    /// is noted and the batch goes on; one about the connection, the phone's
    /// storage or the user's cancel is passed up to end it. What could not be
    /// sent is taken out of what is still owed, so progress stays honest.
    pub(super) fn carry_on(
        &mut self,
        result: Result<(), Failure>,
        label: &str,
        node: &LocalNode,
    ) -> Result<(), Failure> {
        let failure = match result {
            Ok(()) => return Ok(()),
            Err(failure) if failure.stops_the_batch() => return Err(failure),
            Err(failure) => failure,
        };
        // Bytes that did go before it failed are not part of any finished file.
        self.abandon();
        let (files, bytes) = node.files_and_bytes();
        self.state.files_total = self.state.files_total.saturating_sub(files);
        self.state.bytes_total = self.state.bytes_total.saturating_sub(bytes);
        let error = failure.into_error();
        self.note(label, FileOutcome::Failed(error.clone()));
        self.failures.push(FileFailure {
            label: label.to_owned(),
            error,
        });
        Ok(())
    }

    /// A file the phone already has: not sent, and not in what is left to do.
    /// Leaves the phone's file where `node` would have replaced it, it not
    /// being older: neither sent nor counted as there.
    fn keep_theirs(&mut self, node: &LocalNode, label: &str) {
        self.note(label, FileOutcome::Skipped);
        let (files, bytes) = node.files_and_bytes();
        self.kept = (self.kept.0 + files, self.kept.1 + bytes);
        self.state.files_total = self.state.files_total.saturating_sub(files);
        self.state.bytes_total = self.state.bytes_total.saturating_sub(bytes);
    }

    fn leave(&mut self, node: &LocalNode, label: &str) {
        self.note(label, FileOutcome::AlreadyThere);
        self.already_there += 1;
        self.state.files_total = self.state.files_total.saturating_sub(1);
        self.state.bytes_total = self.state.bytes_total.saturating_sub(node.size);
    }

    /// Copies `node` into `parent` as `name`.
    pub(super) async fn put(
        &mut self,
        node: &LocalNode,
        parent: Option<ObjectHandle>,
        name: &str,
        label: String,
    ) -> Result<(), Failure> {
        if self.cancel.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        if !node.is_folder {
            let sent = self.send_file(node, parent, name, label.clone()).await;
            return self.carry_on(sent, &label, node);
        }
        let made = phone_answers(
            &self.heard,
            self.stall_limit,
            self.storage.create_folder(parent, name),
        )
        .await
        .and_then(|made| made.map_err(Failure::Device));
        let folder = match made {
            Ok(folder) => folder,
            // Without the folder there is nowhere for its files to go.
            Err(failure) => return self.carry_on(Err(failure), &label, node),
        };
        for child in &node.children {
            Box::pin(self.put(
                child,
                Some(folder),
                &child.name,
                format!("{label}/{}", child.name),
            ))
            .await?;
        }
        Ok(())
    }

    /// Copies a folder's contents into an existing phone folder: same-named
    /// folders merge. A file that is there as it is (the very file, or the
    /// numbered copy of it an earlier try made) is left. Any other same-named
    /// item is replaced, or with `keep_different` kept, the new one going in
    /// beside it under a numbered name.
    async fn merge(
        &mut self,
        node: &LocalNode,
        into: ObjectHandle,
        label: String,
    ) -> Result<(), Failure> {
        let mut children = Children::new(
            explorer::read_children(self.storage, Some(into), self.limits)
                .await
                .map_err(Failure::Device)?,
        );
        for child in &node.children {
            let child_label = format!("{label}/{}", child.name);
            let found = children.named(&child.name).map(|e| {
                let newer = !e.is_folder() && is_newer(child.modified, e);
                (e.handle, e.is_folder(), is_on_phone(child, e), newer)
            });
            match found.map(|(handle, is_folder, same, _)| (handle, is_folder, same)) {
                Some((handle, true, _)) if child.is_folder => {
                    Box::pin(self.merge(child, handle, child_label)).await?;
                }
                Some((_, _, true)) => self.leave(child, &child_label),
                // Not older on the phone, or a file and a folder: it stays.
                Some(_)
                    if self.only_newer
                        && (child.is_folder || !found.is_some_and(|(.., newer)| newer)) =>
                {
                    self.keep_theirs(child, &child_label);
                }
                _ if self.keep_different
                    && !child.is_folder
                    && children
                        .copy_of(&child.name, child.size, child.modified)
                        .is_some() =>
                {
                    self.leave(child, &child_label);
                }
                Some(_) if self.keep_different => {
                    let name = children.claim(&child.name, child.is_folder);
                    Box::pin(self.put(child, Some(into), &name, child_label)).await?;
                }
                Some((handle, is_folder, _)) => {
                    let cleared =
                        explorer::delete_object(self.storage, self.limits, handle, is_folder)
                            .await
                            .map_err(Failure::Device);
                    match cleared {
                        Ok(()) => {
                            let replacing = std::mem::replace(&mut self.replacing, true);
                            let put =
                                Box::pin(self.put(child, Some(into), &child.name, child_label))
                                    .await;
                            self.replacing = replacing;
                            put?;
                        }
                        Err(failure) => self.carry_on(Err(failure), &child_label, child)?,
                    }
                }
                None => {
                    // (A name this run might give twice is not asked of the
                    // phone: it is taken here.)
                    let name = children.claim(&child.name, child.is_folder);
                    Box::pin(self.put(child, Some(into), &name, child_label)).await?;
                }
            }
        }
        Ok(())
    }

    /// Asks the phone about the file it was just sent and compares the size. A
    /// different size means the phone kept something else: it is removed and the
    /// file counts as failed. What cannot be read back (no size, no answer about
    /// this object) is counted as unconfirmed, and a move keeps its original.
    async fn read_back(&mut self, handle: ObjectHandle, node: &LocalNode) -> Result<(), Failure> {
        let info = phone_answers(
            &self.heard,
            self.stall_limit,
            self.storage.get_object_info(handle),
        )
        .await?;
        match judge(node.size, info.as_ref().map(|info| info.size)) {
            Judged::Confirmed => Ok(()),
            Judged::Unconfirmed => {
                self.unconfirmed += 1;
                Ok(())
            }
            Judged::Wrong => {
                // (A phone that has gone silent is not waited on for the tidying up.)
                let _ =
                    phone_answers(&self.heard, self.stall_limit, self.storage.delete(handle)).await;
                Err(Failure::Device(mtp_rs::Error::invalid_data(
                    "the phone kept a different size from the one sent",
                )))
            }
            Judged::Failed => Err(Failure::Device(
                info.expect_err("only an error is a failure to answer"),
            )),
        }
    }

    pub(super) async fn send_file(
        &mut self,
        node: &LocalNode,
        parent: Option<ObjectHandle>,
        name: &str,
        label: String,
    ) -> Result<(), Failure> {
        let file = tokio::fs::File::open(&node.path)
            .await
            .map_err(|err| Failure::Mac(err, node.path.clone()))?;
        let mut info = NewObjectInfo::file(name, node.size);
        info.modified = node.modified.and_then(mapper::to_device_time);

        self.unfinished = 0;
        self.state.current_file = label;
        self.state.finishing = false;
        self.state.file_bytes_total = node.size;
        self.state.file_bytes_done = 0;
        self.state.elapsed = self.started.elapsed();
        (self.progress)(self.state.clone());

        let base = self.state.bytes_done;
        let (cancel, progress, started) = (self.cancel, self.progress, self.started);
        let (heard, stall_limit) = (&self.heard, self.stall_limit);
        let state = &mut self.state;
        let unfinished = &mut self.unfinished;
        let upload = self.storage.upload_with_progress(
            parent,
            info,
            ReaderStream::with_capacity(file, CHUNK),
            |sent| {
                heard.heard();
                *unfinished = sent.bytes_transferred;
                state.file_bytes_done = sent.bytes_transferred;
                state.bytes_done = base + sent.bytes_transferred;
                // Everything is sent; what is left is the phone writing it
                // down, which can take seconds and should not look like a hang.
                state.finishing = sent.bytes_transferred >= node.size;
                state.elapsed = started.elapsed();
                progress(state.clone());
                if cancel.is_cancelled() {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
        );
        // A phone that went quiet can't be asked for anything more: the copy
        // gives up with a timeout, and the half-made object is left to it.
        // Quiet means nothing on the session made progress: each chunk sent
        // says so on the shared clock, so a slow upload is never given up on.
        let result = phone_answers(heard, stall_limit, upload).await?;

        match result {
            Ok(handle) => {
                let unconfirmed = self.unconfirmed;
                if self.verify {
                    self.read_back(handle, node).await?;
                }
                let outcome =
                    sent_outcome(self.replacing, self.verify, self.unconfirmed == unconfirmed);
                let path = self.state.current_file.clone();
                self.note(&path, outcome);
                self.unfinished = 0;
                self.state.finishing = false;
                self.state.files_done += 1;
                self.state.file_bytes_done = node.size;
                self.state.bytes_done = base + node.size;
                Ok(())
            }
            Err(err) => {
                // What went before it failed is not part of any finished file.
                self.abandon();
                // The phone may keep a half-written object; it is never useful.
                if let Some(partial) = err.partial {
                    let _ =
                        phone_answers(&self.heard, self.stall_limit, self.storage.delete(partial))
                            .await;
                }
                if self.cancel.is_cancelled() || matches!(err.source, mtp_rs::Error::Cancelled) {
                    Err(Failure::Cancelled)
                } else {
                    Err(Failure::Device(err.source))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::service::device;
    use crate::service::testing::virtual_phone;

    struct Setup {
        phone: tempfile::TempDir,
        mac: tempfile::TempDir,
        service: Service,
        storage_id: u64,
    }

    /// A phone with notes.txt ("old") and DCIM/{keep.jpg, a.jpg ("old")};
    /// a Mac folder with notes.txt ("new"), song.mp3 and DCIM/{a.jpg ("new"), b.jpg}.
    async fn setup_with(limit: impl FnOnce(&mut device::VirtualPhone)) -> Setup {
        let phone = tempfile::tempdir().unwrap();
        fs::create_dir_all(phone.path().join("DCIM")).unwrap();
        fs::write(phone.path().join("notes.txt"), b"old").unwrap();
        fs::write(phone.path().join("DCIM/keep.jpg"), b"keep").unwrap();
        fs::write(phone.path().join("DCIM/a.jpg"), b"old").unwrap();

        let mac = tempfile::tempdir().unwrap();
        fs::create_dir_all(mac.path().join("DCIM")).unwrap();
        fs::write(mac.path().join("notes.txt"), b"new").unwrap();
        fs::write(mac.path().join("song.mp3"), b"music").unwrap();
        fs::write(mac.path().join("DCIM/a.jpg"), b"new").unwrap();
        fs::write(mac.path().join("DCIM/b.jpg"), b"bb").unwrap();
        // Dated well before the phone's files, so none counts as the same file
        // unless a test makes it so.
        for name in ["notes.txt", "song.mp3", "DCIM/a.jpg", "DCIM/b.jpg"] {
            set_date(
                &mac.path().join(name),
                UNIX_EPOCH + Duration::from_secs(1_577_836_800),
            );
        }

        let mut config = virtual_phone(phone.path());
        limit(&mut config);
        let device = Arc::new(device::Service::new(device::Source::Virtual(config)));
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));

        Setup {
            phone,
            mac,
            service: Service::new(device, explorer),
            storage_id,
        }
    }

    async fn setup() -> Setup {
        setup_with(|_| {}).await
    }

    impl Setup {
        fn request(&self, names: &[&str], destination: &str) -> UploadRequest {
            UploadRequest {
                sources: names
                    .iter()
                    .map(|n| self.mac.path().join(n).to_string_lossy().into_owned())
                    .collect(),
                storage_id: self.storage_id,
                destination: destination.into(),
            }
        }

        async fn upload(
            &self,
            names: &[&str],
            conflict: Conflict,
        ) -> Result<TransferSummary, Error> {
            self.service
                .upload(&self.request(names, "/"), conflict, &Cancel::new(), |_| {})
                .await
        }

        fn on_phone(&self, path: &str) -> Vec<u8> {
            fs::read(self.phone.path().join(path)).unwrap()
        }
    }

    /// The report's lines as (path, its own name if another, what happened).
    fn lines(summary: &TransferSummary) -> Vec<(String, Option<String>, FileOutcome)> {
        summary
            .report
            .iter()
            .map(|line| (line.path.clone(), line.from.clone(), line.outcome.clone()))
            .collect()
    }

    /// `lines` by path: inside a folder, files go in the Mac's own order.
    fn sorted_lines(summary: &TransferSummary) -> Vec<(String, Option<String>, FileOutcome)> {
        let mut lines = lines(summary);
        lines.sort_by(|a, b| a.0.cmp(&b.0));
        lines
    }

    fn line(
        path: &str,
        from: Option<&str>,
        outcome: FileOutcome,
    ) -> (String, Option<String>, FileOutcome) {
        (path.into(), from.map(Into::into), outcome)
    }

    #[tokio::test]
    async fn an_uploads_report_names_each_file_as_the_phone_has_it() {
        let s = setup().await;
        let kept_both = s
            .upload(&["notes.txt", "DCIM", "song.mp3"], Conflict::KeepBoth)
            .await
            .unwrap();
        assert_eq!(
            sorted_lines(&kept_both),
            [
                line("DCIM (1)/a.jpg", Some("DCIM/a.jpg"), FileOutcome::Sent),
                line("DCIM (1)/b.jpg", Some("DCIM/b.jpg"), FileOutcome::Sent),
                line("notes (1).txt", Some("notes.txt"), FileOutcome::Sent),
                line("song.mp3", None, FileOutcome::Sent),
            ]
        );
        assert!(kept_both.report.iter().all(|line| line.original.is_none()));

        let s = setup().await;
        let replaced = s
            .upload(&["notes.txt", "DCIM"], Conflict::Replace)
            .await
            .unwrap();
        assert_eq!(
            sorted_lines(&replaced),
            [
                line("DCIM/a.jpg", None, FileOutcome::ReplacedOnPhone),
                line("DCIM/b.jpg", None, FileOutcome::Sent),
                line("notes.txt", None, FileOutcome::ReplacedOnPhone),
            ]
        );

        let s = setup().await;
        let skipped = s
            .upload(&["notes.txt", "DCIM", "song.mp3"], Conflict::Skip)
            .await
            .unwrap();
        assert_eq!(
            lines(&skipped),
            [
                line("notes.txt", None, FileOutcome::Skipped),
                line("DCIM", None, FileOutcome::Skipped),
                line("song.mp3", None, FileOutcome::Sent),
            ],
            "what was decided before sending comes first"
        );
    }

    #[test]
    fn a_sent_files_outcome_says_whether_it_replaced_one_and_was_read_back() {
        assert_eq!(
            sent_outcome(true, true, false),
            FileOutcome::ReplacedOnPhone
        );
        assert_eq!(
            sent_outcome(false, true, true),
            FileOutcome::Copied { checked: true }
        );
        assert_eq!(
            sent_outcome(false, true, false),
            FileOutcome::Copied { checked: false },
            "the phone gave no size back"
        );
        assert_eq!(sent_outcome(false, false, true), FileOutcome::Sent);
    }

    #[test]
    fn the_report_stops_growing_at_its_cap_and_counts_the_rest() {
        let mut list: Vec<usize> = (0..REPORT_MOST - 1).collect();
        let mut left_out = 0;
        assert!(push_capped(&mut list, 1, &mut left_out));
        assert!(!push_capped(&mut list, 2, &mut left_out));
        assert_eq!((list.len(), left_out), (REPORT_MOST, 1));
    }

    #[tokio::test]
    async fn what_the_phone_has_already_is_reported_there_at_the_top_and_inside_a_merge() {
        let s = setup().await;
        s.upload(&["song.mp3", "DCIM"], Conflict::Replace)
            .await
            .unwrap();

        // As a retry or a move finds what an earlier try sent.
        let again = s
            .service
            .upload_checked(
                &s.request(&["song.mp3", "DCIM"], "/"),
                Conflict::Merge,
                Options {
                    skip_identical: true,
                    verify: true,
                },
                &Cancel::new(),
                &|_| {},
            )
            .await
            .unwrap()
            .summary;
        let merged = s.upload(&["DCIM"], Conflict::Merge).await.unwrap();

        assert_eq!(
            lines(&again)[0],
            line("song.mp3", None, FileOutcome::AlreadyThere),
            "the very file, at the top"
        );
        assert!(
            sorted_lines(&merged)
                .iter()
                .filter(|(path, ..)| path.starts_with("DCIM/") && path != "DCIM/keep.jpg")
                .all(|(_, _, outcome)| *outcome == FileOutcome::AlreadyThere),
            "{:?}",
            merged.report
        );
        assert!(!merged.report.is_empty());
    }

    #[tokio::test]
    async fn replace_older_reports_what_the_phone_kept_and_a_failure_says_why() {
        let s = setup().await;
        let older = s
            .upload(&["notes.txt", "DCIM"], Conflict::ReplaceOlder)
            .await
            .unwrap();
        assert_eq!(
            sorted_lines(&older),
            [
                line("DCIM/a.jpg", None, FileOutcome::Skipped),
                line("DCIM/b.jpg", None, FileOutcome::Sent),
                line("notes.txt", None, FileOutcome::Skipped),
            ],
            "the Mac's are older: the phone's stay"
        );

        let s = setup().await;
        let locked = s.mac.path().join("DCIM/b.jpg");
        fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
        let partly = s
            .upload(&["song.mp3", "DCIM"], Conflict::Merge)
            .await
            .unwrap();
        fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
        let failed = partly
            .report
            .iter()
            .find(|line| line.path == "DCIM/b.jpg")
            .unwrap();
        assert!(
            matches!(&failed.outcome, FileOutcome::Failed(_)),
            "{:?}",
            failed.outcome
        );
        assert!(
            partly
                .report
                .iter()
                .any(|line| line.path == "song.mp3" && line.outcome == FileOutcome::Sent)
        );
    }

    #[test]
    fn an_uploads_report_lists_the_first_hundred_thousand_lines_and_counts_the_rest() {
        let report_line = |path: &str| FileReport {
            path: path.into(),
            from: None,
            outcome: FileOutcome::Sent,
            original: None,
        };
        let top = vec![(report_line("skipped"), "skipped".to_owned())];
        let sent: Vec<FileReport> = (0..REPORT_MOST).map(|_| report_line("x")).collect();
        let tops = vec!["Trip".to_owned(); REPORT_MOST];

        let (report, tops, left_out) = join_report(top, sent, tops, 7);

        assert_eq!(report.len(), REPORT_MOST);
        assert_eq!(tops.len(), REPORT_MOST);
        assert_eq!(report[0].path, "skipped", "decided first, listed first");
        assert_eq!(tops[1], "Trip");
        assert_eq!(
            left_out, 8,
            "one sent line that didn't fit, and seven never noted"
        );
        let (few, few_tops, none) =
            join_report(Vec::new(), vec![report_line("a")], vec!["a".into()], 0);
        assert_eq!((few.len(), few_tops.len(), none), (1, 1, 0));
    }

    #[tokio::test]
    async fn uploads_a_new_file() {
        let s = setup().await;

        let summary = s.upload(&["song.mp3"], Conflict::Skip).await.unwrap();

        assert_eq!(s.on_phone("song.mp3"), b"music");
        assert_eq!(summary.direction, Direction::Upload);
        assert_eq!((summary.files, summary.bytes), (1, 5));
        assert_eq!(summary.first_item.as_deref(), Some("song.mp3"));
        assert_eq!(
            summary.arrival,
            Some(crate::dto::Arrival {
                files: 1,
                bytes: 5,
                there: 1,
                checked: None,
                complete: true,
            }),
            "sent and taken, but a plain copy is not read back"
        );
    }

    #[tokio::test]
    async fn uploads_into_a_subfolder() {
        let s = setup().await;

        s.service
            .upload(
                &s.request(&["song.mp3"], "/DCIM"),
                Conflict::Skip,
                &Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(s.on_phone("DCIM/song.mp3"), b"music");
    }

    #[tokio::test]
    async fn existing_lists_taken_names_ignoring_case() {
        let s = setup().await;

        let taken = s
            .service
            .existing(&s.request(&["NOTES.txt", "song.mp3", "DCIM"], "/"))
            .await
            .unwrap();

        assert_eq!(taken, ["NOTES.txt", "DCIM"]);
    }

    #[tokio::test]
    async fn skip_leaves_the_existing_item_and_copies_the_rest() {
        let s = setup().await;

        let summary = s
            .upload(&["notes.txt", "song.mp3"], Conflict::Skip)
            .await
            .unwrap();

        assert_eq!(s.on_phone("notes.txt"), b"old");
        assert_eq!(s.on_phone("song.mp3"), b"music");
        assert_eq!((summary.skipped, summary.files), (1, 1));
        assert_eq!(summary.first_item.as_deref(), Some("song.mp3"));
    }

    #[tokio::test]
    async fn keep_both_copies_under_a_numbered_name() {
        let s = setup().await;

        let summary = s
            .upload(&["notes.txt", "DCIM"], Conflict::KeepBoth)
            .await
            .unwrap();

        assert_eq!(s.on_phone("notes.txt"), b"old");
        assert_eq!(s.on_phone("notes (1).txt"), b"new");
        assert_eq!(s.on_phone("DCIM (1)/a.jpg"), b"new");
        assert_eq!(summary.renamed, 2);
    }

    #[tokio::test]
    async fn replace_overwrites_a_file() {
        let s = setup().await;

        let summary = s.upload(&["notes.txt"], Conflict::Replace).await.unwrap();

        assert_eq!(s.on_phone("notes.txt"), b"new");
        assert_eq!(summary.replaced, 1);
    }

    #[tokio::test]
    async fn replace_merges_a_folder_into_the_existing_one() {
        let s = setup().await;

        s.upload(&["DCIM"], Conflict::Replace).await.unwrap();

        assert_eq!(s.on_phone("DCIM/keep.jpg"), b"keep");
        assert_eq!(s.on_phone("DCIM/a.jpg"), b"new");
        assert_eq!(s.on_phone("DCIM/b.jpg"), b"bb");
    }

    #[tokio::test]
    async fn merge_puts_a_folder_into_the_one_that_is_there_and_replaces_nothing() {
        let s = setup().await;

        let summary = s.upload(&["DCIM"], Conflict::Merge).await.unwrap();

        assert_eq!(s.on_phone("DCIM/keep.jpg"), b"keep");
        assert_eq!(s.on_phone("DCIM/a.jpg"), b"old", "a different file stays");
        assert_eq!(
            s.on_phone("DCIM/a (1).jpg"),
            b"new",
            "and this goes beside it"
        );
        assert_eq!(s.on_phone("DCIM/b.jpg"), b"bb");
        assert!(
            !s.phone.path().join("DCIM (1)").exists(),
            "no copy of the folder"
        );
        assert_eq!(
            (
                summary.merged,
                summary.replaced,
                summary.renamed,
                summary.files
            ),
            (1, 0, 0, 2)
        );
        assert_eq!(summary.first_item.as_deref(), Some("DCIM"));
    }

    #[tokio::test]
    async fn merging_twice_makes_no_second_numbered_copy() {
        let s = setup().await;
        s.upload(&["DCIM"], Conflict::Merge).await.unwrap();

        let again = s.upload(&["DCIM"], Conflict::Merge).await.unwrap();

        assert_eq!(again.files, 0, "{again:?}");
        assert_eq!(again.already_there, 2, "b.jpg, and a.jpg as `a (1).jpg`");
        let mut names: Vec<_> = fs::read_dir(s.phone.path().join("DCIM"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a (1).jpg", "a.jpg", "b.jpg", "keep.jpg"]);
    }

    #[tokio::test]
    async fn merge_keeps_a_different_file_and_a_file_where_a_folder_is_beside_it() {
        let s = setup().await;
        fs::create_dir_all(s.mac.path().join("Thing")).unwrap();
        fs::write(s.mac.path().join("Thing/x.txt"), b"x").unwrap();
        fs::write(s.phone.path().join("Thing"), b"a file").unwrap();
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);

        let summary = s
            .upload(&["notes.txt", "Thing"], Conflict::Merge)
            .await
            .unwrap();

        assert_eq!(s.on_phone("notes.txt"), b"old", "never replaced");
        assert_eq!(s.on_phone("notes (1).txt"), b"new");
        assert_eq!(s.on_phone("Thing"), b"a file");
        assert_eq!(s.on_phone("Thing (1)/x.txt"), b"x");
        assert_eq!(
            (summary.renamed, summary.merged, summary.replaced),
            (2, 0, 0)
        );
    }

    #[tokio::test]
    async fn merging_a_file_twice_does_not_number_it_again() {
        // notes.txt is taken by a different file, so the first try put the new
        // one beside it as `notes (1).txt`; the retry finds that copy.
        let s = setup().await;
        s.upload(&["notes.txt"], Conflict::Merge).await.unwrap();

        let again = s.upload(&["notes.txt"], Conflict::Merge).await.unwrap();

        assert_eq!((again.files, again.already_there), (0, 1), "{again:?}");
        assert_eq!(s.on_phone("notes (1).txt"), b"new");
        assert!(!s.phone.path().join("notes (2).txt").exists());
        assert_eq!(s.on_phone("notes.txt"), b"old");
    }

    #[tokio::test]
    async fn merge_puts_a_file_beside_a_folder_that_has_its_name() {
        let s = setup().await;
        fs::create_dir(s.phone.path().join("DCIM/b.jpg")).unwrap();
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);

        s.upload(&["DCIM"], Conflict::Merge).await.unwrap();

        assert!(s.phone.path().join("DCIM/b.jpg").is_dir(), "left alone");
        assert_eq!(s.on_phone("DCIM/b (1).jpg"), b"bb");
    }

    #[tokio::test]
    async fn merging_a_folder_of_five_thousand_files_finds_each_by_name() {
        // Every file is already there: the merge reads the phone's folder once
        // and looks each of 5,000 up in it, where a search for each was 25
        // million name comparisons.
        let s = setup().await;
        fs::create_dir_all(s.mac.path().join("Big")).unwrap();
        for i in 0..5_000 {
            fs::write(s.mac.path().join(format!("Big/{i:05}.jpg")), b"x").unwrap();
        }
        s.upload(&["Big"], Conflict::Skip).await.unwrap();
        let started = std::time::Instant::now();

        let again = s.upload(&["Big"], Conflict::Merge).await.unwrap();

        eprintln!("merge of 5,000, all there: {:?}", started.elapsed());
        assert_eq!(
            (again.files, again.already_there, again.merged),
            (0, 5_000, 1)
        );
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    #[tokio::test]
    async fn merging_replaces_a_folder_that_has_a_files_name() {
        let s = setup().await;
        fs::create_dir(s.phone.path().join("DCIM/b.jpg")).unwrap();
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);

        s.upload(&["DCIM"], Conflict::Replace).await.unwrap();

        assert_eq!(s.on_phone("DCIM/b.jpg"), b"bb");
    }

    #[tokio::test]
    async fn a_file_replaces_a_folder_of_the_same_name() {
        let s = setup().await;
        fs::write(s.mac.path().join("DCIM.txt"), b"x").unwrap();
        fs::remove_dir_all(s.mac.path().join("DCIM")).unwrap();
        fs::write(s.mac.path().join("DCIM"), b"file now").unwrap();

        s.upload(&["DCIM"], Conflict::Replace).await.unwrap();

        assert_eq!(s.on_phone("DCIM"), b"file now");
    }

    #[tokio::test]
    async fn progress_reaches_the_totals() {
        let s = setup().await;
        let log = Mutex::new(Vec::new());

        s.service
            .upload(
                &s.request(&["DCIM", "song.mp3"], "/DCIM"),
                Conflict::Skip,
                &Cancel::new(),
                |p| log.lock().unwrap().push(p),
            )
            .await
            .unwrap();

        let log = log.into_inner().unwrap();
        let last = log.last().unwrap();
        assert_eq!(last.direction, Direction::Upload);
        assert_eq!((last.files_done, last.files_total), (3, 3));
        assert_eq!((last.bytes_done, last.bytes_total), (10, 10));
        assert!(log.iter().any(|p| p.current_file == "DCIM/b.jpg"));
    }

    #[tokio::test]
    async fn a_file_that_did_not_go_through_is_forgotten_by_the_bytes_sent() {
        // A send that failed part way has already counted the bytes that went;
        // the next file must start from what was really sent.
        let s = setup().await;
        let session = s.service.device.session().await.unwrap();
        let storage = session
            .storage(mtp_rs::StorageId(s.storage_id))
            .await
            .unwrap();
        let (cancel, progress) = (Cancel::new(), |_: TransferProgress| {});
        let mut sender = Sender::new(
            &storage,
            &s.service.limits,
            &cancel,
            &progress,
            Instant::now(),
            false,
            Duration::from_secs(1),
            s.service.device.activity(),
        );
        sender.state.bytes_done = 100;
        sender.state.file_bytes_done = 40;
        sender.unfinished = 40;

        sender.abandon();

        assert_eq!(sender.state.bytes_done, 60);
        assert_eq!(sender.state.file_bytes_done, 0);
        assert_eq!(sender.unfinished, 0);
        sender.abandon();
        assert_eq!(sender.state.bytes_done, 60, "and only once");
    }

    #[tokio::test]
    async fn every_chunk_sent_tells_the_session_the_phone_is_alive() {
        // Requests queued behind a long upload (a listing, a delete) judge the
        // phone by this: without it they would call a busy phone silent.
        let s = setup().await;
        fs::write(s.mac.path().join("video.mp4"), vec![1u8; 4 * 1024 * 1024]).unwrap();
        let activity = s.service.device.activity();
        let heard = Mutex::new(Vec::new());

        s.service
            .upload(
                &s.request(&["video.mp4"], "/"),
                Conflict::Skip,
                &Cancel::new(),
                |p| {
                    // (A report that repeats the bytes done is not a new chunk.)
                    let mut heard = heard.lock().unwrap();
                    if p.file_bytes_done > 0
                        && heard
                            .last()
                            .is_none_or(|(sent, _)| *sent != p.file_bytes_done)
                    {
                        heard.push((p.file_bytes_done, activity.times_heard()));
                    }
                },
            )
            .await
            .unwrap();

        let heard: Vec<usize> = heard
            .into_inner()
            .unwrap()
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        assert!(heard.len() >= 3, "several chunks went over: {heard:?}");
        assert!(
            heard.windows(2).all(|pair| pair[0] < pair[1]),
            "each chunk was reported: {heard:?}"
        );
    }

    #[tokio::test]
    async fn cancelling_deletes_the_half_sent_file() {
        let s = setup().await;
        fs::write(s.mac.path().join("video.mp4"), vec![1u8; 3 * 1024 * 1024]).unwrap();
        let cancel = Cancel::new();
        let trigger = cancel.clone();

        let result = s
            .service
            .upload(
                &s.request(&["video.mp4"], "/"),
                Conflict::Skip,
                &cancel,
                |p| {
                    if p.file_bytes_done > 0 {
                        trigger.cancel();
                    }
                },
            )
            .await;

        assert_eq!(result, Err(Error::Cancelled));
        assert!(!s.phone.path().join("video.mp4").exists());
    }

    #[tokio::test]
    async fn mac_clutter_and_folder_links_stay_behind() {
        let s = setup().await;
        let album = s.mac.path().join("Album");
        fs::create_dir(&album).unwrap();
        fs::write(album.join(".DS_Store"), b"x").unwrap();
        fs::write(album.join("._a.jpg"), b"x").unwrap();
        fs::write(album.join("a.jpg"), b"a").unwrap();
        std::os::unix::fs::symlink(s.mac.path(), album.join("loop")).unwrap();
        std::os::unix::fs::symlink(s.mac.path().join("song.mp3"), album.join("song link.mp3"))
            .unwrap();

        let summary = s.upload(&["Album"], Conflict::Skip).await.unwrap();

        let mut names: Vec<String> = fs::read_dir(s.phone.path().join("Album"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.jpg", "song link.mp3"]);
        assert_eq!(s.on_phone("Album/song link.mp3"), b"music");
        assert_eq!(summary.files, 2);
    }

    /// When the phone's file `path` was last changed, as its disk says.
    fn phone_date(s: &Setup, path: &str) -> SystemTime {
        fs::metadata(s.phone.path().join(path))
            .unwrap()
            .modified()
            .unwrap()
    }

    #[tokio::test]
    async fn replace_older_replaces_only_what_is_newer_on_the_mac_and_merges_folders() {
        let s = setup().await;
        // notes.txt is newer on the Mac; DCIM/a.jpg (from 2020) is older.
        let newer = phone_date(&s, "notes.txt") + Duration::from_secs(3_600);
        set_date(&s.mac.path().join("notes.txt"), newer);

        let summary = s
            .upload(&["notes.txt", "DCIM"], Conflict::ReplaceOlder)
            .await
            .unwrap();

        assert_eq!(s.on_phone("notes.txt"), b"new", "newer: replaced");
        assert_eq!(s.on_phone("DCIM/a.jpg"), b"old", "older: the phone's stays");
        assert_eq!(s.on_phone("DCIM/b.jpg"), b"bb", "new: sent");
        assert_eq!(s.on_phone("DCIM/keep.jpg"), b"keep");
        assert!(
            !s.phone.path().join("DCIM (1)").exists(),
            "merged, not numbered"
        );
        assert_eq!(
            (
                summary.replaced,
                summary.merged,
                summary.skipped,
                summary.files
            ),
            (1, 1, 1, 2)
        );
        let arrival = summary.arrival.unwrap();
        assert_eq!(
            (arrival.files, arrival.there),
            (2, 2),
            "what was kept back was never meant to arrive"
        );
    }

    #[tokio::test]
    async fn replace_older_leaves_what_is_as_new_and_a_file_where_a_folder_is() {
        // A folder on the phone (there before it is opened) where the Mac
        // has a newer file, inside a merge.
        let s = setup_with(|phone| {
            fs::create_dir_all(phone.backing_dir.join("DCIM/sub")).unwrap();
        })
        .await;
        // Within the two seconds a file system rounds to: as new, though
        // different (a file of the same size and date would be the same one).
        fs::write(s.mac.path().join("notes.txt"), b"changed").unwrap();
        let as_new = phone_date(&s, "notes.txt") + Duration::from_secs(1);
        set_date(&s.mac.path().join("notes.txt"), as_new);
        fs::write(s.mac.path().join("DCIM/sub"), b"file").unwrap();
        set_date(
            &s.mac.path().join("DCIM/sub"),
            SystemTime::now() + Duration::from_secs(3_600),
        );

        let summary = s
            .upload(&["notes.txt", "DCIM"], Conflict::ReplaceOlder)
            .await
            .unwrap();

        assert_eq!(s.on_phone("notes.txt"), b"old");
        assert!(s.phone.path().join("DCIM/sub").is_dir(), "the folder stays");
        assert_eq!(summary.replaced, 0);
        assert_eq!(summary.skipped, 3, "notes.txt, sub and the older a.jpg");
        assert_eq!(s.on_phone("DCIM/b.jpg"), b"bb");
    }

    #[tokio::test]
    async fn replace_older_never_puts_a_file_for_a_folder_or_a_folder_for_a_file() {
        // The phone has a file DCIM/clip (inside a folder merged into); the
        // Mac a newer folder of that name.
        let s = setup_with(|phone| {
            fs::write(phone.backing_dir.join("DCIM/clip"), b"phone file").unwrap();
        })
        .await;
        fs::create_dir_all(s.mac.path().join("DCIM/clip")).unwrap();
        fs::write(s.mac.path().join("DCIM/clip/inside.jpg"), b"x").unwrap();
        let later = SystemTime::now() + Duration::from_secs(3_600);
        set_date(&s.mac.path().join("DCIM/clip/inside.jpg"), later);
        fs::File::open(s.mac.path().join("DCIM/clip"))
            .unwrap()
            .set_modified(later)
            .unwrap();
        // And at the top: a newer Mac file where the phone has the folder DCIM.
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("DCIM"), b"a file").unwrap();
        set_date(&other.path().join("DCIM"), later);

        s.upload(&["DCIM"], Conflict::ReplaceOlder).await.unwrap();
        let top = UploadRequest {
            sources: vec![other.path().join("DCIM").to_string_lossy().into_owned()],
            storage_id: s.storage_id,
            destination: "/".into(),
        };
        let summary = s
            .service
            .upload(&top, Conflict::ReplaceOlder, &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(s.on_phone("DCIM/clip"), b"phone file", "the file stays");
        assert!(s.phone.path().join("DCIM").is_dir(), "the folder stays");
        assert_eq!(s.on_phone("DCIM/keep.jpg"), b"keep");
        assert_eq!((summary.replaced, summary.skipped), (0, 1));
    }

    #[tokio::test]
    async fn files_keep_their_mac_date() {
        let s = setup().await;
        let when = UNIX_EPOCH + Duration::from_secs(1_791_376_200);
        fs::File::options()
            .write(true)
            .open(s.mac.path().join("song.mp3"))
            .unwrap()
            .set_modified(when)
            .unwrap();

        s.upload(&["song.mp3"], Conflict::Skip).await.unwrap();

        let on_phone = fs::metadata(s.phone.path().join("song.mp3"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(on_phone, when);
    }

    #[tokio::test]
    async fn a_read_only_storage_refuses_uploads() {
        let s = setup_with(|phone| phone.read_only = true).await;

        assert_eq!(
            s.upload(&["song.mp3"], Conflict::Skip).await,
            Err(Error::ReadOnly)
        );
    }

    #[tokio::test]
    async fn a_missing_mac_item_is_path_not_found_and_sends_nothing() {
        let s = setup().await;

        let result = s.upload(&["song.mp3", "ghost"], Conflict::Skip).await;

        let ghost = s.mac.path().join("ghost").to_string_lossy().into_owned();
        assert_eq!(result, Err(Error::PathNotFound(ghost)));
        assert!(!s.phone.path().join("song.mp3").exists());
    }

    #[tokio::test]
    async fn a_source_without_a_name_is_refused() {
        let s = setup().await;
        let request = UploadRequest {
            sources: vec!["/".into()],
            ..s.request(&[], "/")
        };

        let result = s
            .service
            .upload(&request, Conflict::Skip, &Cancel::new(), |_| {})
            .await;

        assert_eq!(result, Err(Error::UnsafeName("/".into())));
    }

    #[tokio::test]
    async fn sources_from_different_mac_folders_upload_together() {
        let s = setup().await;
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("other.txt"), b"other").unwrap();
        let mut request = s.request(&["song.mp3"], "/");
        request.sources.push(
            elsewhere
                .path()
                .join("other.txt")
                .to_string_lossy()
                .into_owned(),
        );

        let summary = s
            .service
            .upload(&request, Conflict::Skip, &Cancel::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(summary.files, 2);
        assert_eq!(s.on_phone("other.txt"), b"other");
        assert_eq!(s.on_phone("song.mp3"), b"music");
    }

    #[tokio::test]
    async fn the_scan_is_bounded() {
        let dir = tempfile::tempdir().unwrap();

        let too_deep = scan(dir.path().to_path_buf(), "x".into(), MAX_DEPTH + 1, &mut 0).await;
        let too_many = scan(
            dir.path().to_path_buf(),
            "x".into(),
            0,
            &mut MAX_ITEMS.clone(),
        )
        .await;

        assert!(matches!(too_deep, Err(Failure::TooLarge)));
        assert!(matches!(too_many, Err(Failure::TooLarge)));
    }

    #[tokio::test]
    async fn special_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );

        let scanned = scan(fifo, "pipe".into(), 0, &mut 0).await.unwrap();

        assert!(scanned.is_none());
    }

    #[test]
    fn free_names_skip_every_taken_one_ignoring_case() {
        let taken = ["a.jpg".to_owned(), "A (1).JPG".to_owned()];

        assert_eq!(free_name(&taken, "a.jpg", false), "a (2).jpg");
        assert_eq!(free_name(&[], "DCIM", true), "DCIM (1)");
    }

    #[test]
    fn clutter_is_recognised() {
        assert!(is_mac_clutter(".DS_Store"));
        assert!(is_mac_clutter("._IMG_001.jpg"));
        assert!(!is_mac_clutter(".nomedia"));
    }

    fn set_date(path: &Path, when: SystemTime) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    /// Dates a phone file and the Mac file with the same name alike, whole
    /// seconds, and tells the phone to look again.
    async fn make_alike(s: &Setup, name: &str, content: &[u8]) {
        fs::write(s.mac.path().join(name), content).unwrap();
        fs::write(s.phone.path().join(name), content).unwrap();
        let when = UNIX_EPOCH + Duration::from_secs(1_791_376_200);
        set_date(&s.mac.path().join(name), when);
        set_date(&s.phone.path().join(name), when);
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);
    }

    #[tokio::test]
    async fn a_file_the_phone_already_has_is_not_a_conflict() {
        let s = setup().await;
        make_alike(&s, "song.mp3", b"music").await;

        let taken = s
            .service
            .existing(&s.request(&["song.mp3", "notes.txt"], "/"))
            .await
            .unwrap();

        assert_eq!(
            taken,
            ["notes.txt"],
            "song.mp3 is already there, notes.txt differs"
        );
    }

    #[tokio::test]
    async fn a_file_the_phone_already_has_is_left_alone_whatever_the_choice() {
        for conflict in [
            Conflict::Skip,
            Conflict::KeepBoth,
            Conflict::Replace,
            Conflict::Merge,
        ] {
            let s = setup().await;
            make_alike(&s, "song.mp3", b"music").await;

            let summary = s.upload(&["song.mp3"], conflict).await.unwrap();

            assert_eq!(
                (summary.already_there, summary.files, summary.bytes),
                (1, 0, 0),
                "{conflict:?}"
            );
            let arrival = summary.arrival.unwrap();
            assert_eq!(
                (arrival.files, arrival.bytes, arrival.there),
                (1, 5, 1),
                "{conflict:?}: what was meant to go is there"
            );
            assert_eq!(
                (summary.skipped, summary.renamed, summary.replaced),
                (0, 0, 0)
            );
            assert!(
                !s.phone.path().join("song (1).mp3").exists(),
                "{conflict:?}"
            );
            assert_eq!(s.on_phone("song.mp3"), b"music");
        }
    }

    #[tokio::test]
    async fn the_same_size_with_another_date_is_still_a_conflict() {
        let s = setup().await;
        // notes.txt is 3 bytes on both sides but dated differently.
        let taken = s
            .service
            .existing(&s.request(&["notes.txt"], "/"))
            .await
            .unwrap();

        assert_eq!(taken, ["notes.txt"]);
    }

    #[tokio::test]
    async fn a_folder_is_always_a_conflict_even_when_everything_in_it_is_there() {
        let s = setup().await;
        make_alike(&s, "DCIM/keep.jpg", b"keep").await;

        let taken = s
            .service
            .existing(&s.request(&["DCIM"], "/"))
            .await
            .unwrap();

        assert_eq!(taken, ["DCIM"]);
    }

    #[tokio::test]
    async fn merging_a_folder_sends_only_what_the_phone_lacks_or_has_different() {
        let s = setup().await;
        make_alike(&s, "DCIM/keep.jpg", b"keep").await;
        let log = Mutex::new(Vec::new());

        let summary = s
            .service
            .upload(
                &s.request(&["DCIM"], "/"),
                Conflict::Replace,
                &Cancel::new(),
                |p| log.lock().unwrap().push(p),
            )
            .await
            .unwrap();

        assert_eq!(s.on_phone("DCIM/keep.jpg"), b"keep");
        assert_eq!(
            s.on_phone("DCIM/a.jpg"),
            b"new",
            "a different file is replaced"
        );
        assert_eq!(s.on_phone("DCIM/b.jpg"), b"bb", "a missing one is added");
        assert_eq!((summary.files, summary.already_there), (2, 1));
        assert_eq!(summary.bytes, 3 + 2, "keep.jpg's bytes were not sent");
        let reports = log.into_inner().unwrap();
        let last = reports.last().unwrap();
        assert_eq!(
            (last.files_done, last.files_total),
            (2, 2),
            "the skipped one left the totals"
        );
        assert_eq!((last.bytes_done, last.bytes_total), (5, 5));
    }

    #[tokio::test]
    async fn uploading_the_same_folder_twice_sends_nothing_the_second_time() {
        let s = setup().await;
        s.upload(&["DCIM"], Conflict::Replace).await.unwrap();
        // The phone now has what was sent, with the Mac's dates.
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);

        let again = s.upload(&["DCIM"], Conflict::Replace).await.unwrap();

        assert_eq!((again.files, again.bytes), (0, 0));
        assert_eq!(
            again.already_there, 2,
            "a.jpg and b.jpg, which the first upload sent"
        );
    }

    /// Makes a Mac file unreadable.
    fn lock(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
    }

    fn unlock(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[tokio::test]
    async fn one_file_that_cannot_be_read_does_not_stop_the_rest() {
        let s = setup().await;
        let locked = s.mac.path().join("locked.bin");
        fs::write(&locked, b"secret").unwrap();
        lock(&locked);

        let summary = s
            .upload(&["locked.bin", "song.mp3"], Conflict::Skip)
            .await
            .unwrap();
        unlock(&locked);

        assert_eq!(s.on_phone("song.mp3"), b"music");
        assert!(
            !s.phone.path().join("locked.bin").exists(),
            "no half-made object"
        );
        assert_eq!(summary.files, 1);
        assert_eq!(summary.failures.len(), 1);
        assert_eq!(summary.failures[0].label, "locked.bin");
        assert!(matches!(
            summary.failures[0].error,
            Error::FolderAccessDenied(_)
        ));
    }

    #[tokio::test]
    async fn a_file_inside_a_folder_that_cannot_be_read_is_one_failure_among_many() {
        let s = setup().await;
        let locked = s.mac.path().join("DCIM/locked.bin");
        fs::write(&locked, b"secret").unwrap();
        lock(&locked);
        let log = Mutex::new(Vec::new());

        let summary = s
            .service
            .upload(
                &s.request(&["DCIM"], "/"),
                Conflict::Replace,
                &Cancel::new(),
                |p| log.lock().unwrap().push(p),
            )
            .await
            .unwrap();
        unlock(&locked);

        assert_eq!(s.on_phone("DCIM/a.jpg"), b"new");
        assert_eq!(s.on_phone("DCIM/b.jpg"), b"bb");
        assert_eq!(summary.files, 2);
        assert_eq!(summary.failures.len(), 1);
        assert_eq!(summary.failures[0].label, "DCIM/locked.bin");
        let reports = log.into_inner().unwrap();
        let last = reports.last().unwrap();
        assert_eq!(
            (last.files_done, last.files_total),
            (2, 2),
            "the failed one is no longer owed"
        );
        assert_eq!(last.bytes_done, last.bytes_total);
    }

    #[tokio::test]
    async fn retrying_after_an_upload_failure_sends_only_what_failed() {
        let s = setup().await;
        let locked = s.mac.path().join("locked.bin");
        fs::write(&locked, b"secret").unwrap();
        lock(&locked);
        let first = s
            .upload(&["locked.bin", "song.mp3"], Conflict::Skip)
            .await
            .unwrap();
        assert_eq!(first.failures.len(), 1);
        unlock(&locked);
        mtp_rs::rescan_virtual_device(&s.service.device.connect().await.unwrap().serial_number);

        let second = s
            .upload(&["locked.bin", "song.mp3"], Conflict::Skip)
            .await
            .unwrap();

        assert!(second.failures.is_empty());
        assert_eq!(
            (second.files, second.already_there),
            (1, 1),
            "locked.bin is sent; song.mp3 is already there"
        );
        assert_eq!(s.on_phone("locked.bin"), b"secret");
    }

    #[tokio::test]
    async fn a_single_file_that_cannot_be_read_is_the_uploads_own_error() {
        let s = setup().await;
        let locked = s.mac.path().join("locked.bin");
        fs::write(&locked, b"secret").unwrap();
        lock(&locked);

        let result = s.upload(&["locked.bin"], Conflict::Skip).await;
        unlock(&locked);

        assert!(
            matches!(result, Err(Error::FolderAccessDenied(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_phone_that_resets_mid_upload_stops_the_batch() {
        let s = setup().await;
        let serial = s.service.device.connect().await.unwrap().serial_number;
        let armed = Mutex::new(false);

        let result = s
            .service
            .upload(
                &s.request(&["song.mp3", "DCIM"], "/"),
                Conflict::Replace,
                &Cancel::new(),
                |p| {
                    if p.file_bytes_total > 0
                        && !std::mem::replace(&mut *armed.lock().unwrap(), true)
                    {
                        mtp_rs::force_operation_wedge(&serial);
                    }
                },
            )
            .await;

        assert_eq!(result, Err(Error::Disconnected));
        assert!(
            !s.phone.path().join("DCIM/b.jpg").exists(),
            "the batch did not go on"
        );
    }

    #[tokio::test]
    async fn the_last_bytes_are_reported_as_finishing() {
        let s = setup().await;
        let log = Mutex::new(Vec::new());

        s.service
            .upload(
                &s.request(&["song.mp3"], "/"),
                Conflict::Skip,
                &Cancel::new(),
                |p| log.lock().unwrap().push(p),
            )
            .await
            .unwrap();

        let reports = log.into_inner().unwrap();
        let sent_all = reports
            .iter()
            .find(|p| p.file_bytes_total > 0 && p.file_bytes_done == p.file_bytes_total)
            .expect("a report with everything sent");
        assert!(
            sent_all.finishing,
            "everything is sent; the phone is writing it down"
        );
        assert!(
            reports
                .iter()
                .filter(|p| p.file_bytes_done < p.file_bytes_total)
                .all(|p| !p.finishing),
            "not before"
        );
    }

    #[tokio::test]
    async fn a_phone_that_goes_quiet_mid_upload_is_given_up_on() {
        let started = Instant::now();

        let result = phone_answers(
            &device::Activity::default(),
            Duration::from_millis(80),
            std::future::pending::<()>(),
        )
        .await;

        assert!(matches!(
            result,
            Err(Failure::Device(mtp_rs::Error::Timeout))
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn steady_progress_keeps_a_slow_upload_from_being_given_up_on() {
        let started = Instant::now();
        let activity = device::Activity::default();
        // Takes far longer than the limit, but never goes quiet for the limit:
        // each chunk says so on the shared clock, as the upload's callback does.
        let slow = async {
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(40)).await;
                activity.heard();
            }
            "done"
        };

        let result = phone_answers(&activity, Duration::from_millis(120), slow).await;

        assert!(matches!(result, Ok("done")));
        assert!(started.elapsed() > Duration::from_millis(300));
    }

    #[tokio::test]
    async fn finished_work_is_returned_at_once() {
        let result = phone_answers(
            &device::Activity::default(),
            Duration::from_secs(60),
            async { 5 },
        )
        .await;

        assert!(matches!(result, Ok(5)));
    }

    #[tokio::test]
    async fn a_cancel_during_the_last_upload_is_a_cancel_not_a_listed_failure() {
        let s = setup().await;
        let big = s.mac.path().join("big.bin");
        fs::write(&big, vec![1u8; 4 * 1024 * 1024]).unwrap();
        let cancel = Cancel::new();
        let trigger = cancel.clone();

        let result = s
            .service
            .upload(
                &s.request(&["song.mp3", "big.bin"], "/"),
                Conflict::Skip,
                &cancel,
                |p| {
                    if p.file_bytes_total > 1_000_000 && p.file_bytes_done > 0 {
                        trigger.cancel();
                    }
                },
            )
            .await;

        assert_eq!(result, Err(Error::Cancelled));
        assert_eq!(
            s.on_phone("song.mp3"),
            b"music",
            "the finished file is kept"
        );
        assert!(
            !s.phone.path().join("big.bin").exists(),
            "no half-sent file is left"
        );
    }

    /// The pane showing a phone folder in full.
    async fn pane_shows(s: &Setup, path: &str) -> Vec<String> {
        let mut listing = s
            .service
            .explorer
            .open_listing(s.storage_id, path, false, Cancel::new(), true)
            .await
            .unwrap();
        let mut names = Vec::new();
        while let Some(update) = listing.next().await {
            names.extend(update.unwrap().entries.into_iter().map(|e| e.name));
        }
        names.sort();
        names
    }

    #[tokio::test]
    async fn checking_names_and_uploading_new_files_read_nothing_of_a_remembered_folder() {
        let s = setup().await;
        pane_shows(&s, "/").await;
        let reads = s.service.explorer.full_reads();

        let taken = s
            .service
            .existing(&s.request(&["song.mp3", "notes.txt"], "/"))
            .await
            .unwrap();
        let summary = s.upload(&["song.mp3"], Conflict::Skip).await.unwrap();

        assert_eq!(taken, ["notes.txt"]);
        assert_eq!(summary.files, 1);
        assert_eq!(
            s.service.explorer.full_reads(),
            reads,
            "no whole-folder read"
        );
    }

    #[tokio::test]
    async fn after_an_upload_the_folder_is_read_again_and_shows_what_was_sent() {
        let s = setup().await;
        pane_shows(&s, "/").await;
        let reads = s.service.explorer.full_reads();

        s.upload(&["song.mp3"], Conflict::Skip).await.unwrap();
        let after = pane_shows(&s, "/").await;

        assert!(after.contains(&"song.mp3".to_owned()), "{after:?}");
        assert_eq!(
            s.service.explorer.full_reads(),
            reads + 1,
            "an upload changes the folder, so it was read for real once"
        );
    }

    #[tokio::test]
    async fn an_upload_that_fails_still_forgets_the_folder() {
        let s = setup_with(|phone| phone.read_only = true).await;
        pane_shows(&s, "/").await;
        let reads = s.service.explorer.full_reads();

        let result = s.upload(&["song.mp3"], Conflict::Skip).await;
        pane_shows(&s, "/").await;

        assert!(result.is_err());
        assert_eq!(s.service.explorer.full_reads(), reads + 1);
    }

    #[tokio::test]
    async fn replacing_a_remembered_file_checks_it_with_the_phone_first() {
        let s = setup().await;
        pane_shows(&s, "/").await;

        let summary = s.upload(&["notes.txt"], Conflict::Replace).await.unwrap();

        assert_eq!(summary.replaced, 1);
        assert_eq!(s.on_phone("notes.txt"), b"new");
    }

    #[tokio::test]
    async fn replacing_a_file_that_was_swapped_on_the_phone_acts_on_the_right_one() {
        let s = setup().await;
        pane_shows(&s, "/").await;
        // The phone re-keys notes.txt (its media scanner does this).
        assert!(
            mtp_rs::rekey_virtual_object(
                &s.service.device.connect().await.unwrap().serial_number,
                Path::new("notes.txt")
            )
            .is_some()
        );

        let summary = s.upload(&["notes.txt"], Conflict::Replace).await.unwrap();

        assert_eq!(summary.replaced, 1);
        assert_eq!(s.on_phone("notes.txt"), b"new");
        assert_eq!(
            fs::read_dir(s.phone.path())
                .unwrap()
                .filter(|e| e.as_ref().unwrap().file_name() == "notes.txt")
                .count(),
            1
        );
    }

    #[test]
    fn reading_a_file_back_compares_what_the_phone_says_with_what_was_sent() {
        assert_eq!(read_back(5, 5), ReadBack::Matches);
        assert_eq!(
            read_back(0, 0),
            ReadBack::Matches,
            "an empty file is a file"
        );
        assert_eq!(read_back(5, 4), ReadBack::Differs);
        assert_eq!(read_back(5, 6), ReadBack::Differs);
        // 4 GB or more: some phones give no size, so nothing can be compared,
        // not even with a file that happens to be 0xFFFFFFFF bytes long.
        assert_eq!(read_back(5_000_000_000, 0xFFFF_FFFF), ReadBack::Unknown);
        assert_eq!(read_back(0xFFFF_FFFF, 0xFFFF_FFFF), ReadBack::Unknown);
        assert_eq!(read_back(5_000_000_000, 5_000_000_000), ReadBack::Matches);
    }

    #[test]
    fn what_the_phone_says_about_a_sent_file_is_judged_before_a_move_trusts_it() {
        // The size matches: the copy can be trusted.
        assert_eq!(judge(1_000, Ok(1_000)), Judged::Confirmed);
        // The phone says 4 GB or more without saying how much, for a file of
        // any size (even one exactly that long): nothing can be compared, so
        // a move keeps its original.
        assert_eq!(judge(5_000_000_000, Ok(0xFFFF_FFFF)), Judged::Unconfirmed);
        assert_eq!(judge(0xFFFF_FFFF, Ok(0xFFFF_FFFF)), Judged::Unconfirmed);
        // The phone kept something else.
        assert_eq!(judge(1_000, Ok(999)), Judged::Wrong);
        // No answer about this one object (or a stale handle): unconfirmed.
        assert_eq!(
            judge(1_000, Err(&mtp_rs::Error::NotFound)),
            Judged::Unconfirmed
        );
        // The phone as a whole does not answer: not about this file.
        assert_eq!(judge(1_000, Err(&mtp_rs::Error::Timeout)), Judged::Failed);
    }
}
